use analyzer_core::{
    graph::{CallGraph, Direction, GraphFilter},
    Config, LanguageRegistry, LanguageStrategy,
};
use analyzer_rust::Rust;
use std::path::Path;

#[test]
fn rust_architecture_golden() {
    let src = include_str!("fixtures/architecture.rs");
    let p = Path::new("architecture.rs");
    let s = Rust;
    let ast = s.parse(src, p);
    let file = s.normalize(src, p, &ast);
    assert!(ast.errors.is_empty());
    assert_eq!(file.functions.len(), 5);
    assert!(file
        .types
        .iter()
        .any(|t| t.name == "Store" && t.implements.contains(&"Repository".to_string())));
    assert!(file
        .functions
        .iter()
        .any(|f| f.fqn == "architecture::Store::query"));
    assert!(file.functions.iter().any(|f| f.name == "run"
        && f.calls
            .iter()
            .any(|c| c.name == "query" && !c.in_loop.is_empty() && !c.guard.is_empty())));
    let mut registry = LanguageRegistry::new();
    registry.register(Rust);
    let graph = CallGraph::build(&[file], &registry, &Config::default());
    let query = graph.query(
        "architecture::Store::run",
        Direction::Callees,
        None,
        &GraphFilter::all(),
        &Config::default(),
    );
    assert!(query
        .edges
        .iter()
        .any(|e| e.to.ends_with("architecture::Store::query")
            && !e.guard.is_empty()
            && !e.in_loop.is_empty()));
}
