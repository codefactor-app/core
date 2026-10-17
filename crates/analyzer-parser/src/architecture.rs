//! Conservative structural witnesses. Unsupported binding forms retain exact spelling.
use super::*;
use analyzer_core::architecture::*;
use std::fmt::Write;

#[derive(Default)]
struct ShadowState {
    isinstance: bool,
    rust_namespaces: BTreeSet<String>,
    qualified_stubs: Vec<(usize, String)>,
}

pub(super) fn extract(
    root: Node<'_>,
    model: &FileModel,
    source: &str,
    path: &Path,
    dialect: Dialect,
) -> FileFacts {
    let mut facts = FileFacts::default();
    // A spelling alone cannot establish that a call names Python's builtin.
    // File-wide suppression is conservative when an import or nested scope shadows it.
    let isinstance_shadowed = dialect == Dialect::Python
        && (model.functions.iter().any(|f| {
            f.name == "isinstance" || f.parameters.iter().any(|p| p.name == "isinstance")
        }) || model.types.iter().any(|t| t.name == "isinstance")
            || model
                .imports
                .iter()
                .any(|i| analyzer_core::scoped_imports::alias(i) == "isinstance"));
    let mut shadows = ShadowState {
        isinstance: isinstance_shadowed,
        ..Default::default()
    };
    if dialect == Dialect::Rust {
        for name in model.types.iter().map(|t| t.name.as_str()).chain(
            model
                .imports
                .iter()
                .map(analyzer_core::scoped_imports::alias),
        ) {
            if matches!(name, "core" | "std") {
                shadows.rust_namespaces.insert(name.into());
            }
        }
    }
    let functions = model
        .functions
        .iter()
        .map(|f| ((f.range.start_line, f.range.start_column), f))
        .collect::<BTreeMap<_, _>>();
    let types = model
        .types
        .iter()
        .map(|t| ((t.range.start_line, t.range.start_column), t))
        .collect::<BTreeMap<_, _>>();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let pos = node.start_position();
        if let Some(t) = types
            .get(&(pos.row + 1, pos.column + 1))
            .filter(|_| is_type(node.kind(), dialect))
        {
            let header = signature(node, child(node, "body"), source);
            if header.split_whitespace().any(|s| s == "abstract")
                || matches!(t.kind, TypeKind::Trait | TypeKind::Interface)
            {
                facts.abstract_types.push(t.fqn.clone());
            }
            if matches!(t.kind, TypeKind::Enum)
                || header
                    .split_whitespace()
                    .any(|s| matches!(s, "final" | "sealed"))
            {
                facts.closed_types.push(t.fqn.clone());
            }
        }
        let mut function_body = None;
        if let Some(f) = functions
            .get(&(pos.row + 1, pos.column + 1))
            .filter(|_| is_function(node.kind(), dialect))
        {
            if let Some(body) = child(node, "body") {
                // Every helper in this pass is iterative; normalization already records depth diagnostics.
                if !body.has_error() {
                    stack.extend(function_facts(
                        body,
                        f,
                        model,
                        source,
                        path,
                        dialect,
                        &mut facts,
                        &mut shadows,
                    ));
                    function_body = Some(body.id());
                }
            }
        }
        shadows.isinstance |= record_shadow(node, source, dialect, &mut facts);
        record_rust_namespace(node, source, dialect, &mut shadows);
        php_class_reference(node, source, path, dialect, &mut facts);
        if let Some(access) =
            value_access(node, None, model, &BTreeMap::new(), source, path, dialect)
        {
            facts.value_accesses.push(access);
        }
        if matches!(dialect, Dialect::JavaScript | Dialect::TypeScript)
            && matches!(node.kind(), "import_statement" | "export_statement")
        {
            if let Some(literal) = child(node, "source") {
                let (name, kind) = match js_module_literal(snippet(literal, source)) {
                    Some(name) => (name, "module_import"),
                    None => (snippet(literal, source).into(), "unknown_module_import"),
                };
                facts.references.push(TypeReference {
                    name,
                    kind: kind.into(),
                    range: range(node, path),
                });
            }
        }
        // Annotation leaves are evidence of type use, not inferred runtime dispatch.
        if matches!(
            node.kind(),
            "type_identifier" | "scoped_type_identifier" | "qualified_name" | "named_type"
        ) || python_annotation_reference(node, dialect)
        {
            let parent_type = node.parent().is_some_and(|p| {
                matches!(
                    p.kind(),
                    "scoped_type_identifier" | "qualified_name" | "named_type"
                )
            });
            if !parent_type
                && !php_class_reference_child(node, dialect)
                && node.parent().is_none_or(|p| {
                    child(p, "name").is_none_or(|n| n.id() != node.id())
                        || !is_type(p.kind(), dialect)
                })
            {
                facts.references.push(TypeReference {
                    name: snippet(node, source).into(),
                    kind: "type_reference".into(),
                    range: range(node, path),
                });
            }
        }
        stack.extend(
            named_children(node)
                .into_iter()
                .rev()
                .filter(|n| Some(n.id()) != function_body),
        );
    }
    if shadows.isinstance {
        for f in &mut facts.functions {
            f.decisions
                .retain(|d| d.cases.iter().all(|c| c.tested_type.is_none()));
        }
    }
    for (index, namespace) in shadows.qualified_stubs {
        if shadows.rust_namespaces.contains(&namespace) {
            facts.functions[index].unsupported_stub = false;
            facts.functions[index].unsupported_stub_qualified = false;
        }
    }
    facts.rust_namespace_shadows = shadows.rust_namespaces.into_iter().collect();
    facts
}
fn php_class_reference_child(node: Node<'_>, dialect: Dialect) -> bool {
    dialect == Dialect::Php
        && node.parent().is_some_and(|p| {
            matches!(
                p.kind(),
                "class_constant_access_expression"
                    | "attribute"
                    | "namespace_use_clause"
                    | "namespace_definition"
            )
        })
}

