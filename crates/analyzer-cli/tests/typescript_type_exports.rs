use analyzer_core::{graph::CallGraph, Confidence, Config, LanguageRegistry, LanguageStrategy};
use std::path::Path;

#[test]
fn javascript_consumers_cannot_call_erased_typescript_exports() {
    let files = [
        analyzer_typescript::TypeScript.analyze(
            "function run(){} class C {constructor(){} static run(){}} export type {run, C};",
            Path::new("dep.ts"),
        ),
        analyzer_javascript::JavaScript.analyze(
            "import {run, C} from './dep'; function entry(){run(); new C(); C.run();}",
            Path::new("app.js"),
        ),
    ];
    for file in &files {
        assert!(file.parse_errors.is_empty(), "{:?}", file.parse_errors);
    }
    let mut registry = LanguageRegistry::new();
    registry.register(analyzer_typescript::TypeScript);
    registry.register(analyzer_javascript::JavaScript);
    let graph = CallGraph::build(&files, &registry, &Config::default());
    let entry = files[1]
        .functions
        .iter()
        .find(|f| f.name == "entry")
        .unwrap();
    assert_eq!(entry.calls.len(), 3);
    for call in &entry.calls {
        let edges = graph
            .edges
            .iter()
            .filter(|edge| edge.callsite == call.range)
            .collect::<Vec<_>>();
        assert!(!edges.is_empty(), "{call:?}");
        assert!(
            edges
                .iter()
                .all(|edge| edge.confidence != Confidence::Resolved),
            "{call:?}: {edges:?}"
        );
    }
}
