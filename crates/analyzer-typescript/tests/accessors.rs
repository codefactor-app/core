use analyzer_core::{graph::CallGraph, Config, LanguageRegistry, LanguageStrategy};
use analyzer_typescript::TypeScript;
use std::path::Path;

#[test]
fn getter_and_setter_have_distinct_stable_graph_nodes_and_calls() {
    for methods in [
        "get value(): number { return readValue(); } set value(next: number) { writeValue(next); }",
        "set value(next: number) { writeValue(next); } get value(): number { return readValue(); }",
    ] {
        let source = format!("function readValue(): number {{ return 1; }} function writeValue(n: number): void {{}} class Store {{ {methods} }}");
        let file = TypeScript.analyze(&source, Path::new("accessors.ts"));
        assert!(file.parse_errors.is_empty());
        let getter = file
            .functions
            .iter()
            .find(|f| f.fqn == "accessors.Store.value#get")
            .unwrap();
        let setter = file
            .functions
            .iter()
            .find(|f| f.fqn == "accessors.Store.value#set")
            .unwrap();
        assert_eq!(getter.name, "value");
        assert_eq!(setter.name, "value");
        assert!(getter
            .modifiers
            .iter()
            .any(|modifier| modifier == "accessor"));
        assert!(setter
            .modifiers
            .iter()
            .any(|modifier| modifier == "accessor"));
        assert!(getter.calls.iter().any(|call| call.name == "readValue"));
        assert!(setter.calls.iter().any(|call| call.name == "writeValue"));
        assert_eq!(file.types[0].methods.len(), 2);
        let mut registry = LanguageRegistry::new();
        registry.register(TypeScript);
        let graph = CallGraph::build(&[file], &registry, &Config::default());
        for (accessor, callee) in [("value#get", "readValue"), ("value#set", "writeValue")] {
            let node = graph
                .nodes
                .iter()
                .find(|node| node.label == format!("accessors.Store.{accessor}"))
                .unwrap();
            assert!(graph
                .edges
                .iter()
                .any(|edge| edge.from.as_ref() == node.id && edge.to.ends_with(callee)));
        }
    }
}

#[test]
fn overload_signatures_are_collapsed_while_named_get_and_set_methods_are_ordinary_methods() {
    let source = "class Store { work(n: number): number; work(n: string): string; work(n: any): any { return dispatch(n); } get() { return readValue(); } set() { writeValue(); } }";
    let file = TypeScript.analyze(source, Path::new("methods.ts"));
    assert!(file.parse_errors.is_empty());
    let overloads = file
        .functions
        .iter()
        .filter(|f| f.name == "work")
        .collect::<Vec<_>>();
    assert_eq!(overloads.len(), 1);
    assert_eq!(overloads[0].fqn, "methods.Store.work");
    assert!(overloads[0]
        .calls
        .iter()
        .any(|call| call.name == "dispatch"));
    for name in ["get", "set"] {
        let method = file.functions.iter().find(|f| f.name == name).unwrap();
        assert_eq!(method.fqn, format!("methods.Store.{name}"));
        assert!(!method
            .modifiers
            .iter()
            .any(|modifier| modifier == "accessor"));
    }
}

#[test]
fn nested_functions_in_a_setter_keep_the_setter_as_their_owner() {
    let file = TypeScript.analyze("class Store { get value() { return 1; } set value(n: number) { function validate() { check(n); } validate(); } }", Path::new("nested.ts"));
    let nested = file
        .functions
        .iter()
        .find(|f| f.name == "validate")
        .unwrap();
    assert_eq!(nested.fqn, "nested.Store.value#set.validate");
    assert_eq!(
        nested.parent_function.as_deref(),
        Some("nested.Store.value#set")
    );
    let setter = file
        .functions
        .iter()
        .find(|f| f.fqn.ends_with("value#set"))
        .unwrap();
    assert!(setter
        .calls
        .iter()
        .any(|call| call.targets.contains(&nested.fqn)));
}

#[test]
fn invoking_a_callable_property_reads_the_getter_and_never_calls_its_setter() {
    for methods in [
        "get value(): () => number { return readValue; } set value(next: () => number) { writeValue(next); }",
        "set value(next: () => number) { writeValue(next); } get value(): () => number { return readValue; }",
    ] {
        let source = format!("function readValue(): number {{ return 1; }} function writeValue(next: () => number) {{}} class Store {{ {methods} }} function useValue(store: Store) {{ return store.value(); }}");
        let file = TypeScript.analyze(&source, Path::new("property.ts"));
        assert!(file.parse_errors.is_empty());
        let caller = file.functions.iter().find(|function| function.name == "useValue").unwrap();
        assert_eq!(caller.calls.len(), 1);
        assert_eq!(caller.calls[0].targets, vec!["property.Store.value#get"]);
        let mut registry = LanguageRegistry::new();
        registry.register(TypeScript);
        let graph = CallGraph::build(&[file], &registry, &Config::default());
        assert!(graph.edges.iter().any(|edge| edge.from.ends_with(".useValue") && edge.to.ends_with(".value#get")));
        assert!(!graph.edges.iter().any(|edge| edge.from.ends_with(".useValue") && edge.to.ends_with(".value#set")));
    }
}
