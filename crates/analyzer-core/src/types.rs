//! A type annotation can identify a receiver without equating generic arguments
//! with that receiver. Unions and callable annotations cannot identify one class.
use serde::{Deserialize, Serialize};

/// Neutral annotation syntax supplied by a language adapter. Persisted on the
/// normalized file so declaration-only consumers do not need a registry.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TypeReferenceSyntax {
    pub borrowed: bool,
    pub quoted_names: bool,
    pub optional_names: bool,
    pub array_receivers: bool,
}
impl TypeReferenceSyntax {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Default language-neutral normalization retained as a compatibility entry point.
/// Use the registered strategy when a concrete language's syntax is available.
pub fn reference(_language: &str, raw: &str) -> Option<String> {
    normalize_reference(raw, &TypeReferenceSyntax::default())
}

pub fn normalize_reference(raw: &str, syntax: &TypeReferenceSyntax) -> Option<String> {
    let mut name = raw.trim().trim_start_matches(':').trim();
    if syntax.borrowed {
        while let Some(rest) = name.strip_prefix('&') {
            name = rest.trim_start();
            if name.starts_with('\'') {
                name = name.split_once(char::is_whitespace)?.1.trim_start();
            }
            name = name.strip_prefix("mut ").unwrap_or(name).trim_start();
        }
        name = name.strip_prefix("mut ").unwrap_or(name);
        name = name
            .strip_prefix("dyn ")
            .or_else(|| name.strip_prefix("impl "))
            .unwrap_or(name);
    } else if syntax.quoted_names {
        if name.len() >= 2
            && ((name.starts_with('\'') && name.ends_with('\''))
                || (name.starts_with('"') && name.ends_with('"')))
        {
            name = &name[1..name.len() - 1];
        }
    }
    name = name
        .trim()
        .trim_start_matches(['?', '\\'])
        .trim_end_matches('?');
    let array = syntax.array_receivers && name.ends_with("[]");
    if array {
        while let Some(element) = name.strip_suffix("[]") {
            name = element.trim_end();
        }
    }
    if syntax.optional_names {
        loop {
            let inner = ["Optional[", "typing.Optional["]
                .into_iter()
                .find_map(|prefix| name.strip_prefix(prefix).and_then(|n| n.strip_suffix(']')));
            let Some(inner) = inner else {
                break;
            };
            name = inner.trim();
        }
    }
    let mut depth = 0isize;
    let mut generic = None;
    for (i, c) in name.char_indices() {
        match c {
            '<' | '[' => {
                if depth == 0 {
                    generic.get_or_insert(i);
                }
                depth += 1;
            }
            '>' | ']' => {
                depth -= 1;
                if depth < 0 || depth == 0 && i + c.len_utf8() != name.len() {
                    return None;
                }
            }
            '|' | '&' if depth == 0 => return None,
            _ => {}
        }
    }
    if depth != 0 {
        return None;
    }
    let base = generic.map(|i| &name[..i]).unwrap_or(name).trim();
    if base.is_empty()
        || !base.chars().all(|c| {
            c.is_alphanumeric() || matches!(c, '_' | '$' | '.' | ':' | '\\' | '/' | '@' | '-')
        })
    {
        return None;
    }
    if matches!(
        base,
        "any" | "unknown" | "mixed" | "dynamic" | "object" | "Unknown" | "None" | "NoneType"
    ) || matches!(
        base,
        "Union" | "typing.Union" | "Callable" | "typing.Callable"
    ) {
        return None;
    }
    Some(if array {
        "Array".into()
    } else {
        base.to_owned()
    })
}
