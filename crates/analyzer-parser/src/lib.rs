mod type_index;
use type_index::*;
mod syntax;
use syntax::*;
mod support;
use support::*;
pub use support::{language_name, Dialect};
mod architecture;
mod bindings;
mod cost;
mod cost_dynamic;
mod imports;
mod js_bindings;
mod php_names;
mod php_traits;
mod properties;
pub mod runtime;
mod runtime_symbols;
mod rust_types;
mod static_js;
mod types;
use analyzer_core::ir::*;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;
use tree_sitter::{Language, Node, Parser, Tree};
pub use types::{reference as type_reference, reference_syntax};

thread_local! {static LAST_TREE:RefCell<Option<(u64,Tree)>>=const{RefCell::new(None)};}
fn parse_key(source: &str, path: &Path, dialect: Dialect) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    path.hash(&mut hasher);
    dialect.hash(&mut hasher);
    hasher.finish()
}

pub fn parse(source: &str, path: &Path, language: Language, dialect: Dialect) -> RawAst {
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .expect("bundled grammar must match tree-sitter");
    let tree = parser
        .parse(source, None)
        .expect("tree-sitter parser did not return a tree");
    LAST_TREE
        .with(|slot| *slot.borrow_mut() = Some((parse_key(source, path, dialect), tree.clone())));
    let mut errors = Vec::new();
    let root = if exceeds_recursive_depth(tree.root_node()) {
        errors.push(ParseError {
            message: format!(
                "detailed AST incomplete: tree exceeds supported depth {MAX_RECURSIVE_DEPTH}"
            ),
            range: range(tree.root_node(), path),
        });
        AstNode {
            kind: ast_kind(tree.root_node().kind(), dialect),
            raw_kind: tree.root_node().kind().into(),
            range: range(tree.root_node(), path),
            text: None,
            children: Vec::new(),
        }
    } else {
        ast_node(tree.root_node(), source, path, dialect, &mut errors)
    };
    let mut recovered_errors = Vec::new();
    collect_errors(tree.root_node(), path, &mut recovered_errors);
    for error in recovered_errors {
        if !errors
            .iter()
            .any(|existing| existing.message == error.message && existing.range == error.range)
        {
            errors.push(error);
        }
    }
    RawAst { root, errors }
}

pub fn normalize(source: &str, path: &Path, language: Language, dialect: Dialect) -> FileModel {
    normalize_with_architecture(source, path, language, dialect, false)
}

pub fn normalize_with_architecture(
    source: &str,
    path: &Path,
    language: Language,
    dialect: Dialect,
    enabled: bool,
) -> FileModel {
    let key = parse_key(source, path, dialect);
    let tree = LAST_TREE
        .with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.as_ref().is_some_and(|(cached, _)| *cached == key) {
                slot.take().map(|(_, tree)| tree)
            } else {
                None
            }
        })
        .unwrap_or_else(|| {
            let mut parser = Parser::new();
            parser
                .set_language(&language)
                .expect("bundled grammar must match tree-sitter");
            parser
                .parse(source, None)
                .expect("tree-sitter parser did not return a tree")
        });
    let path_string = path.to_string_lossy().into_owned();
    let module = module_name(path, dialect);
    let comments = comment_nodes(tree.root_node());
    let mut model = FileModel {
        path: path_string,
        language: language_name(dialect).into(),
        module,
        type_reference_syntax: reference_syntax(dialect),
        loc: count_lines_with_comments(source, &comments),
        comment_ranges: Some(comments.iter().map(|node| range(*node, path)).collect()),
        ..Default::default()
    };
    let mut state = State {
        source,
        path,
        dialect,
        model: &mut model,
        anonymous: 0,
        namespace: String::new(),
        guard_contexts: BTreeMap::new(),
        loop_contexts: BTreeMap::new(),
        cost_type_shadows: if dialect == Dialect::Rust {
            cost::type_shadows(tree.root_node(), source)
        } else {
            cost_dynamic::shadows(tree.root_node(), source, dialect)
        },
    };
    state.collect(tree.root_node(), None, None);
    state.disambiguate_rust_methods();
    state.add_module_initializer(tree.root_node());
    imports::collect_requires(
        tree.root_node(),
        source,
        path,
        &mut state.model.imports,
        dialect,
    );
    if dialect == Dialect::Rust {
        state.resolve_rust_trait_visibility(tree.root_node());
    }
    state.annotate_bindings(tree.root_node());
    if dialect == Dialect::Rust {
        rust_types::annotate(tree.root_node(), source, state.model);
    }
    state.resolve_local_types();
    if matches!(dialect, Dialect::JavaScript | Dialect::TypeScript) {
        js_bindings::annotate(tree.root_node(), source, state.model);
    }
    let mut errors = Vec::new();
    collect_errors(tree.root_node(), path, &mut errors);
    state.model.parse_errors.extend(errors);
    state.model.functions.sort_by(|a, b| {
        a.range
            .start_line
            .cmp(&b.range.start_line)
            .then(a.range.start_column.cmp(&b.range.start_column))
            .then(a.fqn.cmp(&b.fqn))
    });
    if dialect == Dialect::TypeScript {
        let concrete: BTreeSet<String> = state
            .model
            .functions
            .iter()
            .filter(|f| f.body_range != f.range)
            .map(|f| f.fqn.clone())
            .collect();
        let mut seen = BTreeSet::new();
        state.model.functions.retain(|f| {
            if concrete.contains(&f.fqn) && f.body_range == f.range {
                false
            } else {
                seen.insert(f.fqn.clone())
            }
        });
    }
    state.model.types.sort_by(|a, b| {
        a.range
            .start_line
            .cmp(&b.range.start_line)
            .then(a.fqn.cmp(&b.fqn))
    });
    state
        .model
        .imports
        .sort_by(|a, b| a.path.cmp(&b.path).then(a.alias.cmp(&b.alias)));
    imports::collect_exports(tree.root_node(), source, &mut model, dialect);
    if enabled {
        model.architecture_facts = Some(Arc::new(architecture::extract(
            tree.root_node(),
            &model,
            source,
            path,
            dialect,
        )));
    }
    model
}

pub fn count_lines(source: &str, dialect: Dialect) -> Loc {
    if source.is_empty() {
        return Loc::default();
    }
    let language = match dialect {
        Dialect::Python => tree_sitter_python::LANGUAGE.into(),
        Dialect::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Dialect::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Dialect::Php => tree_sitter_php::LANGUAGE_PHP.into(),
        Dialect::Rust => tree_sitter_rust::LANGUAGE.into(),
    };
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .expect("bundled grammar must match tree-sitter");
    let tree = parser
        .parse(source, None)
        .expect("tree-sitter parser did not return a tree");
    count_lines_with_comments(source, &comment_nodes(tree.root_node()))
}

pub fn resolve_call(call: &CallSite, file: &FileModel, dialect: Dialect) -> Vec<String> {
    if call.binding_unknown {
        return Vec::new();
    }
    if dialect == Dialect::Php
        && call.kind == CallKind::Function
        && call.receiver.is_none()
        && !call.local_binding
        && call.name.starts_with('\\')
    {
        // Absolute function names share their declaration's canonical spelling,
        // independently of the caller's namespace and same-name local symbols.
        return vec![call.name.trim_start_matches('\\').to_owned()];
    }
    if call.kind != CallKind::Constructor {
        return call.targets.clone();
    }
    if dialect == Dialect::Php && !call.targets.is_empty() {
        return call
            .targets
            .iter()
            .map(|target| format!("{}::__construct", target.trim_start_matches('\\')))
            .collect();
    }
    let constructor = match dialect {
        Dialect::Python => "__init__",
        Dialect::JavaScript | Dialect::TypeScript => "constructor",
        Dialect::Php => "__construct",
        Dialect::Rust => "new",
    };
    let separator = match dialect {
        Dialect::Php => "::",
        Dialect::Rust => "::",
        _ => ".",
    };
    let mut candidates = Vec::new();
    for ty in &file.types {
        if ty.fqn == call.name || call.targets.contains(&ty.fqn) {
            candidates.push(format!("{}{separator}{constructor}", ty.fqn));
        }
    }
    for import in file
        .imports
        .iter()
        .filter(|i| crate::support::import_applies(i, &call.range))
    {
        let short = import.alias.as_deref().or_else(|| {
            import
                .path
                .rsplit(['.', ':', '\\', '/'])
                .find(|s| !s.is_empty())
        });
        if short == Some(call.name.as_str()) {
            candidates.push(format!("{}{separator}{constructor}", import.path));
        }
    }
    candidates.sort();
    candidates.dedup();
    candidates
}

// Explicit body frames keep normalization iterative on deeply nested inputs.
enum BodyWork<'tree> {
    Enter(Node<'tree>, Arc<[Guard]>, Arc<[LoopContext]>, usize),
    Children {
        node: Node<'tree>,
        children: std::vec::IntoIter<Node<'tree>>,
        guards: Arc<[Guard]>,
        loops: Arc<[LoopContext]>,
        depth: usize,
    },
}
struct BodyEntry<'tree> {
    node: Node<'tree>,
    guards: Arc<[Guard]>,
    loops: Arc<[LoopContext]>,
    depth: usize,
}
struct BodyChildren<'tree> {
    node: Node<'tree>,
    children: std::vec::IntoIter<Node<'tree>>,
    guards: Arc<[Guard]>,
    loops: Arc<[LoopContext]>,
    depth: usize,
}
impl<'tree> BodyChildren<'tree> {
    fn into_work(self) -> BodyWork<'tree> {
        BodyWork::Children {
            node: self.node,
            children: self.children,
            guards: self.guards,
            loops: self.loops,
            depth: self.depth,
        }
    }
}
struct State<'a> {
    source: &'a str,
    path: &'a Path,
    dialect: Dialect,
    model: &'a mut FileModel,
    anonymous: usize,
    namespace: String,
    guard_contexts: BTreeMap<Arc<[Guard]>, Arc<[Guard]>>,
    loop_contexts: BTreeMap<Arc<[LoopContext]>, Arc<[LoopContext]>>,
    cost_type_shadows: BTreeSet<String>,
}

