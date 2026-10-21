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
use std::path::{Path, PathBuf};
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dialect {
    Python,
    JavaScript,
    TypeScript,
    Php,
    Rust,
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

/// The grammar distinguishes comments from literal contents, including template
/// substitutions, raw strings, heredocs, and Python multiline strings.
fn comment_nodes(root: Node<'_>) -> Vec<Node<'_>> {
    let mut comments = Vec::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if matches!(node.kind(), "comment" | "line_comment" | "block_comment") {
            comments.push(node);
        } else {
            pending.extend(named_children(node).into_iter().rev());
        }
    }
    comments.sort_by_key(Node::start_byte);
    comments
}

fn count_lines_with_comments(source: &str, comments: &[Node<'_>]) -> Loc {
    let mut loc = Loc::default();
    let mut offset = 0;
    let mut comment_index = 0;
    for line in source.split_inclusive('\n') {
        loc.total += 1;
        let trimmed = if offset == 0 {
            line.trim_start_matches('\u{feff}').trim()
        } else {
            line.trim()
        };
        if trimmed.is_empty() {
            loc.blank += 1;
        } else {
            let mut has_code = false;
            for (column, ch) in line.char_indices() {
                if ch.is_whitespace() || (offset == 0 && column == 0 && ch == '\u{feff}') {
                    continue;
                }
                let byte = offset + column;
                while comments
                    .get(comment_index)
                    .is_some_and(|node| node.end_byte() <= byte)
                {
                    comment_index += 1;
                }
                if comments
                    .get(comment_index)
                    .is_none_or(|node| node.start_byte() > byte)
                {
                    has_code = true;
                    break;
                }
            }
            if has_code {
                loc.code += 1;
            } else {
                loc.comment += 1;
            }
        }
        offset += line.len();
    }
    loc
}

pub fn language_name(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::Python => "python",
        Dialect::JavaScript => "javascript",
        Dialect::TypeScript => "typescript",
        Dialect::Php => "php",
        Dialect::Rust => "rust",
    }
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
        .filter(|i| bindings::applies(i, &call.range))
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

/// One borrowed type index per file; scope lookup walks only the owner's ancestors.
struct TypeIndex<'a> {
    full: BTreeMap<&'a str, &'a String>,
    namespaces: BTreeMap<&'a str, BTreeMap<&'a str, &'a String>>,
    separator: &'static str,
}
impl<'a> TypeIndex<'a> {
    fn new(types: &'a [TypeModel], dialect: Dialect) -> Self {
        let separator = match dialect {
            Dialect::Rust => "::",
            Dialect::Php => "\\",
            _ => ".",
        };
        let mut full = BTreeMap::new();
        let mut namespaces = BTreeMap::<_, BTreeMap<_, _>>::new();
        for ty in types
            .iter()
            .filter(|t| !matches!(t.kind, TypeKind::Namespace | TypeKind::Impl))
        {
            full.insert(ty.fqn.as_str(), &ty.fqn);
            if let Some(parent) = ty
                .fqn
                .strip_suffix(&ty.name)
                .and_then(|p| p.strip_suffix(separator))
            {
                namespaces
                    .entry(parent)
                    .or_default()
                    .entry(ty.name.as_str())
                    .or_insert(&ty.fqn);
            }
        }
        Self {
            full,
            namespaces,
            separator,
        }
    }
    fn scope<'b>(&'b self, owner: &'b str) -> ScopedTypes<'b, 'a> {
        ScopedTypes { index: self, owner }
    }
}
struct ScopedTypes<'a, 'types> {
    index: &'a TypeIndex<'types>,
    owner: &'a str,
}
impl ScopedTypes<'_, '_> {
    fn get(&self, name: &str) -> Option<&String> {
        if self.index.separator == "\\" {
            if let Some(absolute) = name.strip_prefix('\\') {
                return self.index.full.get(absolute).copied();
            }
        }
        if let Some(ty) = self.index.full.get(name) {
            return Some(ty);
        }
        let mut context = self.owner;
        while let Some((parent, _)) = context.rsplit_once(self.index.separator) {
            if let Some(ty) = self
                .index
                .namespaces
                .get(parent)
                .and_then(|types| types.get(name))
            {
                return Some(ty);
            }
            context = parent;
        }
        None
    }
    fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }
}

fn module_name(path: &Path, dialect: Dialect) -> String {
    let mut p = path.with_extension("").to_string_lossy().replace('\\', "/");
    if p.ends_with("/__init__") {
        p.truncate(p.len() - 9);
    }
    match dialect {
        Dialect::Php => p.replace('/', "\\"),
        Dialect::Rust => p.replace('/', "::"),
        Dialect::Python => p.replace('/', "."),
        Dialect::JavaScript | Dialect::TypeScript => p,
    }
}

thread_local! {
    static RANGE_PATH: RefCell<Option<(PathBuf, Arc<str>)>> = const { RefCell::new(None) };
}
fn range_path(path: &Path) -> Arc<str> {
    RANGE_PATH.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some((previous, shared)) = slot.as_ref() {
            if previous == path {
                return shared.clone();
            }
        }
        let shared: Arc<str> = path.to_string_lossy().as_ref().into();
        *slot = Some((path.to_path_buf(), shared.clone()));
        shared
    })
}
fn range(node: Node<'_>, path: &Path) -> SourceRange {
    let a = node.start_position();
    let b = node.end_position();
    SourceRange {
        file: range_path(path),
        start_line: a.row + 1,
        start_column: a.column + 1,
        end_line: b.row + 1,
        end_column: b.column + 1,
    }
}
fn snippet<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    source.get(node.byte_range()).unwrap_or("")
}
fn child<'a>(node: Node<'a>, field: &str) -> Option<Node<'a>> {
    node.child_by_field_name(field)
}
fn named_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut c = node.walk();
    node.named_children(&mut c).collect()
}
fn first_kind<'a>(node: Node<'a>, kinds: &[&str]) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .find(|n| kinds.contains(&n.kind()));
    found
}
fn text_field(node: Node<'_>, field: &str, source: &str) -> Option<String> {
    child(node, field).map(|n| snippet(n, source).to_string())
}

