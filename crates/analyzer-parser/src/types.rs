//! Annotation syntax belongs to the language adapters; the core only applies
//! the selected neutral normalization rules.
use crate::support::Dialect;
use analyzer_core::types::{normalize_reference, TypeReferenceSyntax};

pub fn reference_syntax(dialect: Dialect) -> TypeReferenceSyntax {
    match dialect {
        Dialect::Rust => TypeReferenceSyntax {
            borrowed: true,
            ..Default::default()
        },
        Dialect::Python => TypeReferenceSyntax {
            quoted_names: true,
            optional_names: true,
            ..Default::default()
        },
        Dialect::JavaScript | Dialect::TypeScript => TypeReferenceSyntax {
            array_receivers: true,
            ..Default::default()
        },
        Dialect::Php => TypeReferenceSyntax::default(),
    }
}

pub fn reference(language: &str, raw: &str) -> Option<String> {
    let dialect = match language {
        "rust" => Dialect::Rust,
        "python" => Dialect::Python,
        "javascript" => Dialect::JavaScript,
        "typescript" => Dialect::TypeScript,
        "php" => Dialect::Php,
        _ => return normalize_reference(raw, &TypeReferenceSyntax::default()),
    };
    normalize_reference(raw, &reference_syntax(dialect))
}
