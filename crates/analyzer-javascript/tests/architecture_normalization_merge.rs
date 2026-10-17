use analyzer_core::{graph::CallGraph, Confidence, Config, LanguageRegistry, LanguageStrategy};
use analyzer_javascript::JavaScript;
use std::path::Path;

#[test]
fn optional_architecture_preserves_accessor_symbol_initializer_and_cost_passes() {
    let source = r#"
        import {KEY} from '.\u002fsymbols.js';
        import {hit} from '.\x2fleaf.js';
        export {KEY} from '.\u002fsymbols.js';
        export * from '.\x2fleaf.js';
        class Store {
            [KEY] = 2;
            get value() { return 1; }
            run() {}
            probe() { this.run(); void this.value; hit(); }
        }
        function linear(values) { for (const value of values) { hit(value); } }
        const store = new Store(); store.probe();
    "#;
    let ordinary = JavaScript.analyze(source, Path::new("reader.js"));
    let advisory = JavaScript.analyze_with_architecture(source, Path::new("reader.js"), true);
    assert!(ordinary.parse_errors.is_empty());
    assert!(advisory.parse_errors.is_empty());
    assert!(ordinary.architecture_facts.is_none());
    let facts = advisory.architecture_facts.as_ref().unwrap();
    assert!(facts
        .functions
        .iter()
        .any(|f| f.fqn == "reader.Store.probe"));
    assert!(facts
        .references
        .iter()
        .any(|r| r.kind == "module_import" && r.name == "./symbols.js"));
    for file in [&ordinary, &advisory] {
        assert!(file.functions.iter().any(|f| f.is_module_initializer()));
        assert!(file
            .functions
            .iter()
            .find(|f| f.name == "linear")
            .unwrap()
            .cost_ir
            .is_some());
        assert_eq!(
            file.imports
                .iter()
                .find(|i| i.alias.as_deref() == Some("KEY"))
                .unwrap()
                .source
                .as_deref(),
            Some("./symbols.js")
        );
        assert!(file
            .exports
            .iter()
            .any(|e| e.name == "*" && e.source.as_deref() == Some("./leaf.js")));
        let probe = file
            .functions
            .iter()
            .find(|f| f.fqn == "reader.Store.probe")
            .unwrap();
        assert!(probe
            .property_accesses
            .iter()
            .any(|p| p.site.name == "value"));
        let field = &file
            .types
            .iter()
            .find(|t| t.name == "Store")
            .unwrap()
            .fields[0];
        assert!(field.unknown_key && field.symbol_key_reference.as_deref() == Some("KEY"));
    }
    let mut registry = LanguageRegistry::new();
    registry.register(JavaScript);
    for reader in [ordinary, advisory] {
        let files = [
            JavaScript.analyze(
                "export const KEY = Symbol.for('state');",
                Path::new("symbols.js"),
            ),
            JavaScript.analyze("export function hit() {}", Path::new("leaf.js")),
            reader,
        ];
        let graph = CallGraph::build(&files, &registry, &Config::default());
        for suffix in ["reader.Store.run", "reader.Store.value#get", "leaf.hit"] {
            assert!(
                graph
                    .edges
                    .iter()
                    .any(|e| e.from.ends_with("reader.Store.probe")
                        && e.to.ends_with(suffix)
                        && e.confidence == Confidence::Resolved),
                "{suffix}: {:?}",
                graph.edges
            );
        }
        assert!(
            graph
                .edges
                .iter()
                .any(|e| e.from.ends_with("reader.<module_initializer>")
                    && e.to.ends_with("reader.Store.probe")
                    && e.confidence == Confidence::Resolved),
            "{:?}",
            graph.edges
        );
    }
}
