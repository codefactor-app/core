use analyzer_core::{
    graph::{CallGraph, Direction, GraphFilter},
    Config, LanguageRegistry, LanguageStrategy,
};
use analyzer_python::Python;
use std::path::Path;

#[test]
fn python_architecture_golden() {
    let src = include_str!("fixtures/architecture.py");
    let p = Path::new("architecture.py");
    let s = Python;
    let ast = s.parse(src, p);
    let file = s.normalize(src, p, &ast);
    assert!(ast.errors.is_empty());
    assert_eq!(file.functions.len(), 5);
    assert_eq!(file.types.len(), 2);
    assert!(file.functions.iter().any(|f| f.name == "run"
        && f.calls
            .iter()
            .any(|c| c.name == "query" && !c.in_loop.is_empty() && !c.guard.is_empty())));
    let service = file.types.iter().find(|t| t.name == "Service").unwrap();
    assert!(service
        .fields
        .iter()
        .any(|f| f.name == "store" && f.type_name.as_deref() == Some("Store")));
    let run = file.functions.iter().find(|f| f.name == "run").unwrap();
    assert!(run.accessed_fields.contains(&"store".to_string()));
    assert!(run.calls.iter().any(|c| c.name == "query"
        && c.receiver_type.as_deref() == Some("architecture.Store")
        && c.targets.contains(&"architecture.Store.query".to_string())));
    assert!(file
        .functions
        .iter()
        .find(|f| f.name == "entry")
        .unwrap()
        .calls
        .iter()
        .any(|c| c.name == "Store" && c.assignment_to.as_deref() == Some("store")));
    let mut registry = LanguageRegistry::new();
    registry.register(Python);
    let graph = CallGraph::build(&[file], &registry, &Config::default());
    let query = graph.query(
        "architecture.Service.run",
        Direction::Callees,
        None,
        &GraphFilter::all(),
        &Config::default(),
    );
    assert!(query
        .edges
        .iter()
        .any(|e| e.to.ends_with("architecture.Store.query")
            && !e.guard.is_empty()
            && !e.in_loop.is_empty()));
}
