use analyzer_core::{graph::CallGraph, Config, LanguageRegistry, LanguageStrategy};
use analyzer_javascript::JavaScript;
use std::path::Path;

#[test]
fn javascript_accessors_have_independent_graph_identities() {
    let file = JavaScript.analyze("function read() { return 1; } function write(n) {} class Store { get value() { return read(); } set value(n) { write(n); } }", Path::new("store.js"));
    assert!(file.parse_errors.is_empty());
    let mut registry = LanguageRegistry::new();
    registry.register(JavaScript);
    let graph = CallGraph::build(&[file], &registry, &Config::default());
    let getter = graph
        .nodes
        .iter()
        .find(|node| node.label == "store.Store.value#get")
        .unwrap();
    let setter = graph
        .nodes
        .iter()
        .find(|node| node.label == "store.Store.value#set")
        .unwrap();
    assert_ne!(getter.id, setter.id);
    assert!(graph
        .edges
        .iter()
        .any(|edge| edge.from.as_ref() == getter.id && edge.to.ends_with(".read")));
    assert!(graph
        .edges
        .iter()
        .any(|edge| edge.from.as_ref() == setter.id && edge.to.ends_with(".write")));
}
