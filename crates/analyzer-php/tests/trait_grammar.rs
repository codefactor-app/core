use analyzer_core::LanguageStrategy;
use analyzer_parser::{parse, Dialect};
use std::path::Path;

#[test]
fn trait_precedence_accepts_multiple_and_qualified_exclusions_in_both_grammars() {
    let body = r#"namespace Demo;
trait T1 { function ping() {} }
trait T2 { function ping() {} }
trait T3 { function ping() {} }
class C { use T1, T2, T3 {
    T1::ping insteadof T2, \Demo\T3;
    T3::ping as pong;
} }
"#;
    let model = analyzer_php::Php.analyze(&format!("<?php\n{body}"), Path::new("traits.php"));
    assert!(model.parse_errors.is_empty(), "{:?}", model.parse_errors);
    let ast = parse(
        body,
        Path::new("traits.php"),
        tree_sitter_php::LANGUAGE_PHP_ONLY.into(),
        Dialect::Php,
    );
    assert!(ast.errors.is_empty(), "{:?}", ast.errors);
}

#[test]
fn malformed_trait_precedence_remains_a_syntax_error() {
    for clause in ["T1::ping insteadof T2,, T3;", "T1::ping insteadof ;"] {
        let source = format!("<?php class C {{ use T1, T2, T3 {{ {clause} }} }}");
        let model = analyzer_php::Php.analyze(&source, Path::new("invalid.php"));
        assert!(
            !model.parse_errors.is_empty(),
            "malformed clause accepted: {clause}"
        );
    }
}

#[test]
fn keyword_named_arguments_are_accepted_by_both_php_grammars() {
    let body = r#"
function takes($list, $match, $array) {}
class Example { function __construct($list) {} function takes($list) {} }
takes(list: [], match: true, array: []);
takes(LiSt: [], MaTcH: true, ArRaY: []);
$object->takes(list: []);
$object?->takes(list: []);
Example::takes(list: []);
new Example(list: []);
"#;
    let tagged = format!("<?php {body}");
    let model = analyzer_php::Php.analyze(&tagged, Path::new("arguments.php"));
    assert!(model.parse_errors.is_empty(), "{:?}", model.parse_errors);
    for (source, language) in [
        (tagged.as_str(), tree_sitter_php::LANGUAGE_PHP),
        (body, tree_sitter_php::LANGUAGE_PHP_ONLY),
    ] {
        let ast = parse(
            source,
            Path::new("arguments.php"),
            language.into(),
            Dialect::Php,
        );
        assert!(ast.errors.is_empty(), "{:?}", ast.errors);
    }
}

#[test]
fn malformed_named_arguments_remain_syntax_errors() {
    for source in ["takes(list:);", "takes(list:: []);", "new Example(list:);"] {
        for (source, language) in [
            (format!("<?php {source}"), tree_sitter_php::LANGUAGE_PHP),
            (source.to_owned(), tree_sitter_php::LANGUAGE_PHP_ONLY),
        ] {
            let ast = parse(
                &source,
                Path::new("invalid.php"),
                language.into(),
                Dialect::Php,
            );
            assert!(
                !ast.errors.is_empty(),
                "accepted malformed named argument: {source}"
            );
        }
    }
}
