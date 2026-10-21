use super::*;
use crate::graph::{CallGraph, NodeKind};
use crate::ir::*;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
type TypeDeclaration<'a> = (&'a FileModel, &'a TypeModel);
type ResolutionCache<'a> =
    HashMap<usize, HashMap<(usize, usize), HashMap<String, Option<TypeDeclaration<'a>>>>>;

pub(super) struct Symbols<'a> {
    pub files: BTreeMap<&'a str, &'a FileModel>,
    types: BTreeMap<(String, String), Vec<(&'a FileModel, &'a TypeModel)>>,
    declarations: BTreeMap<(&'a str, &'a str), Vec<(&'a FileModel, &'a TypeModel)>>,
    modules: BTreeMap<(String, String), Vec<&'a FileModel>>,
    resolver: crate::graph::DeclarationResolver<'a>,
    resolutions: RefCell<ResolutionCache<'a>>,
    aliases: HashMap<(usize, &'a str), Vec<(usize, &'a Import)>>,
}
fn normalized(name: &str) -> String {
    let name = name.trim_start_matches('\\');
    if !name.contains("::") {
        return name.replace('\\', ".");
    }
    if !name.contains('\\') {
        return name.replace("::", ".");
    }
    name.replace("::", ".").replace('\\', ".")
}
fn fixed_primitive(language: &str, name: &str) -> bool {
    let name = name.trim().trim_start_matches('?');
    match language {
        "php" => matches!(
            name,
            "int"
                | "float"
                | "string"
                | "bool"
                | "array"
                | "iterable"
                | "callable"
                | "void"
                | "never"
                | "true"
                | "false"
                | "null"
        ),
        "typescript" | "javascript" => matches!(
            name,
            "number"
                | "string"
                | "boolean"
                | "void"
                | "never"
                | "bigint"
                | "symbol"
                | "undefined"
                | "null"
        ),
        _ => false,
    }
}
impl<'a> Symbols<'a> {
    pub fn new(files: &[&'a FileModel]) -> Self {
        let mut out = Self {
            files: BTreeMap::new(),
            types: BTreeMap::new(),
            declarations: BTreeMap::new(),
            modules: BTreeMap::new(),
            resolver: crate::graph::DeclarationResolver::new(files),
            resolutions: RefCell::new(HashMap::new()),
            aliases: HashMap::new(),
        };
        for &f in files {
            if f.language != "rust" {
                let file_key = f as *const FileModel as usize;
                for (index, import) in f.imports.iter().enumerate() {
                    out.aliases
                        .entry((file_key, crate::scoped_imports::alias(import)))
                        .or_default()
                        .push((index, import));
                }
            }
            out.files.insert(&f.path, f);
            out.modules
                .entry((f.language.clone(), normalized(&f.module)))
                .or_default()
                .push(f);
            for t in &f.types {
                if t.kind == TypeKind::Impl {
                    continue;
                }
                out.types
                    .entry((f.language.clone(), normalized(&t.fqn)))
                    .or_default()
                    .push((f, t));
                out.declarations
                    .entry((&f.language, &t.fqn))
                    .or_default()
                    .push((f, t));
            }
        }
        out
    }
    pub fn declaration(&self, language: &str, fqn: &str) -> Option<(&'a FileModel, &'a TypeModel)> {
        self.declarations
            .get(&(language, fqn))
            .and_then(|v| (v.len() == 1).then(|| v[0]))
    }
    pub fn resolve_type(
        &self,
        file: &FileModel,
        name: &str,
        range: &SourceRange,
    ) -> Option<(&'a FileModel, &'a TypeModel)> {
        if fixed_primitive(&file.language, name) {
            return None;
        }
        let identity = crate::types::normalize_reference(name, &file.type_reference_syntax)?;
        // This address is only a transient memo/index key; no pointer is dereferenced
        // and it never enters diagnostics, fingerprints, serialization or ordering.
        let file_key = file as *const FileModel as usize;
        let owner = file
            .types
            .iter()
            .enumerate()
            .filter(|(_, t)| super::in_range(range, &t.range))
            .min_by_key(|(_, t)| t.range.end_line.saturating_sub(t.range.start_line));
        // Alias identity, enclosing declaration and file fully determine this resolver's context.
        // Rust relative paths have additional lexical rules and bypass this cache.
        let mut alias = 0;
        let mut tied_aliases = false;
        let php_absolute =
            file.language == "php" && name.trim().trim_start_matches('?').starts_with('\\');
        if file.language != "rust" && !php_absolute {
            let mut priority = None;
            let prefixes = identity
                .char_indices()
                .filter_map(|(index, ch)| {
                    (ch == '.' || ch == '\\' || ch == ':' && identity[index..].starts_with("::"))
                        .then_some(index)
                })
                .chain(std::iter::once(identity.len()));
            for end in prefixes {
                for (index, import) in self
                    .aliases
                    .get(&(file_key, &identity[..end]))
                    .into_iter()
                    .flatten()
                {
                    let Some(candidate) =
                        crate::graph::type_import_priority(file, import, &identity, range)
                    else {
                        continue;
                    };
                    if priority.is_none_or(|old| candidate > old) {
                        priority = Some(candidate);
                        alias = index + 1;
                        tied_aliases = false;
                    } else if priority == Some(candidate) {
                        tied_aliases = true;
                    }
                }
            }
        }
        // An overlapping import can disappear at a later source range while the
        // selected alias stays the same. Such ties must not reuse a cached answer.
        let cacheable = file.language != "rust" && !tied_aliases;
        let context = (owner.map_or(usize::MAX, |(i, _)| i), alias);
        if cacheable {
            if let Some(result) = self
                .resolutions
                .borrow()
                .get(&file_key)
                .and_then(|c| c.get(&context))
                .and_then(|names| names.get(name))
            {
                return *result;
            }
        }
        let separator = match file.language.as_str() {
            "php" => "\\",
            "rust" => "::",
            _ => ".",
        };
        let fallback = format!("{}{separator}__architecture_scope", file.module);
        let owner = owner.map(|(_, t)| t.fqn.as_str()).unwrap_or(&fallback);
        let target = self.resolver.type_reference(file, name, owner, range);
        let target = normalized(&target);
        let resolved = if let Some(source) = self
            .resolver
            .type_reference_source(file, name, owner, range)
        {
            self.files.get(source).and_then(|source| {
                let candidates = source
                    .types
                    .iter()
                    .filter(|ty| ty.kind != TypeKind::Impl && normalized(&ty.fqn) == target)
                    .collect::<Vec<_>>();
                (candidates.len() == 1).then(|| (*source, candidates[0]))
            })
        } else {
            self.types
                .get(&(file.language.clone(), target))
                .and_then(|v| (v.len() == 1).then(|| v[0]))
        };
        if cacheable {
            self.resolutions
                .borrow_mut()
                .entry(file_key)
                .or_default()
                .entry(context)
                .or_default()
                .insert(name.to_owned(), resolved);
        }
        resolved
    }
    pub fn signature_type(&self, file: &FileModel, name: &str, range: &SourceRange) -> String {
        if fixed_primitive(&file.language, name) {
            return name.to_owned();
        }
        let Some(raw) = crate::types::normalize_reference(name, &file.type_reference_syntax) else {
            return name.to_owned();
        };
        if let Some((f, t)) = self.resolve_type(file, name, range) {
            return super::type_id(f, t);
        }
        let target = self
            .resolver
            .type_reference(file, name, enclosing_entity(file, range), range);
        if target.contains(['.', '\\', ':', '/']) {
            format!("{}:{target}:{name}", file.language)
        } else {
            format!("unknown:{}:{}:{raw}", file.language, file.module)
        }
    }
    pub fn ancestors(&self, file: &FileModel, t: &TypeModel) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut stack = vec![(file, t)];
        while let Some((f, t)) = stack.pop() {
            for name in t.extends.iter().chain(&t.implements) {
                if let Some((f, base)) = self.resolve_type(f, name, &t.range) {
                    if out.insert(base.fqn.clone()) {
                        stack.push((f, base));
                    }
                }
            }
        }
        out
    }
    fn member_owner(
        &self,
        file: &'a FileModel,
        ty: &'a TypeModel,
        member: &str,
    ) -> Option<TypeDeclaration<'a>> {
        if ty.fields.iter().any(|f| f.name == member) {
            return Some((file, ty));
        }
        let mut owners = BTreeMap::new();
        let mut seen = BTreeSet::new();
        let mut queue = VecDeque::from([(file, ty)]);
        while let Some((file, ty)) = queue.pop_front() {
            for base in ty.extends.iter().chain(&ty.implements) {
                if let Some((file, ty)) = self.resolve_type(file, base, &ty.range) {
                    if !seen.insert((&file.path, &ty.fqn)) {
                        continue;
                    }
                    if ty.fields.iter().any(|f| f.name == member) {
                        owners.insert((&file.path, &ty.fqn), (file, ty));
                    } else {
                        queue.push_back((file, ty));
                    }
                }
            }
        }
        match owners.len() {
            0 => Some((file, ty)), // The declared receiver can still have dynamic properties/hooks.
            1 => owners.values().next().copied(),
            _ => None,
        }
    }
    pub fn import_module(&self, file: &FileModel, i: &Import) -> Option<&'a FileModel> {
        if let Some(source) = self.resolver.import_source(file, i) {
            return self.files.get(source).copied();
        }
        let target = normalized(&self.resolver.import_destination(file, i));
        self.module_target(file, &target, true)
    }
    pub fn source_module(&self, file: &FileModel, source: &str) -> Option<&'a FileModel> {
        if let Some(source) = self.resolver.module_source(file, source) {
            return self.files.get(source.as_str()).copied();
        }
        let target = normalized(&self.resolver.module_destination(file, source));
        self.module_target(file, &target, false)
    }
    pub fn imported_callable(
        &self,
        file: &FileModel,
        symbol: &str,
        range: &SourceRange,
    ) -> Result<Option<String>, ()> {
        self.resolver.imported_callable(file, symbol, range)
    }
    fn module_target(
        &self,
        file: &FileModel,
        target: &str,
        allow_members: bool,
    ) -> Option<&'a FileModel> {
        let mut name = target;
        loop {
            if let Some(v) = self.modules.get(&(file.language.clone(), name.to_owned())) {
                return (v.len() == 1).then(|| v[0]);
            }
            if allow_members {
                if let Some(v) = self.types.get(&(file.language.clone(), name.to_owned())) {
                    return (v.len() == 1).then(|| v[0].0);
                }
            } else {
                // A raw module path may include a file extension, but an unknown
                // submodule must not be resolved to an arbitrary parent module.
                let trimmed = [
                    ".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs", ".py", ".php", ".rs",
                ]
                .into_iter()
                .find_map(|extension| name.strip_suffix(extension));
                if let Some(name) = trimmed {
                    if let Some(v) = self.modules.get(&(file.language.clone(), name.to_owned())) {
                        return (v.len() == 1).then(|| v[0]);
                    }
                }
                let index = format!("{name}/index");
                if let Some(v) = self.modules.get(&(file.language.clone(), index)) {
                    return (v.len() == 1).then(|| v[0]);
                }
                return None;
            }
            if let Some((prefix, _)) = name.rsplit_once('.') {
                name = prefix;
            } else {
                return None;
            }
        }
    }
}
pub(super) fn dependencies<'a>(
    files: &[&'a FileModel],
    calls: &'a CallGraph,
    symbols: &Symbols<'a>,
) -> (Vec<Dependency<'a>>, usize) {
    let mut edges = Vec::new();
    let mut unknown = 0;
    for &file in files {
        let module_imports = file
            .architecture_facts
            .as_ref()
            .map(|facts| {
                facts
                    .references
                    .iter()
                    .filter(|r| {
                        matches!(r.kind.as_str(), "module_import" | "unknown_module_import")
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for i in &file.imports {
            if module_imports
                .iter()
                .any(|r| super::in_range(&i.range, &r.range))
            {
                continue;
            }
            if let Some(target) = symbols.import_module(file, i) {
                push(
                    &mut edges,
                    file,
                    target,
                    "import",
                    &i.range,
                    &file.module,
                    &target.module,
                    false,
                );
            } else {
                unknown += 1;
            }
        }
        for t in &file.types {
            for (name, kind) in t
                .extends
                .iter()
                .map(|n| (n, "inheritance"))
                .chain(t.implements.iter().map(|n| (n, "implementation")))
            {
                if let Some((tf, tt)) = symbols.resolve_type(file, name, &t.range) {
                    push(
                        &mut edges,
                        file,
                        tf,
                        kind,
                        &t.range,
                        &t.fqn,
                        &tt.fqn,
                        !is_abstract(tf, tt),
                    );
                } else {
                    unknown += 1;
                }
            }
            for field in &t.fields {
                if let Some(name) = &field.type_name {
                    if let Some((tf, tt)) = symbols.resolve_type(file, name, &field.range) {
                        push(
                            &mut edges,
                            file,
                            tf,
                            "field_type",
                            &field.range,
                            &t.fqn,
                            &tt.fqn,
                            !is_abstract(tf, tt),
                        );
                    }
                }
            }
        }
        for fun in &file.functions {
            for p in &fun.parameters {
                if let Some(name) = &p.type_name {
                    if let Some((tf, tt)) = symbols.resolve_type(file, name, &p.range) {
                        push(
                            &mut edges,
                            file,
                            tf,
                            "parameter_type",
                            &p.range,
                            &fun.fqn,
                            &tt.fqn,
                            !is_abstract(tf, tt),
                        );
                    }
                }
            }
            if let Some(name) = &fun.return_type {
                if let Some((tf, tt)) = symbols.resolve_type(file, name, &fun.range) {
                    push(
                        &mut edges,
                        file,
                        tf,
                        "return_type",
                        &fun.range,
                        &fun.fqn,
                        &tt.fqn,
                        !is_abstract(tf, tt),
                    );
                }
            }
        }
        if let Some(facts) = &file.architecture_facts {
            for access in &facts.value_accesses {
                if access.binding_unknown {
                    unknown += 1;
                    continue;
                }
                let kind = if access.member.is_some() {
                    "member_access"
                } else {
                    "variable_access"
                };
                if let Some(name) = &access.receiver_type {
                    if let Some((tf, tt)) = symbols.resolve_type(file, name, &access.range) {
                        let Some((tf, tt)) =
                            access.member.as_deref().map_or(Some((tf, tt)), |member| {
                                symbols.member_owner(tf, tt, member)
                            })
                        else {
                            unknown += 1;
                            continue;
                        };
                        push(
                            &mut edges,
                            file,
                            tf,
                            kind,
                            &access.range,
                            enclosing_entity(file, &access.range),
                            &tt.fqn,
                            !is_abstract(tf, tt),
                        );
                    }
                    continue;
                }
                // Reading a class object is a type reference, not a variable or field dependency.
                if access.member.is_none()
                    && symbols
                        .resolve_type(file, &access.receiver, &access.range)
                        .is_some()
                {
                    continue;
                }
                if let Some(member) = &access.member {
                    let separator = if file.language == "rust" {
                        "::"
                    } else if file.language == "php" {
                        "\\"
                    } else {
                        "."
                    };
                    if symbols
                        .resolve_type(
                            file,
                            &format!("{}{separator}{member}", access.receiver),
                            &access.range,
                        )
                        .is_some()
                    {
                        continue;
                    }
                    if let Some((tf, tt)) =
                        symbols.resolve_type(file, &access.receiver, &access.range)
                    {
                        let Some((tf, tt)) = symbols.member_owner(tf, tt, member) else {
                            unknown += 1;
                            continue;
                        };
                        push(
                            &mut edges,
                            file,
                            tf,
                            kind,
                            &access.range,
                            enclosing_entity(file, &access.range),
                            &tt.fqn,
                            !is_abstract(tf, tt),
                        );
                        continue;
                    }
                }
                let imports = file
                    .imports
                    .iter()
                    .filter_map(|import| {
                        if import.type_only
                            || crate::scoped_imports::alias(import) != access.receiver
                        {
                            return None;
                        }
                        crate::graph::type_import_priority(
                            file,
                            import,
                            &access.receiver,
                            &access.range,
                        )
                        .map(|priority| (priority, import))
                    })
                    .collect::<Vec<_>>();
                let best = imports.iter().map(|(priority, _)| *priority).max();
                let candidates = imports
                    .iter()
                    .filter(|(priority, _)| Some(*priority) == best)
                    .collect::<Vec<_>>();
                let target = if let [(_, import)] = candidates.as_slice() {
                    symbols.import_module(file, import).or_else(|| {
                        // Rust's call/type index does not contain module constants.
                        // Resolve their declared import's module, never a guessed symbol.
                        (file.language == "rust")
                            .then(|| import.path.rsplit_once("::"))
                            .flatten()
                            .and_then(|(module, _)| symbols.source_module(file, module))
                    })
                } else if candidates.is_empty() && access.member.is_some() {
                    symbols.source_module(file, &access.receiver)
                } else {
                    None
                };
                if let Some(target) = target {
                    push(
                        &mut edges,
                        file,
                        target,
                        kind,
                        &access.range,
                        enclosing_entity(file, &access.range),
                        &target.module,
                        false,
                    );
                } else {
                    unknown += 1;
                }
            }
            for r in &facts.references {
                if r.kind == "unknown_module_import" {
                    unknown += 1;
                    continue;
                }
                if r.kind == "module_import" {
                    if let Some(target) = symbols.source_module(file, &r.name) {
                        push(
                            &mut edges,
                            file,
                            target,
                            "import",
                            &r.range,
                            &file.module,
                            &target.module,
                            false,
                        );
                    } else {
                        unknown += 1;
                    }
                    continue;
                }
                if let Some((tf, tt)) = symbols.resolve_type(file, &r.name, &r.range) {
                    push(
                        &mut edges,
                        file,
                        tf,
                        &r.kind,
                        &r.range,
                        enclosing_entity(file, &r.range),
                        &tt.fqn,
                        !is_abstract(tf, tt),
                    );
                }
            }
        }
        // Export metadata has no source range. Without parser witnesses we cannot
        // invent an edge with a misleading line; expose this gap in coverage instead.
        if module_imports.is_empty() {
            unknown += file.exports.iter().filter(|e| e.source.is_some()).count();
        }
    }
    // Map node ownership once. A popular caller/callee can have thousands of
    // edges; resolving its long file path in a BTreeMap for every edge is redundant.
    // The hash index is lookup-only, so source/evidence ordering stays unchanged.
    let node_files = calls
        .nodes
        .iter()
        .map(|node| {
            (
                node.id.as_str(),
                (
                    node,
                    node.file
                        .as_deref()
                        .and_then(|path| symbols.files.get(path))
                        .copied(),
                ),
            )
        })
        .collect::<HashMap<_, _>>();
    for e in &calls.edges {
        let Some((a, af)) = node_files.get(e.from.as_ref()) else {
            continue;
        };
        let Some((b, bf)) = node_files.get(e.to.as_ref()) else {
            continue;
        };
        let Some(af) = af else {
            continue;
        };
        if e.confidence != Confidence::Resolved {
            unknown += 1;
            continue;
        }
        if b.kind != NodeKind::Function {
            continue;
        }
        let Some(bf) = bf else {
            continue;
        };
        push(
            &mut edges,
            af,
            bf,
            "call",
            &e.callsite,
            &a.label,
            &b.label,
            b.parent_type
                .as_deref()
                .and_then(|t| symbols.declaration(&bf.language, t))
                .is_some_and(|(f, t)| !is_abstract(f, t)),
        );
    }
    edges.sort_by(|a, b| {
        (
            &a.from,
            &a.to,
            &a.kind,
            &a.range,
            &a.source_entity,
            &a.target_entity,
        )
            .cmp(&(
                &b.from,
                &b.to,
                &b.kind,
                &b.range,
                &b.source_entity,
                &b.target_entity,
            ))
    });
    edges.dedup_by(|a, b| {
        a.from == b.from
            && a.to == b.to
            && a.kind == b.kind
            && a.range == b.range
            && a.target_entity == b.target_entity
    });
    (edges, unknown)
}
fn enclosing_entity<'a>(file: &'a FileModel, range: &SourceRange) -> &'a str {
    file.functions
        .iter()
        .filter(|f| super::in_range(range, &f.range))
        .min_by_key(|f| {
            (
                f.range.end_line - f.range.start_line,
                f.range.end_column.saturating_sub(f.range.start_column),
            )
        })
        .map(|f| f.fqn.as_str())
        .or_else(|| {
            file.types
                .iter()
                .filter(|t| super::in_range(range, &t.range))
                .min_by_key(|t| t.range.end_line - t.range.start_line)
                .map(|t| t.fqn.as_str())
        })
        .unwrap_or(&file.module)
}
fn push<'a>(
    out: &mut Vec<Dependency<'a>>,
    a: &'a FileModel,
    b: &'a FileModel,
    kind: &'a str,
    range: &'a SourceRange,
    source: &'a str,
    target: &'a str,
    concrete: bool,
) {
    if a.language != b.language
        && !(matches!(a.language.as_str(), "javascript" | "typescript")
            && matches!(b.language.as_str(), "javascript" | "typescript"))
    {
        return;
    }
    out.push(Dependency {
        from: &a.path,
        to: &b.path,
        kind,
        range,
        source_entity: source,
        target_entity: target,
        concrete,
    });
}
pub(super) fn is_abstract(f: &FileModel, t: &TypeModel) -> bool {
    matches!(t.kind, TypeKind::Interface | TypeKind::Trait)
        || f.architecture_facts
            .as_ref()
            .is_some_and(|a| a.abstract_types.contains(&t.fqn))
}

/// Iterative Kosaraju; deep module graphs cannot overflow the Rust stack.
pub(super) fn components(edges: &BTreeMap<String, BTreeSet<String>>) -> Vec<Vec<String>> {
    let names = edges
        .keys()
        .chain(edges.values().flatten())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut reverse = BTreeMap::<String, BTreeSet<String>>::new();
    for (a, bs) in edges {
        for b in bs {
            reverse.entry(b.clone()).or_default().insert(a.clone());
        }
    }
    let mut seen = BTreeSet::new();
    let mut order = Vec::new();
    for root in &names {
        if seen.contains(root) {
            continue;
        }
        let mut stack = vec![(root.clone(), false)];
        while let Some((at, exit)) = stack.pop() {
            if exit {
                order.push(at);
                continue;
            }
            if !seen.insert(at.clone()) {
                continue;
            }
            stack.push((at.clone(), true));
            if let Some(next) = edges.get(&at) {
                stack.extend(next.iter().rev().map(|n| (n.clone(), false)));
            }
        }
    }
    seen.clear();
    let mut out = Vec::new();
    for root in order.into_iter().rev() {
        if seen.contains(&root) {
            continue;
        }
        let mut group = Vec::new();
        let mut stack = vec![root];
        while let Some(at) = stack.pop() {
            if !seen.insert(at.clone()) {
                continue;
            }
            group.push(at.clone());
            if let Some(next) = reverse.get(&at) {
                stack.extend(next.iter().cloned());
            }
        }
        group.sort();
        out.push(group);
    }
    out.sort();
    out
}
pub(super) fn witness(edges: &BTreeMap<String, BTreeSet<String>>, group: &[String]) -> Vec<String> {
    let allowed = group.iter().collect::<BTreeSet<_>>();
    let start = &group[0];
    let mut queue = VecDeque::new();
    let mut parent = BTreeMap::<String, String>::new();
    for n in edges
        .get(start)
        .into_iter()
        .flatten()
        .filter(|n| allowed.contains(n))
    {
        parent.insert(n.clone(), start.clone());
        queue.push_back(n.clone());
    }
    while let Some(at) = queue.pop_front() {
        for n in edges
            .get(&at)
            .into_iter()
            .flatten()
            .filter(|n| allowed.contains(n))
        {
            if n == start {
                let mut path = vec![at.clone()];
                let mut cur = &at;
                while cur != start {
                    cur = parent.get(cur).unwrap();
                    path.push(cur.clone());
                }
                path.reverse();
                path.push(start.clone());
                return path;
            }
            if !parent.contains_key(n) {
                parent.insert(n.clone(), at.clone());
                queue.push_back(n.clone());
            }
        }
    }
    Vec::new()
}
/// `packages` are the project package names owning `f` (see `pkg:`).
pub(super) fn selector_matches(s: &str, f: &FileModel, entity: &str, packages: &[String]) -> bool {
    let Some((kind, value)) = s.split_once(':') else {
        return false;
    };
    let prefix = |name: &str| {
        name == value
            || name
                .strip_prefix(value)
                .is_some_and(|r| r.starts_with(['.', '\\', ':', '/']))
    };
    match kind {
        "glob" => globset::Glob::new(value)
            .ok()
            .is_some_and(|g| g.compile_matcher().is_match(&f.path)),
        "dir" => {
            let value = value.trim_end_matches('/');
            f.path == value
                || f.path
                    .strip_prefix(value)
                    .is_some_and(|r| r.starts_with('/'))
        }
        "file" => f.path == value,
        "class" => {
            entity == value
                || f.functions
                    .iter()
                    .any(|fun| fun.fqn == entity && fun.parent_type.as_deref() == Some(value))
        }
        "ns" | "mod" => prefix(&f.module) || prefix(entity),
        "pkg" => prefix(&f.module) || prefix(entity) || packages.iter().any(|p| p == value),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deep_graphs_are_stack_safe_and_cycle_witnesses_are_actual_edges() {
        let mut graph = BTreeMap::new();
        for i in 0..10000 {
            graph.insert(
                format!("n{i:05}"),
                BTreeSet::from([format!("n{:05}", (i + 1) % 10000)]),
            );
        }
        let groups = components(&graph);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 10000);
        let path = witness(&graph, &groups[0]);
        assert_eq!(path.first(), path.last());
        assert!(path
            .windows(2)
            .all(|pair| graph[&pair[0]].contains(&pair[1])));
    }
}