impl State<'_> {
    fn add_module_initializer(&mut self, root: Node<'_>) {
        if self.dialect == Dialect::Rust {
            return;
        }
        for (owner, nodes) in self.module_regions(root) {
            let Some(first) = nodes.first() else { continue };
            let last = nodes.last().unwrap();
            let mut source_range = range(*first, self.path);
            let end = range(*last, self.path);
            source_range.end_line = end.end_line;
            source_range.end_column = end.end_column;
            let separator = if self.dialect == Dialect::Php {
                "\\"
            } else {
                "."
            };
            let base_fqn = format!("{owner}{separator}<module_initializer>");
            let count = self
                .model
                .functions
                .iter()
                .filter(|f| f.fqn == base_fqn || f.fqn.starts_with(&format!("{base_fqn}#")))
                .count();
            let fqn = if count == 0 {
                base_fqn
            } else {
                format!("{base_fqn}#{}", count + 1)
            };
            let mut function = FunctionModel {
                name: "<module_initializer>".into(),
                fqn,
                range: source_range.clone(),
                body_range: source_range,
                signature: "module initialization".into(),
                modifiers: vec!["module_initializer".into(), "synthetic".into()],
                complexity: 1,
                cognitive_complexity: Some(0),
                ..Default::default()
            };
            for node in nodes {
                self.walk_body(node, &mut function, &[], &[], 0);
            }
            // Declaration/import-only modules need no artificial executable entity.
            // Property sites are retained separately and become edges only for known
            // accessors; they are still actual runtime operations worth representing.
            if !function.calls.is_empty()
                || !function.property_accesses.is_empty()
                || !function.property_mutations.is_empty()
            {
                self.model.functions.push(function);
            }
        }
    }

    fn module_regions<'tree>(&self, root: Node<'tree>) -> Vec<(String, Vec<Node<'tree>>)> {
        if self.dialect != Dialect::Php {
            return vec![(self.model.module.clone(), vec![root])];
        }
        let mut regions = Vec::new();
        let mut owner = String::new();
        let mut nodes = Vec::new();
        for node in named_children(root) {
            if node.kind() == "namespace_definition" {
                if !nodes.is_empty() {
                    regions.push((owner.clone(), std::mem::take(&mut nodes)));
                }
                owner = text_field(node, "name", self.source).unwrap_or_default();
                if child(node, "body").is_some() {
                    regions.push((owner.clone(), vec![node]));
                    owner.clear();
                    continue;
                }
            }
            nodes.push(node);
        }
        if !nodes.is_empty() {
            regions.push((owner, nodes));
        }
        regions
    }

    fn collect(
        &mut self,
        node: Node<'_>,
        parent_type: Option<String>,
        parent_function: Option<String>,
    ) {
        // Explicit DFS also handles generated expressions with tens of thousands
        // of nested binary nodes without consuming the worker thread's stack.
        let mut pending = vec![(Some(node), parent_type, parent_function, None)];
        while let Some((node, parent_type, parent_function, restore)) = pending.pop() {
            let Some(node) = node else {
                self.namespace = restore.expect("namespace exit has a previous namespace");
                continue;
            };
            let kind = node.kind();
            if self.is_import(kind) {
                self.add_import(node);
            }
            if self.is_namespace(kind) {
                let previous = self.namespace.clone();
                self.add_namespace(node);
                if child(node, "body").is_some() {
                    pending.push((None, None, None, Some(previous)));
                }
            }
            let (parent_type, parent_function) = if is_type(kind, self.dialect) {
                let owner = parent_function.as_deref().or(parent_type.as_deref());
                (Some(self.add_type(node, owner)), None)
            } else {
                (parent_type, parent_function)
            };
            if is_function(kind, self.dialect) {
                let fqn =
                    self.add_function(node, parent_type.as_deref(), parent_function.as_deref());
                if let Some(body) = child(node, "body") {
                    pending.push((Some(body), parent_type, Some(fqn), None));
                }
                continue;
            }
            for c in named_children(node).into_iter().rev() {
                pending.push((Some(c), parent_type.clone(), parent_function.clone(), None));
            }
        }
    }
    fn is_import(&self, k: &str) -> bool {
        match self.dialect {
            Dialect::Python => ["import_statement", "import_from_statement"].contains(&k),
            Dialect::JavaScript | Dialect::TypeScript => {
                ["import_statement", "import_require_clause"].contains(&k)
            }
            Dialect::Php => k == "namespace_use_declaration",
            Dialect::Rust => k == "use_declaration",
        }
    }
    fn is_namespace(&self, k: &str) -> bool {
        self.dialect == Dialect::Php && k == "namespace_definition"
    }
    fn add_namespace(&mut self, node: Node<'_>) {
        if let Some(n) = child(node, "name") {
            self.namespace = snippet(n, self.source).trim_matches('\\').to_string();
            self.model.module = self.namespace.clone();
        }
    }
    fn add_import(&mut self, node: Node<'_>) {
        let source = snippet(node, self.source);
        if matches!(
            self.dialect,
            Dialect::JavaScript | Dialect::TypeScript | Dialect::Rust
        ) {
            for (path, alias, piece) in imports::bindings(node, self.source, self.dialect) {
                self.record_import(path, alias, piece);
            }
            return;
        }
        let mut pieces = Vec::new();
        let mut php_prefix = None;
        match self.dialect {
            Dialect::Python => {
                let module = child(node, "module_name").map(|m| m.id());
                let mut children_cursor = node.walk();
                for c in node.named_children(&mut children_cursor) {
                    if Some(c.id()) == module {
                        continue;
                    }
                    if ["dotted_name", "aliased_import", "wildcard_import"].contains(&c.kind()) {
                        pieces.push(c);
                    } else {
                        let mut children_cursor = c.walk();
                        for cc in c.named_children(&mut children_cursor) {
                            if ["dotted_name", "aliased_import", "wildcard_import"]
                                .contains(&cc.kind())
                            {
                                pieces.push(cc);
                            }
                        }
                    }
                }
            }
            Dialect::JavaScript | Dialect::TypeScript => {
                let mut children_cursor = node.walk();
                for c in node.named_children(&mut children_cursor) {
                    if c.kind() == "string" {
                        pieces.push(c);
                    }
                }
            }
            Dialect::Php => {
                if let Some(group) =
                    child(node, "body").filter(|n| n.kind() == "namespace_use_group")
                {
                    php_prefix = first_kind(node, &["namespace_name"])
                        .map(|n| snippet(n, self.source).trim().trim_matches('\\').to_owned());
                    pieces.extend(
                        named_children(group)
                            .into_iter()
                            .filter(|n| n.kind() == "namespace_use_clause"),
                    );
                } else {
                    pieces.extend(
                        named_children(node)
                            .into_iter()
                            .filter(|n| n.kind() == "namespace_use_clause"),
                    );
                }
            }
            Dialect::Rust => {
                pieces.push(node);
            }
        }
        if pieces.is_empty() {
            pieces.push(node);
        }
        for piece in pieces {
            let raw = snippet(piece, self.source).trim().trim_end_matches(';');
            let (mut path, alias) = parse_import(raw, self.dialect, source);
            if let Some(prefix) = &php_prefix {
                path = format!("{prefix}\\{path}");
            }
            if !path.is_empty() {
                self.record_import(path, alias, piece);
            }
        }
    }
    fn record_import(&mut self, path: String, alias: Option<String>, node: Node<'_>) {
        let plain_python = self.dialect == Dialect::Python
            && alias.is_none()
            && path.contains('.')
            && node
                .parent()
                .is_some_and(|p| p.kind() == "import_statement");
        let binding_path = plain_python.then(|| path.split('.').next().unwrap_or(&path).to_owned());
        let source = if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript) {
            let mut ancestor = Some(node);
            let mut resource = None;
            while let Some(declaration) = ancestor {
                if matches!(
                    declaration.kind(),
                    "import_statement" | "import_require_clause"
                ) {
                    if let Some(value) = child(declaration, "source")
                        .or_else(|| first_kind(declaration, &["string"]))
                    {
                        resource = js_module_literal(snippet(value, self.source));
                        break;
                    }
                }
                ancestor = declaration.parent();
            }
            resource
        } else {
            None
        };
        self.model.imports.push(Import {
            alias: alias.or_else(|| binding_path.clone()),
            binding_path,
            path,
            source,
            type_only: self.dialect == Dialect::TypeScript && imports::is_type_only_binding(node),
            require: {
                let mut current = Some(node);
                let mut require = false;
                while let Some(ancestor) = current {
                    require |= ancestor.kind() == "import_require_clause";
                    if ancestor.kind() == "import_statement" {
                        break;
                    }
                    current = ancestor.parent();
                }
                require
            },
            range: range(node, self.path),
            scope: Some(imports::scope(node, self.path)),
            module: Some(imports::lexical_module(
                node,
                &self.model.module,
                self.source,
                self.dialect,
            )),
        });
    }
    fn annotate_bindings(&mut self, root: Node<'_>) {
        if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript) {
            let type_index = TypeIndex::new(&self.model.types, self.dialect);
            self.model.constants = bindings::module_constants(
                root,
                &type_index.scope(&self.model.module),
                &self.model.imports,
                self.dialect,
                self.source,
                self.path,
            );
        }
        // This pass only annotates executable call/property sites. A file with
        // none has no bindings to update, even when it declares nested helpers.
        if self.model.functions.iter().all(|function| {
            function.calls.is_empty()
                && function.property_accesses.is_empty()
                && function.property_mutations.is_empty()
        }) {
            return;
        }
        let import_list = &self.model.imports;
        let mut nested = self
            .model
            .functions
            .iter()
            .filter_map(|f| {
                f.parent_function
                    .as_ref()
                    .map(|p| ((p.clone(), f.name.clone()), f.fqn.clone()))
            })
            .collect::<BTreeMap<_, _>>();
        for initializer in self
            .model
            .functions
            .iter()
            .filter(|f| f.is_module_initializer())
        {
            // Expose actual top-level lexical declarations to the program binder
            // without changing their existing parent_function or FQN identities.
            for function in self.model.functions.iter().filter(|f| {
                !f.is_module_initializer()
                    && f.parent_type.is_none()
                    && f.parent_function.is_none()
                    && f.fqn
                        .rsplit_once(if self.dialect == Dialect::Php {
                            "\\"
                        } else {
                            "."
                        })
                        .map(|(owner, _)| owner)
                        == initializer
                            .fqn
                            .rsplit_once(if self.dialect == Dialect::Php {
                                "\\"
                            } else {
                                "."
                            })
                            .map(|(owner, _)| owner)
            }) {
                nested.insert(
                    (initializer.fqn.clone(), function.name.clone()),
                    function.fqn.clone(),
                );
            }
        }
        let indices = self
            .model
            .functions
            .iter()
            .enumerate()
            .filter(|(_, f)| !f.is_module_initializer())
            .map(|(i, f)| ((f.range.start_line, f.range.start_column), i))
            .collect::<BTreeMap<_, _>>();
        let type_index = TypeIndex::new(&self.model.types, self.dialect);
        let mut scopes = BTreeMap::new();
        let mut scope_nodes = vec![root];
        while let Some(node) = scope_nodes.pop() {
            if is_function(node.kind(), self.dialect) {
                if let Some(&index) = indices.get(&(
                    node.start_position().row + 1,
                    node.start_position().column + 1,
                )) {
                    let function = &self.model.functions[index];
                    scopes.insert(
                        (
                            node.start_position().row + 1,
                            node.start_position().column + 1,
                        ),
                        bindings::FunctionScope {
                            fqn: function.fqn.clone(),
                            parent_type: function.parent_type.clone(),
                            parent_function: function.parent_function.clone(),
                            parameters: function.parameters.clone(),
                            kind: node.kind().into(),
                            declaration_scope: imports::scope(node, self.path),
                        },
                    );
                }
            }
            scope_nodes.extend(named_children(node));
        }
        let mut pending = vec![root];
        while let Some(node) = pending.pop() {
            if is_function(node.kind(), self.dialect) {
                if let Some(&index) = indices.get(&(
                    node.start_position().row + 1,
                    node.start_position().column + 1,
                )) {
                    let owner = self.model.functions[index].fqn.clone();
                    let types = type_index.scope(&owner);
                    bindings::annotate(
                        node,
                        &mut self.model.functions[index],
                        &self.model.constants,
                        &types,
                        import_list,
                        &nested,
                        &scopes,
                        self.dialect,
                        self.source,
                    );
                }
            }
            let mut cursor = node.walk();
            pending.extend(node.named_children(&mut cursor));
        }
        let initializer_indices = self
            .model
            .functions
            .iter()
            .enumerate()
            .filter_map(|(i, f)| f.is_module_initializer().then_some(i))
            .collect::<Vec<_>>();
        for index in initializer_indices {
            let owner = self.model.functions[index].fqn.clone();
            let types = type_index.scope(&owner);
            bindings::annotate(
                root,
                &mut self.model.functions[index],
                &self.model.constants,
                &types,
                import_list,
                &nested,
                &scopes,
                self.dialect,
                self.source,
            );
        }
    }
    fn disambiguate_rust_methods(&mut self) {
        if self.dialect != Dialect::Rust {
            return;
        }
        let mut counts = BTreeMap::<String, usize>::new();
        for function in &self.model.functions {
            if function.parent_type.is_some() && function.parent_function.is_none() {
                *counts.entry(function.fqn.clone()).or_default() += 1;
            }
        }
        let renamed = self
            .model
            .functions
            .iter()
            .filter_map(|function| {
                let contract = function.rust_trait.as_ref()?;
                (function.parent_function.is_none()
                    && counts.get(&function.fqn).copied().unwrap_or(0) > 1)
                    .then(|| {
                        (
                            function.fqn.clone(),
                            format!("{}#impl[{}]", function.fqn, contract.replace("::", ".")),
                            function.range.clone(),
                        )
                    })
            })
            .collect::<Vec<_>>();
        if renamed.is_empty() {
            return;
        }
        for function in &mut self.model.functions {
            if let Some((old, new, _)) = renamed.iter().find(|(_, _, range)| {
                function.range.start_line >= range.start_line
                    && (function.range.start_line != range.start_line
                        || function.range.start_column >= range.start_column)
                    && function.range.end_line <= range.end_line
                    && (function.range.end_line != range.end_line
                        || function.range.end_column <= range.end_column)
            }) {
                if function.fqn == *old {
                    function.fqn = new.clone();
                } else if let Some(suffix) = function.fqn.strip_prefix(&format!("{old}::")) {
                    function.fqn = format!("{new}::{suffix}");
                }
                if let Some(parent) = &mut function.parent_type {
                    if let Some(suffix) = parent.strip_prefix(&format!("{old}::")) {
                        *parent = format!("{new}::{suffix}");
                    }
                }
                if let Some(parent) = &mut function.parent_function {
                    if *parent == *old {
                        *parent = new.clone();
                    } else if let Some(suffix) = parent.strip_prefix(&format!("{old}::")) {
                        *parent = format!("{new}::{suffix}");
                    }
                }
            }
        }
        let mut methods = BTreeMap::<String, Vec<String>>::new();
        for function in self
            .model
            .functions
            .iter()
            .filter(|function| function.parent_function.is_none())
        {
            if let Some(owner) = &function.parent_type {
                methods
                    .entry(owner.clone())
                    .or_default()
                    .push(function.fqn.clone());
            }
        }
        for typ in &mut self.model.types {
            if let Some((old, new, _)) = renamed.iter().find(|(_, _, range)| {
                typ.range.start_line >= range.start_line
                    && (typ.range.start_line != range.start_line
                        || typ.range.start_column >= range.start_column)
                    && typ.range.end_line <= range.end_line
                    && (typ.range.end_line != range.end_line
                        || typ.range.end_column <= range.end_column)
            }) {
                if let Some(suffix) = typ.fqn.strip_prefix(&format!("{old}::")) {
                    typ.fqn = format!("{new}::{suffix}");
                }
            }
            if typ.kind != TypeKind::Impl {
                typ.methods = methods.remove(&typ.fqn).unwrap_or_default();
            }
        }
    }

    fn resolve_rust_trait_visibility(&mut self, root: Node<'_>) {
        // Resolve after collection so an impl may precede its trait declaration.
        let traits = self
            .model
            .types
            .iter()
            .filter(|typ| typ.kind == TypeKind::Trait)
            .map(|typ| {
                (
                    typ.fqn.as_str(),
                    typ.visibility.as_deref() == Some("public"),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let import_index = analyzer_core::scoped_imports::ScopedImports::new(
            self.model.imports.iter().map(|import| {
                let module = import.module.as_deref().unwrap_or(&self.model.module);
                (
                    import,
                    rust_path_in_module(&import.path, module, &self.model.module),
                )
            }),
        );
        let indices = self
            .model
            .functions
            .iter()
            .enumerate()
            .filter(|(_, function)| {
                function.parent_type.is_some() && function.parent_function.is_none()
            })
            .map(|(index, function)| {
                (
                    (function.range.start_line, function.range.start_column),
                    index,
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut updates = Vec::new();
        let mut pending = vec![root];
        while let Some(node) = pending.pop() {
            if let Some(&index) = indices.get(&(
                node.start_position().row + 1,
                node.start_position().column + 1,
            )) {
                let owner = node.parent().and_then(|body| body.parent());
                if let Some(owner) = owner {
                    let public = match owner.kind() {
                        "trait_item" => Some(rust_is_public(owner, self.source)),
                        "impl_item" => child(owner, "trait").map(|trait_node| {
                            let function = &self.model.functions[index];
                            let module = imports::lexical_module(
                                owner,
                                &self.model.module,
                                self.source,
                                Dialect::Rust,
                            );
                            let reference =
                                crate::types::reference("rust", snippet(trait_node, self.source))
                                    .unwrap_or_else(|| snippet(trait_node, self.source).into());
                            let scope = import_index.at(&function.range);
                            let aliased = scope
                                .matching_prefixes(&reference)
                                .map(|(alias, target)| {
                                    format!("{target}{}", &reference[alias.len()..])
                                })
                                .last();
                            let resolved = aliased.unwrap_or_else(|| {
                                rust_path_in_module(&reference, &module, &self.model.module)
                            });
                            // An external trait's visibility is unknown here. Rust
                            // forbids pub on its methods; absence of pub proves nothing.
                            traits.get(resolved.as_str()).copied().unwrap_or(true)
                        }),
                        _ => None,
                    };
                    if let Some(public) = public {
                        updates.push((index, !public));
                    }
                }
            }
            pending.extend(named_children(node).into_iter().rev());
        }
        for (index, private) in updates {
            let function = &mut self.model.functions[index];
            function.modifiers.retain(|modifier| modifier != "private");
            if private {
                function.modifiers.push("private".into());
            }
        }
    }
    fn add_type(&mut self, node: Node<'_>, parent: Option<&str>) -> String {
        let impl_target = if node.kind() == "impl_item" {
            text_field(node, "type", self.source)
                .and_then(|raw| crate::types::reference("rust", &raw))
        } else {
            None
        };
        let name = if let Some(target) = &impl_target {
            if let Some(tr) = text_field(node, "trait", self.source) {
                format!("impl {tr} for {target}")
            } else {
                format!("impl {target}")
            }
        } else {
            text_field(node, "name", self.source)
                .or_else(|| {
                    first_kind(node, &["type_identifier", "identifier", "name"])
                        .map(|n| snippet(n, self.source).into())
                })
                .unwrap_or_else(|| {
                    self.anonymous += 1;
                    format!("<anonymous_type_{}>", self.anonymous)
                })
        };
        let owner = parent.unwrap_or(if self.dialect == Dialect::Php {
            &self.namespace
        } else if self.namespace.is_empty() {
            &self.model.module
        } else {
            &self.namespace
        });
        let sep = match self.dialect {
            Dialect::Php => "\\",
            Dialect::Rust => "::",
            Dialect::Python => ".",
            _ => ".",
        };
        let fqn = if owner.is_empty() {
            name.clone()
        } else {
            format!("{owner}{sep}{name}")
        };
        let tk = match node.kind() {
            "class_definition"
            | "class_declaration"
            | "abstract_class_declaration"
            | "class"
            | "anonymous_class" => TypeKind::Class,
            "trait_declaration" | "trait_item" => TypeKind::Trait,
            "interface_declaration" => TypeKind::Interface,
            "enum_declaration" | "enum_item" => TypeKind::Enum,
            "struct_item" => TypeKind::Struct,
            "impl_item" => TypeKind::Impl,
            "mod_item" | "internal_module" => TypeKind::Namespace,
            "type_item"
                if node
                    .parent()
                    .and_then(|body| body.parent())
                    .is_some_and(|owner| matches!(owner.kind(), "impl_item" | "trait_item")) =>
            {
                TypeKind::AssociatedType
            }
            _ => TypeKind::Other,
        };
        let mut typ = TypeModel {
            name,
            fqn: fqn.clone(),
            kind: tk,
            range: range(node, self.path),
            ..Default::default()
        };
        typ.is_abstract = is_abstract_type(node, &typ.kind);
        typ.decorators = decorators(node, self.source);
        if self.dialect == Dialect::Rust {
            typ.rust_cfg = rust_conditions(node, self.source);
        }
        if self.dialect == Dialect::Rust && node.kind() == "trait_item" {
            typ.visibility = Some(
                if rust_is_public(node, self.source) {
                    "public"
                } else {
                    "private"
                }
                .into(),
            );
        }
        if self.dialect == Dialect::Rust && node.kind() == "mod_item" {
            typ.external_module = child(node, "body").is_none();
            typ.module_path = rust_module_path(node, self.source);
            typ.conditional_module_paths = rust_attributes(node, self.source)
                .into_iter()
                .filter_map(|attribute| {
                    let arguments = attribute
                        .strip_prefix("cfg_attr")?
                        .trim()
                        .strip_prefix('(')?
                        .strip_suffix(')')?;
                    let pieces = analyzer_core::resolution::split_arguments(arguments);
                    let condition = pieces.first()?.to_string();
                    pieces.iter().skip(1).find_map(|piece| {
                        let (key, value) = piece.split_once('=')?;
                        (key.trim() == "path")
                            .then(|| rust_path_literal(value.trim()))?
                            .map(|path| ConditionalModulePath {
                                condition: condition.clone(),
                                path,
                            })
                    })
                })
                .collect();
        }
        self.collect_type_members(node, &mut typ);
        if self.dialect == Dialect::Python {
            if let Some(parents) = child(node, "superclasses") {
                for base in named_children(parents) {
                    let raw = snippet(base, self.source);
                    if base.kind() == "keyword_argument" {
                        if raw.split_whitespace().collect::<String>() != "metaclass=type" {
                            typ.inheritance_unknown = true;
                        }
                    } else if raw == "object" {
                        typ.extends.push(raw.into());
                    } else if base.kind() == "string" {
                        typ.inheritance_unknown = true;
                    } else if base.kind() == "subscript" {
                        // A user-defined __class_getitem__/__mro_entries__ can
                        // replace the named base with an unrelated class.
                        typ.inheritance_unknown = true;
                    } else if let Some(reference) = crate::types::reference("python", raw) {
                        typ.extends.push(reference);
                    } else {
                        typ.inheritance_unknown = true;
                    }
                }
            }
        }
        if let Some(target) = &impl_target {
            if let Some(trait_name) = text_field(node, "trait", self.source) {
                typ.implements.push(trait_name.clone());
                if let Some(existing) = self.model.types.iter_mut().find(|t| t.name == *target) {
                    existing.implements.push(trait_name);
                }
            }
        }
        for field in &mut typ.fields {
            if let Some(ty) = field.type_name.as_mut() {
                *ty = trim_type(ty);
            }
        }
        self.model.types.push(typ);
        if let Some(target) = impl_target {
            if owner.is_empty() {
                target
            } else {
                format!("{owner}{sep}{target}")
            }
        } else {
            fqn
        }
    }
    fn collect_type_members(&self, node: Node<'_>, typ: &mut TypeModel) {
        let mut children_cursor = node.walk();
        for c in node.named_children(&mut children_cursor) {
            if ["method_definition", "method_declaration"].contains(&c.kind())
                && text_field(c, "name", self.source)
                    .is_some_and(|n| n == "constructor" || n == "__construct")
            {
                if let Some(params) = child(c, "parameters") {
                    let mut children_cursor = params.walk();
                    for p in params.named_children(&mut children_cursor) {
                        if let Some(name) = promoted_parameter_name(p, self.source) {
                            if !typ.fields.iter().any(|f| f.name == name) {
                                let modifier = child(p, "visibility")
                                    .or_else(|| first_kind(p, &["accessibility_modifier"]));
                                let readonly_type = php_readonly_promotion_type(p, self.source);
                                typ.fields.push(Field {
                                    name,
                                    type_name: readonly_type
                                        .clone()
                                        .or_else(|| text_field(p, "type", self.source))
                                        .filter(|x| !x.is_empty())
                                        .map(|x| trim_type(&x)),
                                    range: range(p, self.path),
                                    // A `readonly` promoted property without a
                                    // modifier is public.
                                    visibility: match readonly_type {
                                        Some(_) => Some("public".into()),
                                        None => {
                                            visibility(snippet(modifier.unwrap_or(p), self.source))
                                        }
                                    },
                                    is_static: false,
                                    unknown_key: false,
                                    symbol_key_reference: None,
                                });
                            }
                        }
                    }
                }
            }
            match c.kind() {
                "use_declaration" if self.dialect == Dialect::Php => {
                    typ.trait_uses
                        .push(php_traits::normalize(c, self.source, self.path));
                }
                "field_definition"
                | "public_field_definition"
                | "property_signature"
                | "property_element"
                | "field_declaration"
                | "property_promotion_parameter" => {
                    let n = text_field(c, "name", self.source)
                        .or_else(|| text_field(c, "property", self.source))
                        .or_else(|| {
                            first_kind(
                                c,
                                &[
                                    "identifier",
                                    "property_identifier",
                                    "variable_name",
                                    "field_identifier",
                                ],
                            )
                            .map(|x| snippet(x, self.source).into())
                        });
                    if let Some(name) = n {
                        let unknown_key =
                            matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
                                && child(c, "name")
                                    .or_else(|| child(c, "property"))
                                    .is_some_and(|name| {
                                        matches!(name.kind(), "computed_property_name" | "string")
                                    })
                                && properties::declared_name(c, self.source).is_none();
                        let symbol_key_reference =
                            matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
                                .then(|| child(c, "name").or_else(|| child(c, "property")))
                                .flatten()
                                .filter(|name| name.kind() == "computed_property_name")
                                .and_then(|name| named_children(name).first().copied())
                                .and_then(|name| {
                                    static_js::symbol_key_reference(name, self.source)
                                });
                        let name =
                            if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript) {
                                properties::declared_name(c, self.source).unwrap_or(name)
                            } else {
                                name
                            };
                        typ.fields.push(Field {
                            name: name.trim_start_matches('$').into(),
                            type_name: text_field(c, "type", self.source)
                                .or_else(|| phpdoc_var(c, self.source)),
                            range: range(c, self.path),
                            visibility: visibility(snippet(c, self.source)),
                            is_static: matches!(
                                self.dialect,
                                Dialect::JavaScript | Dialect::TypeScript
                            ) && modifiers(c, self.source)
                                .iter()
                                .any(|modifier| modifier == "static"),
                            unknown_key,
                            symbol_key_reference,
                        });
                    }
                }
                "property_declaration" | "field_declaration_list" => {
                    let mut children_cursor = c.walk();
                    for p in c.named_children(&mut children_cursor) {
                        if ["property_element", "field_declaration"].contains(&p.kind()) {
                            let n = text_field(p, "name", self.source).or_else(|| {
                                first_kind(p, &["variable_name", "field_identifier", "identifier"])
                                    .map(|x| snippet(x, self.source).into())
                            });
                            if let Some(name) = n {
                                typ.fields.push(Field {
                                    name: name.trim_start_matches('$').into(),
                                    type_name: if self.dialect == Dialect::Rust {
                                        text_field(p, "type", self.source)
                                    } else {
                                        text_field(c, "type", self.source)
                                            .or_else(|| phpdoc_var(c, self.source))
                                    },
                                    range: range(p, self.path),
                                    visibility: visibility(snippet(c, self.source)),
                                    is_static: false,
                                    unknown_key: false,
                                    symbol_key_reference: None,
                                });
                            }
                        }
                    }
                }
                "class_heritage"
                | "class_base_clause"
                | "base_clause"
                | "extends_clause"
                | "class_interface_clause" => {
                    let raw = snippet(c, self.source);
                    if c.kind() == "class_heritage"
                        && named_children(c)
                            .iter()
                            .any(|n| ["extends_clause", "implements_clause"].contains(&n.kind()))
                    {
                        // These clauses are collected individually below.
                    } else if raw.contains("implements") {
                        typ.implements
                            .extend(heritage_types(c, self.source, self.dialect));
                    } else {
                        typ.extends
                            .extend(heritage_types(c, self.source, self.dialect));
                    }
                }
                "implements_clause" | "trait_bounds" => {
                    typ.implements
                        .extend(heritage_types(c, self.source, self.dialect))
                }
                _ => {}
            }
            if !is_function(c.kind(), self.dialect)
                && !is_type(c.kind(), self.dialect)
                && ![
                    "property_declaration",
                    "field_declaration_list",
                    "field_definition",
                    "public_field_definition",
                    "property_signature",
                    "property_element",
                    "field_declaration",
                    "property_promotion_parameter",
                ]
                .contains(&c.kind())
            {
                self.collect_type_members(c, typ);
            }
        }
        if self.dialect == Dialect::Python && node.kind() == "class_definition" {
            if let Some(body) = child(node, "body") {
                let mut children_cursor = body.walk();
                for wrapper in body.named_children(&mut children_cursor) {
                    let method = if wrapper.kind() == "decorated_definition" {
                        child(wrapper, "definition").unwrap_or(wrapper)
                    } else {
                        wrapper
                    };
                    if method.kind() != "function_definition"
                        || text_field(method, "name", self.source).as_deref() != Some("__init__")
                    {
                        continue;
                    }
                    if decorators(method, self.source).iter().any(|decorator| {
                        matches!(
                            decorator.as_str(),
                            "@staticmethod"
                                | "@builtins.staticmethod"
                                | "@classmethod"
                                | "@builtins.classmethod"
                        )
                    }) {
                        continue;
                    }
                    let parameters = child(method, "parameters")
                        .map(|parameters| self.parameters(parameters, true))
                        .unwrap_or_default();
                    let Some(receiver) = parameters
                        .iter()
                        .find(|parameter| parameter.implicit_receiver)
                    else {
                        continue;
                    };
                    let param_types = parameters
                        .iter()
                        .filter_map(|parameter| {
                            parameter
                                .type_name
                                .as_ref()
                                .map(|ty| (parameter.name.clone(), ty.clone()))
                        })
                        .collect();
                    if let Some(block) = child(method, "body") {
                        self.python_fields(block, &receiver.name, &param_types, typ);
                    }
                }
            }
        }
    }
    fn python_fields(
        &self,
        node: Node<'_>,
        receiver: &str,
        params: &BTreeMap<String, String>,
        typ: &mut TypeModel,
    ) {
        if node.kind() == "assignment" {
            if let (Some(left), Some(right)) = (child(node, "left"), child(node, "right")) {
                let field = (left.kind() == "attribute")
                    .then(|| child(left, "object").zip(child(left, "attribute")))
                    .flatten()
                    .filter(|(object, _)| snippet(*object, self.source) == receiver)
                    .map(|(_, field)| snippet(field, self.source));
                if let Some(field) = field {
                    let value = snippet(right, self.source);
                    let ty = child(node, "type")
                        .map(|n| snippet(n, self.source).to_owned())
                        .or_else(|| params.get(value).cloned())
                        .or_else(|| {
                            if right.kind() != "call" {
                                return None;
                            }
                            let target =
                                child(right, "function").map(|n| snippet(n, self.source))?;
                            // A factory's name does not prove the instance's class.
                            if target == typ.name || target == typ.fqn {
                                return Some(typ.fqn.clone());
                            }
                            TypeIndex::new(&self.model.types, self.dialect)
                                .scope(&typ.fqn)
                                .get(target)
                                .cloned()
                        });
                    if !typ.fields.iter().any(|f| f.name == field) {
                        typ.fields.push(Field {
                            name: field.into(),
                            type_name: ty,
                            range: range(left, self.path),
                            visibility: None,
                            is_static: false,
                            unknown_key: false,
                            symbol_key_reference: None,
                        });
                    }
                }
            }
        }
        let mut children_cursor = node.walk();
        for c in node.named_children(&mut children_cursor) {
            if !is_function(c.kind(), self.dialect) && !is_type(c.kind(), self.dialect) {
                self.python_fields(c, receiver, params, typ)
            }
        }
    }
    fn add_function(
        &mut self,
        node: Node<'_>,
        parent_type: Option<&str>,
        parent_function: Option<&str>,
    ) -> String {
        let (name, fqn) = self.function_identity(node, parent_type, parent_function);
        let (mut function, body_node, params) =
            self.function_model(node, parent_type, parent_function, name, &fqn);
        self.rust_function_modifiers(node, params, &mut function);
        self.js_function_modifiers(node, params, &mut function);
        self.other_function_modifiers(node, params, &mut function);
        self.walk_body(body_node, &mut function, &[], &[], 0);
        function.cognitive_complexity =
            Some(cognitive_complexity(body_node, self.source, self.dialect));
        self.attach_function_cost(body_node, &mut function);
        self.attach_function_fields(body_node, params, &mut function);
        self.register_function_method(parent_function, &function);
        self.model.functions.push(function);
        fqn
    }
    fn function_identity(
        &mut self,
        node: Node<'_>,
        parent_type: Option<&str>,
        parent_function: Option<&str>,
    ) -> (String, String) {
        let name = self.function_name(node);
        let owner = parent_function
            .or(parent_type)
            .unwrap_or(if self.dialect == Dialect::Php {
                &self.namespace
            } else if self.namespace.is_empty() {
                &self.model.module
            } else {
                &self.namespace
            });
        let sep = if parent_type.is_some() && parent_function.is_none() {
            match self.dialect {
                Dialect::Php | Dialect::Rust => "::",
                _ => ".",
            }
        } else {
            match self.dialect {
                Dialect::Php => "\\",
                Dialect::Rust => "::",
                _ => ".",
            }
        };
        // Reserve identity delimiters while preserving the actual decoded property name.
        let identity_name = if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
            && properties::declared_name(node, self.source).is_some()
        {
            name.replace('%', "%25")
                .replace('#', "%23")
                .replace('.', "%2E")
        } else {
            name.clone()
        };
        let mut fqn = if owner.is_empty() {
            identity_name.clone()
        } else {
            format!("{owner}{sep}{identity_name}")
        };
        self.qualify_object_function(node, parent_type, owner, sep, &identity_name, &mut fqn);
        self.function_identity_suffix(node, parent_type, parent_function, &mut fqn);
        (name, fqn)
    }
    fn function_name(&mut self, node: Node<'_>) -> String {
        let name = matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
            .then(|| properties::descriptor_function_name(node, self.source))
            .flatten()
            .or_else(|| text_field(node, "name", self.source))
            .or_else(|| {
                if node.kind() == "arrow_function" {
                    return None;
                }
                first_kind(
                    node,
                    &["identifier", "property_identifier", "field_identifier"],
                )
                .map(|n| snippet(n, self.source).into())
            })
            .or_else(|| {
                node.parent().and_then(|p| {
                    if [
                        "variable_declarator",
                        "assignment",
                        "pair",
                        "assignment_expression",
                        "let_declaration",
                        "field_definition",
                        "public_field_definition",
                    ]
                    .contains(&p.kind())
                    {
                        child(p, "name")
                            .or_else(|| child(p, "left"))
                            .or_else(|| child(p, "key"))
                            .or_else(|| child(p, "pattern"))
                            .map(|n| snippet(n, self.source).trim_start_matches('$').into())
                    } else {
                        None
                    }
                })
            })
            .unwrap_or_else(|| {
                self.anonymous += 1;
                format!("<lambda_{}>", self.anonymous)
            });

        if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript) {
            properties::declared_name(node, self.source).unwrap_or(name)
        } else {
            name
        }
    }
    fn qualify_object_function(
        &self,
        node: Node<'_>,
        parent_type: Option<&str>,
        owner: &str,
        sep: &str,
        identity_name: &str,
        fqn: &mut String,
    ) {
        // Object members are separate declarations from a lexical variable or
        // imported function of the same spelling (e.g. API.dispatch vs dispatch).
        if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
            && parent_type.is_none()
            && node
                .parent()
                .is_some_and(|parent| matches!(parent.kind(), "pair" | "object"))
        {
            let mut ancestor = node.parent();
            while let Some(parent) = ancestor {
                if is_function(parent.kind(), self.dialect) {
                    break;
                }
                if parent.kind() == "assignment_expression" {
                    if let Some(binding) = child(parent, "left") {
                        *fqn = format!(
                            "{owner}{sep}{}{sep}{identity_name}",
                            snippet(binding, self.source)
                        );
                    }
                    break;
                }
                if parent.kind() == "variable_declarator" {
                    if let Some(binding) =
                        child(parent, "name").filter(|binding| binding.kind() == "identifier")
                    {
                        *fqn = format!(
                            "{owner}{sep}{}{sep}{identity_name}",
                            snippet(binding, self.source)
                        );
                    }
                    break;
                }
                ancestor = parent.parent();
            }
        }
    }
    fn function_identity_suffix(
        &self,
        node: Node<'_>,
        parent_type: Option<&str>,
        parent_function: Option<&str>,
        fqn: &mut String,
    ) {
        // Static methods execute on a class object, independently of instance members.
        if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
            && accessor_keyword(node).is_none()
            && parent_type.is_some()
            && parent_function.is_none()
            && modifiers(node, self.source)
                .iter()
                .any(|modifier| modifier == "static")
        {
            fqn.push_str("#static");
        }
        // A property has two independently executable bodies. Their display name
        // remains the property name, but graph/cache identities must not collide.
        if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript) {
            if let Some(keyword) = accessor_keyword(node) {
                if modifiers(node, self.source)
                    .iter()
                    .any(|modifier| modifier == "static")
                {
                    fqn.push_str("#static");
                }
                fqn.push('#');
                fqn.push_str(keyword);
            }
        }
    }
    fn function_model<'tree>(
        &self,
        node: Node<'tree>,
        parent_type: Option<&str>,
        parent_function: Option<&str>,
        name: String,
        fqn: &str,
    ) -> (FunctionModel, Node<'tree>, Option<Node<'tree>>) {
        let parent_type = parent_type.filter(|owner| {
            !self
                .model
                .types
                .iter()
                .any(|t| t.fqn == *owner && t.kind == TypeKind::Namespace)
        });
        let body = child(node, "body").or_else(|| {
            first_kind(
                node,
                &[
                    "block",
                    "statement_block",
                    "compound_statement",
                    "declaration_list",
                ],
            )
        });
        let body_node = body.unwrap_or(node);
        let params = child(node, "parameters").or_else(|| {
            first_kind(
                node,
                &[
                    "parameters",
                    "formal_parameters",
                    "lambda_parameters",
                    "closure_parameters",
                ],
            )
        });
        // Detailed AST is produced only by explicit parse/AST requests.
        // Function bodies overlap for nested closures; retaining each subtree
        // duplicates nodes and source ranges throughout scan reports and caches.
        let return_type = text_field(node, "return_type", self.source).map(|raw| {
            let cleaned = trim_type(raw.trim_start_matches("->"));
            if cleaned == "Self" {
                parent_type.unwrap_or("Self").to_string()
            } else {
                cleaned
            }
        });
        let function = FunctionModel {
            name,
            fqn: fqn.to_owned(),
            range: range(node, self.path),
            body_range: range(body_node, self.path),
            parent_type: parent_type.map(str::to_owned),
            parent_function: parent_function.map(str::to_owned),
            parameters: params
                .map(|p| self.parameters(p, parent_type.is_some()))
                .unwrap_or_default(),
            signature: signature(node, body, self.source),
            return_type,
            modifiers: modifiers(node, self.source),
            complexity: 1,
            normalized_ast: None,
            ..Default::default()
        };
        (function, body_node, params)
    }
    fn rust_function_modifiers(
        &self,
        node: Node<'_>,
        _params: Option<Node<'_>>,
        function: &mut FunctionModel,
    ) {
        let parent_function = function.parent_function.as_deref();
        let parent_type = function.parent_type.clone();
        if self.dialect == Dialect::Rust {
            function.rust_cfg = rust_conditions(node, self.source);
            if parent_function.is_none() {
                let mut ancestor = node.parent();
                while let Some(owner) = ancestor {
                    if owner.kind() == "impl_item" {
                        function.rust_trait = text_field(owner, "trait", self.source);
                        break;
                    }
                    if is_function(owner.kind(), self.dialect) || owner.kind() == "trait_item" {
                        break;
                    }
                    ancestor = owner.parent();
                }
            }
            if parent_type.is_some()
                && parent_function.is_none()
                && !rust_is_public(node, self.source)
            {
                function.modifiers.push("private".into());
            }
            let bounds = rust_trait_bounds(node, self.source);
            for parameter in &mut function.parameters {
                if let Some(ty) = parameter.type_name.as_mut() {
                    let clean = trim_type(ty);
                    if let Some(bound) = bounds.get(&clean) {
                        *ty = bound.clone();
                    } else if let Some(bound) = clean
                        .strip_prefix("dyn ")
                        .or_else(|| clean.strip_prefix("impl "))
                    {
                        *ty = bound.split('+').next().unwrap_or(bound).trim().to_string();
                    }
                }
            }
        }
    }
    fn js_function_modifiers(
        &self,
        node: Node<'_>,
        _params: Option<Node<'_>>,
        function: &mut FunctionModel,
    ) {
        if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
            && node.kind() == "arrow_function"
        {
            let mut ancestor = node.parent();
            while let Some(owner) = ancestor {
                if owner.kind() != "arrow_function"
                    && (is_function(owner.kind(), self.dialect)
                        || matches!(owner.kind(), "field_definition" | "public_field_definition"))
                {
                    if modifiers(owner, self.source)
                        .iter()
                        .any(|modifier| modifier == "static")
                    {
                        function.modifiers.push("lexical_static_this".into());
                    }
                    break;
                }
                ancestor = owner.parent();
            }
        }
    }
    fn other_function_modifiers(
        &self,
        node: Node<'_>,
        _params: Option<Node<'_>>,
        function: &mut FunctionModel,
    ) {
        function.modifiers.extend(decorators(node, self.source));
        if self.dialect == Dialect::Python
            && function
                .modifiers
                .iter()
                .any(|m| matches!(m.as_str(), "@staticmethod" | "@builtins.staticmethod"))
        {
            for p in &mut function.parameters {
                p.implicit_receiver = false;
            }
        }
        if node.kind().contains("generator") {
            function.modifiers.push("generator".into());
        }
        if self.dialect == Dialect::Php {
            apply_phpdoc(node, self.source, function);
        }
    }
    fn attach_function_cost(&self, body_node: Node<'_>, function: &mut FunctionModel) {
        // Cost inference still uses recursive expression evaluators. Bound this
        // optional representation before entering any of them; normal metrics,
        // runtime operations and bindings remain complete for deep bodies.
        function.cost_ir = Some(Arc::new(if exceeds_recursive_depth(body_node) {
            analyzer_core::algorithmic::CostNode {
                range: range(body_node, self.path),
                kind: analyzer_core::algorithmic::CostKind::Unknown(
                    "cost extraction depth limit exceeded".into(),
                ),
            }
        } else if self.dialect == Dialect::Rust {
            cost::extract(
                body_node,
                function,
                self.source,
                self.path,
                &self.cost_type_shadows,
            )
        } else {
            cost_dynamic::extract(
                body_node,
                function,
                self.source,
                self.path,
                self.dialect,
                &self.cost_type_shadows,
                !self.namespace.is_empty(),
            )
        }));
    }
    fn attach_function_fields(
        &self,
        body_node: Node<'_>,
        params: Option<Node<'_>>,
        function: &mut FunctionModel,
    ) {
        let parent_function = function.parent_function.as_deref();
        let parent_type = function.parent_type.as_deref();
        let mut fields = BTreeSet::new();
        collect_accessed_fields(
            body_node,
            self.source,
            self.dialect,
            &receiver_names(function, self.dialect),
            &mut fields,
        );
        // A promoted constructor parameter is an implicit `$this->x = $x` /
        // `this.x = x` on entry, so the constructor accesses that field.
        if parent_type.is_some()
            && parent_function.is_none()
            && matches!(function.name.as_str(), "constructor" | "__construct")
        {
            if let Some(params) = params {
                let mut children_cursor = params.walk();
                fields.extend(
                    params
                        .named_children(&mut children_cursor)
                        .filter_map(|p| promoted_parameter_name(p, self.source)),
                );
            }
        }
        function.accessed_fields = fields.into_iter().collect();
        function.accessor = classify_accessor(body_node, function, self.source, self.dialect);
    }
    fn register_function_method(
        &mut self,
        parent_function: Option<&str>,
        function: &FunctionModel,
    ) {
        let parent_type = function.parent_type.as_deref();
        let fqn = &function.fqn;
        if parent_type.is_some() && parent_function.is_none() {
            if let Some(t) = self
                .model
                .types
                .iter_mut()
                .find(|t| Some(t.fqn.as_str()) == parent_type)
            {
                t.methods.push(fqn.clone());
            }
        }
    }

    fn parameters(&self, node: Node<'_>, method: bool) -> Vec<Parameter> {
        let mut out = Vec::new();
        let mut children_cursor = node.walk();
        for p in node.named_children(&mut children_cursor) {
            if [
                "identifier",
                "typed_parameter",
                "default_parameter",
                "typed_default_parameter",
                "list_splat_pattern",
                "dictionary_splat_pattern",
                "required_parameter",
                "optional_parameter",
                "rest_pattern",
                "formal_parameter",
                "simple_parameter",
                "variadic_parameter",
                "property_promotion_parameter",
                "parameter",
                "self_parameter",
                "variadic_parameter_declaration",
                "assignment_pattern",
            ]
            .contains(&p.kind())
                || p.kind().contains("parameter")
            {
                let raw = snippet(p, self.source);
                let name = text_field(p, "name", self.source)
                    .or_else(|| {
                        first_kind(
                            p,
                            &[
                                "identifier",
                                "variable_name",
                                "self",
                                "pattern_identifier",
                                "reference_pattern",
                            ],
                        )
                        .map(|n| snippet(n, self.source).to_string())
                    })
                    .unwrap_or_else(|| {
                        raw.split([':', '=', ' ']).next().unwrap_or(raw).to_string()
                    });
                let name = name.trim().trim_start_matches(['$', '*', '&']).to_string();
                let implicit = match self.dialect {
                    Dialect::Python => method && out.is_empty(),
                    Dialect::Rust => p.kind() == "self_parameter" || (method && name == "self"),
                    Dialect::TypeScript => method && name == "this",
                    _ => false,
                };
                let ty = text_field(p, "type", self.source)
                    .or_else(|| text_field(p, "type_annotation", self.source))
                    .or_else(|| parameter_type(raw, self.dialect));
                out.push(Parameter {
                    name,
                    type_name: ty,
                    default: text_field(p, "value", self.source)
                        .or_else(|| text_field(p, "default", self.source)),
                    variadic: raw.trim_start().starts_with('*') || raw.contains("..."),
                    implicit_receiver: implicit,
                    range: range(p, self.path),
                });
            }
        }
        out
    }
    fn walk_body(
        &mut self,
        node: Node<'_>,
        function: &mut FunctionModel,
        guards: &[Guard],
        loops: &[LoopContext],
        depth: usize,
    ) {
        let mut pending = vec![BodyWork::Enter(node, guards.into(), loops.into(), depth)];
        while let Some(work) = pending.pop() {
            if let Some(frame) = self.enter_body(work, function, &mut pending) {
                self.visit_body_child(frame, function, &mut pending);
            }
        }
    }
    fn enter_body<'tree>(
        &mut self,
        work: BodyWork<'tree>,
        function: &mut FunctionModel,
        pending: &mut Vec<BodyWork<'tree>>,
    ) -> Option<BodyChildren<'tree>> {
        match work {
            BodyWork::Children {
                node,
                children,
                guards,
                loops,
                depth,
            } => Some(BodyChildren {
                node,
                children,
                guards,
                loops,
                depth,
            }),
            BodyWork::Enter(node, guards, loops, depth) => {
                let kind = node.kind();
                if self.enter_initializer(
                    BodyEntry {
                        node,
                        guards: guards.clone(),
                        loops: loops.clone(),
                        depth,
                    },
                    function,
                    pending,
                ) {
                    return None;
                }
                if node.id() != function.body_range.start_line as usize
                    && is_function(kind, self.dialect)
                    && node.start_position().row + 1 != function.range.start_line
                {
                    return None;
                }
                if ["return_statement", "return_expression"].contains(&kind) {
                    function.return_points += 1;
                }
                if is_call(kind, self.dialect) {
                    function.calls.push(self.call_site(node, &guards, &loops));
                }
                if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript) {
                    function.property_accesses.extend(properties::collect(
                        node,
                        self.source,
                        self.path,
                        &guards,
                        &loops,
                    ));
                    function.property_mutations.extend(properties::mutations(
                        node,
                        self.source,
                        self.path,
                        &guards,
                        &loops,
                    ));
                }
                function.nesting_depth = function.nesting_depth.max(depth);
                if branch_complexity(kind) || is_logical_binary(node, self.source) {
                    function.complexity += 1;
                }
                Some(BodyChildren {
                    node,
                    children: named_children(node).into_iter(),
                    guards,
                    loops,
                    depth,
                })
            }
        }
    }
    fn enter_initializer<'tree>(
        &mut self,
        entry: BodyEntry<'tree>,
        function: &mut FunctionModel,
        pending: &mut Vec<BodyWork<'tree>>,
    ) -> bool {
        let BodyEntry {
            node,
            guards,
            loops,
            depth,
        } = entry;
        let kind = node.kind();
        if function.is_module_initializer() {
            let runtime = if is_function(kind, self.dialect) {
                if self.dialect == Dialect::Python {
                    [child(node, "parameters"), child(node, "return_type")]
                        .into_iter()
                        .flatten()
                        .collect()
                } else {
                    Vec::new()
                }
            } else if matches!(
                kind,
                "import_statement"
                    | "import_require_clause"
                    | "type_annotation"
                    | "type_query"
                    | "type_alias_declaration"
                    | "interface_declaration"
                    | "ambient_declaration"
                    | "function_type"
                    | "constructor_type"
            ) || (self.dialect == Dialect::Php && is_type(kind, self.dialect))
            {
                Vec::new()
            } else if self.dialect == Dialect::Python && kind == "class_definition" {
                [child(node, "superclasses"), child(node, "body")]
                    .into_iter()
                    .flatten()
                    .collect()
            } else if matches!(
                kind,
                "class" | "class_declaration" | "abstract_class_declaration"
            ) {
                self.module_class_runtime_nodes(node)
            } else {
                self.record_decorator_call(node, function, &guards, &loops);
                // Ordinary nodes are handled below.
                Vec::new()
            };
            if is_function(kind, self.dialect)
                || (self.dialect == Dialect::Php && is_type(kind, self.dialect))
                || (self.dialect == Dialect::Python && kind == "class_definition")
                || matches!(
                    kind,
                    "import_statement"
                        | "import_require_clause"
                        | "type_annotation"
                        | "type_query"
                        | "type_alias_declaration"
                        | "interface_declaration"
                        | "ambient_declaration"
                        | "function_type"
                        | "constructor_type"
                        | "class"
                        | "class_declaration"
                        | "abstract_class_declaration"
                )
            {
                pending.extend(
                    runtime
                        .into_iter()
                        .rev()
                        .map(|node| BodyWork::Enter(node, guards.clone(), loops.clone(), depth)),
                );
                return true;
            }
        }
        false
    }
    fn record_decorator_call(
        &self,
        node: Node<'_>,
        function: &mut FunctionModel,
        guards: &Arc<[Guard]>,
        loops: &Arc<[LoopContext]>,
    ) {
        if self.dialect == Dialect::Python && node.kind() == "decorator" {
            if let Some(expression) = named_children(node).first().copied() {
                let raw = snippet(expression, self.source);
                let (receiver, name, call_kind) = split_call(raw, "call", self.dialect);
                let factory = is_call(expression.kind(), self.dialect);
                function.calls.push(CallSite {
                    name: if factory {
                        "<decorator_result>".into()
                    } else {
                        name
                    },
                    receiver: if factory { None } else { receiver },
                    kind: if factory {
                        CallKind::Dynamic
                    } else {
                        call_kind
                    },
                    binding_unknown: factory,
                    range: range(expression, self.path),
                    guard: guards.clone(),
                    in_loop: loops.clone(),
                    ..Default::default()
                });
            }
        }
    }
    fn skip_nested_body<'tree>(
        &mut self,
        frame: &mut BodyChildren<'tree>,
        c: Node<'tree>,
        function: &FunctionModel,
        pending: &mut Vec<BodyWork<'tree>>,
    ) -> bool {
        let node = frame.node;
        let kind = node.kind();
        let tail_guards = frame.guards.clone();
        let loops = frame.loops.clone();
        let depth = frame.depth;
        if c.id() != node.id() && is_function(c.kind(), self.dialect) {
            pending.push(BodyWork::Children {
                node,
                children: std::mem::take(&mut frame.children),
                guards: tail_guards.clone(),
                loops: loops.clone(),
                depth,
            });
            if function.is_module_initializer() && self.dialect == Dialect::Python {
                pending.push(BodyWork::Enter(c, tail_guards, loops, depth));
            } else if function.is_module_initializer()
                && c.kind() == "method_definition"
                && kind == "object"
            {
                if let Some(key) =
                    child(c, "name").filter(|key| key.kind() == "computed_property_name")
                {
                    pending.push(BodyWork::Enter(key, tail_guards, loops, depth));
                }
            }
            return true;
        }
        false
    }
    fn skip_comprehension_condition<'tree>(
        &mut self,
        frame: &mut BodyChildren<'tree>,
        c: Node<'tree>,
        _function: &FunctionModel,
        pending: &mut Vec<BodyWork<'tree>>,
    ) -> bool {
        let node = frame.node;
        let kind = node.kind();
        let tail_guards = frame.guards.clone();
        let loops = frame.loops.clone();
        let depth = frame.depth;
        if [
            "list_comprehension",
            "dictionary_comprehension",
            "set_comprehension",
            "generator_expression",
        ]
        .contains(&kind)
            && c.kind() == "if_clause"
        {
            pending.push(BodyWork::Children {
                node,
                children: std::mem::take(&mut frame.children),
                guards: tail_guards,
                loops,
                depth,
            });
            return true;
        }
        false
    }
    fn body_child_context<'tree>(
        &self,
        frame: &BodyChildren<'tree>,
        c: Node<'tree>,
    ) -> BodyEntry<'tree> {
        let node = frame.node;
        let kind = node.kind();
        let tail_guards = &frame.guards;
        let loops = &frame.loops;
        let depth = frame.depth;
        let mut g = tail_guards.clone();
        let mut l = loops.clone();
        let mut d = depth;
        if is_loop(kind) {
            let mut contexts = l.to_vec();
            contexts.push(LoopContext {
                header: loop_header(node, self.source),
                line: node.start_position().row + 1,
                range: range(node, self.path),
            });
            l = contexts.into();
            d += 1;
        }
        if kind == "if_statement"
            && self.dialect == Dialect::Python
            && matches!(c.kind(), "elif_clause" | "else_clause")
        {
            for previous in named_children(node)
                .into_iter()
                .take_while(|n| n.id() != c.id())
            {
                if previous.kind() == "elif_clause" {
                    if let Some(condition) = child(previous, "condition") {
                        let raw = snippet(condition, self.source).trim();
                        let mut guard = make_guard("If", raw, "else", range(previous, self.path));
                        guard.normalized = negate_condition(raw);
                        let mut contexts = g.to_vec();
                        contexts.push(guard);
                        g = contexts.into();
                    }
                }
            }
        }
        if is_branch(kind) {
            if let Some(guard) = self.guard_for(node, c) {
                let mut contexts = g.to_vec();
                contexts.insert(tail_guards.len(), guard);
                g = contexts.into();
                d += 1;
            }
        }
        if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript) {
            if let Some(guard) =
                properties::logical_assignment_guard(node, c, self.source, self.path)
            {
                let mut contexts = g.to_vec();
                contexts.push(guard);
                g = contexts.into();
            }
        }
        BodyEntry {
            node: c,
            guards: g,
            loops: l,
            depth: d,
        }
    }
    fn record_early_exit<'tree>(&self, frame: &mut BodyChildren<'tree>, c: Node<'tree>) {
        let node = frame.node;
        let kind = node.kind();
        let mut tail_guards = frame.guards.clone();
        if [
            "block",
            "statement_block",
            "compound_statement",
            "declaration_list",
            "module",
            "program",
            "source_file",
        ]
        .contains(&kind)
            && ["if_statement", "if_expression"].contains(&c.kind())
            && child(c, "alternative").is_none()
        {
            if let Some(consequence) = child(c, "consequence").or_else(|| child(c, "body")) {
                if contains_terminator(consequence) {
                    if let Some(condition) = child(c, "condition") {
                        let raw = snippet(condition, self.source).trim();
                        let mut guard =
                            make_guard("EarlyExit", raw, "after_exit", range(c, self.path));
                        guard.normalized = negate_condition(raw);
                        let mut contexts = tail_guards.to_vec();
                        contexts.push(guard);
                        tail_guards = contexts.into();
                    }
                }
            }
        }
        frame.guards = tail_guards;
    }
    fn record_try_success<'tree>(&self, frame: &mut BodyChildren<'tree>, c: Node<'tree>) {
        let node = frame.node;
        let kind = node.kind();
        let mut tail_guards = frame.guards.clone();
        if [
            "block",
            "statement_block",
            "compound_statement",
            "declaration_list",
        ]
        .contains(&kind)
        {
            if let Some(try_node) = find_unconditional_try(c) {
                let raw = snippet(try_node, self.source).trim();
                let mut contexts = tail_guards.to_vec();
                contexts.push(make_guard(
                    "ShortCircuit",
                    raw,
                    "success",
                    range(try_node, self.path),
                ));
                tail_guards = contexts.into();
            }
        }
        frame.guards = tail_guards;
    }
    fn visit_body_child<'tree>(
        &mut self,
        mut frame: BodyChildren<'tree>,
        function: &mut FunctionModel,
        pending: &mut Vec<BodyWork<'tree>>,
    ) {
        let Some(c) = frame.children.next() else {
            return;
        };
        if self.skip_nested_body(&mut frame, c, function, pending)
            || self.skip_comprehension_condition(&mut frame, c, function, pending)
        {
            return;
        }
        let entry = self.body_child_context(&frame, c);
        self.record_early_exit(&mut frame, c);
        self.record_try_success(&mut frame, c);
        pending.push(frame.into_work());
        pending.push(BodyWork::Enter(
            entry.node,
            entry.guards,
            entry.loops,
            entry.depth,
        ));
    }

    fn module_class_runtime_nodes<'tree>(&self, node: Node<'tree>) -> Vec<Node<'tree>> {
        let mut nodes = Vec::new();
        for child_node in named_children(node) {
            if matches!(
                child_node.kind(),
                "class_heritage" | "extends_clause" | "decorator"
            ) {
                nodes.push(child_node);
            }
        }
        let Some(body) = child(node, "body") else {
            return nodes;
        };
        for member in named_children(body) {
            if let Some(key) = child(member, "name")
                .or_else(|| child(member, "property"))
                .filter(|key| key.kind() == "computed_property_name")
            {
                nodes.push(key);
            }
            nodes.extend(
                named_children(member)
                    .into_iter()
                    .filter(|node| node.kind() == "decorator"),
            );
            if member.kind() == "class_static_block" {
                nodes.push(member);
            } else if matches!(
                member.kind(),
                "field_definition" | "public_field_definition"
            ) && modifiers(member, self.source)
                .iter()
                .any(|modifier| modifier == "static")
            {
                if let Some(value) = child(member, "value") {
                    nodes.push(value);
                }
            }
        }
        nodes
    }
    fn guard_for(&self, parent: Node<'_>, child_node: Node<'_>) -> Option<Guard> {
        let kind = parent.kind();
        if ["binary_expression", "binary_operator", "boolean_operator"].contains(&kind) {
            let raw = child(parent, "operator")
                .map(|n| snippet(n, self.source))
                .unwrap_or_else(|| snippet(parent, self.source));
            let op = if raw.contains("&&") || raw == "and" {
                "&&"
            } else if raw.contains("||") || raw == "or" {
                "||"
            } else if raw.contains("??") {
                "??"
            } else {
                return None;
            };
            let right = child(parent, "right").or_else(|| named_children(parent).last().copied());
            if right.map(|n| n.id()) != Some(child_node.id()) {
                return None;
            }
            let left = child(parent, "left").or_else(|| named_children(parent).first().copied())?;
            let condition = snippet(left, self.source).trim();
            let mut guard = make_guard("ShortCircuit", condition, op, range(parent, self.path));
            if op == "||" {
                guard.normalized = negate_condition(condition);
            } else if op == "??" {
                guard.normalized = format!("{} == null", normalize_condition(condition));
            }
            return Some(guard);
        }
        let cond = child(parent, "condition")
            .or_else(|| child(parent, "test"))
            .or_else(|| {
                first_kind(
                    parent,
                    &[
                        "comparison_operator",
                        "binary_expression",
                        "identifier",
                        "parenthesized_expression",
                    ],
                )
            });
        let raw = cond.map(|n| snippet(n, self.source)).unwrap_or("").trim();
        let branch = match kind {
            "if_statement" | "if_expression" | "elif_clause" => {
                if child_node.kind().contains("else") || child_node.kind() == "elif_clause" {
                    "else"
                } else if cond.map(|n| n.id()) == Some(child_node.id()) {
                    return None;
                } else {
                    "then"
                }
            }
            "conditional_expression" | "ternary_expression" => {
                if cond.map(|n| n.id()) == Some(child_node.id()) {
                    return None;
                } else if child(parent, "alternative").map(|n| n.id()) == Some(child_node.id()) {
                    "else"
                } else {
                    "then"
                }
            }
            "switch_statement" | "match_expression" | "match_statement" => {
                if child_node.kind().contains("case") || child_node.kind().contains("arm") {
                    "case"
                } else {
                    return None;
                }
            }
            "switch_case" | "match_arm" | "case_clause" | "match_conditional_expression" => "case",
            "switch_default" | "match_default_expression" => "default",
            "try_statement" => {
                if child_node.kind().contains("catch") || child_node.kind().contains("except") {
                    "catch"
                } else if child_node.kind().contains("finally") {
                    "finally"
                } else if child_node.kind().contains("else") {
                    "else"
                } else {
                    "try"
                }
            }
            "catch_clause" | "except_clause" | "finally_clause" => return None,
            _ => return None,
        };
        let branch_raw = if branch == "case" {
            if kind == "match_arm" {
                snippet(parent, self.source)
                    .split_once("=>")
                    .map(|(x, _)| x.trim())
                    .unwrap_or(raw)
            } else {
                child(parent, "value")
                    .or_else(|| child(parent, "pattern"))
                    .or_else(|| child(parent, "conditional_expressions"))
                    .or_else(|| first_kind(parent, &["case_pattern"]))
                    .map(|n| snippet(n, self.source))
                    .unwrap_or_else(|| snippet(parent, self.source).lines().next().unwrap_or(raw))
                    .trim()
            }
        } else if branch == "catch" {
            match self.dialect {
                Dialect::JavaScript | Dialect::TypeScript => "",
                Dialect::Python => child(child_node, "value")
                    .map(|value| {
                        if value.kind() == "as_pattern" {
                            named_children(value)
                                .into_iter()
                                .find(|n| Some(n.id()) != child(value, "alias").map(|a| a.id()))
                                .map(|n| snippet(n, self.source))
                                .unwrap_or("")
                        } else {
                            snippet(value, self.source)
                        }
                    })
                    .unwrap_or(""),
                _ => child(child_node, "type")
                    .map(|n| snippet(n, self.source))
                    .unwrap_or(""),
            }
        } else {
            raw
        };
        let mut guard = make_guard(
            match kind {
                "try_statement" | "catch_clause" | "except_clause" | "finally_clause" => "TryCatch",
                "switch_statement"
                | "switch_case"
                | "switch_default"
                | "match_expression"
                | "match_statement"
                | "match_arm"
                | "case_clause"
                | "match_conditional_expression"
                | "match_default_expression" => "Switch/Match",
                "conditional_expression" | "ternary_expression" => "Ternary",
                _ => "If",
            },
            branch_raw,
            branch,
            range(parent, self.path),
        );
        if branch == "else" && !branch_raw.is_empty() {
            guard.normalized = negate_condition(branch_raw);
        }
        Some(guard)
    }
    fn call_site(&mut self, node: Node<'_>, guards: &[Guard], loops: &[LoopContext]) -> CallSite {
        let callee = child(node, "function")
            .or_else(|| child(node, "constructor"))
            .or_else(|| child(node, "name"))
            .or_else(|| {
                first_kind(
                    node,
                    &[
                        "member_expression",
                        "field_expression",
                        "scoped_identifier",
                        "identifier",
                        "variable_name",
                        "name",
                        "qualified_name",
                        "attribute",
                    ],
                )
            });
        // tree-sitter-typescript parses `await a.b<T>(x)` as a call whose function is
        // `await a.b`; the awaited expression is the real callee.
        let callee = callee.map(|n| {
            if n.kind() == "await_expression" {
                named_children(n).first().copied().unwrap_or(n)
            } else {
                n
            }
        });
        let target = callee
            .map(|n| snippet(n, self.source).trim())
            .unwrap_or_else(|| snippet(node, self.source).split('(').next().unwrap_or(""));
        let (receiver, name, kind) =
            if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
                && node.kind() == "call_expression"
            {
                callee
                    .and_then(|callee| properties::member(callee, self.source))
                    .map(|(receiver, name)| (Some(receiver), name, CallKind::Method))
                    .unwrap_or_else(|| split_call(target, node.kind(), self.dialect))
            } else {
                split_call(target, node.kind(), self.dialect)
            };
        let (receiver, kind) = if self.dialect == Dialect::Php {
            if let Some(r) = child(node, "object") {
                (
                    Some(snippet(r, self.source).trim_start_matches('$').into()),
                    CallKind::Method,
                )
            } else if let Some(r) = child(node, "scope") {
                (Some(snippet(r, self.source).into()), CallKind::Static)
            } else {
                (receiver, kind)
            }
        } else {
            (receiver, kind)
        };
        let (receiver, rust_trait) = if self.dialect == Dialect::Rust {
            receiver
                .map(|receiver| {
                    receiver
                        .strip_prefix('<')
                        .and_then(|raw| raw.strip_suffix('>'))
                        .and_then(|raw| raw.split_once(" as "))
                        .map(|(owner, contract)| {
                            (
                                Some(owner.trim().to_owned()),
                                Some(contract.trim().to_owned()),
                            )
                        })
                        .unwrap_or((Some(receiver), None))
                })
                .unwrap_or((None, None))
        } else {
            (receiver, None)
        };
        let args = child(node, "arguments")
            .map(|n| {
                named_children(n)
                    .into_iter()
                    .map(|a| snippet(a, self.source).to_string())
                    .collect()
            })
            .unwrap_or_default();
        let mut guard = guards.to_vec();
        if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript) {
            if let Some(callee) = callee {
                guard.extend(properties::presence_guards(callee, self.source, self.path));
            }
        } else if let Some((r, _)) = target.rsplit_once("?.") {
            guard.push(Guard {
                kind: "ShortCircuit".into(),
                raw: r.into(),
                normalized: format!("{} != null", normalize_condition(r)),
                branch: "present".into(),
                range: range(node, self.path),
            });
        }
        if self.dialect == Dialect::Php && node.kind() == "nullsafe_member_call_expression" {
            if let Some(r) = child(node, "object") {
                let raw = snippet(r, self.source);
                guard.push(Guard {
                    kind: "ShortCircuit".into(),
                    raw: raw.into(),
                    normalized: format!("{} != null", normalize_condition(raw)),
                    branch: "present".into(),
                    range: range(node, self.path),
                });
            }
        }
        let guard = if let Some(shared) = self.guard_contexts.get(guard.as_slice()) {
            shared.clone()
        } else {
            let shared: Arc<[Guard]> = guard.into();
            self.guard_contexts.insert(shared.clone(), shared.clone());
            shared
        };
        let in_loop = if let Some(shared) = self.loop_contexts.get(loops) {
            shared.clone()
        } else {
            let shared: Arc<[LoopContext]> = loops.into();
            self.loop_contexts.insert(shared.clone(), shared.clone());
            shared
        };
        // Catch variables have lexical scope: two handlers may reuse $e with different types.
        let mut receiver_type = None;
        if self.dialect == Dialect::Php {
            let mut ancestor = node.parent();
            while let Some(parent) = ancestor {
                if is_function(parent.kind(), self.dialect) {
                    break;
                }
                if parent.kind() == "catch_clause"
                    && child(parent, "name").is_some_and(|n| {
                        receiver.as_deref() == Some(snippet(n, self.source).trim_start_matches('$'))
                    })
                {
                    receiver_type = child(parent, "type").and_then(|n| {
                        let types = named_children(n);
                        (types.len() == 1).then(|| trim_type(snippet(types[0], self.source)))
                    });
                    break;
                }
                ancestor = parent.parent();
            }
        }
        CallSite {
            name,
            receiver_type,
            rust_trait,
            range: range(node, self.path),
            kind,
            receiver,
            assignment_to: assigned_name(node, self.source),
            symbol_constant_assignment: matches!(
                self.dialect,
                Dialect::JavaScript | Dialect::TypeScript
            ) && static_js::symbol_module_constant_assignment(
                node,
                self.source,
            ),
            guard,
            in_loop,
            arguments: args,
            ..Default::default()
        }
    }
    fn resolve_local_types(&mut self) {
        let names: BTreeSet<String> = self.model.functions.iter().map(|f| f.fqn.clone()).collect();
        let method_map: BTreeMap<(String, String, bool), String> = self
            .model
            .functions
            .iter()
            // Invoking a callable property first reads its getter. A setter is
            // never the target of a method invocation with the same property name.
            .filter(|function| {
                !function
                    .modifiers
                    .iter()
                    .any(|modifier| modifier == "accessor")
                    || !function.fqn.ends_with("#set")
            })
            .filter(|function| function.rust_trait.is_none() && function.parent_function.is_none())
            .filter_map(|f| {
                f.parent_type.as_ref().map(|t| {
                    (
                        (
                            t.clone(),
                            f.name.clone(),
                            matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
                                && f.modifiers.iter().any(|modifier| modifier == "static"),
                        ),
                        f.fqn.clone(),
                    )
                })
            })
            .collect();
        let properties: BTreeMap<(String, String), String> = self
            .model
            .types
            .iter()
            .flat_map(|t| {
                t.fields.iter().filter_map(move |f| {
                    f.type_name
                        .as_ref()
                        .map(|ty| ((t.fqn.clone(), f.name.clone()), ty.clone()))
                })
            })
            .collect();
        let import_index = analyzer_core::scoped_imports::ScopedImports::new(
            self.model
                .imports
                .iter()
                .map(|i| (i, i.binding_path.as_ref().unwrap_or(&i.path).clone())),
        );
        let type_index = TypeIndex::new(&self.model.types, self.dialect);
        for f in &mut self.model.functions {
            let function_static = f
                .modifiers
                .iter()
                .any(|modifier| matches!(modifier.as_str(), "static" | "lexical_static_this"));
            let type_names = type_index.scope(&f.fqn);
            let mut vars: BTreeMap<String, String> = f
                .parameters
                .iter()
                .filter_map(|p| p.type_name.as_ref().map(|t| (p.name.clone(), trim_type(t))))
                .collect();
            if let Some(t) = &f.parent_type {
                for name in receiver_names(f, self.dialect) {
                    vars.insert(name.to_owned(), t.clone());
                }
            }
            for c in f
                .calls
                .iter_mut()
                .chain(
                    f.property_accesses
                        .iter_mut()
                        .map(|access| &mut access.site),
                )
                .chain(
                    f.property_mutations
                        .iter_mut()
                        .map(|mutation| &mut mutation.site),
                )
            {
                if c.binding_unknown || !c.receiver_types.is_empty() {
                    continue;
                }
                let imports = import_index.at(&c.range);
                if self.dialect != Dialect::Php
                    && c.kind == CallKind::Function
                    && (type_names.contains_key(&c.name))
                {
                    c.kind = CallKind::Constructor;
                }
                if c.kind == CallKind::Constructor && self.dialect == Dialect::Php {
                    if let Some(target) =
                        php_names::reference(&c.name, &f.fqn, &self.model.imports, &c.range)
                    {
                        c.targets = vec![target.trim_start_matches('\\').into()];
                        c.local_binding = true;
                    }
                } else if c.kind == CallKind::Constructor {
                    if let Some(t) = type_names.get(&c.name) {
                        c.targets.push(t.clone());
                        c.confidence = Confidence::Resolved;
                    }
                }
                if let Some(receiver) = &c.receiver {
                    let property_ty = receiver
                        .split_once('.')
                        .or_else(|| receiver.split_once("->"))
                        .and_then(|(root, field)| {
                            let root_ty = c
                                .receiver_root_type
                                .as_ref()
                                .or_else(|| (!c.local_binding).then(|| vars.get(root)).flatten());
                            root_ty.and_then(|parent| {
                                let resolved = type_names
                                    .get(parent)
                                    .or_else(|| imports.get(parent))
                                    .unwrap_or(parent);
                                properties.get(&(resolved.clone(), field.to_string()))
                            })
                        })
                        .cloned();
                    if let Some(ty) = c
                        .receiver_type
                        .clone()
                        .or(property_ty)
                        .or_else(|| {
                            (!c.local_binding)
                                .then(|| vars.get(receiver).cloned())
                                .flatten()
                        })
                        .or_else(|| {
                            (!c.local_binding)
                                .then(|| type_names.get(receiver).cloned())
                                .flatten()
                        })
                    {
                        let ty = type_names
                            .get(&ty)
                            .cloned()
                            .unwrap_or_else(|| imports.get(&ty).cloned().unwrap_or(ty));
                        c.receiver_type = Some(
                            if self.dialect == Dialect::Php
                                && !matches!(ty.as_str(), "self" | "static" | "parent")
                            {
                                format!("\\{}", ty.trim_start_matches('\\'))
                            } else {
                                ty.clone()
                            },
                        );
                        let static_receiver =
                            matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
                                && if matches!(receiver.as_str(), "this" | "super") {
                                    function_static
                                } else {
                                    !c.local_binding
                                        && (type_names.contains_key(receiver)
                                            || imports.contains_key(receiver))
                                };
                        if let Some(target) =
                            method_map.get(&(ty.clone(), c.name.clone(), static_receiver))
                        {
                            c.targets.push(target.clone());
                            c.confidence = Confidence::Resolved;
                        }
                    } else if !c.local_binding {
                        if let Some(t) = imports.get(receiver) {
                            c.receiver_type = Some(t.clone());
                        }
                    }
                } else if !c.local_binding {
                    let lexical = if self.dialect == Dialect::Rust
                        || (matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
                            && f.parent_type.is_none()
                            && f.parent_function.is_none())
                    {
                        f.fqn
                            .rsplit_once(if self.dialect == Dialect::Rust {
                                "::"
                            } else {
                                "."
                            })
                            .map(|(p, _)| p)
                            .unwrap_or(&self.model.module)
                    } else if self.dialect == Dialect::Php {
                        f.fqn
                            .split("::")
                            .next()
                            .unwrap_or(&f.fqn)
                            .rsplit_once('\\')
                            .map(|(p, _)| p)
                            .unwrap_or("")
                    } else {
                        &self.model.module
                    };
                    let candidate = if lexical.is_empty() {
                        c.name.clone()
                    } else {
                        format!(
                            "{lexical}{}{}",
                            match self.dialect {
                                Dialect::Rust => "::",
                                Dialect::Php => "\\",
                                _ => ".",
                            },
                            c.name
                        )
                    };
                    if names.contains(&candidate) {
                        c.targets.push(candidate);
                        c.confidence = Confidence::Resolved;
                    }
                }
            }
            // Property targets require descriptor lookup, including read/write direction
            // and inherited overrides, rather than ordinary method-name aliases.
            for access in &mut f.property_accesses {
                access.site.targets.clear();
                access.site.confidence = Confidence::Dynamic;
            }
        }
    }
}

