use analyzer_core::{graph::CallGraph, Confidence, Config, LanguageRegistry, LanguageStrategy};
use std::path::Path;

fn graph(sources: &[(&str, &str)]) -> CallGraph {
    let strategy = analyzer_rust::Rust;
    let files = sources
        .iter()
        .map(|(path, source)| {
            let file = strategy.analyze(source, Path::new(path));
            assert!(
                file.parse_errors.is_empty(),
                "{path}: {:?}",
                file.parse_errors
            );
            file
        })
        .collect::<Vec<_>>();
    let mut registry = LanguageRegistry::new();
    registry.register(strategy);
    CallGraph::build(&files, &registry, &Config::default())
}

fn targets<'a>(graph: &'a CallGraph, source: &str) -> Vec<&'a str> {
    let source = graph
        .nodes
        .iter()
        .find(|node| node.label == source)
        .unwrap();
    graph
        .outgoing_edges(&source.id)
        .map(|edge| graph.node(&edge.to).unwrap().label.as_str())
        .collect()
}

#[test]
fn ordinary_external_modules_resolve_self_crate_and_super_calls() {
    let graph = graph(&[
        ("src/lib.rs", "mod child; fn root(){} fn flow(){child::work(); self::child::work(); crate::child::work();}"),
        ("src/child.rs", "pub fn work(){super::root(); crate::root(); self::leaf();} fn leaf(){}"),
    ]);
    assert_eq!(targets(&graph, "src::lib::flow"), ["src::child::work"; 3]);
    let targets = targets(&graph, "src::child::work");
    assert_eq!(
        targets
            .iter()
            .filter(|target| **target == "src::lib::root")
            .count(),
        2
    );
    assert_eq!(
        targets
            .iter()
            .filter(|target| **target == "src::child::leaf")
            .count(),
        1
    );
    assert!(graph
        .edges
        .iter()
        .all(|edge| edge.confidence == Confidence::Resolved));
}

#[test]
fn directory_mod_rs_supports_nested_modules_and_multiple_super_hops() {
    let graph = graph(&[
        (
            "src/lib.rs",
            "mod child; fn root(){} fn flow(){crate::child::nested::work();}",
        ),
        ("src/child/mod.rs", "pub mod nested; pub fn local(){}"),
        (
            "src/child/nested.rs",
            "pub fn work(){super::local(); super::super::root(); self::leaf();} fn leaf(){}",
        ),
    ]);
    assert_eq!(
        targets(&graph, "src::lib::flow"),
        ["src::child::nested::work"]
    );
    let targets = targets(&graph, "src::child::nested::work");
    assert!(targets.contains(&"src::child::mod::local"));
    assert!(targets.contains(&"src::lib::root"));
    assert!(targets.contains(&"src::child::nested::leaf"));
}

#[test]
fn inline_modules_resolve_their_own_external_declarations_without_collisions() {
    let graph = graph(&[
        (
            "src/lib.rs",
            "mod dep; mod inline {mod dep; pub fn flow(){dep::work();}} fn flow(){dep::work();}",
        ),
        ("src/dep.rs", "pub fn work(){}"),
        ("src/inline/dep.rs", "pub fn work(){}"),
        ("unrelated/dep.rs", "pub fn work(){}"),
    ]);
    assert_eq!(
        targets(&graph, "src::lib::inline::flow"),
        ["src::inline::dep::work"]
    );
    assert_eq!(targets(&graph, "src::lib::flow"), ["src::dep::work"]);
}

#[test]
fn inline_scopes_in_external_files_keep_logical_parents_and_scoped_imports() {
    let graph = graph(&[
        ("src/lib.rs", "mod child; fn root(){}"),
        ("src/child.rs", "pub fn work(){} mod inner {use super::work as run; pub fn flow(){run(); super::work(); crate::root();}}"),
    ]);
    let targets = targets(&graph, "src::child::inner::flow");
    assert_eq!(
        targets
            .iter()
            .filter(|target| **target == "src::child::work")
            .count(),
        2
    );
    assert_eq!(
        targets
            .iter()
            .filter(|target| **target == "src::lib::root")
            .count(),
        1
    );
}

#[test]
fn undeclared_or_ambiguous_external_files_do_not_resolve_by_filename() {
    for sources in [
        vec![
            ("src/lib.rs", "fn flow(){child::work();}"),
            ("src/child.rs", "pub fn work(){}"),
        ],
        vec![
            ("src/lib.rs", "mod child; fn flow(){child::work();}"),
            ("src/child.rs", "pub fn work(){}"),
            ("src/child/mod.rs", "pub fn work(){}"),
        ],
        vec![
            ("src/lib.rs", "mod child {} fn flow(){child::work();}"),
            ("src/child.rs", "pub fn work(){}"),
        ],
    ] {
        let graph = graph(&sources);
        let flow = graph
            .nodes
            .iter()
            .find(|node| node.label == "src::lib::flow")
            .unwrap();
        assert!(
            graph
                .outgoing_edges(&flow.id)
                .all(|edge| edge.confidence == Confidence::Dynamic),
            "{graph:?}"
        );
    }
}

