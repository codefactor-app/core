use super::*;
pub(crate) fn signature(node: Node<'_>, body: Option<Node<'_>>, src: &str) -> String {
    let raw = if let Some(b) = body {
        src.get(node.start_byte()..b.start_byte()).unwrap_or("")
    } else {
        snippet(node, src)
    };
    raw.trim().chars().take(300).collect()
}

pub(crate) fn is_abstract_type(node: Node<'_>, kind: &TypeKind) -> bool {
    matches!(kind, TypeKind::Trait | TypeKind::Interface)
        || node.kind() == "abstract_class_declaration"
        || named_children(node)
            .iter()
            .any(|child| child.kind() == "abstract_modifier")
}

/// Name of a constructor parameter that also declares a property: PHP
/// constructor promotion or a TypeScript parameter property (accessibility,
/// `override` or `readonly` modifier, possibly after decorators).
pub(crate) fn promoted_parameter_name(p: Node<'_>, source: &str) -> Option<String> {
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
pub(crate) fn php_readonly_promotion_type(p: Node<'_>, source: &str) -> Option<String> {
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

pub(crate) fn visibility(raw: &str) -> Option<String> {
    for v in ["private", "protected", "public"] {
        if raw.trim_start().starts_with(v) {
            return Some(v.into());
        }
    }
    None
}

pub(crate) fn decorators(node: Node<'_>, source: &str) -> Vec<String> {
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

pub(crate) fn modifiers(node: Node<'_>, src: &str) -> Vec<String> {
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

pub(crate) fn accessor_keyword(node: Node<'_>) -> Option<&'static str> {
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

pub(crate) fn rust_attributes<'a>(node: Node<'_>, source: &'a str) -> Vec<&'a str> {
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

pub(crate) fn rust_conditions(node: Node<'_>, source: &str) -> Vec<String> {
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

pub(crate) fn rust_module_path(node: Node<'_>, source: &str) -> Option<String> {
    rust_attributes(node, source)
        .into_iter()
        .find_map(|attribute| {
            let (key, value) = attribute.split_once('=')?;
            (key.trim() == "path").then(|| rust_path_literal(value.trim()))?
        })
}

pub(crate) fn rust_path_literal(raw: &str) -> Option<String> {
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

pub(crate) fn rust_is_public(node: Node<'_>, source: &str) -> bool {
    named_children(node).into_iter().any(|child| {
        child.kind() == "visibility_modifier" && snippet(child, source).trim() == "pub"
    })
}

pub(crate) fn rust_path_in_module(reference: &str, module: &str, base: &str) -> String {
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
pub(crate) fn cognitive_complexity(body: Node<'_>, source: &str, dialect: Dialect) -> usize {
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

pub(crate) fn boolean_operator<'a>(node: Node<'_>, source: &'a str) -> Option<&'a str> {
    let operator = snippet(child(node, "operator")?, source);
    matches!(operator, "&&" | "||" | "and" | "or" | "xor").then_some(operator)
}

pub(crate) fn heritage_types(node: Node<'_>, source: &str, dialect: Dialect) -> Vec<String> {
    named_children(node)
        .into_iter()
        .filter(|n| n.kind() != "type_arguments")
        .filter_map(|n| crate::types::reference(language_name(dialect), snippet(n, source)))
        .collect()
}

pub(crate) fn trim_type(raw: &str) -> String {
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

pub(crate) fn rust_trait_bounds(node: Node<'_>, source: &str) -> BTreeMap<String, String> {
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

pub(crate) fn parameter_type(raw: &str, d: Dialect) -> Option<String> {
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

pub(crate) fn apply_phpdoc(node: Node<'_>, source: &str, function: &mut FunctionModel) {
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

pub(crate) fn phpdoc_var(node: Node<'_>, source: &str) -> Option<String> {
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