// Static JS/TS import/export strings use ECMAScript string values, not source spelling.
// Invalid escapes, legacy octal and lone UTF-16 surrogates cannot prove a module path.
fn js_module_literal(raw: &str) -> Option<String> {
    let quote = raw.as_bytes().first().copied()?;
    if !matches!(quote, b'\'' | b'"') || raw.as_bytes().last().copied()? != quote || raw.len() < 2 {
        return None;
    }
    let mut chars = raw[1..raw.len() - 1].chars().peekable();
    let mut value = String::with_capacity(raw.len() - 2);
    while let Some(c) = chars.next() {
        if c != '\\' {
            if matches!(c, '\n' | '\r') {
                return None;
            }
            value.push(c);
            continue;
        }
        let escaped = chars.next()?;
        match escaped {
            '\n' | '\u{2028}' | '\u{2029}' => {}
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
            }
            'n' => value.push('\n'),
            'r' => value.push('\r'),
            't' => value.push('\t'),
            'b' => value.push('\u{8}'),
            'f' => value.push('\u{c}'),
            'v' => value.push('\u{b}'),
            '0' if !chars.peek().is_some_and(|c| c.is_ascii_digit()) => value.push('\0'),
            '0'..='9' => return None,
            'x' => {
                let mut code = 0;
                for _ in 0..2 {
                    code = code * 16 + chars.next()?.to_digit(16)?;
                }
                value.push(char::from_u32(code)?);
            }
            'u' => {
                let mut code = 0u32;
                if chars.peek() == Some(&'{') {
                    chars.next();
                    let mut digits = 0;
                    loop {
                        let c = chars.next()?;
                        if c == '}' {
                            if digits == 0 {
                                return None;
                            }
                            break;
                        }
                        code = code.checked_mul(16)?.checked_add(c.to_digit(16)?)?;
                        digits += 1;
                    }
                } else {
                    for _ in 0..4 {
                        code = code * 16 + chars.next()?.to_digit(16)?;
                    }
                    if (0xd800..=0xdbff).contains(&code) {
                        if chars.next()? != '\\' || chars.next()? != 'u' {
                            return None;
                        }
                        let mut low = 0;
                        for _ in 0..4 {
                            low = low * 16 + chars.next()?.to_digit(16)?;
                        }
                        if !(0xdc00..=0xdfff).contains(&low) {
                            return None;
                        }
                        code = 0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00);
                    }
                }
                value.push(char::from_u32(code)?);
            }
            // Quotes, backslashes and valid ECMAScript NonEscapeCharacters.
            c => value.push(c),
        }
    }
    Some(value)
}

fn ast_kind(kind: &str, dialect: Dialect) -> AstKind {
    if is_function(kind, dialect) {
        AstKind::Function
    } else if is_type(kind, dialect) {
        AstKind::Type
    } else if is_call(kind, dialect) {
        AstKind::Call
    } else if is_branch(kind) {
        AstKind::Branch
    } else if is_loop(kind) {
        AstKind::Loop
    } else if kind.contains("return") {
        AstKind::Return
    } else if ["module", "source_file", "program", "php_tag"].contains(&kind) {
        AstKind::File
    } else {
        AstKind::Other(kind.into())
    }
}
// Detailed owned ASTs and optional cost inference have finite recursive stack
// budgets. Normal metric and binding traversal uses explicit work stacks.
const MAX_RECURSIVE_DEPTH: usize = 256;
fn exceeds_recursive_depth(node: Node<'_>) -> bool {
    let mut pending = vec![(node, 0usize)];
    while let Some((node, depth)) = pending.pop() {
        if depth > MAX_RECURSIVE_DEPTH {
            return true;
        }
        let mut cursor = node.walk();
        pending.extend(
            node.named_children(&mut cursor)
                .map(|child| (child, depth + 1)),
        );
    }
    false
}

fn ast_node(
    node: Node<'_>,
    source: &str,
    path: &Path,
    dialect: Dialect,
    errors: &mut Vec<ParseError>,
) -> AstNode {
    if node.is_error() || node.is_missing() {
        errors.push(ParseError {
            message: format!("syntax error: {}", node.kind()),
            range: range(node, path),
        });
    }
    let mut cursor = node.walk();
    let children = node
        .named_children(&mut cursor)
        .map(|n| ast_node(n, source, path, dialect, errors))
        .collect();
    let text = if node.named_child_count() == 0 {
        Some(snippet(node, source).chars().take(160).collect())
    } else {
        None
    };
    AstNode {
        kind: ast_kind(node.kind(), dialect),
        raw_kind: node.kind().into(),
        range: range(node, path),
        text,
        children,
    }
}
fn collect_errors(node: Node<'_>, path: &Path, errors: &mut Vec<ParseError>) {
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        if !node.has_error() && !node.is_error() && !node.is_missing() {
            continue;
        }
        if node.is_error() || node.is_missing() {
            errors.push(ParseError {
                message: format!("syntax error: {}", node.kind()),
                range: range(node, path),
            });
        }
        // Missing punctuation is often an anonymous child. Named-only traversal
        // silently loses recovered delimiters such as ')' and '}'.
        let mut cursor = node.walk();
        let children = node.children(&mut cursor).collect::<Vec<_>>();
        pending.extend(children.into_iter().rev());
    }
}