// A fallible expression inside a branch, loop, or closure need not execute.
// Do not impose its success condition on subsequent sibling statements.

#[cfg(test)]
mod condition_normalization_tests {
    use super::normalize_condition;

    #[test]
    fn inverts_only_a_single_top_level_comparison() {
        assert_eq!(normalize_condition("!(count >= 3)"), "!(count >= 3)");
        assert_eq!(normalize_condition("!(value === 0)"), "value !== 0");
        assert_eq!(normalize_condition("!(value !== 0)"), "value === 0");
        assert_eq!(normalize_condition("!(a > b && c)"), "!(a > b && c)");
        assert_eq!(
            normalize_condition("!(a == b || c == d)"),
            "!(a == b || c == d)"
        );
        assert_eq!(
            normalize_condition("!(a > b and ready)"),
            "!(a > b and ready)"
        );
        assert_eq!(normalize_condition("!(a < b < c)"), "!(a < b < c)");
        assert_eq!(normalize_condition("!(x => x > 0)"), "!(x => x > 0)");
        assert_eq!(normalize_condition("!a == b"), "!a == b");
        assert_eq!(
            normalize_condition("!(call(1 > 0) && ready)"),
            "!(call(1 > 0) && ready)"
        );
    }

    #[test]
    fn preserves_partial_ordering_and_overloaded_comparisons() {
        for expression in ["x > 0", "x >= 0", "x < 0", "x <= 0", "x == y", "x != y"] {
            for negation in [format!("!({expression})"), format!("not ({expression})")] {
                assert_eq!(normalize_condition(&negation), negation);
            }
        }
        assert_eq!(normalize_condition("!(!(ready))"), "ready");
    }
}

