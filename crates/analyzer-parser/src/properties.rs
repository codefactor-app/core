//! JS/TS property operations, kept separate from syntactically explicit calls.
use super::*;

pub(super) fn literal_key(node: Node<'_>, source: &str) -> Option<String> {
    if node.kind() != "string" {
        return None;
    }
    static_js::string(node, source)
}

pub(super) fn member(node: Node<'_>, source: &str) -> Option<(String, String)> {
    let object = child(node, "object")?;
    let property = match node.kind() {
        "member_expression" => snippet(child(node, "property")?, source).to_owned(),
        "subscript_expression" => static_js::string(child(node, "index")?, source)?,
        _ => return None,
    };
    Some((expression_path(object, source), property))
}

pub(super) fn declared_name(node: Node<'_>, source: &str) -> Option<String> {
    let name = child(node, "name").or_else(|| child(node, "property"))?;
    if name.kind() == "string" {
        literal_key(name, source)
    } else if name.kind() == "computed_property_name" {
        named_children(name)
            .first()
            .and_then(|node| static_js::string(*node, source))
    } else {
        None
    }
}

pub(super) fn logical_assignment_guard(
    parent: Node<'_>,
    node: Node<'_>,
    source: &str,
    path: &Path,
) -> Option<Guard> {
    if parent.kind() != "augmented_assignment_expression"
        || child(parent, "right").is_none_or(|right| right.id() != node.id())
    {
        return None;
    }
    let raw = snippet(child(parent, "left")?, source);
    let normalized = match operator(parent, source) {
        "&&=" => normalize_condition(raw),
        "||=" => negate_condition(raw),
        "??=" => format!("{} == null", normalize_condition(raw)),
        _ => return None,
    };
    let mut guard = make_guard("ShortCircuit", raw, "assign", range(parent, path));
    guard.normalized = normalized;
    Some(guard)
}

fn expression_path(node: Node<'_>, source: &str) -> String {
    if let Some((receiver, name)) = member(node, source) {
        format!("{receiver}.{name}")
    } else if matches!(
        node.kind(),
        "parenthesized_expression" | "non_null_expression"
    ) {
        named_children(node)
            .first()
            .map(|node| expression_path(*node, source))
            .unwrap_or_else(|| snippet(node, source).to_owned())
    } else {
        snippet(node, source).to_owned()
    }
}

pub(super) fn mutations(
    node: Node<'_>,
    source: &str,
    path: &Path,
    guards: &[Guard],
    loops: &[LoopContext],
) -> Vec<PropertyMutation> {
    let mut out = Vec::new();
    if node.kind() == "unary_expression"
        && snippet(node, source).trim_start().starts_with("delete ")
    {
        if let Some(value) = child(node, "argument") {
            if let Some((receiver, name)) = member(value, source) {
                out.push(mutation(node, receiver, name, source, path, guards, loops));
            } else if value.kind() == "subscript_expression" {
                if let Some(object) = child(value, "object") {
                    out.push(mutation(
                        node,
                        expression_path(object, source),
                        "*".into(),
                        source,
                        path,
                        guards,
                        loops,
                    ));
                }
            }
        }
        return out;
    }
    if node.kind() != "call_expression" {
        return out;
    }
    let Some(callee) = child(node, "function") else {
        return out;
    };
    let Some((receiver, operation)) = member(callee, source) else {
        return out;
    };
    if !matches!(receiver.as_str(), "Object" | "Reflect") {
        return out;
    }
    let intrinsic = receiver == "Object" && static_js::unshadowed(node, "Object", source);
    let arguments = child(node, "arguments")
        .map(named_children)
        .unwrap_or_default();
    if arguments.len() < 2 {
        return out;
    }
    let target = expression_path(arguments[0], source);
    match operation.as_str() {
        "defineProperty" | "deleteProperty" => {
            let key = static_js::string(arguments[1], source);
            let mut change = mutation(
                node,
                target,
                key.clone().unwrap_or_else(|| "*".into()),
                source,
                path,
                guards,
                loops,
            );
            if let (true, Some(_), Some(descriptor)) = (
                intrinsic && operation == "defineProperty",
                key,
                arguments.get(2),
            ) {
                descriptor_fields(*descriptor, source, path, &mut change);
            }
            out.push(change);
        }
        "defineProperties" => {
            if arguments[1].kind() == "object" {
                for property in named_children(arguments[1]) {
                    let key = child(property, "key").and_then(|n| object_key(n, source));
                    if let Some(key) = &key {
                        // The descriptor map is an object literal: duplicate
                        // own keys replace earlier values before the API runs.
                        out.retain(|change| change.site.name != *key);
                    }
                    let mut change = mutation(
                        node,
                        target.clone(),
                        key.clone().unwrap_or_else(|| "*".into()),
                        source,
                        path,
                        guards,
                        loops,
                    );
                    if let (true, Some(_), Some(value)) = (intrinsic, key, child(property, "value"))
                    {
                        descriptor_fields(value, source, path, &mut change);
                    }
                    out.push(change);
                }
            } else {
                out.push(mutation(
                    node,
                    target,
                    "*".into(),
                    source,
                    path,
                    guards,
                    loops,
                ));
            }
        }
        "setPrototypeOf" => out.push(mutation(
            node,
            target,
            "*".into(),
            source,
            path,
            guards,
            loops,
        )),
        _ => {}
    }
    out
}

