use analyzer_core::{graph::CallGraph, Config, LanguageRegistry, LanguageStrategy};
use analyzer_rust::Rust;
use std::path::Path;

#[test]
fn rust_closures_capture_self_but_nested_function_items_do_not() {
    let file = Rust.analyze("struct Store; impl Store { fn work(&self) {} fn flow(&self) { let captured = || self.work(); fn ordinary() { self.work(); } captured(); } }", Path::new("capture.rs"));
    assert!(file.parse_errors.is_empty(), "{:?}", file.parse_errors);
    let mut registry = LanguageRegistry::new();
    registry.register(Rust);
    let graph = CallGraph::build(&[file], &registry, &Config::default());
    assert!(
        graph
            .edges
            .iter()
            .any(|edge| edge.from.ends_with("::captured") && edge.to.ends_with("::Store::work")),
        "{:?}",
        graph.edges
    );
    assert!(!graph
        .edges
        .iter()
        .any(|edge| edge.from.ends_with("::ordinary") && edge.to.ends_with("::Store::work")));
}