/// Python uses ordinary value identifier/attribute nodes in annotations. Keep
/// each named type leaf without treating a container element as its receiver.
fn python_annotation_reference(node: Node<'_>, dialect: Dialect) -> bool {
    if dialect != Dialect::Python
        || !matches!(node.kind(), "identifier" | "attribute")
        || node
            .parent()
            .is_some_and(|parent| parent.kind() == "attribute")
    {
        return false;
    }
    let mut parent = node.parent();
    while let Some(node) = parent {
        match node.kind() {
            "type" => return true,
            "generic_type" | "type_parameter" | "binary_operator" | "parenthesized_expression" => {
                parent = node.parent();
            }
            _ => return false,
        }
    }
    false
}

fn php_class_reference(
    node: Node<'_>,
    source: &str,
    path: &Path,
    dialect: Dialect,
    facts: &mut FileFacts,
) {
    if dialect != Dialect::Php {
        return;
    }
    if node.kind() == "attribute" {
        if let Some(target) = node
            .named_child(0)
            .filter(|n| matches!(n.kind(), "name" | "qualified_name" | "relative_name"))
        {
            facts.references.push(TypeReference {
                name: snippet(target, source).into(),
                kind: "metadata_reference".into(),
                range: range(target, path),
            });
        }
        return;
    }
    if node.kind() != "class_constant_access_expression" {
        return;
    }
    let children = named_children(node);
    let [target, member] = children.as_slice() else {
        return;
    };
    if !matches!(target.kind(), "name" | "qualified_name" | "relative_name")
        || member.kind() != "name"
    {
        return;
    }
    let mut parent = node.parent();
    let mut metadata = false;
    while let Some(p) = parent {
        if p.kind() == "attribute" {
            metadata = true;
            break;
        }
        if is_function(p.kind(), dialect) || is_type(p.kind(), dialect) {
            break;
        }
        parent = p.parent();
    }
    facts.references.push(TypeReference {
        name: snippet(*target, source).into(),
        kind: if metadata {
            "metadata_reference"
        } else {
            "class_reference"
        }
        .into(),
        range: range(node, path),
    });
}