pub(super) fn descriptor_function_name(node: Node<'_>, source: &str) -> Option<String> {
    let parent = node.parent()?;
    let key = if node.kind() == "method_definition" {
        child(node, "name")
    } else {
        child(parent, "key")
    }?;
    let key = object_key(key, source)?;
    if !matches!(key.as_str(), "get" | "set") {
        return None;
    }
    let mut ancestor = node.parent();
    while let Some(at) = ancestor {
        if is_function(at.kind(), Dialect::TypeScript) || is_type(at.kind(), Dialect::TypeScript) {
            return None;
        }
        if at.kind() == "call_expression" {
            let (receiver, operation) = member(child(at, "function")?, source)?;
            return (receiver == "Object"
                && matches!(operation.as_str(), "defineProperty" | "defineProperties")
                && static_js::unshadowed(at, "Object", source))
            .then(|| {
                format!(
                    "{key}#descriptor_{}_{}",
                    node.start_position().row + 1,
                    node.start_position().column + 1
                )
            });
        }
        ancestor = at.parent();
    }
    None
}

fn mutation(
    node: Node<'_>,
    mut receiver: String,
    name: String,
    source: &str,
    path: &Path,
    guards: &[Guard],
    loops: &[LoopContext],
) -> PropertyMutation {
    let prototype = receiver.ends_with(".prototype");
    if prototype {
        receiver.truncate(receiver.len() - ".prototype".len());
    }
    PropertyMutation {
        site: site(
            node,
            receiver,
            name,
            AccessorKind::Getter,
            source,
            path,
            guards,
            loops,
        )
        .site,
        prototype,
        unknown: true,
        ..PropertyMutation::default()
    }
}

fn object_key(node: Node<'_>, source: &str) -> Option<String> {
    match node.kind() {
        "property_identifier" | "identifier" => Some(snippet(node, source).into()),
        "string" => literal_key(node, source),
        "computed_property_name" => named_children(node)
            .first()
            .and_then(|n| static_js::string(*n, source)),
        _ => None,
    }
}

fn descriptor_fields(node: Node<'_>, source: &str, path: &Path, change: &mut PropertyMutation) {
    if node.kind() != "object" {
        return;
    }
    let mut keys = BTreeSet::new();
    let mut data = false;
    for property in named_children(node) {
        let Some(key) = child(property, "key")
            .or_else(|| child(property, "name"))
            .and_then(|n| object_key(n, source))
        else {
            return;
        };
        if !keys.insert(key.clone()) {
            return;
        }
        let value = child(property, "value");
        match key.as_str() {
            "get" | "set" => {
                let getter = key == "get";
                let function = if property.kind() == "method_definition" {
                    Some(property)
                } else {
                    value.filter(|n| is_function(n.kind(), Dialect::TypeScript))
                };
                let absent = value.is_some_and(|n| {
                    n.kind() == "identifier"
                        && snippet(n, source) == "undefined"
                        && static_js::unshadowed(n, "undefined", source)
                });
                if function.is_none() && !absent {
                    return;
                }
                if getter {
                    change.getter_present = true;
                    change.getter = function.map(|n| range(n, path));
                } else {
                    change.setter_present = true;
                    change.setter = function.map(|n| range(n, path));
                }
            }
            "value" | "writable" => data = true,
            "enumerable" | "configurable" => {
                // Reading a literal descriptor must not invoke arbitrary getters.
                if value.is_none_or(|n| !matches!(n.kind(), "true" | "false")) {
                    return;
                }
            }
            _ => return,
        }
    }
    if data && (change.getter_present || change.setter_present) {
        return;
    }
    if !data && !change.getter_present && !change.setter_present {
        return;
    }
    change.data = data;
    change.unknown = false;
}

