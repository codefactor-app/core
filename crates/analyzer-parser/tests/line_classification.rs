use analyzer_core::FileModel;
use analyzer_parser::{count_lines, normalize, Dialect};
use std::path::Path;
use tree_sitter::Language;

fn language(dialect: Dialect) -> Language {
    match dialect {
        Dialect::Python => tree_sitter_python::LANGUAGE.into(),
        Dialect::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Dialect::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Dialect::Php => tree_sitter_php::LANGUAGE_PHP.into(),
        Dialect::Rust => tree_sitter_rust::LANGUAGE.into(),
    }
}

fn model(source: &str, dialect: Dialect) -> FileModel {
    let model = normalize(source, Path::new("fixture"), language(dialect), dialect);
    assert!(model.parse_errors.is_empty(), "{:?}", model.parse_errors);
    model
}

#[test]
fn multiline_literals_cannot_start_comments_or_hide_subsequent_code() {
    let fixtures = [
        (Dialect::JavaScript, "const text = `\n/*\n// literal text\n`;\nfunction next() {}\n// actual comment\n"),
        (Dialect::TypeScript, "const text: string = `\n/*\n// literal text\n`;\nfunction next(): void {}\n// actual comment\n"),
        (Dialect::Rust, "const TEXT: &str = r##\"\n/*\n// literal text\n\"##;\nfn next() {}\n// actual comment\n"),
        (Dialect::Php, "<?php\n$text = <<<'TEXT'\n/*\n// literal text\nTEXT;\nfunction next() {}\n// actual comment\n"),
        (Dialect::Python, "text = \"\"\"\n/*\n# literal text\n\"\"\"\ndef next(): pass\n# actual comment\n"),
    ];
    for (dialect, source) in fixtures {
        let actual = model(source, dialect);
        let expected_total = if dialect == Dialect::Php { 7 } else { 6 };
        assert_eq!(actual.loc.total, expected_total, "{dialect:?}");
        assert_eq!(actual.loc.code, expected_total - 1, "{dialect:?}");
        assert_eq!(actual.loc.comment, 1, "{dialect:?}");
        assert_eq!(actual.loc.blank, 0, "{dialect:?}");
        let standalone = count_lines(source, dialect);
        assert_eq!(format!("{standalone:?}"), format!("{:?}", actual.loc));
        let comments = actual.comment_ranges.unwrap();
        assert_eq!(comments.len(), 1, "{dialect:?}");
        assert_eq!(comments[0].start_line, expected_total, "{dialect:?}");
    }
}

#[test]
fn comments_inside_template_substitutions_are_real_comments() {
    let source = "const text = `/* literal ${\n// actual comment\n1\n} */ literal`;\n";
    let actual = model(source, Dialect::JavaScript);
    assert_eq!(actual.loc.total, 4);
    assert_eq!(actual.loc.comment, 1);
    assert_eq!(actual.loc.code, 3);
    let comments = actual.comment_ranges.unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0].start_line, 2);
}

#[test]
fn nested_rust_comments_mixed_lines_and_blank_lines_are_classified_by_syntax() {
    let source = "\u{feff}/* outer\r\n/* inner */\r\n\r\nend */ fn first() {}\r\n/* comment */\r\nfn second() {} // trailing comment\r\n";
    let actual = model(source, Dialect::Rust);
    assert_eq!(actual.loc.total, 6);
    assert_eq!(actual.loc.comment, 3);
    assert_eq!(actual.loc.blank, 1);
    assert_eq!(actual.loc.code, 2);
}

#[test]
fn an_empty_comment_set_is_distinct_from_an_unknown_strategy() {
    let actual = model(
        "const text = 'analyzer-ignore: function_params';",
        Dialect::JavaScript,
    );
    assert_eq!(actual.comment_ranges, Some(Vec::new()));
    assert_eq!(count_lines("", Dialect::JavaScript).total, 0);
    assert_eq!(count_lines("\n", Dialect::JavaScript).blank, 1);
}
