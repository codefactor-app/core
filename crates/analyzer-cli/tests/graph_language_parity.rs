use analyzer_core::{graph::CallGraph, Confidence, LanguageRegistry};
use std::path::Path;

fn graph(sources: &[(&str, &str)]) -> CallGraph {
    let mut registry = LanguageRegistry::new();
    registry.register(analyzer_javascript::JavaScript);
    registry.register(analyzer_typescript::TypeScript);
    let files = sources
        .iter()
        .map(|(path, source)| {
            let path = Path::new(path);
            let file = registry
                .by_extension(path.extension().unwrap().to_str().unwrap())
                .unwrap()
                .analyze(source, path);
            assert!(file.parse_errors.is_empty(), "{:?}", file.parse_errors);
            file
        })
        .collect::<Vec<_>>();
    CallGraph::build(&files, &registry, &Default::default())
}

#[test]
fn local_methods_and_accessors_keep_their_source_when_js_and_ts_names_match() {
    let graph = graph(&[
        ("value.js", "class Value { read(){return 1;} get size(){return 1;} run(){this.read();return this.size;} }"),
        ("value.ts", "class Value { read(){return 2;} get size(){return 2;} run(){this.read();return this.size;} }"),
    ]);
    for path in ["value.js", "value.ts"] {
        let caller = graph
            .nodes
            .iter()
            .find(|n| n.file.as_deref() == Some(path) && n.label.ends_with(".run"))
            .unwrap();
        let edges = graph
            .edges
            .iter()
            .filter(|e| e.from.as_ref() == caller.id)
            .collect::<Vec<_>>();
        assert_eq!(edges.len(), 2, "{path}: {edges:?}");
        for edge in edges {
            let target = graph.node(&edge.to).unwrap();
            assert_eq!(edge.confidence, Confidence::Resolved);
            assert_eq!(target.file.as_deref(), Some(path));
        }
    }
}

#[test]
fn explicit_cross_language_imports_keep_the_imported_declaration() {
    let graph = graph(&[
        ("value.js", "export class Value { read(){return 1;} get size(){return 1;} }"),
        ("value.ts", "export class Value { read(){return 2;} get size(){return 2;} }"),
        ("main.js", "import {Value} from './value.ts'; export function fromTs(){const value=new Value();value.read();return value.size;}"),
        ("main.ts", "import {Value} from './value.js'; export function fromJs(){const value=new Value();value.read();return value.size;}"),
    ]);
    for (path, target_path) in [("main.js", "value.ts"), ("main.ts", "value.js")] {
        let caller = graph
            .nodes
            .iter()
            .find(|n| n.file.as_deref() == Some(path) && n.label.contains(".from"))
            .unwrap();
        let edges = graph
            .edges
            .iter()
            .filter(|edge| {
                edge.from.as_ref() == caller.id
                    && graph.node(&edge.to).is_some_and(|node| {
                        node.label.ends_with(".read") || node.label.ends_with(".size#get")
                    })
            })
            .collect::<Vec<_>>();
        assert_eq!(edges.len(), 2, "{path}: {edges:?}");
        for edge in edges {
            assert_eq!(edge.confidence, Confidence::Resolved);
            assert_eq!(
                graph.node(&edge.to).unwrap().file.as_deref(),
                Some(target_path)
            );
        }
    }
}
