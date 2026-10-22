use super::*;
pub(crate) fn parse_import(raw: &str, d: Dialect, full: &str) -> (String, Option<String>) {
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

pub(crate) fn split_call(
    raw: &str,
    node_kind: &str,
    d: Dialect,
) -> (Option<String>, String, CallKind) {
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

pub(crate) fn assigned_name(node: Node<'_>, source: &str) -> Option<String> {
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
