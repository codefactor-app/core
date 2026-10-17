use analyzer_core::{
    graph::{CallGraph, NodeKind},
    Confidence, Config, FileModel, LanguageRegistry, LanguageStrategy,
};
use std::path::Path;

fn models(sources: &[(&str, &str)]) -> Vec<FileModel> {
    sources
        .iter()
        .map(|(path, source)| {
            let strategy: &dyn LanguageStrategy = if path.ends_with(".ts") || path.ends_with(".tsx")
            {
                &analyzer_typescript::TypeScript
            } else {
                &analyzer_javascript::JavaScript
            };
            let mut file = strategy.analyze(source, Path::new(path));
            assert!(
                file.parse_errors.is_empty(),
                "{path}: {:?}",
                file.parse_errors
            );
            file.vendor = path.contains("node_modules/");
            file
        })
        .collect()
}

fn build(files: &[FileModel]) -> CallGraph {
    let mut registry = LanguageRegistry::new();
    registry.register(analyzer_typescript::TypeScript);
    registry.register(analyzer_javascript::JavaScript);
    CallGraph::build(files, &registry, &Config::default())
}

fn target_labels<'a>(graph: &'a CallGraph, label: &str) -> Vec<&'a str> {
    let source = graph.nodes.iter().find(|node| node.label == label).unwrap();
    graph
        .outgoing_edges(&source.id)
        .map(|edge| graph.node(&edge.to).unwrap().label.as_str())
        .collect()
}

#[test]
fn bare_package_named_import_becomes_callable_after_vendor_is_parsed() {
    let mut files = models(&[(
        "src/main.ts",
        "import {work} from 'demo'; function flow(){work();}",
    )]);
    let before = build(&files);
    assert!(before
        .edges
        .iter()
        .all(|edge| edge.confidence == Confidence::Dynamic));
    files.extend(models(&[(
        "node_modules/demo/index.ts",
        "export function work(){leaf();} function leaf(){}",
    )]));
    let after = build(&files);
    assert_eq!(
        target_labels(&after, "src/main.flow"),
        ["node_modules/demo/index.work"]
    );
    assert_eq!(
        target_labels(&after, "node_modules/demo/index.work"),
        ["node_modules/demo/index.leaf"]
    );
    assert!(after
        .edges
        .iter()
        .all(|edge| edge.confidence == Confidence::Resolved));
}

#[test]
fn ts_can_call_js_namespace_functions_and_default_exported_classes() {
    let files = models(&[
        ("src/main.ts", "import Client from 'demo'; import * as api from 'demo'; function flow(){api.work();const client=new Client();client.run();}"),
        ("node_modules/demo/index.js", "export default class Client {constructor(){} run(){}} export function work(){}"),
    ]);
    let graph = build(&files);
    let targets = target_labels(&graph, "src/main.flow");
    for expected in [
        "node_modules/demo/index.work",
        "node_modules/demo/index.Client.constructor",
        "node_modules/demo/index.Client.run",
    ] {
        assert!(targets.contains(&expected), "{graph:?}");
    }
    assert!(graph
        .edges
        .iter()
        .all(|edge| edge.confidence == Confidence::Resolved));
}

#[test]
fn scoped_packages_and_subpath_file_or_index_entries_are_deterministic() {
    let files = models(&[
        ("src/main.ts", "import {work as root} from '@acme/demo';import {work as sub} from '@acme/demo/sub';import {work as nested} from 'other/sub';function flow(){root();sub();nested();}"),
        ("node_modules/@acme/demo/index.ts", "export function work(){}"),
        ("node_modules/@acme/demo/sub.js", "export function work(){}"),
        ("node_modules/other/sub/index.tsx", "export function work(){}"),
    ]);
    let graph = build(&files);
    let targets = target_labels(&graph, "src/main.flow");
    for expected in [
        "node_modules/@acme/demo/index.work",
        "node_modules/@acme/demo/sub.work",
        "node_modules/other/sub/index.work",
    ] {
        assert!(targets.contains(&expected), "{graph:?}");
    }
}

