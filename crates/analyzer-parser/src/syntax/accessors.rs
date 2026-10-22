use super::*;
pub(crate) fn collect_accessed_fields(
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
pub(crate) fn receiver_names(f: &FunctionModel, dialect: Dialect) -> Vec<&str> {
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
pub(crate) fn classify_accessor(
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

pub(crate) fn direct_receiver_field(
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