fn operator<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    if let Some(op) = child(node, "operator") {
        return snippet(op, source);
    }
    let mut cursor = node.walk();
    let value = node
        .children(&mut cursor)
        .find(|node| !node.is_named() && node.kind().ends_with('='))
        .map(|node| snippet(node, source))
        .unwrap_or("=");
    value
}

fn site(
    node: Node<'_>,
    receiver: String,
    name: String,
    access: AccessorKind,
    source: &str,
    path: &Path,
    guards: &[Guard],
    loops: &[LoopContext],
) -> PropertyAccess {
    let mut guard = guards.to_vec();
    guard.extend(presence_guards(node, source, path));
    let assignment_to = (access == AccessorKind::Getter)
        .then(|| assigned_name(node, source))
        .flatten();
    PropertyAccess {
        access,
        site: CallSite {
            name,
            receiver: Some(receiver),
            kind: CallKind::Method,
            range: range(node, path),
            guard: guard.into(),
            in_loop: loops.to_vec().into(),
            assignment_to,
            ..CallSite::default()
        },
    }
}

pub(super) fn presence_guards(node: Node<'_>, source: &str, path: &Path) -> Vec<Guard> {
    let mut guard = Vec::new();
    // Optional chains suppress this operation and every later property operation
    // until a parenthesized expression ends the chain.
    let mut chain = Some(node);
    while let Some(at) = chain {
        if matches!(at.kind(), "member_expression" | "subscript_expression") {
            if child(at, "optional_chain").is_some()
                || named_children(at)
                    .iter()
                    .any(|n| n.kind() == "optional_chain")
            {
                if let Some(object) = child(at, "object") {
                    let raw = snippet(object, source);
                    let mut condition = make_guard("ShortCircuit", raw, "present", range(at, path));
                    condition.normalized = format!("{} != null", normalize_condition(raw));
                    guard.push(condition);
                }
            }
            chain = child(at, "object");
        } else {
            break;
        }
    }
    guard
}

