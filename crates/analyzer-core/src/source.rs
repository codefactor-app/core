//! Shared source coordinates, line facts and package paths. No engine dependencies.
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct SourceRange {
    pub file: Arc<str>,
    pub start_line: usize,
    pub start_column: usize,
    pub end_line: usize,
    pub end_column: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Loc {
    pub total: usize,
    pub code: usize,
    pub comment: usize,
    pub blank: usize,
}

pub fn basic_line_counts(source: &str) -> Loc {
    let mut loc = Loc::default();
    for line in source.lines() {
        loc.total += 1;
        let t = line.trim();
        if t.is_empty() {
            loc.blank += 1;
        } else if t.starts_with("//")
            || t.starts_with('#')
            || t.starts_with("/*")
            || t.starts_with('*')
        {
            loc.comment += 1;
        } else {
            loc.code += 1;
        }
    }
    loc
}

pub fn vendor_package(path: &str) -> String {
    let parts = path.split('/').collect::<Vec<_>>();
    // Dependency roots can be nested below a language directory. Group by the
    // package immediately below that root rather than by the first two path
    // components (which would collapse every python/vendor package together).
    let count = if let Some(index) = parts.iter().position(|part| *part == "node_modules") {
        index
            + if parts
                .get(index + 1)
                .is_some_and(|name| name.starts_with('@'))
            {
                3
            } else {
                2
            }
    } else if let Some(index) = parts.iter().position(|part| *part == "vendor") {
        index + 2
    } else if let Some(index) = parts.iter().position(|part| *part == "site-packages") {
        index + 2
    } else if let Some(index) = parts.iter().position(|part| *part == ".venv") {
        index + 2
    } else {
        2
    };
    let mut value = parts
        .iter()
        .take(count)
        .copied()
        .collect::<Vec<_>>()
        .join("/");
    if !value.contains('/') && parts.len() == 1 {
        value = parts[0].into()
    }
    value
}
