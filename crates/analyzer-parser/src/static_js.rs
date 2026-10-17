//! Bounded lexical string proofs. This never invokes JavaScript or user code.
use super::*;

const MAX_DEPTH: usize = 16;
const MAX_NODES: usize = 128;
const MAX_STRING: usize = 4096;

pub(super) fn string(node: Node<'_>, source: &str) -> Option<String> {
    let mut budget = MAX_NODES;
    eval(node, source, 0, &mut budget)
}

pub(super) fn symbol_constant_assignment(node: Node<'_>, source: &str) -> bool {
    if node.kind() != "call_expression" {
        return false;
    }
    let Some(callee) = child(node, "function") else {
        return false;
    };
    if !matches!(snippet(callee, source), "Symbol" | "Symbol.for")
        || !unshadowed(node, "Symbol", source)
    {
        return false;
    }
    let mut value = node;
    while let Some(parent) = value.parent().filter(|parent| {
        matches!(
            parent.kind(),
            "parenthesized_expression"
                | "as_expression"
                | "satisfies_expression"
                | "non_null_expression"
        )
    }) {
        value = parent;
    }
    let Some(declarator) = value.parent().filter(|parent| {
        parent.kind() == "variable_declarator"
            && child(*parent, "value").is_some_and(|child| child.id() == value.id())
    }) else {
        return false;
    };
    let Some(declaration) = declarator.parent() else {
        return false;
    };
    child(declarator, "name").is_some_and(|name| name.kind() == "identifier")
        && declaration.kind() == "lexical_declaration"
        && (child(declaration, "kind").is_some_and(|kind| snippet(kind, source) == "const")
            || snippet(declaration, source)
                .trim_start()
                .starts_with("const "))
}

pub(super) fn symbol_key_reference(node: Node<'_>, source: &str) -> Option<String> {
    if node.kind() != "identifier" {
        return None;
    }
    let name = snippet(node, source);
    let binding = declaration(node, name, source)??;
    if binding.kind() == "import_statement" {
        return Some(name.into());
    }
    let mut value = child(binding, "value")?;
    while matches!(
        value.kind(),
        "parenthesized_expression"
            | "as_expression"
            | "satisfies_expression"
            | "non_null_expression"
    ) {
        value = *named_children(value).first()?;
    }
    symbol_constant_assignment(value, source).then(|| {
        let start = value.start_position();
        format!("#symbol:{}:{}", start.row + 1, start.column + 1)
    })
}

/// Only module declarations can supply an imported Symbol constant. A block
/// const with the same spelling must not become proof for a module export.
pub(super) fn symbol_module_constant_assignment(node: Node<'_>, source: &str) -> bool {
    if !symbol_constant_assignment(node, source) {
        return false;
    }
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        if parent.kind() == "lexical_declaration" {
            return parent.parent().is_some_and(|scope| {
                scope.kind() == "program"
                    || scope.kind() == "export_statement"
                        && scope
                            .parent()
                            .is_some_and(|scope| scope.kind() == "program")
            });
        }
        ancestor = parent.parent();
    }
    false
}

fn eval(node: Node<'_>, source: &str, depth: usize, budget: &mut usize) -> Option<String> {
    if depth >= MAX_DEPTH || *budget == 0 {
        return None;
    }
    *budget -= 1;
    let value = match node.kind() {
        "string" => {
            let raw = snippet(node, source);
            let quote = raw.chars().next()?;
            if !matches!(quote, '\'' | '"') || !raw.ends_with(quote) {
                return None;
            }
            decode(&raw[1..raw.len() - 1])?
        }
        "parenthesized_expression"
        | "as_expression"
        | "satisfies_expression"
        | "non_null_expression" => eval(*named_children(node).first()?, source, depth + 1, budget)?,
        "identifier" => {
            let declaration = declaration(node, snippet(node, source), source)??;
            let value = child(declaration, "value")?;
            eval(value, source, depth + 1, budget)?
        }
        "binary_expression"
            if child(node, "operator").is_some_and(|op| snippet(op, source) == "+") =>
        {
            let mut left = eval(child(node, "left")?, source, depth + 1, budget)?;
            left.push_str(&eval(child(node, "right")?, source, depth + 1, budget)?);
            left
        }
        "template_string" => {
            let mut value = String::new();
            for part in named_children(node) {
                if part.kind() == "template_substitution" {
                    value.push_str(&eval(
                        *named_children(part).first()?,
                        source,
                        depth + 1,
                        budget,
                    )?);
                } else if matches!(part.kind(), "string_fragment" | "escape_sequence") {
                    value.push_str(&decode(snippet(part, source))?);
                } else {
                    return None;
                }
                if value.len() > MAX_STRING {
                    return None;
                }
            }
            value
        }
        _ => return None,
    };
    (value.len() <= MAX_STRING).then_some(value)
}

