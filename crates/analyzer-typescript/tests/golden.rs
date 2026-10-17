use analyzer_core::LanguageStrategy;
use analyzer_typescript::TypeScript;
use std::path::Path;

#[test]
fn typescript_architecture_golden() {
    let src = include_str!("fixtures/architecture.ts");
    let p = Path::new("architecture.ts");
    let s = TypeScript;
    let ast = s.parse(src, p);
    let file = s.normalize(src, p, &ast);
    assert!(ast.errors.is_empty());
    assert_eq!(file.types.len(), 2);
    assert_eq!(
        file.functions
            .iter()
            .filter(|f| f.name == "overload")
            .count(),
        1
    );
    assert_eq!(
        file.types
            .iter()
            .find(|t| t.name == "Store")
            .unwrap()
            .fields
            .len(),
        1
    );
    assert!(file
        .functions
        .iter()
        .find(|f| f.name == "constructor")
        .unwrap()
        .accessed_fields
        .contains(&"url".to_string()));
    assert!(file.functions.iter().any(|f| f.name == "entry"
        && f.calls
            .iter()
            .any(|c| c.name == "query" && !c.in_loop.is_empty() && !c.guard.is_empty())));
    assert!(file
        .functions
        .iter()
        .find(|f| f.name == "entry")
        .unwrap()
        .calls
        .iter()
        .any(|c| c.name == "query" && c.targets.contains(&"architecture.Store.query".to_string())));
}

#[test]
fn structural_scan_keeps_nested_functions_without_retaining_duplicate_asts() {
    let source = "function outer(flag: boolean) { function inner(x: number) { if (x > 0) { sink(x); } return x; } if (flag) { inner(1); } }";
    let path = Path::new("nested.ts");
    let strategy = TypeScript;
    let detailed = strategy.parse(source, path);
    let previous_path = strategy.normalize(source, path, &detailed);
    let structural = strategy.analyze(source, path);
    assert_eq!(structural.functions.len(), 2);
    assert!(structural
        .functions
        .iter()
        .all(|f| f.normalized_ast.is_none()));
    for (actual, expected) in structural.functions.iter().zip(&previous_path.functions) {
        assert_eq!(actual.fqn, expected.fqn);
        assert_eq!(actual.range, expected.range);
        assert_eq!(actual.complexity, expected.complexity);
        assert_eq!(actual.nesting_depth, expected.nesting_depth);
        assert_eq!(actual.return_points, expected.return_points);
        assert_eq!(
            format!("{:?}", actual.calls),
            format!("{:?}", expected.calls)
        );
    }
    let inner = structural
        .functions
        .iter()
        .find(|f| f.name == "inner")
        .unwrap();
    assert!(inner
        .calls
        .iter()
        .any(|call| call.name == "sink" && !call.guard.is_empty()));
    let mut function_nodes = Vec::new();
    detailed.root.find(
        &|node| node.kind == analyzer_core::AstKind::Function,
        &mut function_nodes,
    );
    assert_eq!(function_nodes.len(), 2);
    assert!(function_nodes
        .iter()
        .any(|node| node.range == inner.range && !node.children.is_empty()));
    // Explicit AST retrieval still carries every detailed identifier/range.
    let mut identifiers = Vec::new();
    detailed.root.find(
        &|node| node.text.as_deref() == Some("sink"),
        &mut identifiers,
    );
    assert_eq!(identifiers.len(), 1);
    assert_eq!(identifiers[0].range.file.as_ref(), "nested.ts");
}

#[test]
fn lexical_contexts_are_shared_by_calls_and_graph_edges() {
    let source =
        "function outer(flag: boolean) { while (flag) { if (flag) { sink(1); sink(2); } } }";
    let file = TypeScript.analyze(source, Path::new("context.ts"));
    let calls = &file.functions[0].calls;
    assert_eq!(calls.len(), 2);
    assert!(!calls[0].guard.is_empty());
    assert!(!calls[0].in_loop.is_empty());
    assert!(std::sync::Arc::ptr_eq(&calls[0].guard, &calls[1].guard));
    assert!(std::sync::Arc::ptr_eq(&calls[0].in_loop, &calls[1].in_loop));
    let mut registry = analyzer_core::LanguageRegistry::new();
    registry.register(TypeScript);
    let graph = analyzer_core::graph::CallGraph::build(
        &[file.clone()],
        &registry,
        &analyzer_core::Config::default(),
    );
    for call in calls {
        let edge = graph
            .edges
            .iter()
            .find(|edge| edge.callsite == call.range)
            .unwrap();
        assert!(std::sync::Arc::ptr_eq(&call.guard, &edge.guard));
        assert!(std::sync::Arc::ptr_eq(&call.in_loop, &edge.in_loop));
    }
}