pub(super) fn collect(
    node: Node<'_>,
    source: &str,
    path: &Path,
    guards: &[Guard],
    loops: &[LoopContext],
) -> Vec<PropertyAccess> {
    let mut out = Vec::new();
    // Ordinary scalar expressions cannot read/write a property. Avoid parent
    // traversal and field lookups unless this node can produce an operation.
    if !matches!(
        node.kind(),
        "member_expression"
            | "subscript_expression"
            | "variable_declarator"
            | "assignment_expression"
    ) {
        return out;
    }
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        if matches!(
            parent.kind(),
            "type_query" | "type_annotation" | "type_alias_declaration" | "interface_declaration"
        ) {
            return out;
        }
        if is_function(parent.kind(), Dialect::TypeScript) {
            break;
        }
        ancestor = parent.parent();
    }
    if let Some((receiver, name)) = member(node, source) {
        let mut context = node;
        while let Some(parent) = context.parent().filter(|parent| {
            matches!(
                parent.kind(),
                "parenthesized_expression"
                    | "non_null_expression"
                    | "as_expression"
                    | "satisfies_expression"
            )
        }) {
            context = parent;
        }
        let parent = context.parent();
        // The explicit call already reads a callable getter; avoid a duplicate edge.
        if parent.is_some_and(|parent| {
            parent.kind() == "call_expression"
                && child(parent, "function").is_some_and(|callee| callee.id() == context.id())
        }) {
            return out;
        }
        if parent.is_some_and(|parent| {
            parent.kind() == "unary_expression"
                && snippet(parent, source).trim_start().starts_with("delete ")
        }) {
            return out;
        }
        let assignment = parent.filter(|parent| {
            matches!(
                parent.kind(),
                "assignment_expression" | "augmented_assignment_expression" | "for_in_statement"
            ) && child(*parent, "left").is_some_and(|left| left.id() == context.id())
        });
        let update = parent.is_some_and(|parent| parent.kind() == "update_expression");
        let op = assignment.map(|node| operator(node, source));
        let write = assignment.is_some() || update || destructuring_target(context);
        let read = !write || update || op.is_some_and(|op| !matches!(op, "=" | "in" | "of"));
        if read {
            out.push(site(
                node,
                receiver.clone(),
                name.clone(),
                AccessorKind::Getter,
                source,
                path,
                guards,
                loops,
            ));
        }
        if write {
            let mut access = site(
                node,
                receiver,
                name,
                AccessorKind::Setter,
                source,
                path,
                guards,
                loops,
            );
            if let Some(assignment) = assignment {
                // Compound and iteration writes do not pass their raw RHS as
                // the setter value; keep an unknown argument until evaluated.
                if op.is_some_and(|op| matches!(op, "=" | "&&=" | "||=" | "??=")) {
                    access.site.arguments = child(assignment, "right")
                        .map(|value| vec![snippet(value, source).to_owned()])
                        .unwrap_or_default();
                }
                if let Some(op @ ("&&=" | "||=" | "??=")) = op {
                    let raw = snippet(node, source);
                    let mut condition =
                        make_guard("ShortCircuit", raw, "assign", range(assignment, path));
                    condition.normalized = match op {
                        "&&=" => normalize_condition(raw),
                        "||=" => negate_condition(raw),
                        _ => format!("{} == null", normalize_condition(raw)),
                    };
                    access.site.guard = access
                        .site
                        .guard
                        .iter()
                        .cloned()
                        .chain([condition])
                        .collect::<Vec<_>>()
                        .into();
                }
            }
            out.push(access);
        }
    }
    // Destructuring performs property reads even though the source code has no
    // member-expression node for them. Bind each read at its actual pattern node.
    if matches!(node.kind(), "variable_declarator" | "assignment_expression") {
        if let Some(pattern) = child(node, "name").or_else(|| child(node, "left")) {
            if pattern.kind() == "object_pattern" {
                if let Some(value) = child(node, "value").or_else(|| child(node, "right")) {
                    destructuring_reads(
                        pattern,
                        &expression_path(value, source),
                        source,
                        path,
                        guards,
                        loops,
                        &mut out,
                    );
                }
            }
        }
    }
    out
}

fn destructuring_target(mut node: Node<'_>) -> bool {
    while let Some(parent) = node.parent() {
        if matches!(
            parent.kind(),
            "object_pattern"
                | "array_pattern"
                | "pair_pattern"
                | "assignment_pattern"
                | "rest_pattern"
        ) {
            node = parent;
        } else {
            return matches!(parent.kind(), "assignment_expression" | "for_in_statement")
                && child(parent, "left").is_some_and(|left| left.id() == node.id());
        }
    }
    false
}

fn destructuring_reads(
    pattern: Node<'_>,
    receiver: &str,
    source: &str,
    path: &Path,
    guards: &[Guard],
    loops: &[LoopContext],
    out: &mut Vec<PropertyAccess>,
) {
    for property in named_children(pattern) {
        let (key, nested) = match property.kind() {
            "shorthand_property_identifier_pattern" => {
                (Some(snippet(property, source).to_owned()), None)
            }
            "object_assignment_pattern" => (
                child(property, "left").map(|node| snippet(node, source).to_owned()),
                None,
            ),
            "pair_pattern" => {
                let key = child(property, "key").and_then(|key| {
                    if key.kind() == "string" {
                        literal_key(key, source)
                    } else if key.kind() == "property_identifier" {
                        Some(snippet(key, source).to_owned())
                    } else if key.kind() == "computed_property_name" {
                        named_children(key)
                            .first()
                            .and_then(|node| static_js::string(*node, source))
                    } else {
                        None
                    }
                });
                (key, child(property, "value"))
            }
            _ => continue,
        };
        let Some(key) = key else {
            continue;
        };
        out.push(site(
            property,
            receiver.to_owned(),
            key.clone(),
            AccessorKind::Getter,
            source,
            path,
            guards,
            loops,
        ));
        if let Some(nested) = nested.filter(|node| node.kind() == "object_pattern") {
            destructuring_reads(
                nested,
                &format!("{receiver}.{key}"),
                source,
                path,
                guards,
                loops,
                out,
            );
        }
    }
}
