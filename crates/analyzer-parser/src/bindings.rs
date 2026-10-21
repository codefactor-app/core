//! Lexical, source-ordered bindings. Never infer a receiver from a later assignment.
use super::*;
#[derive(Clone, Debug, PartialEq, Eq)]
enum Binding {
    Type(String),
    Object(String, String),
    Types(BTreeSet<String>),
    Import(String),
    ErasedImport,
    Function(String),
    Pending,
    Unknown,
}
fn merge_types(values: impl IntoIterator<Item = Binding>) -> Binding {
    let values = values.into_iter().collect::<Vec<_>>();
    if values.is_empty() {
        return Binding::Unknown;
    }
    if values.iter().all(|value| value == &values[0]) {
        return values[0].clone();
    }
    let mut types = BTreeSet::new();
    for value in values {
        match value {
            Binding::Type(ty) => {
                types.insert(ty);
            }
            Binding::Object(ty, _) => {
                types.insert(ty);
            }
            Binding::Types(known) => types.extend(known),
            _ => return Binding::Unknown,
        }
    }
    match types.len() {
        1 => Binding::Type(types.into_iter().next().unwrap()),
        2..=4 => Binding::Types(types),
        _ => Binding::Unknown,
    }
}

pub(super) struct FunctionScope {
    pub fqn: String,
    pub parent_type: Option<String>,
    pub parent_function: Option<String>,
    pub parameters: Vec<Parameter>,
    pub kind: String,
    pub declaration_scope: SourceRange,
}
// Preserve the replacement order, borrowing ordinary receiver names.
fn receiver_path(raw: &str) -> std::borrow::Cow<'_, str> {
    let mut path = std::borrow::Cow::Borrowed(raw);
    for separator in ["?->", "->", "?.", "::"] {
        if path.contains(separator) {
            path = std::borrow::Cow::Owned(path.replace(separator, "."));
        }
    }
    path
}
fn typed_in_scope(
    raw: &str,
    dialect: Dialect,
    owner: &str,
    imports: &[Import],
    range: &SourceRange,
) -> Binding {
    if dialect == Dialect::Php {
        return php_names::reference(raw, owner, imports, range)
            .map(Binding::Type)
            .unwrap_or(Binding::Unknown);
    }
    typed(raw, dialect)
}
fn typed(raw: &str, dialect: Dialect) -> Binding {
    crate::types::reference(language_name(dialect), raw)
        .map(Binding::Type)
        .unwrap_or(Binding::Unknown)
}
fn contains(scope: &SourceRange, line: usize, col: usize) -> bool {
    (scope.start_line, scope.start_column) <= (line, col)
        && (line, col) <= (scope.end_line, scope.end_column)
}
pub(super) fn applies(import: &Import, range: &SourceRange) -> bool {
    import
        .scope
        .as_ref()
        .is_none_or(|s| contains(s, range.start_line, range.start_column))
}
fn import_name(i: &Import) -> &str {
    i.alias.as_deref().unwrap_or_else(|| {
        i.path
            .rsplit(['.', ':', '/', '\\'])
            .find(|s| !s.is_empty())
            .unwrap_or(&i.path)
    })
}
fn import_binding(import: &Import) -> Binding {
    if import.type_only {
        Binding::ErasedImport
    } else {
        Binding::Import(import.binding_path.as_ref().unwrap_or(&import.path).clone())
    }
}
fn name(node: Node<'_>, source: &str) -> Option<String> {
    ["identifier", "variable_name", "self", "self_parameter"]
        .contains(&node.kind())
        .then(|| {
            snippet(node, source)
                .trim()
                .trim_start_matches('$')
                .to_owned()
        })
}
fn pattern_names(node: Node<'_>, source: &str) -> Vec<String> {
    if let Some(n) = name(node, source) {
        return vec![n];
    }
    if [
        "shorthand_property_identifier_pattern",
        "shorthand_property_identifier",
    ]
    .contains(&node.kind())
    {
        return vec![snippet(node, source).to_owned()];
    }
    if [
        "pair_pattern",
        "assignment_pattern",
        "object_assignment_pattern",
    ]
    .contains(&node.kind())
    {
        return child(node, "value")
            .or_else(|| child(node, "left"))
            .map(|n| pattern_names(n, source))
            .unwrap_or_default();
    }
    if node.kind().contains("pattern")
        || [
            "pattern_list",
            "list_literal",
            "tuple",
            "list",
            "by_ref",
            "pair",
        ]
        .contains(&node.kind())
    {
        return named_children(node)
            .into_iter()
            .flat_map(|n| pattern_names(n, source))
            .collect();
    }
    Vec::new()
}
fn assignment(node: Node<'_>) -> Option<(Node<'_>, Option<Node<'_>>)> {
    if ![
        "assignment",
        "assignment_expression",
        "variable_declarator",
        "let_declaration",
    ]
    .contains(&node.kind())
    {
        return None;
    }
    let left = child(node, "left")
        .or_else(|| child(node, "name"))
        .or_else(|| child(node, "pattern"))?;
    Some((left, child(node, "right").or_else(|| child(node, "value"))))
}
fn exception(node: Node<'_>, source: &str, dialect: Dialect) -> Option<(String, Binding)> {
    if dialect == Dialect::Php && node.kind() == "catch_clause" {
        let n = name(child(node, "name")?, source)?;
        let ty = child(node, "type").and_then(|n| {
            let children = named_children(n);
            (children.len() == 1).then(|| snippet(children[0], source).trim().to_owned())
        });
        return Some((n, ty.map(Binding::Type).unwrap_or(Binding::Unknown)));
    }
    if matches!(dialect, Dialect::JavaScript | Dialect::TypeScript) && node.kind() == "catch_clause"
    {
        return Some((name(child(node, "parameter")?, source)?, Binding::Unknown));
    }
    if dialect == Dialect::Python && node.kind() == "except_clause" {
        let value = child(node, "value")?;
        let alias = child(node, "alias").or_else(|| child(value, "alias"));
        let alias = alias.map(|n| snippet(n, source).to_owned())?;
        let ty = if value.kind() == "as_pattern" {
            named_children(value)
                .into_iter()
                .find(|n| Some(n.id()) != child(value, "alias").map(|n| n.id()))
        } else {
            Some(value)
        };
        let ty = ty
            .filter(|n| ["identifier", "attribute"].contains(&n.kind()))
            .map(|n| snippet(n, source).to_owned());
        return Some((alias, ty.map(Binding::Type).unwrap_or(Binding::Unknown)));
    }
    None
}
struct Walker<'a> {
    dialect: Dialect,
    source: &'a str,
    types: &'a ScopedTypes<'a, 'a>,
    imports: &'a [Import],
    nested: &'a BTreeMap<(String, String), String>,
    owner: &'a str,
    calls: BTreeMap<(usize, usize, usize, usize), Vec<usize>>,
    function: &'a mut FunctionModel,
}
impl Walker<'_> {
    fn visit(&mut self, node: Node<'_>, env: &mut BTreeMap<String, Binding>, root: bool) {
        if self.function.is_module_initializer() {
            let r = range(node, Path::new(""));
            let scope = &self.function.range;
            if (r.end_line, r.end_column) < (scope.start_line, scope.start_column)
                || (r.start_line, r.start_column) > (scope.end_line, scope.end_column)
            {
                return;
            }
        }
        if is_type(node.kind(), self.dialect) {
            if self.function.is_module_initializer() && self.dialect == Dialect::Python {
                if let Some(bases) = child(node, "superclasses") {
                    self.visit(bases, env, false);
                }
                if let Some(body) = child(node, "body") {
                    self.visit(body, &mut env.clone(), false);
                }
            }
            return;
        }
        if is_function(node.kind(), self.dialect) {
            if self.function.is_module_initializer() && self.dialect == Dialect::Python {
                if let Some(parameters) = child(node, "parameters") {
                    self.visit(parameters, env, false);
                }
                if let Some(annotation) = child(node, "return_type") {
                    self.visit(annotation, env, false);
                }
            }
            if let Some(n) = text_field(node, "name", self.source) {
                if let Some(fqn) = self.nested.get(&(self.owner.to_owned(), n.clone())) {
                    env.insert(n, Binding::Function(fqn.clone()));
                }
            }
            return;
        }
        let start = node.start_position();
        let end = node.end_position();
        if let Some(indices) = self
            .calls
            .get(&(start.row + 1, start.column + 1, end.row + 1, end.column + 1))
            .cloned()
        {
            for index in indices {
                let call = &mut self.function.calls[index];
                let lookup = call.receiver.as_deref().unwrap_or(&call.name);
                let parts = receiver_path(lookup);
                let (first, rest) = parts
                    .split_once('.')
                    .map_or((parts.as_ref(), None), |(first, rest)| (first, Some(rest)));
                let first = first.trim_start_matches('$');
                let reserved = self.dialect == Dialect::Php
                    && call.kind == CallKind::Static
                    && matches!(call.receiver.as_deref(), Some("self" | "static" | "parent"));
                let class_binding = reserved
                    .then(|| self.function.parent_type.clone())
                    .flatten()
                    .map(|ty| Binding::Type(format!("\\{ty}")));
                let binding = if reserved {
                    class_binding.as_ref().filter(|_| first != "parent")
                } else {
                    env.get(first)
                };
                if let Some(binding) = binding {
                    call.local_binding = !matches!(binding, Binding::Import(_));
                    call.receiver_type = None;
                    call.receiver_types.clear();
                    call.receiver_identity = None;
                    call.receiver_root_type = None;
                    call.targets.clear();
                    call.binding_unknown = false;
                    call.confidence = Confidence::Dynamic;
                    match binding {
                        Binding::Unknown | Binding::ErasedImport => call.binding_unknown = true,
                        Binding::Pending => {}
                        Binding::Function(target) if call.receiver.is_none() => {
                            call.targets.push(target.clone());
                            call.confidence = Confidence::Resolved;
                        }
                        Binding::Type(ty) | Binding::Import(ty) | Binding::Object(ty, _)
                            if call.receiver.is_some() =>
                        {
                            if let Binding::Object(_, identity) = binding {
                                if rest.is_none() {
                                    call.receiver_identity = Some(identity.clone());
                                }
                            }
                            // Properties are resolved using the actual class's field metadata later.
                            if rest.is_some()
                                && matches!(binding, Binding::Type(_) | Binding::Object(_, _))
                            {
                                call.receiver_root_type = Some(ty.clone());
                            }
                            if rest.is_none() || matches!(binding, Binding::Import(_)) {
                                call.receiver_type = Some(if rest.is_none() {
                                    ty.clone()
                                } else {
                                    format!("{ty}.{}", rest.unwrap_or_default())
                                });
                            }
                        }
                        Binding::Types(types) if call.receiver.is_some() => {
                            call.receiver_types = types.iter().cloned().collect();
                            call.confidence = Confidence::Ambiguous;
                        }
                        Binding::Import(target) => {
                            call.targets.push(target.clone());
                        }
                        _ => call.binding_unknown = true,
                    }
                } else if call.kind == CallKind::Dynamic {
                    call.local_binding = true;
                    call.binding_unknown = true;
                }
            }
        }
        if [
            "import_statement",
            "import_from_statement",
            "namespace_use_declaration",
            "use_declaration",
        ]
        .contains(&node.kind())
        {
            let r = range(node, Path::new(""));
            for i in self
                .imports
                .iter()
                .filter(|i| contains(&r, i.range.start_line, i.range.start_column))
            {
                env.insert(import_name(i).to_owned(), import_binding(i));
            }
            return;
        }
        if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
            && node.kind() == "catch_clause"
        {
            let mut local = env.clone();
            if let Some(parameter) = child(node, "parameter") {
                for n in pattern_names(parameter, self.source) {
                    local.insert(n, Binding::Unknown);
                }
            }
            let mut children_cursor = node.walk();
            for c in node.named_children(&mut children_cursor) {
                self.visit(c, &mut local, false);
            }
            return;
        }
        if let Some((n, ty)) = exception(node, self.source, self.dialect) {
            let ty = if self.dialect == Dialect::Php {
                match ty {
                    Binding::Type(raw) => typed_in_scope(
                        &raw,
                        self.dialect,
                        self.owner,
                        self.imports,
                        &range(node, Path::new("")),
                    ),
                    binding => binding,
                }
            } else {
                ty
            };
            let mut local = env.clone();
            local.insert(n.clone(), ty);
            let mut children_cursor = node.walk();
            for c in node.named_children(&mut children_cursor) {
                self.visit(c, &mut local, false);
            }
            if self.dialect == Dialect::Python {
                env.insert(n, Binding::Unknown);
            }
            return;
        }
        if is_loop(node.kind()) {
            let mut local = env.clone();
            let mut bound = child(node, "left")
                .or_else(|| child(node, "pattern"))
                .map(|n| pattern_names(n, self.source))
                .unwrap_or_default();
            if node.kind() == "foreach_statement" {
                for n in named_children(node)
                    .into_iter()
                    .filter(|n| child(node, "body").is_none_or(|b| b.id() != n.id()))
                    .skip(1)
                {
                    bound.extend(pattern_names(n, self.source));
                }
            }
            let lexical_initializer =
                child(node, "initializer").filter(|n| n.kind() == "lexical_declaration");
            if let Some(initializer) = lexical_initializer {
                let mut children_cursor = initializer.walk();
                for d in initializer.named_children(&mut children_cursor) {
                    if let Some(pattern) = child(d, "name") {
                        bound.extend(pattern_names(pattern, self.source));
                    }
                }
            }
            let scoped = self.dialect == Dialect::Rust
                || (matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
                    && (lexical_initializer.is_some()
                        || child(node, "kind")
                            .is_some_and(|n| ["let", "const"].contains(&snippet(n, self.source)))));
            for n in &bound {
                local.insert(n.clone(), Binding::Unknown);
            }
            let body = child(node, "body");
            for c in named_children(node)
                .into_iter()
                .filter(|c| body.is_none_or(|b| b.id() != c.id()))
            {
                self.visit(c, &mut local, false);
            }
            if let Some(body) = body {
                self.visit(body, &mut local, false);
            }
            for (n, value) in local {
                if env.get(&n) != Some(&value) && !(scoped && bound.contains(&n)) {
                    env.insert(n, Binding::Unknown);
                }
            }
            return;
        }
        if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
            && matches!(node.kind(), "if_statement" | "ternary_expression")
        {
            if let Some(condition) = child(node, "condition") {
                self.visit(condition, env, false);
            }
            let mut consequent = env.clone();
            if let Some(branch) = child(node, "consequence") {
                self.visit(branch, &mut consequent, false);
            }
            let mut alternative = env.clone();
            if let Some(branch) = child(node, "alternative") {
                self.visit(branch, &mut alternative, false);
            }
            for key in env.keys().cloned().collect::<Vec<_>>() {
                let merged = merge_types([
                    consequent.get(&key).cloned().unwrap_or(Binding::Unknown),
                    alternative.get(&key).cloned().unwrap_or(Binding::Unknown),
                ]);
                env.insert(key, merged);
            }
            return;
        }
        if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
            && matches!(
                node.kind(),
                "augmented_assignment_expression" | "binary_expression"
            )
            && child(node, "operator").is_some_and(|operator| {
                matches!(
                    snippet(operator, self.source),
                    "&&=" | "||=" | "??=" | "&&" | "||" | "??"
                )
            })
        {
            if let Some(left) = child(node, "left") {
                self.visit(left, env, false);
            }
            let mut right = env.clone();
            if let Some(value) = child(node, "right") {
                self.visit(value, &mut right, false);
                if node.kind() == "augmented_assignment_expression" {
                    if let Some(left) = child(node, "left").and_then(|left| name(left, self.source))
                    {
                        right.insert(left, self.value(value, &right));
                    }
                }
            }
            for key in env.keys().cloned().collect::<Vec<_>>() {
                env.insert(
                    key.clone(),
                    merge_types([
                        env.get(&key).cloned().unwrap_or(Binding::Unknown),
                        right.get(&key).cloned().unwrap_or(Binding::Unknown),
                    ]),
                );
            }
            return;
        }
        let control = [
            "if_statement",
            "try_statement",
            "match_statement",
            "match_expression",
            "switch_statement",
            "conditional_expression",
            "ternary_expression",
        ]
        .contains(&node.kind())
            || (node.kind() == "augmented_assignment_expression"
                && child(node, "operator").is_some_and(|operator| {
                    matches!(snippet(operator, self.source), "&&=" | "||=" | "??=")
                }));
        if control {
            let mut changed = BTreeSet::new();
            let mut children_cursor = node.walk();
            for c in node.named_children(&mut children_cursor) {
                let mut local = env.clone();
                self.visit(c, &mut local, false);
                for (name, value) in &local {
                    if env.get(name) != Some(value) {
                        changed.insert(name.clone());
                    }
                }
            }
            for n in changed {
                env.insert(n, Binding::Unknown);
            }
            return;
        }
        let block = !root
            && matches!(
                self.dialect,
                Dialect::JavaScript | Dialect::TypeScript | Dialect::Rust
            )
            && ["block", "statement_block"].contains(&node.kind());
        let mut declared = Vec::new();
        if block {
            let mut children_cursor = node.walk();
            for c in node.named_children(&mut children_cursor) {
                if c.kind() == "let_declaration" {
                    if let Some(n) = child(c, "pattern").and_then(|n| name(n, self.source)) {
                        declared.push(n);
                    }
                }
                if c.kind() == "lexical_declaration" {
                    let mut children_cursor = c.walk();
                    for d in c.named_children(&mut children_cursor) {
                        if let Some(n) = child(d, "name").and_then(|n| name(n, self.source)) {
                            declared.push(n);
                        }
                    }
                }
            }
        }
        // Only declarations in this block need restoration. Other assignments escape.
        let before: Vec<_> = declared
            .iter()
            .map(|name| (name.clone(), env.get(name).cloned()))
            .collect();
        for n in &declared {
            env.insert(n.clone(), Binding::Unknown);
        }
        let mut children_cursor = node.walk();
        for c in node.named_children(&mut children_cursor) {
            self.visit(c, env, false);
        }
        if let Some((left, value)) = assignment(node) {
            if let Some(n) = name(left, self.source) {
                let callable = value
                    .filter(|v| is_function(v.kind(), self.dialect))
                    .and_then(|_| self.nested.get(&(self.owner.to_owned(), n.clone())))
                    .map(|fqn| Binding::Function(fqn.clone()));
                let ty = callable
                    .or_else(|| {
                        child(node, "type").and_then(|n| {
                            let binding = typed(snippet(n, self.source), self.dialect);
                            (!(matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
                                && binding == Binding::Unknown
                                && value.is_some()))
                            .then_some(binding)
                        })
                    })
                    .or_else(|| value.map(|v| self.value(v, env)))
                    .unwrap_or(Binding::Unknown);
                env.insert(n, ty);
            }
        }
        if let Some((left, _)) = assignment(node) {
            if name(left, self.source).is_none() {
                for n in pattern_names(left, self.source) {
                    env.insert(n, Binding::Unknown);
                }
            }
        }
        for (n, value) in before {
            if let Some(value) = value {
                env.insert(n, value);
            } else {
                env.remove(&n);
            }
        }
    }
    fn value(&self, node: Node<'_>, env: &BTreeMap<String, Binding>) -> Binding {
        if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
            && node.kind() == "ternary_expression"
        {
            return merge_types(["consequence", "alternative"].map(|field| {
                child(node, field)
                    .map(|n| self.value(n, env))
                    .unwrap_or(Binding::Unknown)
            }));
        }
        if matches!(
            node.kind(),
            "parenthesized_expression"
                | "as_expression"
                | "satisfies_expression"
                | "non_null_expression"
        ) {
            return named_children(node)
                .first()
                .map(|node| self.value(*node, env))
                .unwrap_or(Binding::Unknown);
        }
        if let Some(n) = name(node, self.source) {
            return env.get(&n).cloned().unwrap_or(Binding::Unknown);
        }
        if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
            && node.kind() == "binary_expression"
            && child(node, "operator")
                .is_some_and(|operator| matches!(snippet(operator, self.source), "||" | "??"))
        {
            // Lazy singleton idiom `cached ?? new Client()`: the fallback states
            // the type when the cached operand carries no type evidence.
            let operand = |field| {
                child(node, field)
                    .map(|n| self.value(n, env))
                    .unwrap_or(Binding::Unknown)
            };
            return match (operand("left"), operand("right")) {
                (
                    Binding::Pending | Binding::Unknown,
                    Binding::Type(ty) | Binding::Object(ty, _),
                ) => Binding::Type(ty),
                (cached, fallback) => merge_types([cached, fallback]),
            };
        }
        if ["new_expression", "object_creation_expression"].contains(&node.kind()) {
            let raw = snippet(node, self.source)
                .trim_start_matches("new ")
                .split('(')
                .next()
                .unwrap_or("")
                .trim();
            return if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript) {
                let constructor = child(node, "constructor")
                    .map(|node| snippet(node, self.source))
                    .unwrap_or(raw);
                if constructor.split('.').next().and_then(|name| env.get(name))
                    == Some(&Binding::ErasedImport)
                {
                    return Binding::Unknown;
                }
                Binding::Object(
                    raw.to_owned(),
                    format!(
                        "{}:{}",
                        node.start_position().row + 1,
                        node.start_position().column + 1
                    ),
                )
            } else if self.dialect == Dialect::Php {
                php_names::reference(raw, self.owner, self.imports, &range(node, Path::new("")))
                    .map(Binding::Type)
                    .unwrap_or(Binding::Unknown)
            } else {
                Binding::Type(raw.to_owned())
            };
        }
        if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
            && properties::member(node, self.source).is_some()
        {
            // A typed native getter can establish this binding in graph source
            // order; ordinary/untyped property values provide no type evidence.
            return Binding::Pending;
        }
        if is_call(node.kind(), self.dialect) {
            let target = child(node, "function")
                .map(|n| snippet(n, self.source))
                .unwrap_or("");
            if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
                && target == "require"
                && !env.contains_key("require")
            {
                if let Some(arguments) = child(node, "arguments") {
                    let arguments = named_children(arguments);
                    if arguments.len() == 1 {
                        if let Some(module) = properties::literal_key(arguments[0], self.source) {
                            let path = [".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs"]
                                .iter()
                                .find_map(|extension| module.strip_suffix(extension))
                                .unwrap_or(&module);
                            return Binding::Import(path.to_owned());
                        }
                    }
                }
            }
            if self.dialect == Dialect::Python {
                if let Some(ty) = self.types.get(target) {
                    return Binding::Type(ty.clone());
                }
            }
            if self.dialect == Dialect::Rust {
                if let Some((ty, method)) = target.rsplit_once("::") {
                    if ["new", "default"].contains(&method) {
                        return Binding::Type(ty.to_owned());
                    }
                }
            }
            return Binding::Pending;
        }
        Binding::Unknown
    }
}
fn declared_names(
    body: Node<'_>,
    source: &str,
    dialect: Dialect,
    imports: &[Import],
) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut pending = vec![body];
    while let Some(node) = pending.pop() {
        if is_function(node.kind(), dialect) || is_type(node.kind(), dialect) {
            continue;
        }
        if dialect == Dialect::Python {
            if let Some((left, _)) = assignment(node) {
                if let Some(n) = name(left, source) {
                    names.insert(n);
                } else {
                    names.extend(pattern_names(left, source));
                }
            }
            if let Some(left) = child(node, "left").filter(|_| node.kind() == "for_statement") {
                names.extend(pattern_names(left, source));
            }
            if let Some((n, _)) = exception(node, source, dialect) {
                names.insert(n);
            }
            if ["import_statement", "import_from_statement"].contains(&node.kind()) {
                let r = range(node, Path::new(""));
                for i in imports
                    .iter()
                    .filter(|i| contains(&r, i.range.start_line, i.range.start_column))
                {
                    names.insert(import_name(i).to_owned());
                }
                continue;
            }
        } else if matches!(dialect, Dialect::JavaScript | Dialect::TypeScript)
            && node.kind() == "variable_declaration"
        {
            let mut children_cursor = node.walk();
            for d in node.named_children(&mut children_cursor) {
                if let Some(n) = child(d, "name").and_then(|n| name(n, source)) {
                    names.insert(n);
                }
            }
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    names
}
/// JS/TS module-scope `const` declarations with one declared or constructed
/// type. A `const` is never reassigned, so every function of the module and
/// every importer may use that type.
pub(super) fn module_constants(
    root: Node<'_>,
    types: &ScopedTypes<'_, '_>,
    imports: &[Import],
    dialect: Dialect,
    source: &str,
    path: &Path,
) -> Vec<Field> {
    let module = range(root, path);
    let mut env = imports
        .iter()
        .filter(|i| applies(i, &module))
        .map(|i| (import_name(i).to_owned(), import_binding(i)))
        .collect::<BTreeMap<_, _>>();
    let mut function = FunctionModel::default();
    let walker = Walker {
        dialect,
        source,
        types,
        imports,
        nested: &BTreeMap::new(),
        owner: "",
        calls: BTreeMap::new(),
        function: &mut function,
    };
    let mut constants = Vec::new();
    for statement in named_children(root) {
        let declaration = if statement.kind() == "export_statement" {
            child(statement, "declaration")
        } else {
            Some(statement)
        };
        let Some(declaration) = declaration.filter(|d| {
            d.kind() == "lexical_declaration"
                && child(*d, "kind").is_some_and(|kind| snippet(kind, source) == "const")
        }) else {
            continue;
        };
        for declarator in named_children(declaration) {
            let Some(n) = child(declarator, "name").and_then(|n| name(n, source)) else {
                continue;
            };
            let declared = child(declarator, "type")
                .map(|ty| typed(snippet(ty, source), dialect))
                .filter(|binding| *binding != Binding::Unknown);
            let binding = declared
                .or_else(|| child(declarator, "value").map(|v| walker.value(v, &env)))
                .unwrap_or(Binding::Unknown);
            let ty = match &binding {
                Binding::Type(ty) | Binding::Object(ty, _) => Some(ty.clone()),
                _ => None,
            };
            env.insert(n.clone(), ty.clone().map_or(binding, Binding::Type));
            if let Some(ty) = ty {
                constants.push(Field {
                    name: n,
                    type_name: Some(ty),
                    range: range(declarator, path),
                    ..Field::default()
                });
            }
        }
    }
    constants
}
pub(super) fn annotate(
    node: Node<'_>,
    f: &mut FunctionModel,
    constants: &[Field],
    types: &ScopedTypes<'_, '_>,
    imports: &[Import],
    nested: &BTreeMap<(String, String), String>,
    scopes: &BTreeMap<(usize, usize), FunctionScope>,
    dialect: Dialect,
    source: &str,
) {
    if f.calls.is_empty() && f.property_accesses.is_empty() && f.property_mutations.is_empty() {
        return;
    }
    let Some(body) = child(node, "body").or_else(|| f.is_module_initializer().then_some(node))
    else {
        return;
    };
    // Property reads/writes share the explicit call binder and source ordering.
    // Compound operations can have multiple operations at exactly the same range.
    let explicit_calls = f.calls.len();
    f.calls
        .extend(f.property_accesses.iter().map(|access| access.site.clone()));
    f.calls.extend(
        f.property_mutations
            .iter()
            .map(|mutation| mutation.site.clone()),
    );
    // Module constants are the outermost scope; everything below shadows them.
    // The module initializer binds them itself in source order.
    let mut env = constants
        .iter()
        .filter(|_| !f.is_module_initializer())
        .filter_map(|c| Some((c.name.clone(), Binding::Type(c.type_name.clone()?))))
        .collect::<BTreeMap<_, _>>();
    let mut ancestors = Vec::new();
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        if is_function(parent.kind(), dialect) {
            if let Some(scope) = scopes.get(&(
                parent.start_position().row + 1,
                parent.start_position().column + 1,
            )) {
                ancestors.push(scope);
            }
        }
        ancestor = parent.parent();
    }
    // Rust function items cannot capture; closures can. JS/TS nested functions
    // capture lexical names, while only arrows inherit an enclosing `this`.
    let capture_names = dialect != Dialect::Rust || node.kind() == "closure_expression";
    if capture_names {
        for scope in ancestors.iter().rev() {
            for parameter in &scope.parameters {
                if !matches!(parameter.name.as_str(), "this" | "self") {
                    env.insert(
                        parameter.name.clone(),
                        parameter
                            .type_name
                            .as_ref()
                            .map(|ty| {
                                typed_in_scope(ty, dialect, &scope.fqn, imports, &parameter.range)
                            })
                            .unwrap_or(Binding::Unknown),
                    );
                }
            }
            for ((owner, name), target) in nested {
                if owner == &scope.fqn
                    && scopes.values().any(|scope| {
                        scope.fqn == *target
                            && contains(
                                &scope.declaration_scope,
                                node.start_position().row + 1,
                                node.start_position().column + 1,
                            )
                            && matches!(
                                scope.kind.as_str(),
                                "function_declaration"
                                    | "generator_function_declaration"
                                    | "function_item"
                            )
                    })
                {
                    env.insert(name.clone(), Binding::Function(target.clone()));
                }
            }
        }
    }
    if matches!(dialect, Dialect::JavaScript | Dialect::TypeScript)
        && node.kind() == "arrow_function"
    {
        if let Some(scope) = ancestors
            .iter()
            .find(|scope| scope.kind != "arrow_function" || scope.parent_function.is_none())
        {
            if scope.kind == "method_definition"
                || (scope.kind == "arrow_function" && scope.parent_function.is_none())
            {
                if let Some(owner) = &scope.parent_type {
                    env.insert("this".into(), Binding::Type(owner.clone()));
                }
            }
        }
    } else if dialect == Dialect::Rust && node.kind() == "closure_expression" {
        if let Some(scope) = ancestors
            .iter()
            .find(|scope| scope.kind != "closure_expression")
        {
            if let Some(owner) = &scope.parent_type {
                if scope
                    .parameters
                    .iter()
                    .any(|parameter| parameter.implicit_receiver)
                {
                    env.insert("self".into(), Binding::Type(owner.clone()));
                }
            }
        }
    }
    for i in imports.iter().filter(|i| applies(i, &f.range)) {
        env.insert(import_name(i).to_owned(), import_binding(i));
    }
    for n in declared_names(body, source, dialect, imports) {
        env.insert(n, Binding::Unknown);
    }
    if matches!(dialect, Dialect::JavaScript | Dialect::TypeScript) {
        for c in named_children(body)
            .into_iter()
            .filter(|n| n.kind() == "lexical_declaration")
        {
            let mut children_cursor = c.walk();
            for d in c.named_children(&mut children_cursor) {
                if let Some(n) = child(d, "name").and_then(|n| name(n, source)) {
                    env.insert(n, Binding::Unknown);
                }
            }
        }
    }
    for p in &f.parameters {
        env.insert(
            p.name.clone(),
            p.type_name
                .as_ref()
                .map(|t| typed_in_scope(t, dialect, &f.fqn, imports, &p.range))
                .unwrap_or(Binding::Unknown),
        );
    }
    if let Some(params) = child(node, "parameters") {
        let mut children_cursor = params.walk();
        for p in params.named_children(&mut children_cursor) {
            let pattern = child(p, "pattern")
                .or_else(|| child(p, "name"))
                .or_else(|| child(p, "left"))
                .unwrap_or(p);
            if name(pattern, source).is_none() {
                for n in pattern_names(pattern, source) {
                    env.insert(n, Binding::Unknown);
                }
            }
        }
    }
    if f.parent_function.is_none() {
        if let Some(ty) = &f.parent_type {
            for n in receiver_names(f, dialect) {
                env.insert(
                    n.to_owned(),
                    Binding::Type(if dialect == Dialect::Php {
                        format!("\\{ty}")
                    } else {
                        ty.clone()
                    }),
                );
            }
        }
    }
    if dialect != Dialect::Python {
        for ((owner, name), fqn) in nested {
            if owner == &f.fqn
                && scopes.values().any(|scope| {
                    scope.fqn == *fqn
                        && contains(
                            &scope.declaration_scope,
                            body.start_position().row + 1,
                            body.start_position().column + 1,
                        )
                        && matches!(
                            scope.kind.as_str(),
                            "function_declaration"
                                | "generator_function_declaration"
                                | "function_item"
                        )
                })
            {
                env.insert(name.clone(), Binding::Function(fqn.clone()));
            }
        }
    }
    let mut calls = BTreeMap::<_, Vec<usize>>::new();
    for (i, c) in f.calls.iter().enumerate() {
        calls
            .entry((
                c.range.start_line,
                c.range.start_column,
                c.range.end_line,
                c.range.end_column,
            ))
            .or_default()
            .push(i);
    }
    let owner = f.fqn.clone();
    let mut walker = Walker {
        dialect,
        source,
        types,
        imports,
        nested,
        owner: &owner,
        calls,
        function: f,
    };
    walker.visit(body, &mut env, true);
    let mut sites = f.calls.drain(explicit_calls..);
    for access in &mut f.property_accesses {
        access.site = sites.next().unwrap();
    }
    for mutation in &mut f.property_mutations {
        mutation.site = sites.next().unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::receiver_path;

    #[test]
    fn receiver_paths_preserve_order_and_empty_segments() {
        for raw in [
            "value",
            "$value",
            "value.field",
            "value..field",
            "value.",
            "",
            "?->",
            "?->->",
            "??->",
            "value?->field::method",
            "value?.field->method",
            "тип?.поле::метод",
            "?::->",
            "value:::field",
        ] {
            let old = raw
                .replace("?->", ".")
                .replace("->", ".")
                .replace("?.", ".")
                .replace("::", ".");
            let new = receiver_path(raw);
            assert_eq!(new, old, "{raw}");
            let old_parts: Vec<_> = old.split('.').collect();
            let (first, rest) = new
                .split_once('.')
                .map_or((new.as_ref(), None), |(first, rest)| (first, Some(rest)));
            assert_eq!(first, old_parts[0]);
            assert_eq!(
                rest,
                (old_parts.len() > 1)
                    .then(|| old_parts[1..].join("."))
                    .as_deref()
            );
        }
        assert!(matches!(
            receiver_path("value.field"),
            std::borrow::Cow::Borrowed(_)
        ));
    }
}
