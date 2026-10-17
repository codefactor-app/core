use analyzer_core::{
    codemap::{DependencyIndex, MapOptions},
    graph::{CallGraph, GraphEdge},
    AnalysisReport, Confidence, Config, FileModel, LanguageRegistry,
};
use std::{collections::BTreeMap, path::Path, sync::Arc};

fn registry() -> LanguageRegistry {
    let mut registry = LanguageRegistry::new();
    registry.register(analyzer_php::Php);
    registry.register(analyzer_python::Python);
    registry.register(analyzer_javascript::JavaScript);
    registry.register(analyzer_typescript::TypeScript);
    registry.register(analyzer_rust::Rust);
    registry
}

fn report(sources: &[(&str, &str)], registry: &LanguageRegistry) -> AnalysisReport {
    let files = sources
        .iter()
        .map(|(path, source)| {
            let path = Path::new(path);
            let file = registry
                .by_extension(path.extension().unwrap().to_str().unwrap())
                .unwrap()
                .analyze_with_architecture(source, path, true);
            assert!(file.parse_errors.is_empty(), "{file:?}");
            file
        })
        .collect::<Vec<FileModel>>();
    let graph = CallGraph::build(&files, registry, &Config::default());
    AnalysisReport {
        version: String::new(),
        schema_version: String::new(),
        root: String::new(),
        config_path: None,
        strategy_capabilities: BTreeMap::new(),
        summary: Default::default(),
        aggregates: vec![],
        files,
        metrics: vec![],
        violations: vec![],
        parse_errors: vec![],
        graph,
        deferred_vendor: vec![],
        resolution_diagnostics: vec![],
        architecture: Default::default(),
    }
}

#[test]
fn unresolved_relative_imports_have_source_local_identities() {
    let registry = registry();
    let report = report(
        &[
            ("one/main.ts", "import { work } from './missing'; work();"),
            ("two/main.ts", "import { work } from './missing'; work();"),
        ],
        &registry,
    );
    let index = DependencyIndex::build(&report, &Config::default(), &registry);
    let imports = index
        .witnesses
        .iter()
        .filter(|w| w.kind == "import")
        .collect::<Vec<_>>();
    assert_eq!(imports.len(), 2);
    assert_ne!(imports[0].to, imports[1].to);
    for witness in imports {
        assert_eq!(index.entities[&witness.to].status, "unresolved");
        assert_eq!(witness.confidence, Confidence::Dynamic);
    }
    let view = index.view(&MapOptions::default());
    assert!(!view.coverage.complete);
    assert!(view.coverage.unresolved_dependencies >= 2);
}

#[test]
fn duplicate_physical_calls_preserve_the_least_certain_context() {
    let registry = registry();
    let mut report = report(
        &[("lib.rs", "pub fn target() {} pub fn entry() { target(); }")],
        &registry,
    );
    let edge = report.graph.edges[0].clone();
    let contexts = report
        .graph
        .nodes
        .iter()
        .map(|node| {
            let mut contextual = node.clone();
            contextual.id = format!("context:{}", node.id);
            contextual.label = format!("lib#cargo[bin]::{}", node.label);
            contextual.source_fqn = Some(node.label.clone());
            contextual.cargo_target = Some("bin".into());
            contextual
        })
        .collect::<Vec<_>>();
    report.graph.nodes.extend(contexts);
    report.graph.edges.push(GraphEdge {
        from: Arc::from(format!("context:{}", edge.from)),
        to: Arc::from(format!("context:{}", edge.to)),
        confidence: Confidence::Ambiguous,
        ..edge.clone()
    });
    for reverse in [false, true] {
        if reverse {
            report.graph.edges.reverse();
        }
        let index = DependencyIndex::build(&report, &Config::default(), &registry);
        assert_eq!(
            index
                .entities
                .values()
                .filter(|e| e.kind == "function")
                .count(),
            2
        );
        assert!(index.entities.values().all(|e| e.kind != "boundary"));
        let calls = index
            .witnesses
            .iter()
            .filter(|w| w.kind == "call")
            .collect::<Vec<_>>();
        assert_eq!(
            calls.len(),
            1,
            "Cargo contexts must not multiply source sites"
        );
        assert_eq!(calls[0].confidence, Confidence::Ambiguous);
        let view = index.view(&MapOptions {
            group: "file".into(),
            kinds: "call".into(),
            ..Default::default()
        });
        assert_eq!(view.links[0].count, 1);
        assert_eq!(view.links[0].uncertain, 1);
    }
}

