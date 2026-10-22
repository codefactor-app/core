//! Shared syntax/source primitives. No dependency on parser orchestration.
use analyzer_core::ir::*;
use std::cell::RefCell;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tree_sitter::Node;
thread_local! {
    static RANGE_PATH: RefCell<Option<(PathBuf, Arc<str>)>> = const { RefCell::new(None) };
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dialect {
    Python,
    JavaScript,
    TypeScript,
    Php,
    Rust,
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

pub(crate) fn module_name(path: &Path, dialect: Dialect) -> String {
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

pub(crate) fn range_path(path: &Path) -> Arc<str> {
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

pub(crate) fn range(node: Node<'_>, path: &Path) -> SourceRange {
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

pub(crate) fn snippet<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    source.get(node.byte_range()).unwrap_or("")
}

pub(crate) fn child<'a>(node: Node<'a>, field: &str) -> Option<Node<'a>> {
    node.child_by_field_name(field)
}

pub(crate) fn named_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut c = node.walk();
    node.named_children(&mut c).collect()
}

pub(crate) fn first_kind<'a>(node: Node<'a>, kinds: &[&str]) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .find(|n| kinds.contains(&n.kind()));
    found
}

pub(crate) fn text_field(node: Node<'_>, field: &str, source: &str) -> Option<String> {
    child(node, field).map(|n| snippet(n, source).to_string())
}

pub(crate) fn js_module_literal(raw: &str) -> Option<String> {
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

pub(crate) fn ast_kind(kind: &str, dialect: Dialect) -> AstKind {
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

pub(crate) const MAX_RECURSIVE_DEPTH: usize = 256;

pub(crate) fn exceeds_recursive_depth(node: Node<'_>) -> bool {
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

pub(crate) fn ast_node(
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

pub(crate) fn collect_errors(node: Node<'_>, path: &Path, errors: &mut Vec<ParseError>) {
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

pub(crate) fn is_function(kind: &str, d: Dialect) -> bool {
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

pub(crate) fn is_type(kind: &str, d: Dialect) -> bool {
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

pub(crate) fn is_call(kind: &str, d: Dialect) -> bool {
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

pub(crate) fn is_branch(kind: &str) -> bool {
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

pub(crate) fn is_loop(kind: &str) -> bool {
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

pub(crate) fn branch_complexity(k: &str) -> bool {
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

pub(crate) fn contains(scope: &SourceRange, line: usize, col: usize) -> bool {
    (scope.start_line, scope.start_column) <= (line, col)
        && (line, col) <= (scope.end_line, scope.end_column)
}
pub(crate) fn import_applies(import: &Import, range: &SourceRange) -> bool {
    import
        .scope
        .as_ref()
        .is_none_or(|s| contains(s, range.start_line, range.start_column))
}
