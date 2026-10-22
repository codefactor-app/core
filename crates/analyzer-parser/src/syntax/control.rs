use super::*;
pub(crate) fn make_guard(kind: &str, raw: &str, branch: &str, range: SourceRange) -> Guard {
    Guard {
        kind: kind.into(),
        raw: raw.into(),
        normalized: normalize_condition(raw),
        branch: branch.into(),
        range,
    }
}

pub(crate) fn contains_terminator(mut node: Node<'_>) -> bool {
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

pub(crate) fn find_unconditional_try(node: Node<'_>) -> Option<Node<'_>> {
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

pub(crate) fn find_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        if node.kind() == kind {
            return Some(node);
        }
        pending.extend(named_children(node).into_iter().rev());
    }
    None
}

pub(crate) fn negate_condition(raw: &str) -> String {
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

pub(crate) fn single_comparison(condition: &str) -> Option<(usize, &'static str)> {
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

pub(crate) fn word_at(source: &str, index: usize, word: &str) -> bool {
    let bytes = source.as_bytes();
    source[index..].starts_with(word)
        && (index == 0 || !bytes[index - 1].is_ascii_alphanumeric() && bytes[index - 1] != b'_')
        && (index + word.len() == bytes.len()
            || !bytes[index + word.len()].is_ascii_alphanumeric()
                && bytes[index + word.len()] != b'_')
}

pub(crate) fn strip_parens(mut s: &str) -> &str {
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

pub(crate) fn is_logical_binary(node: Node<'_>, source: &str) -> bool {
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

pub(crate) fn loop_header(node: Node<'_>, src: &str) -> String {
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