#[test]
fn shared_source_with_two_crate_roots_does_not_guess_super_target() {
    let graph = graph(&[
        ("src/lib.rs", "mod shared; fn hook(){}"),
        ("src/main.rs", "mod shared; fn hook(){} fn main(){}"),
        ("src/shared.rs", "pub fn flow(){super::hook();}"),
    ]);
    let flow = graph
        .nodes
        .iter()
        .find(|node| node.label == "src::shared::flow")
        .unwrap();
    assert!(
        graph
            .outgoing_edges(&flow.id)
            .all(|edge| edge.confidence == Confidence::Dynamic),
        "{graph:?}"
    );
}

#[test]
fn explicit_path_module_attribute_uses_declaring_file_location() {
    let graph = graph(&[
        (
            "src/lib.rs",
            "#[path=\"helpers/tasks.rs\"] mod tasks; fn root(){} fn flow(){tasks::work();}",
        ),
        ("src/helpers/tasks.rs", "pub fn work(){super::root();}"),
    ]);
    assert_eq!(
        targets(&graph, "src::lib::flow"),
        ["src::helpers::tasks::work"]
    );
    assert_eq!(
        targets(&graph, "src::helpers::tasks::work"),
        ["src::lib::root"]
    );
}

#[test]
fn root_level_module_files_need_declarations_and_local_scopes_win() {
    let graph = graph(&[
        ("lib.rs", "mod child; mod inside {mod child {} pub fn flow(){child::work();}} fn flow(){child::work();}"),
        ("child.rs", "pub fn work(){}"),
    ]);
    assert_eq!(targets(&graph, "lib::flow"), ["child::work"]);
    let source = graph
        .nodes
        .iter()
        .find(|node| node.label == "lib::inside::flow")
        .unwrap();
    assert!(graph
        .outgoing_edges(&source.id)
        .all(|edge| edge.confidence == Confidence::Dynamic));
    let undeclared = crate::graph(&[
        ("lib.rs", "fn flow(){child::work();}"),
        ("child.rs", "pub fn work(){}"),
    ]);
    assert!(undeclared
        .edges
        .iter()
        .all(|edge| edge.confidence == Confidence::Dynamic));
}

#[test]
fn external_modules_preserve_imported_types_and_sibling_aliases() {
    let graph = graph(&[
        ("lib.rs", "mod first; mod second; fn root(){}"),
        ("first.rs", "use super::second::{Client, work as run}; pub fn flow(client:&Client){run();client.query();} struct Local; impl Local { fn flow(&self){super::root();}}"),
        ("second.rs", "pub fn work(){} pub struct Client; impl Client {pub fn query(&self){}}"),
    ]);
    let calls = targets(&graph, "first::flow");
    assert_eq!(calls.len(), 2);
    assert!(calls.contains(&"second::work"));
    assert!(calls.contains(&"second::Client::query"));
    assert_eq!(targets(&graph, "first::Local::flow"), ["lib::root"]);
}

#[test]
fn inline_path_attributes_select_real_sources_and_ignore_default_path_decoys() {
    let graph = graph(&[
        (
            "src/lib.rs",
            "#[path=\"elsewhere\"] mod wrapper {mod child;pub fn flow(){child::work();}}",
        ),
        ("src/elsewhere/child.rs", "pub fn work(){}"),
        ("src/wrapper/child.rs", "pub fn work(){}"),
    ]);
    assert_eq!(
        targets(&graph, "src::lib::wrapper::flow"),
        ["src::elsewhere::child::work"]
    );
    assert!(graph
        .edges
        .iter()
        .all(|edge| edge.confidence == Confidence::Resolved));
    let graph = crate::graph(&[
        ("src/lib.rs", "mod foo;"),
        (
            "src/foo.rs",
            "#[path=\"alt\"] mod inner {mod leaf;pub fn flow(){leaf::work();}}",
        ),
        ("src/alt/leaf.rs", "pub fn work(){}"),
        ("src/foo/inner/leaf.rs", "pub fn work(){}"),
        ("src/foo/alt/leaf.rs", "pub fn work(){}"),
    ]);
    assert_eq!(
        targets(&graph, "src::foo::inner::flow"),
        ["src::alt::leaf::work"]
    );
    assert!(graph
        .edges
        .iter()
        .all(|edge| edge.confidence == Confidence::Resolved));
}