/// None means no binding; Some(None) is a binding without a const string proof.
fn declaration<'a>(at: Node<'a>, name: &str, source: &str) -> Option<Option<Node<'a>>> {
    let mut parent = at.parent();
    while let Some(scope) = parent {
        if is_function(scope.kind(), Dialect::TypeScript) {
            if child(scope, "parameters").is_some_and(|params| binds(params, name, source))
                || child(scope, "parameter").is_some_and(|param| binds(param, name, source))
                || child(scope, "name").is_some_and(|n| snippet(n, source) == name)
            {
                return Some(None);
            }
        }
        if scope.kind() == "catch_clause"
            && child(scope, "parameter").is_some_and(|n| binds(n, name, source))
        {
            return Some(None);
        }
        if matches!(scope.kind(), "for_statement" | "for_in_statement") {
            if child(scope, "left")
                .or_else(|| child(scope, "initializer"))
                .is_some_and(|n| binds(n, name, source))
            {
                return Some(None);
            }
        }
        if matches!(scope.kind(), "program" | "statement_block" | "switch_body") {
            for mut statement in named_children(scope) {
                if statement.kind() == "export_statement" {
                    if let Some(declaration) = child(statement, "declaration") {
                        statement = declaration;
                    }
                }
                if matches!(
                    statement.kind(),
                    "lexical_declaration" | "variable_declaration"
                ) {
                    for declaration in named_children(statement) {
                        if child(declaration, "name").is_some_and(|n| binds(n, name, source)) {
                            let is_const = child(statement, "kind")
                                .is_some_and(|n| snippet(n, source) == "const")
                                || snippet(statement, source)
                                    .trim_start()
                                    .starts_with("const ");
                            return Some(
                                (is_const && declaration.end_byte() <= at.start_byte())
                                    .then_some(declaration),
                            );
                        }
                    }
                }
                if matches!(
                    statement.kind(),
                    "function_declaration" | "generator_function_declaration" | "class_declaration"
                ) && child(statement, "name").is_some_and(|n| snippet(n, source) == name)
                {
                    return Some(None);
                }
                if statement.kind() == "import_statement"
                    && named_children(statement)
                        .into_iter()
                        .filter(|n| n.kind() == "import_clause")
                        .any(|n| binds(n, name, source))
                {
                    return Some(Some(statement));
                }
            }
            // Function-scoped var declarations can be nested in blocks.
            if scope.kind() == "program"
                || scope
                    .parent()
                    .is_some_and(|n| is_function(n.kind(), Dialect::TypeScript))
            {
                let mut pending = named_children(scope);
                let mut remaining = MAX_NODES;
                while let Some(node) = pending.pop() {
                    if remaining == 0 {
                        return Some(None);
                    }
                    remaining -= 1;
                    if is_function(node.kind(), Dialect::TypeScript)
                        || is_type(node.kind(), Dialect::TypeScript)
                    {
                        continue;
                    }
                    if node.kind() == "variable_declaration"
                        && named_children(node)
                            .into_iter()
                            .any(|n| child(n, "name").is_some_and(|n| binds(n, name, source)))
                    {
                        return Some(None);
                    }
                    pending.extend(named_children(node));
                }
            }
        }
        parent = scope.parent();
    }
    None
}

