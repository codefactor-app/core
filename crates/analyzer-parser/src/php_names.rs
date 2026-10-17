//! Source PHP class references become absolute before lexical type lookup.
use super::*;

pub(super) fn reference(
    raw: &str,
    owner: &str,
    imports: &[Import],
    range: &SourceRange,
) -> Option<String> {
    let absolute = raw.trim().trim_start_matches('?').starts_with('\\');
    let name = crate::types::reference("php", raw)?;
    if matches!(
        name.to_ascii_lowercase().as_str(),
        "self"
            | "static"
            | "parent"
            | "int"
            | "float"
            | "string"
            | "bool"
            | "array"
            | "iterable"
            | "callable"
            | "void"
            | "never"
            | "null"
            | "false"
            | "true"
    ) {
        return Some(name);
    }
    if name.starts_with('$') {
        return None;
    }
    if absolute {
        return Some(format!("\\{name}"));
    }
    if !name.starts_with("namespace\\") {
        let mut matches = imports
            .iter()
            .filter(|import| bindings::applies(import, range))
            .filter_map(|import| {
                let alias = import
                    .alias
                    .as_deref()
                    .unwrap_or_else(|| import.path.rsplit('\\').next().unwrap_or(&import.path));
                let suffix = if name == alias {
                    ""
                } else {
                    name.strip_prefix(alias)?.strip_prefix('\\')?
                };
                Some((
                    alias.len(),
                    import
                        .scope
                        .as_ref()
                        .map(|scope| (scope.start_line, scope.start_column))
                        .unwrap_or_default(),
                    import,
                    suffix,
                ))
            })
            .collect::<Vec<_>>();
        matches.sort_by_key(|(length, scope, _, _)| (*length, *scope));
        if let Some((_, _, import, suffix)) = matches.last() {
            return Some(format!(
                "\\{}{}{}",
                import.path.trim_start_matches('\\'),
                if suffix.is_empty() { "" } else { "\\" },
                suffix
            ));
        }
    }
    let namespace = owner
        .split("::")
        .next()
        .unwrap_or(owner)
        .rsplit_once('\\')
        .map(|(namespace, _)| namespace)
        .unwrap_or("");
    let name = name.strip_prefix("namespace\\").unwrap_or(&name);
    Some(if namespace.is_empty() {
        format!("\\{name}")
    } else {
        format!("\\{namespace}\\{name}")
    })
}
