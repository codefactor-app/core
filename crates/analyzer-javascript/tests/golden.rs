use analyzer_core::LanguageStrategy;
use analyzer_javascript::JavaScript;
use std::path::Path;

#[test]
fn javascript_architecture_golden() {
    let src = include_str!("fixtures/architecture.js");
    let p = Path::new("architecture.js");
    let s = JavaScript;
    let ast = s.parse(src, p);
    let file = s.normalize(src, p, &ast);
    assert!(ast.errors.is_empty());
    assert_eq!(file.functions.len(), 3);
    assert_eq!(file.types.len(), 1);
    assert!(file.functions.iter().any(|f| f.name == "entry"
        && f.calls
            .iter()
            .any(|c| c.name == "query" && !c.in_loop.is_empty() && !c.guard.is_empty())));
    let entry = file.functions.iter().find(|f| f.name == "entry").unwrap();
    assert!(entry
        .calls
        .iter()
        .any(|c| c.name == "query" && c.targets.contains(&"architecture.Store.query".to_string())));
    assert!(entry
        .calls
        .iter()
        .any(|c| c.name == "Store" && c.assignment_to.as_deref() == Some("store")));
}