fn function_facts<'tree>(
    body: Node<'tree>,
    f: &FunctionModel,
    model: &FileModel,
    source: &str,
    path: &Path,
    dialect: Dialect,
    facts: &mut FileFacts,
    shadows: &mut ShadowState,
) -> Vec<Node<'tree>> {
    let mut ff = FunctionFacts {
        fqn: f.fqn.clone(),
        range: f.range.clone(),
        body_present: true,
        ..Default::default()
    };
    let mut statements = named_children(body)
        .into_iter()
        .filter(|n| !n.kind().contains("comment"))
        .collect::<Vec<_>>();
    if dialect == Dialect::Python
        && statements
            .first()
            .is_some_and(|n| python_docstring(*n, source))
    {
        statements.remove(0);
    }
    let mut qualified_namespace = None;
    let stub = if statements.len() == 1 {
        let n = statements[0];
        let raw = snippet(n, source).trim();
        match dialect {
            Dialect::Python => {
                n.kind() == "raise_statement"
                    && (raw == "raise NotImplementedError"
                        || raw == "raise NotImplementedError()"
                        || raw.starts_with("raise NotImplementedError("))
            }
            Dialect::Rust => {
                let witness = rust_unsupported_macro(n, source);
                qualified_namespace = witness.flatten();
                witness.is_some()
            }
            _ => false,
        }
    } else {
        false
    };
    // A local binding with this name defeats the builtin interpretation.
    ff.unsupported_stub = stub && !f.parameters.iter().any(|p| p.name == "NotImplementedError");
    ff.unsupported_stub_qualified = ff.unsupported_stub && qualified_namespace.is_some();
    ff.normal_exit = !ff.unsupported_stub
        && !statements.is_empty()
        && !statements
            .iter()
            .all(|n| matches!(n.kind(), "raise_statement" | "throw_statement"));
    let mut nested = Vec::new();
    let mut stack = vec![body];
    let mut blocks = vec![body];
    let mut rebound_roots = BTreeSet::new();
    let mut chain_roots = Vec::new();
    let mut local_types = BTreeMap::new();
    let mut writes = BTreeMap::<String, usize>::new();
    let mut accesses = Vec::new();
    while let Some(n) = stack.pop() {
        shadows.isinstance |= record_shadow(n, source, dialect, facts);
        record_rust_namespace(n, source, dialect, shadows);
        php_class_reference(n, source, path, dialect, facts);
        for_each_binding_name(n, source, |name| {
            *writes.entry(name.into()).or_default() += 1;
            if ["self", "this"].contains(&name) || f.parameters.iter().any(|p| p.name == name) {
                rebound_roots.insert(name);
            }
        });
        if n.id() != body.id() && (is_function(n.kind(), dialect) || is_type(n.kind(), dialect)) {
            nested.push(n);
            continue;
        }
        if matches!(
            n.kind(),
            "variable_declarator" | "let_declaration" | "assignment" | "assignment_expression"
        ) {
            if let Some(target) = child(n, "name")
                .or_else(|| child(n, "pattern"))
                .or_else(|| child(n, "left"))
            {
                if matches!(target.kind(), "identifier" | "variable_name") {
                    let ty = child(n, "type")
                        .map(|ty| trim_type(snippet(ty, source)))
                        .or_else(|| {
                            child(n, "value")
                                .or_else(|| child(n, "right"))
                                .filter(|v| {
                                    matches!(
                                        v.kind(),
                                        "new_expression"
                                            | "object_creation_expression"
                                            | "struct_expression"
                                    )
                                })
                                .and_then(|v| creation_name(v, source, dialect))
                        });
                    if let Some(ty) = ty {
                        local_types.insert(
                            snippet(target, source).trim_start_matches('$').to_string(),
                            ty,
                        );
                    }
                }
            }
        }
        if let Some(access) = value_access(n, Some(f), model, &local_types, source, path, dialect) {
            accesses.push(access);
        }
        if (matches!(
            n.kind(),
            "type_identifier" | "scoped_type_identifier" | "qualified_name" | "named_type"
        ) || python_annotation_reference(n, dialect))
            && !php_class_reference_child(n, dialect)
            && n.parent().is_none_or(|p| {
                !matches!(
                    p.kind(),
                    "scoped_type_identifier" | "qualified_name" | "named_type"
                )
            })
        {
            facts.references.push(TypeReference {
                name: snippet(n, source).into(),
                kind: "type_reference".into(),
                range: range(n, path),
            });
        }
        if n.kind().contains("subscript")
            || n.kind().contains("computed_property")
            || snippet(n, source).starts_with("getattr(")
        {
            ff.dynamic_access = true;
        }
        if let Some((object, field)) = member(n, dialect) {
            let dynamic_field = dialect == Dialect::Php && field.kind() != "name";
            ff.dynamic_access |= dynamic_field;
            if !dynamic_field && ["self", "this", "$this"].contains(&snippet(object, source).trim())
            {
                // Method names are not fields, even when their AST is a member expression.
                let called = is_call(n.kind(), dialect)
                    || n.parent().is_some_and(|p| {
                        child(p, "function").is_some_and(|c| c.id() == n.id())
                            || child(p, "name").is_some_and(|c| c.id() == field.id())
                                && is_call(p.kind(), dialect)
                    });
                if !called {
                    ff.own_fields
                        .push(snippet(field, source).trim().trim_start_matches('$').into());
                }
            }
        }
        if matches!(n.kind(), "if_statement" | "if_expression") {
            // Else-if nodes belong to their outer decision.
            let nested_else = n.parent().is_some_and(|p| {
                child(p, "alternative").is_some_and(|a| a.id() == n.id())
                    || p.kind() == "else_clause"
            });
            if !nested_else {
                if let Some(d) = decision(n, f, source, path, dialect) {
                    ff.decisions.push(d);
                }
            }
        }
        if matches!(
            n.kind(),
            "switch_statement" | "match_expression" | "match_statement"
        ) {
            if let Some(d) = switch_decision(n, f, source, path, dialect) {
                ff.decisions.push(d);
            }
        }
        if matches!(
            n.kind(),
            "new_expression" | "object_creation_expression" | "struct_expression"
        ) {
            if let Some(name) = creation_name(n, source, dialect) {
                facts.references.push(TypeReference {
                    name,
                    kind: "construction".into(),
                    range: range(n, path),
                });
            }
        }
        if is_call(n.kind(), dialect) {
            if let Some(fun) = (dialect == Dialect::Php && member(n, dialect).is_some())
                .then_some(n)
                .or_else(|| child(n, "function"))
                .or_else(|| child(n, "object"))
            {
                let mut at = fun;
                let mut members = Vec::new();
                while let Some((obj, field)) = member(at, dialect) {
                    members.push(snippet(field, source).into());
                    at = obj;
                }
                if members.len() >= 3 {
                    members.reverse();
                    let root = snippet(at, source).trim().trim_start_matches('$');
                    let ty = f
                        .parameters
                        .iter()
                        .find(|p| p.name == root)
                        .and_then(|p| p.type_name.clone())
                        .or_else(|| {
                            ["this", "self"]
                                .contains(&root)
                                .then(|| f.parent_type.clone())
                                .flatten()
                        });
                    ff.member_chains.push(MemberChain {
                        root_type: ty,
                        members,
                        range: range(n, path),
                    });
                    chain_roots.push((ff.member_chains.len() - 1, root));
                }
            }
        }
        if n.id() != body.id() && is_block(n) {
            blocks.push(n);
        }
        stack.extend(named_children(n).into_iter().rev());
    }
    ff.own_fields.sort();
    ff.own_fields.dedup();
    for mut access in accesses {
        let root = access
            .receiver
            .trim_start_matches('$')
            .split(['.', '-', ':', '\\'])
            .next()
            .unwrap_or("");
        let assigned = writes.get(root).copied().unwrap_or(0);
        if rebound_roots.contains(root)
            || assigned > usize::from(local_types.contains_key(root))
            || (assigned > 0 && access.receiver_type.is_none())
        {
            access.receiver_type = None;
            access.binding_unknown = true;
        }
        facts.value_accesses.push(access);
    }
    // Reassignments or lexical shadows defeat the original parameter annotation.
    // Without a binding identity for the complete chain, keep its range but no type proof.
    for (index, root) in chain_roots {
        if rebound_roots.contains(root) {
            ff.member_chains[index].root_type = None;
        }
    }
    // Full body + disjoint innermost blocks bound representation memory to roughly 2x source.
    let block_ids = blocks.iter().map(|n| n.id()).collect::<BTreeSet<_>>();
    let candidates = blocks
        .into_iter()
        .filter(|n| n.id() == body.id() || !has_child_block(*n, &block_ids))
        .collect::<Vec<_>>();
    let bindings = bindings(body, f, source, dialect);
    for candidate in candidates {
        let (canonical, tokens, statements, renamed) =
            canonical(candidate, source, bindings.as_ref(), dialect);
        if tokens < 30 || statements < 2 {
            continue;
        }
        facts.fragments.push(Fragment {
            entity: f.fqn.clone(),
            range: range(candidate, path),
            tokens,
            statements,
            canonical,
            renamed,
        });
    }

    if let Some(namespace) = qualified_namespace {
        shadows
            .qualified_stubs
            .push((facts.functions.len(), namespace.into()));
    }
    facts.functions.push(ff);
    nested
}
fn python_docstring(n: Node<'_>, source: &str) -> bool {
    if n.kind() != "expression_statement" {
        return false;
    }
    let children = named_children(n)
        .into_iter()
        .filter(|c| !c.kind().contains("comment"))
        .collect::<Vec<_>>();
    let [value] = children.as_slice() else {
        return false;
    };
    let mut value = *value;
    while value.kind() == "parenthesized_expression" {
        let children = named_children(value)
            .into_iter()
            .filter(|c| !c.kind().contains("comment"))
            .collect::<Vec<_>>();
        let [inner] = children.as_slice() else {
            return false;
        };
        value = *inner;
    }
    let strings = match value.kind() {
        "string" => vec![value],
        "concatenated_string" => named_children(value)
            .into_iter()
            .filter(|c| !c.kind().contains("comment"))
            .collect(),
        _ => return false,
    };
    !strings.is_empty()
        && strings.iter().all(|s| {
            if s.kind() != "string" {
                return false;
            }
            let raw = snippet(*s, source);
            raw.find(['\'', '"']).is_some_and(|quote| {
                raw[..quote]
                    .chars()
                    .all(|c| matches!(c, 'r' | 'R' | 'u' | 'U'))
            })
        })
}
// Only exact intrinsic paths establish origin. A namespace declaration/alias may
// defeat even an absolute spelling in projects with an unknown Rust edition.
fn rust_unsupported_macro<'source>(
    n: Node<'_>,
    source: &'source str,
) -> Option<Option<&'source str>> {
    let invocation = if n.kind() == "macro_invocation" {
        n
    } else if n.kind() == "expression_statement" {
        let children = named_children(n)
            .into_iter()
            .filter(|c| !c.kind().contains("comment"))
            .collect::<Vec<_>>();
        let [value] = children.as_slice() else {
            return None;
        };
        *value
    } else {
        return None;
    };
    if invocation.kind() != "macro_invocation" {
        return None;
    }
    let raw = snippet(child(invocation, "macro")?, source).trim();
    let name = if raw.contains(char::is_whitespace) {
        std::borrow::Cow::Owned(raw.split_whitespace().collect::<String>())
    } else {
        std::borrow::Cow::Borrowed(raw)
    };
    match name.as_ref() {
        "unimplemented" => Some(None),
        "core::unimplemented" | "::core::unimplemented" => Some(Some("core")),
        "std::unimplemented" | "::std::unimplemented" => Some(Some("std")),
        _ => None,
    }
}
fn record_rust_namespace(n: Node<'_>, source: &str, dialect: Dialect, shadows: &mut ShadowState) {
    if dialect != Dialect::Rust || n.kind() != "extern_crate_declaration" {
        return;
    }
    if let Some(name) = child(n, "alias").or_else(|| child(n, "name")) {
        let name = snippet(name, source);
        if matches!(name, "core" | "std") {
            shadows.rust_namespaces.insert(name.into());
        }
    }
}
fn is_block(n: Node<'_>) -> bool {
    matches!(n.kind(), "block" | "statement_block" | "compound_statement")
}
fn has_child_block(n: Node<'_>, ids: &BTreeSet<usize>) -> bool {
    let mut stack = named_children(n);
    while let Some(c) = stack.pop() {
        if ids.contains(&c.id()) {
            return true;
        }
        stack.extend(named_children(c));
    }
    false
}
fn member(n: Node<'_>, dialect: Dialect) -> Option<(Node<'_>, Node<'_>)> {
    match (dialect, n.kind()) {
        (Dialect::Python, "attribute") => Some((child(n, "object")?, child(n, "attribute")?)),
        (Dialect::JavaScript | Dialect::TypeScript, "member_expression") => {
            Some((child(n, "object")?, child(n, "property")?))
        }
        (
            Dialect::Php,
            "member_access_expression"
            | "member_call_expression"
            | "nullsafe_member_access_expression"
            | "nullsafe_member_call_expression",
        ) => Some((child(n, "object")?, child(n, "name")?)),
        (Dialect::Rust, "field_expression") => Some((child(n, "value")?, child(n, "field")?)),
        _ => None,
    }
}
fn value_context(n: Node<'_>, dialect: Dialect) -> bool {
    let mut parent = n.parent();
    while let Some(p) = parent {
        let kind = p.kind();
        if kind.contains("import")
            || kind.starts_with("namespace_")
            || matches!(
                kind,
                "use_declaration"
                    | "attribute"
                    | "attribute_list"
                    | "attribute_group"
                    | "decorator"
                    | "decorated_definition"
                    | "type"
                    | "type_annotation"
                    | "type_arguments"
                    | "type_parameters"
                    | "type_alias_declaration"
                    | "generic_type"
                    | "named_type"
                    | "optional_type"
                    | "union_type"
                    | "intersection_type"
                    | "scoped_type_identifier"
                    | "token_tree"
                    | "macro_invocation"
                    | "macro_definition"
                    | "attribute_item"
                    | "inner_attribute_item"
            )
        {
            return false;
        }
        if child(p, "type")
            .is_some_and(|ty| n.start_byte() >= ty.start_byte() && n.end_byte() <= ty.end_byte())
        {
            return false;
        }
        if is_function(kind, dialect) {
            return child(p, "body").is_some_and(|body| {
                n.start_byte() >= body.start_byte() && n.end_byte() <= body.end_byte()
            });
        }
        if is_type(kind, dialect) {
            return child(p, "body").is_some_and(|body| {
                n.start_byte() >= body.start_byte() && n.end_byte() <= body.end_byte()
            });
        }
        parent = p.parent();
    }
    true
}
fn value_receiver_type(
    mut n: Node<'_>,
    f: Option<&FunctionModel>,
    model: &FileModel,
    locals: &BTreeMap<String, String>,
    source: &str,
    dialect: Dialect,
) -> Option<String> {
    let mut fields = Vec::new();
    while let Some((object, field)) = member(n, dialect) {
        if fields.len() >= 256 {
            return None;
        }
        fields.push(snippet(field, source).trim_start_matches('$'));
        n = object;
    }
    let raw = snippet(n, source).trim().trim_start_matches('$');
    let mut ty = f
        .and_then(|f| {
            if let Some(p) = f.parameters.iter().find(|p| p.name == raw) {
                return if p.implicit_receiver {
                    f.parent_type.clone()
                } else {
                    p.type_name.clone()
                };
            }
            if ["self", "this"].contains(&raw)
                && f.parent_function.is_none()
                && !f.modifiers.iter().any(|m| m == "static")
            {
                return f.parent_type.clone();
            }
            None
        })
        .or_else(|| locals.get(raw).cloned())
        .or_else(|| {
            matches!(
                n.kind(),
                "new_expression" | "object_creation_expression" | "struct_expression"
            )
            .then(|| creation_name(n, source, dialect))
            .flatten()
        })?;
    for field in fields.into_iter().rev() {
        let owner = model.types.iter().find(|t| t.fqn == ty || t.name == ty)?;
        ty = owner
            .fields
            .iter()
            .find(|p| p.name == field)
            .and_then(|p| p.type_name.clone())?;
    }
    Some(ty)
}