#[test]
fn module_grouping_uses_each_declarations_namespace() {
    let registry = registry();
    for (path, source, first_namespace, second_namespace) in [
        (
            "names.php",
            "<?php namespace First; function a() {} namespace Second; function b() { \\First\\a(); }",
            "First",
            "Second",
        ),
        (
            "lib.rs",
            "pub mod first {pub fn a(){}} pub mod second {pub fn b(){super::first::a();}}",
            "lib::first",
            "lib::second",
        ),
        (
            "names.ts",
            "namespace First {export function a(){}} namespace Second {export function b(){First.a();}}",
            "names.First",
            "names.Second",
        ),
    ] {
        let report = report(&[(path, source)], &registry);
        let index = DependencyIndex::build(&report, &Config::default(), &registry);
        let first = index
            .entities
            .values()
            .find(|e| e.kind == "function" && e.label.ends_with(if path.ends_with("php") { "\\a" } else if path.ends_with("rs") { "::a" } else { ".a" }))
            .unwrap();
        let second_entity = index
            .entities
            .values()
            .find(|e| e.kind == "function" && e.label.ends_with(if path.ends_with("php") { "\\b" } else if path.ends_with("rs") { "::b" } else { ".b" }))
            .unwrap();
        assert_eq!(first.module, first_namespace, "{path}");
        assert_eq!(second_entity.module, second_namespace, "{path}");
        let initial = index.view(&MapOptions {
            group: "module".into(),
            ..Default::default()
        });
        let expanded = serde_json::to_string(
            &initial
                .blocks
                .iter()
                .filter(|block| block.kind == "module")
                .map(|block| &block.id)
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let expanded = index.view(&MapOptions {
            group: "module".into(),
            expanded,
            kinds: "call".into(),
            ..Default::default()
        });
        let leaf = |entity: &str| {
            expanded
                .blocks
                .iter()
                .find(|block| {
                    block.kind == "file"
                        && expanded.members[&block.id].contains(entity)
                })
                .unwrap()
        };
        let first_leaf = leaf(&first.id);
        let second_leaf = leaf(&second_entity.id);
        assert_ne!(first_leaf.id, second_leaf.id, "{path}");
        assert_eq!(first_leaf.parent.as_deref(), Some(format!("module:{}:{first_namespace}", first.language).as_str()));
        assert_eq!(second_leaf.parent.as_deref(), Some(format!("module:{}:{second_namespace}", first.language).as_str()));
        assert!(expanded.frontier.contains(&first_leaf.id));
        assert!(expanded.frontier.contains(&second_leaf.id));
        if path.ends_with("ts") {
            // Namespace-qualified runtime dispatch remains a resolver boundary.
            assert!(!expanded.coverage.complete);
        } else {
            assert!(expanded.links.iter().any(|link| link.from == second_leaf.id && link.to == first_leaf.id), "{path}: {:?}", expanded.links);
        }
    }
}

#[test]
fn php_imports_keep_their_declaring_namespace() {
    let registry = registry();
    let report = report(
        &[
            ("regions.php", "<?php namespace First; use Library\\Work; function a(){} namespace Second; use Library\\Model; function b(){}"),
            ("library.php", "<?php namespace Library; class Work {} class Model {}"),
        ],
        &registry,
    );
    let index = DependencyIndex::build(&report, &Config::default(), &registry);
    let imports = index
        .witnesses
        .iter()
        .filter(|w| w.kind == "import")
        .collect::<Vec<_>>();
    assert_eq!(imports.len(), 2);
    let namespaces = imports
        .iter()
        .map(|w| index.entities[&w.from].module.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(namespaces, ["First", "Second"].into_iter().collect());
    let view = index.view(&MapOptions {
        group: "file".into(),
        ..Default::default()
    });
    assert_eq!(
        view.blocks
            .iter()
            .find(|b| b.id == "file:regions.php")
            .unwrap()
            .counts
            .files,
        1
    );
    let modules = index.view(&MapOptions {
        group: "module".into(),
        kinds: "import".into(),
        ..Default::default()
    });
    for namespace in ["First", "Second"] {
        assert!(modules
            .links
            .iter()
            .any(|l| l.from == format!("module:php:{namespace}") && l.to == "module:php:Library"));
    }
}