#[test]
fn nearest_package_install_wins_and_multiple_entry_variants_stay_ambiguous() {
    let files = models(&[
        (
            "packages/app/src/main.ts",
            "import {work} from 'demo'; function flow(){work();}",
        ),
        (
            "packages/app/node_modules/demo/index.ts",
            "export function work(){}",
        ),
        ("node_modules/demo/index.ts", "export function work(){}"),
    ]);
    let graph = build(&files);
    assert_eq!(
        target_labels(&graph, "packages/app/src/main.flow"),
        ["packages/app/node_modules/demo/index.work"]
    );
    let files = models(&[
        (
            "src/main.ts",
            "import {work} from 'demo'; function flow(){work();}",
        ),
        ("node_modules/demo/index.ts", "export function work(){}"),
        ("node_modules/demo/index.js", "export function work(){}"),
    ]);
    let graph = build(&files);
    assert_eq!(graph.edges.len(), 1);
    assert_eq!(graph.edges[0].confidence, Confidence::Ambiguous);
    assert_eq!(
        graph.node(&graph.edges[0].to).unwrap().kind,
        NodeKind::Unknown
    );
}

#[test]
fn accessor_callable_property_lookup_targets_getter_and_preserves_setter_body() {
    let files = models(&[("accessors.ts", "function read(){}function write(){}class Client {get value(){read();return ()=>{};}set value(value){write();}}function flow(client:Client){client.value();}")]);
    let graph = build(&files);
    assert_eq!(
        target_labels(&graph, "accessors.flow"),
        ["accessors.Client.value#get"]
    );
    assert_eq!(
        target_labels(&graph, "accessors.Client.value#get"),
        ["accessors.read"]
    );
    assert_eq!(
        target_labels(&graph, "accessors.Client.value#set"),
        ["accessors.write"]
    );
}

#[test]
fn named_import_symbols_cannot_be_hijacked_by_dotted_package_resources() {
    let files = models(&[
        ("src/main.ts", "import {work} from 'demo';import * as dotted from 'demo.work';function flow(){work();dotted.other();}"),
        ("node_modules/demo/index.ts", "export function work(){}"),
        ("node_modules/demo.work/index.ts", "export function other(){}"),
    ]);
    assert_eq!(
        files[0]
            .imports
            .iter()
            .find(|import| import.alias.as_deref() == Some("work"))
            .unwrap()
            .source
            .as_deref(),
        Some("demo")
    );
    assert_eq!(
        files[0]
            .imports
            .iter()
            .find(|import| import.alias.as_deref() == Some("dotted"))
            .unwrap()
            .source
            .as_deref(),
        Some("demo.work")
    );
    let graph = build(&files);
    let targets = target_labels(&graph, "src/main.flow");
    assert_eq!(targets.len(), 2);
    assert!(targets.contains(&"node_modules/demo/index.work"));
    assert!(targets.contains(&"node_modules/demo.work/index.other"));
    assert!(graph
        .edges
        .iter()
        .all(|edge| edge.confidence == Confidence::Resolved));
}

#[test]
fn explicit_relative_extension_selects_only_the_actual_requested_file() {
    let files = models(&[
        (
            "src/main.ts",
            "import {work} from './dep.js';function flow(){work();}",
        ),
        ("src/dep.js", "export function work(){}"),
        ("src/dep.ts", "export function work(){}"),
    ]);
    assert_eq!(files[0].imports[0].source.as_deref(), Some("./dep.js"));
    let graph = build(&files);
    assert_eq!(graph.edges.len(), 1);
    assert_eq!(graph.edges[0].confidence, Confidence::Resolved);
    assert_eq!(
        graph.node(&graph.edges[0].to).unwrap().file.as_deref(),
        Some("src/dep.js")
    );
}
