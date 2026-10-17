use analyzer_core::LanguageStrategy;
use analyzer_php::Php;
use std::path::Path;

#[test]
fn php_architecture_golden() {
    let src = include_str!("fixtures/architecture.php");
    let p = Path::new("architecture.php");
    let s = Php;
    let ast = s.parse(src, p);
    let file = s.normalize(src, p, &ast);
    assert!(ast.errors.is_empty());
    assert_eq!(file.types.len(), 3);
    assert_eq!(file.functions.len(), 5);
    assert_eq!(
        file.types
            .iter()
            .find(|t| t.name == "Service")
            .unwrap()
            .fields
            .len(),
        1
    );
    assert!(file
        .functions
        .iter()
        .find(|f| f.name == "run")
        .unwrap()
        .accessed_fields
        .contains(&"store".to_string()));
    assert!(
        file.functions
            .iter()
            .find(|f| f.name == "run")
            .unwrap()
            .calls
            .iter()
            .any(|c| c.name == "query"
                && c.receiver_type.as_deref() == Some("\\App\\Database\\Store"))
    );
    assert!(file.functions.iter().any(|f| f.name == "run"
        && f.calls
            .iter()
            .any(|c| c.name == "query" && !c.in_loop.is_empty() && !c.guard.is_empty())));
}

#[test]
fn deeply_nested_concatenation_scan_and_ast_are_stack_safe() {
    // Mirrors PHP-CS-Fixer issue_8828_c, far beyond a Rayon worker's stack.
    let expression = std::iter::repeat_n("\" text \".$variable", 20_000)
        .collect::<Vec<_>>()
        .join(".");
    let source = format!("<?php if (true) {{ $foo = {expression}; }}");
    let worker = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || {
            let file = Php.analyze(&source, Path::new("deep.php"));
            assert!(file.parse_errors.is_empty());
            assert!(file.functions.is_empty());
            let ast = Php.parse(&source, Path::new("deep.php"));
            assert_eq!(ast.errors.len(), 1);
            assert!(ast.errors[0].message.contains("detailed AST incomplete"));
            assert!(ast.root.children.is_empty());
            let wrapped = format!("<?php function deep() {{ $foo = {expression}; }}");
            let file = Php.analyze(&wrapped, Path::new("deep-function.php"));
            assert!(file.functions.is_empty());
            assert_eq!(file.parse_errors.len(), 1);
            assert!(file.parse_errors[0].message.contains("analysis incomplete"));
        })
        .unwrap();
    worker.join().unwrap();
}