fn value_access(
    n: Node<'_>,
    f: Option<&FunctionModel>,
    model: &FileModel,
    locals: &BTreeMap<String, String>,
    source: &str,
    path: &Path,
    dialect: Dialect,
) -> Option<ValueAccess> {
    if !matches!(
        n.kind(),
        "identifier"
            | "name"
            | "variable_name"
            | "attribute"
            | "member_expression"
            | "member_access_expression"
            | "nullsafe_member_access_expression"
            | "field_expression"
            | "class_constant_access_expression"
            | "scoped_property_access_expression"
            | "scoped_identifier"
    ) {
        return None;
    }
    if !value_context(n, dialect) {
        return None;
    }
    if dialect == Dialect::Rust
        && n.parent().is_some_and(|p| {
            p.kind() == "scoped_identifier"
                && child(p, "path").is_some_and(|path| path.id() == n.id())
        })
    {
        return None; // A qualifier such as crate::module is not a value access.
    }
    // Calls have separately resolved graph edges; their callee spelling is not a value witness.
    if is_call(n.kind(), dialect)
        || n.parent().is_some_and(|p| {
            is_call(p.kind(), dialect) && child(p, "function").is_some_and(|c| c.id() == n.id())
        })
    {
        return None;
    }
    let pair = member(n, dialect).or_else(|| match (dialect, n.kind()) {
        (Dialect::Php, "class_constant_access_expression") => {
            let children = named_children(n);
            (children.len() == 2 && !snippet(children[1], source).eq_ignore_ascii_case("class"))
                .then(|| (children[0], children[1]))
        }
        (Dialect::Php, "scoped_property_access_expression") => {
            Some((child(n, "scope")?, child(n, "name")?))
        }
        (Dialect::Rust, "scoped_identifier") => Some((child(n, "path")?, child(n, "name")?)),
        _ => None,
    });
    if let Some((receiver, field)) = pair {
        let raw = snippet(receiver, source).trim().trim_start_matches('$');
        let receiver_type = value_receiver_type(receiver, f, model, locals, source, dialect);
        let binding_unknown = receiver_type.is_none()
            && f.is_some_and(|f| f.parameters.iter().any(|p| p.name == raw));
        return Some(ValueAccess {
            receiver: raw.into(),
            member: Some(snippet(field, source).trim_start_matches('$').into()),
            receiver_type,
            binding_unknown,
            range: range(n, path),
        });
    }
    if !matches!(n.kind(), "identifier" | "name" | "variable_name") {
        return None;
    }
    let raw = snippet(n, source).trim_start_matches('$');
    if !model
        .imports
        .iter()
        .any(|i| !i.type_only && analyzer_core::scoped_imports::alias(i) == raw)
    {
        return None;
    }
    if n.parent().is_some_and(|p| {
        member(p, dialect).is_some()
            || matches!(
                p.kind(),
                "scoped_identifier"
                    | "class_constant_access_expression"
                    | "scoped_property_access_expression"
            )
            || ["name", "left", "pattern"]
                .iter()
                .any(|field| child(p, field).is_some_and(|c| c.id() == n.id()))
    }) {
        return None;
    }
    Some(ValueAccess {
        receiver: raw.into(),
        member: None,
        receiver_type: None,
        binding_unknown: f.is_some_and(|f| f.parameters.iter().any(|p| p.name == raw))
            || locals.contains_key(raw),
        range: range(n, path),
    })
}
fn creation_name(n: Node<'_>, source: &str, dialect: Dialect) -> Option<String> {
    let ty = child(n, "constructor")
        .or_else(|| child(n, "name"))
        .or_else(|| child(n, "type"))
        .or_else(|| {
            named_children(n)
                .into_iter()
                .find(|c| !c.kind().contains("argument") && !c.kind().contains("initializer"))
        })?;
    let raw = snippet(ty, source).trim();
    crate::types::reference(language_name(dialect), raw)
}
fn creations(n: Node<'_>, source: &str, path: &Path, dialect: Dialect) -> Vec<TypeReference> {
    let mut out = Vec::new();
    let mut stack = vec![n];
    while let Some(c) = stack.pop() {
        if is_function(c.kind(), dialect) || is_type(c.kind(), dialect) {
            continue;
        }
        if matches!(
            c.kind(),
            "new_expression" | "object_creation_expression" | "struct_expression"
        ) {
            if let Some(name) = creation_name(c, source, dialect) {
                out.push(TypeReference {
                    name,
                    kind: "construction".into(),
                    range: range(c, path),
                });
            }
        } else if dialect == Dialect::Python && c.kind() == "call" {
            if let Some(fun) =
                child(c, "function").filter(|f| matches!(f.kind(), "identifier" | "attribute"))
            {
                out.push(TypeReference {
                    name: snippet(fun, source).into(),
                    kind: "construction_candidate".into(),
                    range: range(c, path),
                });
            }
        }
        stack.extend(named_children(c));
    }
    out
}
fn condition(
    n: Node<'_>,
    source: &str,
    dialect: Dialect,
) -> Option<(String, String, Option<String>)> {
    let n = if n.kind() == "parenthesized_expression" {
        named_children(n).into_iter().next()?
    } else {
        n
    };
    if dialect == Dialect::Python
        && n.kind() == "call"
        && text_field(n, "function", source).as_deref() == Some("isinstance")
    {
        let args = named_children(child(n, "arguments")?);
        if args.len() != 2 || !matches!(args[1].kind(), "identifier" | "attribute") {
            return None;
        }
        let ty = snippet(args[1], source).to_string();
        return Some((snippet(args[0], source).into(), ty.clone(), Some(ty)));
    }
    if matches!(
        n.kind(),
        "binary_expression" | "binary_operator" | "instanceof_expression"
    ) {
        let left = child(n, "left")?;
        let right = child(n, "right")?;
        let op = text_field(n, "operator", source)
            .unwrap_or_else(|| source[left.end_byte()..right.start_byte()].trim().into());
        if op == "instanceof" {
            let ty = snippet(right, source).to_string();
            return Some((snippet(left, source).into(), ty.clone(), Some(ty)));
        }
        if ["==", "===", "is"].contains(&op.as_str())
            && matches!(
                right.kind(),
                "string"
                    | "string_literal"
                    | "integer"
                    | "number"
                    | "integer_literal"
                    | "attribute"
                    | "member_expression"
            )
        {
            return Some((
                snippet(left, source).into(),
                snippet(right, source).into(),
                None,
            ));
        }
    }
    None
}
fn subject_type(f: &FunctionModel, subject: &str) -> Option<String> {
    let s = subject.trim().trim_start_matches('$');
    f.parameters
        .iter()
        .find(|p| p.name == s)
        .and_then(|p| p.type_name.clone())
}
fn decision(
    n: Node<'_>,
    f: &FunctionModel,
    source: &str,
    path: &Path,
    dialect: Dialect,
) -> Option<Decision> {
    let mut cases = Vec::new();
    let mut at = n;
    let mut subject = None;
    loop {
        let cond = child(at, "condition")?;
        let (s, label, tested_type) = condition(cond, source, dialect)?;
        if subject.as_ref().is_some_and(|old| old != &s) {
            return None;
        }
        subject = Some(s);
        let body = child(at, "consequence").or_else(|| child(at, "body"))?;
        cases.push(DecisionCase {
            label,
            tested_type,
            range: range(cond, path),
            creations: creations(body, source, path, dialect),
        });
        if matches!(dialect, Dialect::Python | Dialect::Php) {
            for c in named_children(at)
                .into_iter()
                .filter(|c| matches!(c.kind(), "elif_clause" | "else_if_clause"))
            {
                let cond = child(c, "condition")?;
                let (s, label, tested_type) = condition(cond, source, dialect)?;
                if subject.as_ref() != Some(&s) {
                    return None;
                }
                let body = child(c, "consequence").or_else(|| child(c, "body"))?;
                cases.push(DecisionCase {
                    label,
                    tested_type,
                    range: range(cond, path),
                    creations: creations(body, source, path, dialect),
                });
            }
            break;
        }
        let alternative = child(at, "alternative").or_else(|| {
            named_children(at)
                .into_iter()
                .find(|n| matches!(n.kind(), "else_if_clause" | "else_clause"))
        });
        let Some(mut next) = alternative else {
            break;
        };
        if next.kind() == "else_clause" {
            let Some(inner) = named_children(next)
                .into_iter()
                .find(|c| matches!(c.kind(), "if_statement" | "if_expression"))
            else {
                break;
            };
            next = inner;
        }
        if !matches!(
            next.kind(),
            "if_statement" | "if_expression" | "else_if_clause"
        ) {
            break;
        }
        at = next;
    }
    if cases.len() < 2 {
        return None;
    }
    let subject = subject?;
    Some(Decision {
        range: range(n, path),
        subject_type: subject_type(f, &subject),
        subject,
        cases,
    })
}
fn switch_decision(
    n: Node<'_>,
    f: &FunctionModel,
    source: &str,
    path: &Path,
    dialect: Dialect,
) -> Option<Decision> {
    let subject = child(n, "value").or_else(|| child(n, "condition"))?;
    let text = snippet(subject, source)
        .trim()
        .trim_matches(['(', ')'])
        .to_string();
    let body = child(n, "body")?;
    let mut cases = Vec::new();
    for c in named_children(body) {
        if !matches!(
            c.kind(),
            "switch_case" | "case_statement" | "match_arm" | "case_clause"
        ) {
            continue;
        }
        let label = child(c, "value").or_else(|| child(c, "pattern"))?;
        cases.push(DecisionCase {
            label: snippet(label, source).into(),
            tested_type: None,
            range: range(label, path),
            creations: creations(c, source, path, dialect),
        });
    }
    (cases.len() >= 2).then(|| Decision {
        range: range(n, path),
        subject_type: subject_type(f, &text),
        subject: text,
        cases,
    })
}
// Alpha-renaming is deliberately narrower than lexical binding inference for calls.
// Reject scopes, implicit binders and metaprogramming we cannot certify in this pass.
fn bindings(
    body: Node<'_>,
    f: &FunctionModel,
    source: &str,
    dialect: Dialect,
) -> Option<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    for (i, p) in f
        .parameters
        .iter()
        .filter(|p| !p.implicit_receiver)
        .enumerate()
    {
        if p.variadic
            || p.name.is_empty()
            || !p.name.chars().all(|c| c.is_alphanumeric() || c == '_')
        {
            return None;
        }
        map.insert(p.name.clone(), format!("@p{i}"));
    }
    let mut stack = vec![body];
    let mut declared = BTreeSet::new();
    let mut local = 0;
    while let Some(n) = stack.pop() {
        // Opaque syntax may refer to surrounding bindings after expansion. Keeping
        // its spelling alone cannot justify renaming other uses of the same name.
        if dialect == Dialect::Rust
            && matches!(
                n.kind(),
                "macro_invocation" | "macro_definition" | "attribute_item" | "inner_attribute_item"
            )
        {
            return None;
        }
        if dialect == Dialect::Php && n.kind() == "variable_name" {
            let raw = snippet(n, source);
            if !raw.strip_prefix('$').is_some_and(|s| {
                !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_')
            }) {
                return None;
            }
        }
        if n.id() != body.id() && (is_function(n.kind(), dialect) || is_type(n.kind(), dialect)) {
            return None;
        }
        if matches!(
            n.kind(),
            "global_statement"
                | "nonlocal_statement"
                | "catch_clause"
                | "except_clause"
                | "for_statement"
                | "for_in_statement"
                | "for_expression"
                | "with_statement"
                | "using_declaration"
        ) || n.kind().contains("pattern") && n.kind() != "identifier"
        {
            return None;
        }
        if matches!(n.kind(), "call" | "call_expression")
            && child(n, "function").is_some_and(|fun| {
                [
                    "eval", "exec", "locals", "globals", "getattr", "setattr", "extract", "compact",
                ]
                .contains(&snippet(fun, source))
            })
        {
            return None;
        }
        if dialect == Dialect::Python && n.kind() == "call" {
            let Some(fun) = child(n, "function") else {
                return None;
            };
            let name = member(fun, dialect).map_or(fun, |(_, field)| field);
            if matches!(snippet(name, source), "vars" | "dir") {
                let args = child(n, "arguments")
                    .map(named_children)
                    .unwrap_or_default();
                if args.iter().all(|a| a.kind().contains("comment"))
                    || args
                        .iter()
                        .any(|a| matches!(a.kind(), "list_splat" | "dictionary_splat"))
                {
                    return None;
                }
            }
        }
        if matches!(
            n.kind(),
            "assignment" | "assignment_expression" | "variable_declarator" | "let_declaration"
        ) {
            let left = child(n, "left")
                .or_else(|| child(n, "name"))
                .or_else(|| child(n, "pattern"))?;
            if matches!(left.kind(), "identifier" | "variable_name") {
                let name = snippet(left, source)
                    .trim()
                    .trim_start_matches('$')
                    .to_string();
                if matches!(
                    dialect,
                    Dialect::JavaScript | Dialect::TypeScript | Dialect::Rust
                ) {
                    if matches!(n.kind(), "variable_declarator" | "let_declaration") {
                        if matches!(dialect, Dialect::JavaScript | Dialect::TypeScript)
                            && n.parent()
                                .is_some_and(|p| p.kind() == "lexical_declaration")
                        {
                            let mut parent = n.parent();
                            while let Some(p) = parent {
                                if p.id() == body.id() {
                                    break;
                                }
                                if is_block(p) || p.kind() == "switch_body" {
                                    return None;
                                }
                                parent = p.parent();
                            }
                        }
                        if map.contains_key(&name) || !declared.insert(name.clone()) {
                            return None;
                        }
                        map.insert(name, format!("@v{local}"));
                        local += 1;
                    }
                } else if !map.contains_key(&name) {
                    map.insert(name, format!("@v{local}"));
                    local += 1;
                }
            } else if !matches!(
                left.kind(),
                "attribute"
                    | "member_expression"
                    | "field_expression"
                    | "subscript"
                    | "subscript_expression"
                    | "member_access_expression"
            ) {
                return None;
            }
        }
        stack.extend(named_children(n).into_iter().rev());
    }
    Some(map)
}
fn canonical(
    n: Node<'_>,
    source: &str,
    bindings: Option<&BTreeMap<String, String>>,
    dialect: Dialect,
) -> (String, usize, usize, bool) {
    let mut out = String::with_capacity(n.end_byte().saturating_sub(n.start_byte()) * 2);
    let mut renamed = false;
    let mut tokens = 0;
    let mut statements = 0;
    let mut stack = vec![(n, false, false)];
    while let Some((c, exit, parent_opaque)) = stack.pop() {
        // Python's indentation does not appear among syntax leaves. Keep explicit block boundaries.
        if exit {
            out.push_str("@end;");
            continue;
        }
        if c.kind().contains("comment") {
            continue;
        }
        let opaque = parent_opaque
            || dialect == Dialect::Rust
                && matches!(
                    c.kind(),
                    "macro_invocation"
                        | "macro_definition"
                        | "attribute_item"
                        | "inner_attribute_item"
                );
        if opaque && !parent_opaque {
            let raw = snippet(c, source);
            write!(out, "@rust:{}:{raw};", raw.len()).unwrap();
        }
        if c.kind().ends_with("statement")
            || c.kind() == "let_declaration"
            || c.kind() == "assignment"
        {
            statements += 1;
        }
        let atomic = c.child_count() == 0
            || matches!(
                c.kind(),
                "string" | "string_literal" | "template_string" | "variable_name" | "char_literal"
            );
        if !atomic {
            if !opaque
                && (dialect == Dialect::Python && is_block(c)
                    || matches!(dialect, Dialect::JavaScript | Dialect::TypeScript)
                        && c.kind().ends_with("statement"))
            {
                write!(out, "@{};", c.kind()).unwrap();
                stack.push((c, true, opaque));
            }
            stack.extend(
                all_children(c)
                    .into_iter()
                    .rev()
                    .map(|c| (c, false, opaque)),
            );
            continue;
        }
        if opaque {
            tokens += 1;
            continue;
        }
        let text = snippet(c, source);
        let mut normalized = None;
        if atomic && matches!(c.kind(), "identifier" | "variable_name") {
            let property = c.parent().is_some_and(|p| {
                member(p, dialect).is_some_and(|(_, field)| field.id() == c.id())
                    || ["key", "type", "label"]
                        .iter()
                        .any(|field| child(p, field).is_some_and(|k| k.id() == c.id()))
                    || p.kind() == "keyword_argument"
                        && child(p, "name").is_some_and(|k| k.id() == c.id())
                    || p.kind().contains("type")
            });
            if !property {
                normalized = bindings
                    .and_then(|m| m.get(text.trim_start_matches('$')))
                    .map(String::as_str);
            }
        }
        let value = normalized.unwrap_or(text);
        renamed |= normalized.is_some();
        write!(out, "{}:{value};", value.len()).unwrap();
        tokens += 1;
    }
    (out, tokens, statements, renamed)
}
// Only binding targets are traversed; ordinary property writes do not rebind the root.
fn for_each_binding_name<'source>(
    n: Node<'_>,
    source: &'source str,
    mut visit: impl FnMut(&'source str),
) {
    if n.kind() == "foreach_statement" {
        for target in named_children(n)
            .into_iter()
            .filter(|c| child(n, "body").is_none_or(|b| b.id() != c.id()))
            .skip(1)
        {
            visit_binding_target(target, source, &mut visit);
        }
        return;
    }
    let target = match n.kind() {
        "assignment"
        | "assignment_expression"
        | "augmented_assignment"
        | "augmented_assignment_expression"
        | "compound_assignment_expr"
        | "named_expression"
        | "variable_declarator"
        | "let_declaration"
        | "let_condition"
        | "for_statement"
        | "for_in_statement"
        | "for_in_clause"
        | "for_expression"
        | "match_arm" => child(n, "left")
            .or_else(|| child(n, "name"))
            .or_else(|| child(n, "pattern")),
        "update_expression" => child(n, "argument"),
        "catch_clause" => child(n, "parameter").or_else(|| child(n, "name")),
        "as_pattern" | "except_clause" => child(n, "alias"),
        "function_definition"
        | "function_declaration"
        | "class_definition"
        | "class_declaration" => child(n, "name"),
        "capture_pattern" | "case_pattern" | "import_statement" | "import_from_statement" => {
            Some(n)
        }
        _ => None,
    };
    let Some(target) = target else {
        return;
    };
    visit_binding_target(target, source, &mut visit);
}
fn visit_binding_target<'source>(
    target: Node<'_>,
    source: &'source str,
    visit: &mut impl FnMut(&'source str),
) {
    if matches!(target.kind(), "identifier" | "variable_name" | "self") {
        visit(snippet(target, source).trim_start_matches('$'));
        return;
    }
    let mut stack = vec![target];
    while let Some(n) = stack.pop() {
        if matches!(n.kind(), "identifier" | "variable_name" | "self") {
            visit(snippet(n, source).trim_start_matches('$'));
        } else if !matches!(
            n.kind(),
            "attribute"
                | "member_expression"
                | "field_expression"
                | "subscript"
                | "subscript_expression"
                | "member_access_expression"
        ) {
            stack.extend(named_children(n));
        }
    }
}
fn record_shadow(n: Node<'_>, source: &str, dialect: Dialect, facts: &mut FileFacts) -> bool {
    if dialect == Dialect::Python {
        let mut isinstance_shadowed = false;
        for_each_binding_name(n, source, |name| {
            facts.unsupported_operation_shadowed |= name == "NotImplementedError";
            isinstance_shadowed |= name == "isinstance";
        });
        return isinstance_shadowed;
    }
    if dialect == Dialect::Rust
        && n.kind() == "macro_definition"
        && child(n, "name").is_some_and(|name| snippet(name, source) == "unimplemented")
    {
        facts.unsupported_operation_shadowed = true;
    }
    false
}
fn all_children(n: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = n.walk();
    n.children(&mut cursor).collect()
}
