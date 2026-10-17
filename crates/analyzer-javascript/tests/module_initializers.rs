use analyzer_core::{graph::CallGraph, Config, LanguageRegistry, LanguageStrategy};
use analyzer_javascript::JavaScript;
use std::path::Path;

#[test]
fn source_ordered_module_calls_resolve_without_stealing_the_first_declarations_binding() {
    let source = "function leaf() {}\nconst callback=()=>{leaf();};\ncallback();\nconst alias=leaf;\nalias();\n";
    let file = JavaScript.analyze(source, Path::new("app.js"));
    assert!(file.parse_errors.is_empty());
    let leaf = file.functions.iter().find(|f| f.name == "leaf").unwrap();
    assert!(leaf.calls.is_empty());
    assert_eq!(leaf.range.start_line, 1);
    let initializer = file
        .functions
        .iter()
        .find(|f| f.is_module_initializer())
        .unwrap();
    assert_eq!(
        initializer
            .calls
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["callback", "alias"]
    );
    let callback = file
        .functions
        .iter()
        .find(|f| f.name == "callback")
        .unwrap();
    assert_eq!(callback.fqn, "app.callback");
    assert!(callback.parent_function.is_none());
    let mut registry = LanguageRegistry::new();
    registry.register(JavaScript);
    let graph = CallGraph::build(&[file], &registry, &Config::default());
    let edges = graph
        .edges
        .iter()
        .filter(|e| e.from.ends_with("app.<module_initializer>"))
        .collect::<Vec<_>>();
    assert_eq!(edges.len(), 2, "{edges:?}");
    assert!(edges
        .iter()
        .any(|e| e.to.ends_with("app.callback") && e.callsite.start_line == 3));
    assert!(edges
        .iter()
        .any(|e| e.to.ends_with("app.leaf") && e.callsite.start_line == 5));
}

#[test]
fn object_and_class_computed_keys_execute_at_definition_but_member_bodies_and_instance_values_wait()
{
    let source = "function key(){return 'value';} function body(){} function staticValue(){} function instanceValue(){}\nconst object={ [key()](){body();} };\nclass Example { [key()]=instanceValue(); static value=staticValue(); method(){body();} }\n";
    let file = JavaScript.analyze(source, Path::new("app.js"));
    assert!(file.parse_errors.is_empty());
    let initializer = file
        .functions
        .iter()
        .find(|f| f.is_module_initializer())
        .unwrap();
    assert_eq!(
        initializer
            .calls
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["key", "key", "staticValue"]
    );
    assert!(!initializer
        .calls
        .iter()
        .any(|c| matches!(c.name.as_str(), "body" | "instanceValue")));
}
