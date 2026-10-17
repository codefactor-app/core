use analyzer_core::{graph::CallGraph, Config, LanguageRegistry, LanguageStrategy};
use analyzer_javascript::JavaScript;
use std::path::Path;

fn graph(source: &str) -> CallGraph {
    let file = JavaScript.analyze(source, Path::new("property.js"));
    assert!(file.parse_errors.is_empty(), "{:?}", file.parse_errors);
    let mut registry = LanguageRegistry::new();
    registry.register(JavaScript);
    CallGraph::build(&[file], &registry, &Config::default())
}

#[test]
fn constructor_inference_resolves_javascript_reads_writes_and_compound_operations() {
    let graph = graph(
        r#"
        class Store { get value() { return 1; } set value(n) {} plain = 0; }
        function use(unknown, key) {
            const store = new Store();
            const a = store.value;
            store.value = 1;
            store.value++;
            store["value"] += 2;
            const {value: b} = store;
            const c = store.plain;
            const d = unknown.value;
            const e = store[key];
        }
    "#,
    );
    let edges = graph
        .edges
        .iter()
        .filter(|edge| edge.from.ends_with(".use"))
        .collect::<Vec<_>>();
    assert_eq!(
        edges
            .iter()
            .filter(|edge| edge.to.ends_with(".value#get"))
            .count(),
        4,
        "{edges:?}"
    );
    assert_eq!(
        edges
            .iter()
            .filter(|edge| edge.to.ends_with(".value#set"))
            .count(),
        3
    );
    assert!(!edges.iter().any(|edge| edge.to.starts_with("unknown:")
        && (edge.to.ends_with("value") || edge.to.ends_with("plain"))));
}

#[test]
fn callable_getter_reads_once_and_optional_access_preserves_a_presence_guard() {
    let graph = graph(
        r#"
        class Store { get value() { return () => 1; } set value(fn) {} }
        function use() { const store = new Store(); store.value(); const fn = store?.value; }
    "#,
    );
    let getters = graph
        .edges
        .iter()
        .filter(|edge| edge.from.ends_with(".use") && edge.to.ends_with(".value#get"))
        .collect::<Vec<_>>();
    assert_eq!(getters.len(), 2, "{getters:?}");
    assert!(getters.iter().any(|edge| edge
        .guard
        .iter()
        .any(|guard| guard.normalized == "store != null")));
    assert!(!graph
        .edges
        .iter()
        .any(|edge| edge.from.ends_with(".use") && edge.to.ends_with(".value#set")));
}

#[test]
fn own_data_fields_replace_native_accessors_for_reads_writes_and_calls() {
    let graph = graph(
        r#"
        class Store {
            get value() { return () => 1; }
            set value(fn) {}
            value = () => 2;
            static get item() { return () => 3; }
            static item = () => 4;
        }
        function use() {
            const store = new Store();
            void store.value;
            store.value = 2;
            store.value();
            void Store.item;
            Store.item();
        }
    "#,
    );
    let edges = graph
        .edges
        .iter()
        .filter(|edge| edge.from.ends_with(".use"));
    assert!(
        !edges
            .clone()
            .any(|edge| edge.to.ends_with("#get") || edge.to.ends_with("#set")),
        "{:?}",
        edges.collect::<Vec<_>>()
    );
}

#[test]
fn proven_computed_data_keys_block_own_and_inherited_accessors() {
    let source = r#"
        const externalKey = 'value';
        class Parent { get value() { return () => 1; } set value(fn) {} }
        class Dynamic extends Parent { [externalKey] = 2; }
        class Escaped extends Parent { ['val\u0075e'] = 2; }
        class Own { get value() { return () => 1; } [externalKey] = 2; }
        function use() {
            const dynamic = new Dynamic();
            const escaped = new Escaped();
            const own = new Own();
            void dynamic.value;
            dynamic.value = 3;
            dynamic.value();
            void escaped.value;
            escaped.value();
            void own.value;
            own.value();
        }
    "#;
    let file = JavaScript.analyze(source, Path::new("property.js"));
    assert_eq!(
        file.types
            .iter()
            .flat_map(|ty| &ty.fields)
            .filter(|field| field.unknown_key)
            .count(),
        0,
        "{:?}",
        file.types
    );
    let graph = graph(source);
    let edges = graph
        .edges
        .iter()
        .filter(|edge| edge.from.ends_with(".use"));
    assert!(
        !edges
            .clone()
            .any(|edge| edge.to.ends_with("#get") || edge.to.ends_with("#set")),
        "{:?}",
        edges.collect::<Vec<_>>()
    );
}

#[test]
fn computed_data_key_blockers_keep_static_instance_isolation_and_known_inheritance() {
    let graph = graph(
        r#"
        const externalKey = 'value';
        class Parent {
            get value() { return 1; }
            static get value() { return 2; }
        }
        class InstanceDynamic extends Parent { [externalKey] = 3; }
        class StaticDynamic extends Parent { static [externalKey] = 3; }
        class Known extends Parent { unrelated = 3; }
        function use() {
            const instance = new InstanceDynamic();
            const statics = new StaticDynamic();
            const known = new Known();
            void instance.value;
            void InstanceDynamic.value;
            void statics.value;
            void StaticDynamic.value;
            void known.value;
            void Known.value;
        }
    "#,
    );
    let targets = graph
        .edges
        .iter()
        .filter(|edge| edge.from.ends_with(".use") && edge.to.ends_with("#get"))
        .map(|edge| edge.to.as_ref())
        .collect::<Vec<_>>();
    assert_eq!(
        targets
            .iter()
            .filter(|target| target.ends_with(".value#get"))
            .count(),
        2,
        "{targets:?}"
    );
    assert_eq!(
        targets
            .iter()
            .filter(|target| target.ends_with(".value#static#get"))
            .count(),
        2,
        "{targets:?}"
    );
}