fn is_function(kind: &str, d: Dialect) -> bool {
    match d {
        Dialect::Python => ["function_definition", "lambda"].contains(&kind),
        Dialect::JavaScript | Dialect::TypeScript => [
            "function_declaration",
            "generator_function_declaration",
            "function_expression",
            "generator_function",
            "arrow_function",
            "method_definition",
            "function_signature",
            "method_signature",
            "abstract_method_signature",
        ]
        .contains(&kind),
        Dialect::Php => [
            "function_definition",
            "method_declaration",
            "anonymous_function",
            "arrow_function",
        ]
        .contains(&kind),
        Dialect::Rust => [
            "function_item",
            "function_signature_item",
            "closure_expression",
        ]
        .contains(&kind),
    }
}
fn is_type(kind: &str, d: Dialect) -> bool {
    match d {
        Dialect::Python => kind == "class_definition",
        Dialect::JavaScript => ["class_declaration", "class"].contains(&kind),
        Dialect::TypeScript => [
            "class_declaration",
            "abstract_class_declaration",
            "class",
            "interface_declaration",
            "enum_declaration",
            "internal_module",
        ]
        .contains(&kind),
        Dialect::Php => [
            "class_declaration",
            "trait_declaration",
            "interface_declaration",
            "enum_declaration",
            "anonymous_class",
        ]
        .contains(&kind),
        Dialect::Rust => [
            "struct_item",
            "enum_item",
            "trait_item",
            "impl_item",
            "mod_item",
            "type_item",
        ]
        .contains(&kind),
    }
}
fn is_call(kind: &str, d: Dialect) -> bool {
    match d {
        Dialect::Python => kind == "call",
        Dialect::JavaScript | Dialect::TypeScript => {
            ["call_expression", "new_expression"].contains(&kind)
        }
        Dialect::Php => [
            "function_call_expression",
            "member_call_expression",
            "nullsafe_member_call_expression",
            "scoped_call_expression",
            "object_creation_expression",
        ]
        .contains(&kind),
        Dialect::Rust => ["call_expression", "macro_invocation"].contains(&kind),
    }
}
fn is_branch(kind: &str) -> bool {
    [
        "if_statement",
        "if_expression",
        "elif_clause",
        "else_clause",
        "switch_statement",
        "switch_case",
        "switch_default",
        "match_expression",
        "match_statement",
        "match_arm",
        "case_clause",
        "try_statement",
        "except_clause",
        "catch_clause",
        "finally_clause",
        "conditional_expression",
        "ternary_expression",
        "match_conditional_expression",
        "match_default_expression",
        "binary_expression",
        "binary_operator",
        "boolean_operator",
    ]
    .contains(&kind)
}
fn is_loop(kind: &str) -> bool {
    [
        "for_statement",
        "for_in_statement",
        "for_expression",
        "for_in_clause",
        "while_statement",
        "while_expression",
        "loop_expression",
        "do_statement",
        "do_while_statement",
        "foreach_statement",
        "list_comprehension",
        "dictionary_comprehension",
        "set_comprehension",
        "generator_expression",
    ]
    .contains(&kind)
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
        let name = if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript) {
            properties::declared_name(node, self.source).unwrap_or(name)
        } else {
            name
        };
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
                        fqn = format!(
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
                        fqn = format!(
                            "{owner}{sep}{}{sep}{identity_name}",
                            snippet(binding, self.source)
                        );
                    }
                    break;
                }
                ancestor = parent.parent();
            }
        }
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
        let mut function = FunctionModel {
            name,
            fqn: fqn.clone(),
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
            apply_phpdoc(node, self.source, &mut function);
        }
        self.walk_body(body_node, &mut function, &[], &[], 0);
        function.cognitive_complexity =
            Some(cognitive_complexity(body_node, self.source, self.dialect));
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
                &function,
                self.source,
                self.path,
                &self.cost_type_shadows,
            )
        } else {
            cost_dynamic::extract(
                body_node,
                &function,
                self.source,
                self.path,
                self.dialect,
                &self.cost_type_shadows,
                !self.namespace.is_empty(),
            )
        }));
        let mut fields = BTreeSet::new();
        collect_accessed_fields(
            body_node,
            self.source,
            self.dialect,
            &receiver_names(&function, self.dialect),
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
        function.accessor = classify_accessor(body_node, &function, self.source, self.dialect);
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
        self.model.functions.push(function);
        fqn
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
        // Resume sibling traversal after each child, retaining source order and
        // the early-exit guards inherited only by subsequent siblings. Contexts
        // are shared so expression depth does not clone branch environments.
        enum Work<'tree> {
            Enter(Node<'tree>, Arc<[Guard]>, Arc<[LoopContext]>, usize),
            Children {
                node: Node<'tree>,
                children: std::vec::IntoIter<Node<'tree>>,
                guards: Arc<[Guard]>,
                loops: Arc<[LoopContext]>,
                depth: usize,
            },
        }
        let mut pending = vec![Work::Enter(node, guards.into(), loops.into(), depth)];
        while let Some(work) = pending.pop() {
            let (node, mut children, mut tail_guards, loops, depth) = match work {
                Work::Enter(node, guards, loops, depth) => {
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
                            if self.dialect == Dialect::Python && kind == "decorator" {
                                if let Some(expression) = named_children(node).first().copied() {
                                    let raw = snippet(expression, self.source);
                                    let (receiver, name, call_kind) =
                                        split_call(raw, "call", self.dialect);
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
                            pending.extend(runtime.into_iter().rev().map(|node| {
                                Work::Enter(node, guards.clone(), loops.clone(), depth)
                            }));
                            continue;
                        }
                    }
                    if node.id() != function.body_range.start_line as usize
                        && is_function(kind, self.dialect)
                        && node.start_position().row + 1 != function.range.start_line
                    {
                        continue;
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
                    (node, named_children(node).into_iter(), guards, loops, depth)
                }
                Work::Children {
                    node,
                    children,
                    guards,
                    loops,
                    depth,
                } => (node, children, guards, loops, depth),
            };
            let Some(c) = children.next() else { continue };
            let kind = node.kind();
            if c.id() != node.id() && is_function(c.kind(), self.dialect) {
                pending.push(Work::Children {
                    node,
                    children,
                    guards: tail_guards.clone(),
                    loops: loops.clone(),
                    depth,
                });
                if function.is_module_initializer() && self.dialect == Dialect::Python {
                    pending.push(Work::Enter(c, tail_guards, loops, depth));
                } else if function.is_module_initializer()
                    && c.kind() == "method_definition"
                    && kind == "object"
                {
                    if let Some(key) =
                        child(c, "name").filter(|key| key.kind() == "computed_property_name")
                    {
                        pending.push(Work::Enter(key, tail_guards, loops, depth));
                    }
                }
                continue;
            }
            if [
                "list_comprehension",
                "dictionary_comprehension",
                "set_comprehension",
                "generator_expression",
            ]
            .contains(&kind)
                && c.kind() == "if_clause"
            {
                pending.push(Work::Children {
                    node,
                    children,
                    guards: tail_guards,
                    loops,
                    depth,
                });
                continue;
            }
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
                            let mut guard =
                                make_guard("If", raw, "else", range(previous, self.path));
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
            pending.push(Work::Children {
                node,
                children,
                guards: tail_guards,
                loops,
                depth,
            });
            pending.push(Work::Enter(c, g, l, d));
        }
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

fn make_guard(kind: &str, raw: &str, branch: &str, range: SourceRange) -> Guard {
    Guard {
        kind: kind.into(),
        raw: raw.into(),
        normalized: normalize_condition(raw),
        branch: branch.into(),
        range,
    }
}
fn contains_terminator(mut node: Node<'_>) -> bool {
    loop {
        if [
            "return_statement",
            "return_expression",
            "throw_statement",
            "raise_statement",
            "continue_statement",
            "break_statement",
        ]
        .contains(&node.kind())
        {
            return true;
        }
        if !["block", "statement_block", "compound_statement"].contains(&node.kind()) {
            return false;
        }
        let Some(last) = named_children(node).last().copied() else {
            return false;
        };
        node = last;
    }
}
// A fallible expression inside a branch, loop, or closure need not execute.
// Do not impose its success condition on subsequent sibling statements.
fn find_unconditional_try(node: Node<'_>) -> Option<Node<'_>> {
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        if node.kind() == "try_expression" {
            return Some(node);
        }
        if is_branch(node.kind())
            || is_loop(node.kind())
            || [
                Dialect::Python,
                Dialect::JavaScript,
                Dialect::TypeScript,
                Dialect::Php,
                Dialect::Rust,
            ]
            .into_iter()
            .any(|dialect| is_function(node.kind(), dialect))
        {
            continue;
        }
        pending.extend(named_children(node).into_iter().rev());
    }
    None
}
fn find_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        if node.kind() == kind {
            return Some(node);
        }
        pending.extend(named_children(node).into_iter().rev());
    }
    None
}
fn negate_condition(raw: &str) -> String {
    let normalized = normalize_condition(raw);
    let inverted = normalize_condition(&format!("!({normalized})"));
    if inverted == format!("!({normalized})") {
        if normalized
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '$')
        {
            format!("!{normalized}")
        } else {
            inverted
        }
    } else {
        inverted
    }
}
pub fn normalize_condition(raw: &str) -> String {
    let s = raw.trim();
    let s = strip_parens(s);
    let symbolic_negation = s.starts_with('!');
    let neg = s.strip_prefix('!').or_else(|| s.strip_prefix("not "));
    if let Some(rest) = neg {
        let inner = strip_parens(rest.trim());
        if let Some(x) = inner
            .strip_prefix('!')
            .or_else(|| inner.strip_prefix("not "))
        {
            return strip_parens(x.trim()).into();
        }
        if let Some((index, operator)) = (!symbolic_negation || inner != rest.trim())
            .then(|| single_comparison(inner))
            .flatten()
        {
            let opposite = match operator {
                "===" => "!==",
                "!==" => "===",
                // Ordering is not total for NaN, and equality/inequality can
                // be overloaded independently. Text alone cannot prove their
                // complements. Strict equality operators are complements.
                _ => return s.into(),
            };
            return format!(
                "{} {} {}",
                inner[..index].trim(),
                opposite,
                inner[index + operator.len()..].trim()
            );
        }
    }
    s.into()
}
fn single_comparison(condition: &str) -> Option<(usize, &'static str)> {
    let bytes = condition.as_bytes();
    let mut index = 0;
    let mut depth = 0usize;
    let mut quoted = None;
    let mut escaped = false;
    let mut comparison = None;
    while index < bytes.len() {
        let character = condition[index..].chars().next()?;
        if let Some(quote) = quoted {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == quote {
                quoted = None;
            }
            index += character.len_utf8();
            continue;
        }
        if matches!(character, '\'' | '"' | '`') {
            quoted = Some(character);
            index += 1;
            continue;
        }
        if matches!(character, '(' | '[' | '{') {
            depth += 1;
            index += 1;
            continue;
        }
        if matches!(character, ')' | ']' | '}') {
            depth = depth.saturating_sub(1);
            index += 1;
            continue;
        }
        if depth == 0 {
            let rest = &condition[index..];
            if rest.starts_with("=>") {
                return None;
            }
            if rest.starts_with("&&")
                || rest.starts_with("||")
                || rest.starts_with("??")
                || rest.starts_with('?')
                || word_at(condition, index, "and")
                || word_at(condition, index, "or")
            {
                return None;
            }
            let operator = ["===", "!==", ">=", "<=", "==", "!=", ">", "<"]
                .into_iter()
                .find(|operator| rest.starts_with(operator));
            if let Some(operator) = operator {
                if comparison.is_some()
                    || index == 0
                    || condition[index + operator.len()..].trim().is_empty()
                {
                    return None;
                }
                comparison = Some((index, operator));
                index += operator.len();
                continue;
            }
        }
        index += character.len_utf8();
    }
    comparison
}
fn word_at(source: &str, index: usize, word: &str) -> bool {
    let bytes = source.as_bytes();
    source[index..].starts_with(word)
        && (index == 0 || !bytes[index - 1].is_ascii_alphanumeric() && bytes[index - 1] != b'_')
        && (index + word.len() == bytes.len()
            || !bytes[index + word.len()].is_ascii_alphanumeric()
                && bytes[index + word.len()] != b'_')
}
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
fn strip_parens(mut s: &str) -> &str {
    loop {
        let t = s.trim();
        if t.starts_with('(') && t.ends_with(')') {
            let mut depth = 0;
            let mut wraps = true;
            for (i, ch) in t.char_indices() {
                if ch == '(' {
                    depth += 1
                } else if ch == ')' {
                    depth -= 1;
                    if depth == 0 && i < t.len() - 1 {
                        wraps = false;
                        break;
                    }
                }
            }
            if wraps && depth == 0 {
                s = &t[1..t.len() - 1];
                continue;
            }
        }
        return t;
    }
}
fn branch_complexity(k: &str) -> bool {
    [
        "if_statement",
        "if_expression",
        "elif_clause",
        "switch_case",
        "case_clause",
        "match_arm",
        "match_conditional_expression",
        "catch_clause",
        "except_clause",
        "for_statement",
        "for_in_statement",
        "foreach_statement",
        "for_expression",
        "while_statement",
        "while_expression",
        "do_statement",
        "do_while_statement",
        "loop_expression",
        "conditional_expression",
        "ternary_expression",
        "boolean_operator",
    ]
    .contains(&k)
}
fn is_logical_binary(node: Node<'_>, source: &str) -> bool {
    if !["binary_expression", "binary_operator"].contains(&node.kind()) {
        return false;
    }
    let operator = child(node, "operator")
        .map(|n| snippet(n, source).trim())
        .or_else(|| {
            let left = child(node, "left")?;
            let right = child(node, "right")?;
            source
                .get(left.end_byte()..right.start_byte())
                .map(str::trim)
        });
    matches!(operator, Some("&&" | "||" | "??" | "and" | "or"))
}
fn loop_header(node: Node<'_>, src: &str) -> String {
    if [
        "list_comprehension",
        "dictionary_comprehension",
        "set_comprehension",
        "generator_expression",
    ]
    .contains(&node.kind())
    {
        if let Some(for_clause) = find_kind(node, "for_in_clause") {
            return snippet(for_clause, src).trim().into();
        }
    }
    let raw = snippet(node, src);
    raw.lines()
        .next()
        .unwrap_or(raw)
        .trim()
        .trim_end_matches(['{', ':'])
        .trim()
        .into()
}
fn signature(node: Node<'_>, body: Option<Node<'_>>, src: &str) -> String {
    let raw = if let Some(b) = body {
        src.get(node.start_byte()..b.start_byte()).unwrap_or("")
    } else {
        snippet(node, src)
    };
    raw.trim().chars().take(300).collect()
}
/// Name of a constructor parameter that also declares a property: PHP
/// constructor promotion or a TypeScript parameter property (accessibility,
/// `override` or `readonly` modifier, possibly after decorators).
fn promoted_parameter_name(p: Node<'_>, source: &str) -> Option<String> {
    let promoted = match p.kind() {
        "property_promotion_parameter" => true,
        "required_parameter" | "optional_parameter" => {
            let mut cursor = p.walk();
            let found = p.children(&mut cursor).any(|c| {
                matches!(
                    c.kind(),
                    "accessibility_modifier" | "override_modifier" | "readonly"
                )
            });
            found
        }
        "simple_parameter" => php_readonly_promotion_type(p, source).is_some(),
        _ => false,
    };
    if !promoted {
        return None;
    }
    let name = child(p, "name")
        .or_else(|| child(p, "pattern").filter(|n| n.kind() == "identifier"))
        .or_else(|| first_kind(p, &["identifier", "variable_name"]))?;
    let name = snippet(name, source).trim_start_matches(['&', '$']);
    (!name.is_empty()).then(|| name.to_string())
}
/// Declared type of a PHP `readonly` promoted parameter without a visibility
/// modifier (`readonly Foo $x`). tree-sitter-php has no node for this form and
/// recovers it as a `simple_parameter` whose type text starts with `readonly`
/// (`readonly` as a type name followed by an ERROR node), or with an ERROR
/// `readonly` node inside the parameter or right before it.
fn php_readonly_promotion_type(p: Node<'_>, source: &str) -> Option<String> {
    if p.kind() != "simple_parameter" {
        return None;
    }
    let start = first_kind(p, &["attribute_list"]).map_or(p.start_byte(), |a| a.end_byte());
    let head = source.get(start..child(p, "name")?.start_byte())?.trim();
    if let Some(declared) = head
        .get(..8)
        .filter(|kw| kw.eq_ignore_ascii_case("readonly"))
        .map(|_| &head[8..])
        .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
    {
        return Some(declared.trim().trim_end_matches('&').trim().to_string());
    }
    let is_readonly =
        |n: Node<'_>| n.is_error() && snippet(n, source).eq_ignore_ascii_case("readonly");
    let mut cursor = p.walk();
    let error_inside = p.named_children(&mut cursor).any(is_readonly);
    (error_inside || p.prev_named_sibling().is_some_and(is_readonly))
        .then(|| text_field(p, "type", source).unwrap_or_default())
}
fn visibility(raw: &str) -> Option<String> {
    for v in ["private", "protected", "public"] {
        if raw.trim_start().starts_with(v) {
            return Some(v.into());
        }
    }
    None
}
fn decorators(node: Node<'_>, source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut children_cursor = node.walk();
    for c in node.named_children(&mut children_cursor) {
        if ["decorator", "attribute_list", "attribute_group"].contains(&c.kind()) {
            out.push(snippet(c, source).trim().into())
        }
    }
    if let Some(p) = node.parent() {
        if p.kind() == "decorated_definition" {
            let mut children_cursor = p.walk();
            for c in p.named_children(&mut children_cursor) {
                if c.kind() == "decorator" {
                    out.push(snippet(c, source).trim().into())
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}
fn modifiers(node: Node<'_>, src: &str) -> Vec<String> {
    let text = snippet(node, src);
    let head = text.split(['(', '{']).next().unwrap_or(text);
    let words = head.split_whitespace().collect::<Vec<_>>();
    let mut out = [
        "async",
        "static",
        "pub",
        "private",
        "protected",
        "abstract",
        "generator",
    ]
    .into_iter()
    .filter(|m| words.contains(m))
    .map(str::to_owned)
    .collect::<Vec<_>>();
    if accessor_keyword(node).is_some() {
        out.push("accessor".into());
    }
    out
}
fn accessor_keyword(node: Node<'_>) -> Option<&'static str> {
    if !matches!(
        node.kind(),
        "method_definition" | "method_signature" | "abstract_method_signature"
    ) {
        return None;
    }
    let mut cursor = node.walk();
    let keyword = node
        .children(&mut cursor)
        .find_map(|child| match child.kind() {
            "get" => Some("get"),
            "set" => Some("set"),
            _ => None,
        });
    keyword
}

fn rust_attributes<'a>(node: Node<'_>, source: &'a str) -> Vec<&'a str> {
    let mut attributes = Vec::new();
    let mut previous = node.prev_named_sibling();
    while let Some(sibling) = previous {
        previous = sibling.prev_named_sibling();
        if matches!(sibling.kind(), "line_comment" | "block_comment") {
            continue;
        }
        if sibling.kind() != "attribute_item" {
            break;
        }
        if let Some(attribute) = first_kind(sibling, &["attribute"]) {
            attributes.push(snippet(attribute, source).trim());
        }
    }
    attributes.reverse();
    attributes
}
fn rust_conditions(node: Node<'_>, source: &str) -> Vec<String> {
    let mut conditions = Vec::new();
    let mut current = Some(node);
    while let Some(owner) = current {
        let mut attributes = rust_attributes(owner, source);
        let containers = std::iter::once(owner).chain(child(owner, "body"));
        for container in containers {
            for item in named_children(container)
                .into_iter()
                .filter(|child| child.kind() == "inner_attribute_item")
            {
                if let Some(attribute) = first_kind(item, &["attribute"]) {
                    attributes.push(snippet(attribute, source).trim());
                }
            }
        }
        for attribute in attributes {
            // `#[test]` and harness attributes such as `#[tokio::test]` compile
            // their item only in a `cfg(test)` build.
            let path = attribute.split('(').next().unwrap_or(attribute).trim();
            if path == "test" || path.ends_with("::test") {
                conditions.push("test".to_owned());
            } else if let Some(condition) = attribute
                .strip_prefix("cfg")
                .map(str::trim)
                .and_then(|tail| tail.strip_prefix('('))
                .and_then(|tail| tail.strip_suffix(')'))
            {
                conditions.push(condition.to_owned());
            } else if let Some(arguments) = attribute
                .strip_prefix("cfg_attr")
                .map(str::trim)
                .and_then(|tail| tail.strip_prefix('('))
                .and_then(|tail| tail.strip_suffix(')'))
            {
                let pieces = analyzer_core::resolution::split_arguments(arguments);
                if let Some(condition) = pieces.first() {
                    for attr in pieces.iter().skip(1) {
                        if let Some(inner) = attr
                            .strip_prefix("cfg")
                            .map(str::trim)
                            .and_then(|tail| tail.strip_prefix('('))
                            .and_then(|tail| tail.strip_suffix(')'))
                        {
                            conditions.push(format!("any(not({condition}), {inner})"));
                        }
                    }
                }
            }
        }
        current = owner.parent();
    }
    conditions
}
fn rust_module_path(node: Node<'_>, source: &str) -> Option<String> {
    rust_attributes(node, source)
        .into_iter()
        .find_map(|attribute| {
            let (key, value) = attribute.split_once('=')?;
            (key.trim() == "path").then(|| rust_path_literal(value.trim()))?
        })
}
fn rust_path_literal(raw: &str) -> Option<String> {
    if raw.starts_with('r') {
        let start = raw.find('"')?;
        let end = raw.rfind('"')?;
        return Some(raw[start + 1..end].to_string());
    }
    let quoted = raw.strip_prefix('"')?.strip_suffix('"')?;
    let mut decoded = String::new();
    let mut chars = quoted.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            decoded.push(ch);
        } else {
            decoded.push(match chars.next()? {
                '\\' => '\\',
                '"' => '"',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                '0' => '\0',
                _ => return None,
            });
        }
    }
    Some(decoded)
}

fn rust_is_public(node: Node<'_>, source: &str) -> bool {
    named_children(node).into_iter().any(|child| {
        child.kind() == "visibility_modifier" && snippet(child, source).trim() == "pub"
    })
}

fn rust_path_in_module(reference: &str, module: &str, base: &str) -> String {
    if let Some(tail) = reference.strip_prefix("crate::") {
        return format!("{base}::{tail}");
    }
    if let Some(tail) = reference.strip_prefix("self::") {
        return format!("{module}::{tail}");
    }
    let mut context = module;
    let mut tail = reference;
    while let Some(rest) = tail.strip_prefix("super::") {
        context = context
            .rsplit_once("::")
            .map_or(context, |(parent, _)| parent);
        tail = rest;
    }
    if reference.starts_with("::") {
        reference.trim_start_matches("::").into()
    } else {
        format!("{context}::{tail}")
    }
}

/// Codefactor's structural cognitive score: decisions add 1 + control nesting;
/// else/elif and each boolean operator sequence add 1. Calls have no effect.
/// This intentionally does not claim compatibility with Sonar's metric.
fn cognitive_complexity(body: Node<'_>, source: &str, dialect: Dialect) -> usize {
    let mut total = 0;
    let mut pending = vec![(body, 0_usize)];
    while let Some((node, nesting)) = pending.pop() {
        if node.id() != body.id() && is_function(node.kind(), dialect) {
            continue;
        }
        let kind = node.kind();
        let is_if = matches!(kind, "if_statement" | "if_expression");
        let else_if = is_if
            && node.parent().is_some_and(|parent| {
                parent.kind() == "else_clause"
                    || (matches!(parent.kind(), "if_statement" | "if_expression")
                        && child(parent, "alternative").is_some_and(|alt| alt.id() == node.id()))
            });
        let decision = is_if
            || matches!(
                kind,
                "switch_statement"
                    | "match_expression"
                    | "match_statement"
                    | "conditional_expression"
                    | "ternary_expression"
                    | "catch_clause"
                    | "except_clause"
                    | "if_clause"
            )
            || (is_loop(kind)
                && !matches!(
                    kind,
                    "list_comprehension"
                        | "dictionary_comprehension"
                        | "set_comprehension"
                        | "generator_expression"
                ));
        let mut child_nesting = nesting;
        if else_if || kind == "elif_clause" {
            total += 1;
        } else if decision {
            total += 1 + nesting;
            child_nesting += 1;
        } else if kind == "else_clause"
            && !named_children(node)
                .iter()
                .any(|child| matches!(child.kind(), "if_statement" | "if_expression"))
        {
            total += 1;
        }
        if is_if {
            if let Some(alternative) = child(node, "alternative") {
                if !matches!(
                    alternative.kind(),
                    "else_clause" | "if_statement" | "if_expression"
                ) {
                    total += 1;
                }
            }
        }
        if let Some(operator) = boolean_operator(node, source) {
            let mut parent = node.parent();
            while parent.is_some_and(|node| node.kind() == "parenthesized_expression") {
                parent = parent.and_then(|node| node.parent());
            }
            if parent.and_then(|node| boolean_operator(node, source)) != Some(operator) {
                total += 1;
            }
        }
        pending.extend(
            named_children(node)
                .into_iter()
                .rev()
                .map(|child| (child, child_nesting)),
        );
    }
    total
}

fn boolean_operator<'a>(node: Node<'_>, source: &'a str) -> Option<&'a str> {
    let operator = snippet(child(node, "operator")?, source);
    matches!(operator, "&&" | "||" | "and" | "or" | "xor").then_some(operator)
}
fn heritage_types(node: Node<'_>, source: &str, dialect: Dialect) -> Vec<String> {
    named_children(node)
        .into_iter()
        .filter(|n| n.kind() != "type_arguments")
        .filter_map(|n| crate::types::reference(language_name(dialect), snippet(n, source)))
        .collect()
}
fn trim_type(raw: &str) -> String {
    raw.trim()
        .trim_start_matches(':')
        .trim_start_matches(['?', '\\'])
        .trim()
        .trim_start_matches('&')
        .trim_start_matches('?')
        .trim()
        .trim_end_matches('?')
        .to_string()
}
fn rust_trait_bounds(node: Node<'_>, source: &str) -> BTreeMap<String, String> {
    let mut bounds = BTreeMap::new();
    for field in ["type_parameters", "where_clause"] {
        let Some(part) = child(node, field).or_else(|| first_kind(node, &[field])) else {
            continue;
        };
        let raw = snippet(part, source)
            .trim()
            .trim_start_matches('<')
            .trim_end_matches('>')
            .trim_start_matches("where")
            .trim();
        for entry in raw.split(',') {
            let Some((name, bound)) = entry.split_once(':') else {
                continue;
            };
            let name = name.trim();
            let bound = bound.split('+').next().unwrap_or(bound).trim();
            if !name.is_empty() && !bound.is_empty() && !name.starts_with('\'') {
                bounds.insert(name.to_string(), bound.to_string());
            }
        }
    }
    bounds
}
fn parameter_type(raw: &str, d: Dialect) -> Option<String> {
    match d {
        Dialect::Rust => raw.split_once(':').map(|(_, r)| trim_type(r)),
        Dialect::TypeScript | Dialect::JavaScript | Dialect::Python => raw
            .split_once(':')
            .map(|(_, r)| trim_type(r.split('=').next().unwrap_or(r))),
        Dialect::Php => raw.split_once('$').and_then(|(prefix, _)| {
            let prefix = prefix.trim().trim_end_matches("...").trim();
            let prefix = prefix.trim_end_matches('&').trim();
            let annotation = prefix
                .split_whitespace()
                .filter(|word| !matches!(*word, "public" | "protected" | "private" | "readonly"))
                .collect::<Vec<_>>()
                .join(" ");
            (!annotation.is_empty()).then_some(annotation)
        }),
    }
}
fn apply_phpdoc(node: Node<'_>, source: &str, function: &mut FunctionModel) {
    let Some(previous) = node.prev_named_sibling() else {
        return;
    };
    if previous.kind() != "comment" || previous.end_position().row + 2 < node.start_position().row {
        return;
    }
    let parts = snippet(previous, source)
        .split_whitespace()
        .collect::<Vec<_>>();
    for (index, token) in parts.iter().enumerate() {
        match *token {
            "@return" => {
                if function.return_type.is_none() {
                    if let Some(ty) = parts.get(index + 1) {
                        function.return_type = Some(ty.trim_end_matches("*/").into())
                    }
                }
            }
            "@param" => {
                if let (Some(ty), Some(name)) = (parts.get(index + 1), parts.get(index + 2)) {
                    let name = name.trim_start_matches('$');
                    if let Some(p) = function.parameters.iter_mut().find(|p| {
                        p.name == name
                            && (p.type_name.is_none() || p.type_name.as_deref() == Some("$"))
                    }) {
                        p.type_name = Some((*ty).into())
                    }
                }
            }
            _ => {}
        }
    }
}
fn phpdoc_var(node: Node<'_>, source: &str) -> Option<String> {
    let prev = node.prev_named_sibling()?;
    if prev.kind() != "comment" || prev.end_position().row + 2 < node.start_position().row {
        return None;
    }
    let parts = snippet(prev, source).split_whitespace().collect::<Vec<_>>();
    parts
        .windows(2)
        .find(|w| w[0] == "@var")
        .map(|w| w[1].trim_end_matches("*/").to_string())
}
fn parse_import(raw: &str, d: Dialect, full: &str) -> (String, Option<String>) {
    match d {
        Dialect::Python => {
            let val = raw.trim_start_matches("import ").trim();
            let (p, a) = val
                .split_once(" as ")
                .map(|(p, a)| (p.trim(), Some(a.trim().to_string())))
                .unwrap_or((val, None));
            let prefix = if full.trim_start().starts_with("from ") {
                full.trim_start()
                    .strip_prefix("from ")
                    .and_then(|x| x.split_once(" import "))
                    .map(|(p, _)| p)
                    .unwrap_or("")
            } else {
                ""
            };
            (
                if prefix.is_empty() {
                    p.into()
                } else if prefix.chars().all(|c| c == '.') {
                    format!("{prefix}{p}")
                } else {
                    format!("{prefix}.{p}")
                },
                a,
            )
        }
        Dialect::JavaScript | Dialect::TypeScript => {
            (raw.trim_matches(['\'', '"', '`']).into(), None)
        }
        Dialect::Php => {
            let (p, a) = raw
                .split_once(" as ")
                .map(|(p, a)| (p.trim(), Some(a.trim().to_string())))
                .unwrap_or((raw, None));
            (
                p.trim_start_matches("use ")
                    .trim_start_matches("function ")
                    .trim_start_matches("const ")
                    .trim_start_matches('\\')
                    .into(),
                a,
            )
        }
        Dialect::Rust => (
            raw.trim_start_matches("use ").trim_end_matches(';').into(),
            None,
        ),
    }
}
fn split_call(raw: &str, node_kind: &str, d: Dialect) -> (Option<String>, String, CallKind) {
    if node_kind == "macro_invocation" {
        return (None, raw.trim_end_matches('!').into(), CallKind::Macro);
    }
    if ["new_expression", "object_creation_expression"].contains(&node_kind) {
        return (
            None,
            raw.trim_start_matches("new ").trim().into(),
            CallKind::Constructor,
        );
    }
    let raw = raw.trim();
    for (sep, kind) in [
        ("?->", CallKind::Method),
        ("->", CallKind::Method),
        ("::", CallKind::Static),
        ("?.", CallKind::Method),
        (".", CallKind::Method),
    ] {
        if let Some((left, right)) = raw.rsplit_once(sep) {
            return (
                Some(left.trim().trim_start_matches('$').into()),
                right.trim().into(),
                kind,
            );
        }
    }
    if d == Dialect::Php && raw.starts_with('$') {
        return (None, raw.trim_start_matches('$').into(), CallKind::Dynamic);
    }
    (None, raw.into(), CallKind::Function)
}
fn assigned_name(node: Node<'_>, source: &str) -> Option<String> {
    let mut current = node;
    loop {
        let parent = current.parent()?;
        if [
            "await_expression",
            "try_expression",
            "parenthesized_expression",
            "as_expression",
            "satisfies_expression",
            "non_null_expression",
        ]
        .contains(&parent.kind())
        {
            current = parent;
            continue;
        }
        if [
            "assignment",
            "assignment_expression",
            "variable_declarator",
            "let_declaration",
        ]
        .contains(&parent.kind())
        {
            let rhs = child(parent, "right").or_else(|| child(parent, "value"))?;
            if rhs.id() != current.id() {
                return None;
            }
            let left = child(parent, "left")
                .or_else(|| child(parent, "name"))
                .or_else(|| child(parent, "pattern"))?;
            let name = snippet(left, source)
                .trim()
                .trim_start_matches('$')
                .trim_start_matches("mut ")
                .to_string();
            return if name.is_empty() { None } else { Some(name) };
        }
        return None;
    }
}
fn collect_accessed_fields(
    node: Node<'_>,
    source: &str,
    dialect: Dialect,
    receivers: &[&str],
    fields: &mut BTreeSet<String>,
) {
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        let kind = node.kind();
        let (object, field) = match (dialect, kind) {
            (Dialect::Python, "attribute") => (child(node, "object"), child(node, "attribute")),
            (Dialect::JavaScript | Dialect::TypeScript, "member_expression") => {
                (child(node, "object"), child(node, "property"))
            }
            (Dialect::Php, "member_access_expression" | "nullsafe_member_access_expression") => {
                (child(node, "object"), child(node, "name"))
            }
            (Dialect::Rust, "field_expression") => (child(node, "value"), child(node, "field")),
            _ => (None, None),
        };
        if let (Some(object), Some(field)) = (object, field) {
            let receiver = snippet(object, source).trim();
            if receivers.contains(&receiver.trim_start_matches('$')) {
                let name = snippet(field, source).trim();
                if !name.is_empty() {
                    fields.insert(name.into());
                }
            }
        }
        pending.extend(
            named_children(node)
                .into_iter()
                .filter(|c| !is_function(c.kind(), dialect)),
        );
    }
}

/// Reserved receivers and actual implicit parameters are language-specific.
fn receiver_names(f: &FunctionModel, dialect: Dialect) -> Vec<&str> {
    if f.parent_type.is_none() || f.parent_function.is_some() {
        return Vec::new();
    }
    let mut names = f
        .parameters
        .iter()
        .filter(|p| {
            p.implicit_receiver
                && !(dialect == Dialect::TypeScript && p.name == "this" && p.type_name.is_some())
        })
        .map(|p| p.name.as_str())
        .collect::<Vec<_>>();
    match dialect {
        Dialect::Php => {
            if !f.modifiers.iter().any(|m| m == "static") {
                names.push("this");
            }
        }
        Dialect::JavaScript | Dialect::TypeScript => {
            if !f
                .parameters
                .iter()
                .any(|p| p.name == "this" && p.type_name.is_some())
            {
                names.push("this");
            }
        }
        Dialect::Rust => names.push("Self"),
        Dialect::Python => {}
    }
    names
}

/// Recognize direct field reads/writes; a method's name does not prove its behavior.
fn classify_accessor(
    body: Node<'_>,
    f: &FunctionModel,
    source: &str,
    dialect: Dialect,
) -> Option<AccessorKind> {
    if f.modifiers.iter().any(|modifier| modifier == "accessor") {
        if f.fqn.ends_with("#get") {
            return Some(AccessorKind::Getter);
        }
        if f.fqn.ends_with("#set") {
            return Some(AccessorKind::Setter);
        }
    }
    if f.parent_type.is_none()
        || f.parent_function.is_some()
        || !f.calls.is_empty()
        || f.complexity > 1
    {
        return None;
    }
    if matches!(dialect, Dialect::Python | Dialect::Rust)
        && !f.parameters.iter().any(|p| p.implicit_receiver)
    {
        return None;
    }
    let params = f
        .parameters
        .iter()
        .filter(|p| !p.implicit_receiver)
        .collect::<Vec<_>>();
    let receiver = f
        .parameters
        .iter()
        .find(|p| p.implicit_receiver)
        .map(|p| p.name.as_str())
        .unwrap_or(match dialect {
            Dialect::Php => "$this",
            Dialect::JavaScript | Dialect::TypeScript => "this",
            _ => "self",
        });
    let mut statements =
        if ["block", "statement_block", "compound_statement"].contains(&body.kind()) {
            named_children(body)
        } else {
            vec![body]
        };
    statements.retain(|n| !n.kind().contains("comment"));
    // A Python docstring does not add behavior.
    if dialect == Dialect::Python
        && statements.first().is_some_and(|n| {
            n.kind() == "expression_statement"
                && named_children(*n)
                    .first()
                    .is_some_and(|c| c.kind() == "string")
        })
    {
        statements.remove(0);
    }
    let first = *statements.first()?;
    let value = if first.kind() == "return_statement" {
        named_children(first).last().copied()
    } else if dialect == Dialect::Rust && first.kind() != "expression_statement" {
        // Only Rust returns a block's tail expression implicitly. A discarded field read
        // (including a Rust expression with a semicolon) is not a getter.
        Some(first)
    } else {
        None
    };
    if statements.len() == 1
        && params.is_empty()
        && value.is_some_and(|value| direct_receiver_field(value, receiver, source, dialect))
    {
        return Some(AccessorKind::Getter);
    }
    if params.len() != 1 || statements.len() > 2 {
        return None;
    }
    let assignment = if first.kind() == "expression_statement" {
        *named_children(first).first()?
    } else {
        first
    };
    if !["assignment", "assignment_expression"].contains(&assignment.kind()) {
        return None;
    }
    let left = child(assignment, "left")?;
    let right = child(assignment, "right")?;
    // Reject augmented assignment, computed properties, conversion and validation logic.
    let mut cursor = assignment.walk();
    if !assignment.children(&mut cursor).any(|n| n.kind() == "=")
        || !direct_receiver_field(left, receiver, source, dialect)
    {
        return None;
    }
    if !["identifier", "variable_name"].contains(&right.kind())
        || snippet(right, source).trim().trim_start_matches('$')
            != params[0].name.trim_start_matches('$')
    {
        return None;
    }
    if statements.len() == 2 {
        let ret = statements[1];
        if ret.kind() != "return_statement" {
            return None;
        }
        let returned = *named_children(ret).last()?;
        if snippet(returned, source).trim() != receiver {
            return None;
        }
    }
    Some(AccessorKind::Setter)
}
fn direct_receiver_field(
    mut node: Node<'_>,
    receiver: &str,
    source: &str,
    dialect: Dialect,
) -> bool {
    if node.kind() == "expression_statement" {
        let children = named_children(node);
        if children.len() != 1 {
            return false;
        }
        node = children[0];
    }
    // Rust borrows of fields are accessor-shaped as well.
    if dialect == Dialect::Rust && node.kind() == "reference_expression" {
        let Some(value) = child(node, "value") else {
            return false;
        };
        node = value;
    }
    let (object, field) = match (dialect, node.kind()) {
        (Dialect::Python, "attribute") => (child(node, "object"), child(node, "attribute")),
        (Dialect::JavaScript | Dialect::TypeScript, "member_expression") => {
            (child(node, "object"), child(node, "property"))
        }
        (Dialect::Php, "member_access_expression") => (child(node, "object"), child(node, "name")),
        (Dialect::Rust, "field_expression") => (child(node, "value"), child(node, "field")),
        _ => return false,
    };
    object.is_some_and(|o| snippet(o, source).trim() == receiver)
        && field.is_some_and(|n| {
            [
                "identifier",
                "property_identifier",
                "field_identifier",
                "name",
                "private_property_identifier",
            ]
            .contains(&n.kind())
        })
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