fn wrapper_expression(node: Node<'_>) -> Option<Node<'_>> {
    if !matches!(
        node.kind(),
        "parenthesized_expression"
            | "as_expression"
            | "satisfies_expression"
            | "non_null_expression"
            | "type_assertion"
    ) {
        return None;
    }
    // Angle casts put type_arguments before their expression. Comments are
    // named grammar extras and cannot be used as the runtime expression.
    named_children(node)
        .into_iter()
        .find(|child| !matches!(child.kind(), "comment" | "type_arguments"))
}

fn intrinsic_target(node: Node<'_>, name: &str, source: &str, depth: usize) -> bool {
    if depth >= MAX_DEPTH {
        return true;
    }
    if let Some(expression) = wrapper_expression(node) {
        return intrinsic_target(expression, name, source, depth + 1);
    }
    if node.kind() == "identifier" {
        return snippet(node, source) == name;
    }
    if matches!(node.kind(), "member_expression" | "subscript_expression") {
        let Some(object) = child(node, "object") else {
            return false;
        };
        if intrinsic_target(object, name, source, depth + 1) {
            return true;
        }
        let mut root = object;
        while let Some(inner) = wrapper_expression(root) {
            root = inner;
        }
        if root.kind() == "identifier" && snippet(root, source) == "globalThis" {
            return if node.kind() == "member_expression" {
                child(node, "property").is_some_and(|property| snippet(property, source) == name)
            } else {
                child(node, "index")
                    .and_then(|index| string(index, source))
                    .is_none_or(|property| property == name)
            };
        }
    }
    false
}

pub(super) fn unshadowed(at: Node<'_>, name: &str, source: &str) -> bool {
    if declaration(at, name, source).is_some() {
        return false;
    }
    let mut ancestor = at.parent();
    while let Some(scope) = ancestor {
        if scope.kind() == "program" || is_function(scope.kind(), Dialect::TypeScript) {
            let mut pending = child(scope, "body")
                .map(named_children)
                .unwrap_or_else(|| named_children(scope));
            let mut budget = MAX_NODES;
            while let Some(node) = pending.pop() {
                if budget == 0 {
                    return false;
                }
                budget -= 1;
                if is_function(node.kind(), Dialect::TypeScript)
                    || is_type(node.kind(), Dialect::TypeScript)
                {
                    continue;
                }
                if matches!(
                    node.kind(),
                    "assignment_expression"
                        | "augmented_assignment_expression"
                        | "update_expression"
                ) {
                    let left = child(node, "left").or_else(|| child(node, "argument"));
                    if left.is_some_and(|left| intrinsic_target(left, name, source, 0)) {
                        return false;
                    }
                }
                if node.kind() == "call_expression"
                    && child(node, "arguments")
                        .and_then(|arguments| {
                            named_children(arguments)
                                .into_iter()
                                .find(|argument| argument.kind() != "comment")
                        })
                        .is_some_and(|argument| intrinsic_target(argument, name, source, 0))
                {
                    return false;
                }
                pending.extend(named_children(node));
            }
        }
        ancestor = scope.parent();
    }
    true
}

fn binds(node: Node<'_>, name: &str, source: &str) -> bool {
    if matches!(
        node.kind(),
        "identifier" | "shorthand_property_identifier_pattern"
    ) {
        return snippet(node, source) == name;
    }
    if matches!(node.kind(), "pair_pattern" | "assignment_pattern") {
        return child(node, "value")
            .or_else(|| child(node, "left"))
            .is_some_and(|n| binds(n, name, source));
    }
    named_children(node)
        .into_iter()
        .any(|n| binds(n, name, source))
}

fn decode(raw: &str) -> Option<String> {
    let mut chars = raw.chars().peekable();
    let mut units = Vec::new();
    while let Some(c) = chars.next() {
        if c != '\\' {
            units.extend(c.encode_utf16(&mut [0; 2]).iter().copied());
            continue;
        }
        let escaped = chars.next()?;
        let value = match escaped {
            '\'' => 39,
            '"' => 34,
            '\\' => 92,
            '`' => 96,
            '$' => 36,
            'b' => 8,
            'f' => 12,
            'n' => 10,
            'r' => 13,
            't' => 9,
            'v' => 11,
            '0' if chars.peek().is_none_or(|c| !c.is_ascii_digit()) => 0,
            '\n' => continue,
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                continue;
            }
            'x' => hex(&mut chars, 2)?,
            'u' if chars.peek() == Some(&'{') => {
                chars.next();
                let mut digits = String::new();
                loop {
                    let c = chars.next()?;
                    if c == '}' {
                        break;
                    }
                    if digits.len() >= 6 || !c.is_ascii_hexdigit() {
                        return None;
                    }
                    digits.push(c);
                }
                let c = char::from_u32(u32::from_str_radix(&digits, 16).ok()?)?;
                units.extend(c.encode_utf16(&mut [0; 2]).iter().copied());
                continue;
            }
            'u' => hex(&mut chars, 4)?,
            _ => return None,
        };
        units.push(value);
        if units.len() > MAX_STRING {
            return None;
        }
    }
    String::from_utf16(&units).ok()
}

fn hex(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, count: usize) -> Option<u16> {
    let mut value = 0;
    for _ in 0..count {
        value = value * 16 + chars.next()?.to_digit(16)? as u16;
    }
    Some(value)
}