#[cfg(test)]
mod type_index_tests {
    use super::*;

    // The original materialized scope is an independent oracle for precedence.
    fn original(types: &[TypeModel], owner: &str, sep: &str) -> BTreeMap<String, String> {
        let concrete = types
            .iter()
            .filter(|t| !matches!(t.kind, TypeKind::Namespace | TypeKind::Impl))
            .collect::<Vec<_>>();
        let mut result = concrete
            .iter()
            .map(|t| (t.fqn.clone(), t.fqn.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut context = owner;
        while let Some((parent, _)) = context.rsplit_once(sep) {
            for ty in &concrete {
                if ty.fqn == format!("{parent}{sep}{}", ty.name) {
                    result
                        .entry(ty.name.clone())
                        .or_insert_with(|| ty.fqn.clone());
                }
            }
            context = parent;
        }
        result
    }

    #[test]
    fn indexed_scopes_match_materialized_scopes_across_languages() {
        for dialect in [
            Dialect::Rust,
            Dialect::Python,
            Dialect::Php,
            Dialect::JavaScript,
            Dialect::TypeScript,
        ] {
            let sep = match dialect {
                Dialect::Rust => "::",
                Dialect::Php => "\\",
                _ => ".",
            };
            let mut types = Vec::new();
            for namespace in ["", "root", "root.inner", "root.inner.deep", "other"] {
                let namespace = namespace.replace('.', sep);
                for i in 0..30 {
                    let name = format!("T{i}");
                    types.push(TypeModel {
                        fqn: if namespace.is_empty() {
                            name.clone()
                        } else {
                            format!("{namespace}{sep}{name}")
                        },
                        name,
                        kind: if i % 10 == 0 {
                            TypeKind::Namespace
                        } else if i % 10 == 1 {
                            TypeKind::Impl
                        } else {
                            TypeKind::Class
                        },
                        ..Default::default()
                    });
                }
            }
            // Inconsistent metadata and names containing separators must retain old behavior.
            types.push(TypeModel {
                name: format!("inner{sep}Special"),
                fqn: format!("root{sep}inner{sep}Special"),
                ..Default::default()
            });
            types.push(TypeModel {
                name: "Alias".into(),
                fqn: "different".into(),
                ..Default::default()
            });
            let index = TypeIndex::new(&types, dialect);
            for owner in [
                "f",
                "root.f",
                "root.inner.f",
                "root.inner.deep.method.f",
                "other.f",
                "missing.f",
            ] {
                let owner = owner.replace('.', sep);
                let expected = original(&types, &owner, sep);
                let scoped = index.scope(&owner);
                for query in types
                    .iter()
                    .flat_map(|t| [&t.name, &t.fqn])
                    .map(String::as_str)
                    .chain(["missing"])
                {
                    assert_eq!(
                        scoped.get(query),
                        expected.get(query),
                        "{dialect:?} {owner} {query}"
                    );
                }
            }
        }
    }
}
