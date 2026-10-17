use analyzer_core::{
    config::Config,
    graph::{CallGraph, GraphEdge, GraphNode, NodeKind, RelationOptions},
    ir::{Confidence, SourceRange},
};

fn node(id: &str, file: &str, module: &str) -> GraphNode {
    GraphNode {
        id: id.into(),
        label: format!("{module}::{id}"),
        kind: NodeKind::Function,
        file: Some(file.into()),
        line: Some(1),
        language: Some("rust".into()),
        module: Some(module.into()),
        parent_type: None,
        vendor: false,
        package: None,
        category: None,
        library: None,
        operation: None,
        argument: None,
        algorithmic_complexity: None,
        cargo_target: None,
        cargo_target_id: None,
        source_fqn: None,
    }
}

fn edge(from: &str, to: &str, file: &str, line: usize) -> GraphEdge {
    GraphEdge {
        from: from.into(),
        to: to.into(),
        callsite: SourceRange {
            file: file.into(),
            start_line: line,
            ..SourceRange::default()
        },
        confidence: Confidence::Resolved,
        guard: vec![].into(),
        in_loop: vec![].into(),
        recursive: false,
        hidden_vendor: false,
        argument: None,
    }
}

fn graph() -> CallGraph {
    let mut graph = CallGraph::default();
    graph.nodes = vec![
        node("a", "src/a.rs", "app::a"),
        node("m", "middle/m.rs", "middle"),
        node("b", "other/b.rs", "app::b"),
        node("decoy", "src2/decoy.rs", "app::ab"),
    ];
    graph.edges = vec![
        edge("a", "m", "src/a.rs", 3),
        edge("m", "b", "middle/m.rs", 7),
    ];
    graph.reindex();
    graph
}

#[test]
fn transitive_relations_preserve_original_source_and_every_real_callsite() {
    let graph = graph();
    let result = graph.relations(&["dir:src".into()], &["dir:other".into()], Some(2));
    assert_eq!(result.a, ["a"]);
    assert!(
        result.a_to_b.is_empty(),
        "indirect routes are not real direct calls"
    );
    let path = &result.paths_a_to_b[0];
    assert_eq!((&*path.from, &*path.to), ("a", "b"));
    assert_eq!(path.nodes, ["a", "m", "b"]);
    assert_eq!(
        path.edges
            .iter()
            .map(|edge| edge.callsite.start_line)
            .collect::<Vec<_>>(),
        [3, 7]
    );
    assert_eq!(path.edges[0].from.as_ref(), "a");
    assert_eq!(path.edges[1].from.as_ref(), "m");
    assert_eq!(result.matrix["src/a.rs"]["other/b.rs"], 1);
    assert!(!result.matrix.contains_key("middle/m.rs"));
    assert_eq!(
        result
            .nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect::<Vec<_>>(),
        ["a", "b", "m"]
    );
    assert!(graph
        .relations(&["dir:src".into()], &["dir:other".into()], Some(1))
        .paths_a_to_b
        .is_empty());
}

#[test]
fn repeated_direct_calls_keep_evidence_without_duplicate_indirect_endpoint_pairs() {
    let mut graph = graph();
    graph.edges.extend([
        edge("a", "b", "src/a.rs", 9),
        edge("a", "b", "src/a.rs", 10),
    ]);
    graph.reindex();
    let result = graph.relations(&["dir:src".into()], &["dir:other".into()], Some(4));
    assert_eq!(result.a_to_b.len(), 2);
    assert_eq!(result.paths_a_to_b.len(), 2);
    assert!(result.paths_a_to_b.iter().all(|path| path.edges.len() == 1));
    assert_eq!(result.matrix["src/a.rs"]["other/b.rs"], 2);
}

#[test]
fn named_sets_exclusions_vendor_selection_and_preview_counts() {
    let mut graph = graph();
    let mut vendor = node("v", "node_modules/acme/index.js", "acme");
    vendor.vendor = true;
    vendor.package = Some("acme".into());
    vendor.parent_type = Some("acme.Client".into());
    graph.nodes.push(vendor);
    graph.reindex();
    let mut config = Config::default();
    config.sets.insert(
        "project".into(),
        vec!["glob:**".into(), "!dir:middle".into()],
    );
    config
        .sets
        .insert("library".into(), vec!["vendor:acme".into()]);
    let result = graph.relations_with_config(
        &RelationOptions {
            a: vec!["set:project".into()],
            b: vec!["library".into()],
            exclude_a: vec!["dir:src2".into()],
            ..RelationOptions::default()
        },
        &config,
    );
    assert_eq!(result.a, ["a", "b"]);
    assert_eq!(result.b, ["v"]);
    assert_eq!(result.counts_a.functions, 2);
    assert_eq!(result.counts_a.files, 2);
    assert_eq!(result.counts_a.vendor, 0);
    assert_eq!(result.counts_b.classes, 1);
    assert_eq!(result.counts_b.vendor, 1);
}

#[test]
fn missing_or_recursive_sets_report_warnings_and_keep_valid_members() {
    let graph = graph();
    let mut config = Config::default();
    config
        .sets
        .insert("first".into(), vec!["set:second".into(), "dir:src".into()]);
    config
        .sets
        .insert("second".into(), vec!["set:first".into()]);
    let result = graph.relations_with_config(
        &RelationOptions {
            a: vec!["set:first".into(), "set:missing".into()],
            ..RelationOptions::default()
        },
        &config,
    );
    assert_eq!(result.a, ["a"]);
    assert_eq!(result.warnings.len(), 2);
    assert!(result
        .warnings
        .iter()
        .any(|warning| warning.contains("first -> second -> first")));
    assert!(result
        .warnings
        .iter()
        .any(|warning| warning.contains("unknown named set: missing")));
}

#[test]
fn namespace_and_directory_selectors_require_component_boundaries() {
    let mut graph = graph();
    graph.nodes.extend([
        node("dot", "x.py", "app.users"),
        node("dot_decoy", "y.py", "app.users_old"),
        node("php", "x.php", "App\\Users"),
        node("php_decoy", "y.php", "App\\UsersOld"),
    ]);
    graph.reindex();
    for (selector, expected) in [
        ("mod:app::a", "a"),
        ("pkg:app.users", "dot"),
        ("ns:App\\Users", "php"),
        ("dir:src/", "a"),
    ] {
        assert_eq!(graph.relations(&[selector.into()], &[], None).a, [expected]);
    }
}

#[test]
fn cycle_does_not_change_relation_origin_or_prevent_reverse_paths() {
    let mut graph = graph();
    graph.edges.extend([
        edge("m", "a", "middle/m.rs", 8),
        edge("b", "m", "other/b.rs", 5),
    ]);
    graph.reindex();
    let result = graph.relations(&["dir:src".into()], &["dir:other".into()], Some(100));
    assert_eq!(result.paths_a_to_b.len(), 1);
    assert_eq!(result.paths_b_to_a.len(), 1);
    assert_eq!(result.paths_b_to_a[0].nodes, ["b", "m", "a"]);
    assert_eq!(result.matrix["src/a.rs"]["other/b.rs"], 1);
    assert_eq!(result.matrix["other/b.rs"]["src/a.rs"], 1);
}

#[test]
fn overlap_matrix_counts_distinct_callsites_once_at_every_level() {
    let mut graph = graph();
    graph.edges = vec![
        edge("a", "b", "src/a.rs", 3),
        edge("a", "b", "src/a.rs", 7),
        edge("b", "a", "other/b.rs", 9),
    ];
    graph
        .nodes
        .iter_mut()
        .find(|node| node.id == "a")
        .unwrap()
        .parent_type = Some("app.A".into());
    graph
        .nodes
        .iter_mut()
        .find(|node| node.id == "b")
        .unwrap()
        .parent_type = Some("app.B".into());
    graph.reindex();
    let selectors = ["glob:**".into()];
    let mut result = graph.relations(&selectors, &selectors, None);
    assert_eq!(result.a_to_b.len(), 3);
    assert_eq!(result.b_to_a.len(), 3);
    assert_eq!(result.paths_a_to_b.len(), 3);
    assert_eq!(result.paths_b_to_a.len(), 3);
    assert_eq!(result.matrix["app.A"]["app.B"], 2);
    assert_eq!(result.matrix["app.B"]["app.A"], 1);
    result.regroup_matrix("file");
    assert_eq!(result.matrix["src/a.rs"]["other/b.rs"], 2);
    result.regroup_matrix("function");
    assert_eq!(result.matrix["app::a::a"]["app::b::b"], 2);
    assert_eq!(result.matrix["app::b::b"]["app::a::a"], 1);
}

#[test]
fn overlap_matrix_counts_the_same_indirect_route_once() {
    let graph = graph();
    let selectors = ["glob:**".into()];
    let result = graph.relations(&selectors, &selectors, Some(2));
    assert_eq!(result.paths_a_to_b.len(), 3);
    assert_eq!(result.paths_b_to_a.len(), 3);
    assert_eq!(result.matrix["src/a.rs"]["other/b.rs"], 1);
    assert_eq!(result.matrix["src/a.rs"]["middle/m.rs"], 1);
    assert_eq!(result.matrix["middle/m.rs"]["other/b.rs"], 1);
}

#[test]
fn matrix_uses_paths_as_authority_and_only_falls_back_for_legacy_results() {
    let mut graph = graph();
    graph.edges = vec![edge("a", "b", "src/a.rs", 3), edge("a", "b", "src/a.rs", 7)];
    graph.reindex();
    let mut result = graph.relations(&["glob:**".into()], &["glob:**".into()], None);
    result.a_to_b.push(edge("a", "b", "src/a.rs", 100));
    result.regroup_matrix("file");
    assert_eq!(
        result.matrix["src/a.rs"]["other/b.rs"], 2,
        "direct fields must not duplicate authoritative paths"
    );
    result.paths_a_to_b.clear();
    result.paths_b_to_a.clear();
    result.routes_evaluated = false;
    result.a_to_b.push(edge("a", "b", "src/a.rs", 3));
    result.regroup_matrix("file");
    assert_eq!(
        result.matrix["src/a.rs"]["other/b.rs"], 3,
        "legacy identical callsites count once; distinct ones remain"
    );
}

#[test]
fn route_modes_retain_parallel_callsites_guards_and_nonshortest_simple_routes() {
    use analyzer_core::graph::RelationPathMode;
    let mut graph = graph();
    graph.nodes.push(node("n", "middle/n.rs", "middle"));
    let mut guarded = edge("a", "m", "src/a.rs", 4);
    guarded.guard = vec![analyzer_core::ir::Guard {
        normalized: "enabled".into(),
        ..Default::default()
    }]
    .into();
    graph.edges.extend([
        guarded,
        edge("a", "n", "src/a.rs", 5),
        edge("n", "b", "middle/n.rs", 6),
        edge("m", "n", "middle/m.rs", 8),
        edge("n", "a", "middle/n.rs", 9),
    ]);
    graph.reindex();
    let mut options = RelationOptions {
        a: vec!["dir:src".into()],
        b: vec!["dir:other".into()],
        transitive_depth: Some(4),
        ..Default::default()
    };
    let one = graph.relations_with_config(&options, &Config::default());
    assert_eq!(one.paths_a_to_b.len(), 1);
    options.path_mode = RelationPathMode::AllShortest;
    let all = graph.relations_with_config(&options, &Config::default());
    assert_eq!(all.paths_a_to_b.len(), 3);
    assert!(all.paths_a_to_b.iter().all(|path| path.edges.len() == 2));
    assert!(all
        .paths_a_to_b
        .iter()
        .any(|path| !path.edges[0].guard.is_empty()));
    assert_eq!(all.route_count, 3);
    assert_eq!(all.direct_call_count, 0);
    options.path_mode = RelationPathMode::Simple;
    let simple = graph.relations_with_config(&options, &Config::default());
    assert_eq!(simple.paths_a_to_b.len(), 5);
    assert!(simple.paths_a_to_b.iter().all(|path| path
        .nodes
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        == path.nodes.len()));
    assert!(simple.paths_a_to_b.iter().any(|path| path.edges.len() == 3));
    assert!(!simple.truncated);
    assert_eq!(
        serde_json::to_value(&simple).unwrap(),
        serde_json::to_value(graph.relations_with_config(&options, &Config::default())).unwrap()
    );
}

#[test]
fn simple_mode_retains_longer_routes_even_when_pair_has_a_direct_call() {
    use analyzer_core::graph::RelationPathMode;
    let mut graph = graph();
    graph.edges.push(edge("a", "b", "src/a.rs", 8));
    graph.reindex();
    let options = RelationOptions {
        a: vec!["dir:src".into()],
        b: vec!["dir:other".into()],
        transitive_depth: Some(4),
        path_mode: RelationPathMode::Simple,
        ..Default::default()
    };
    let result = graph.relations_with_config(&options, &Config::default());
    assert_eq!(result.route_count, 2);
    assert_eq!(result.direct_call_count, 1);
    assert_eq!(result.matrix["src/a.rs"]["other/b.rs"], 2);
    assert_eq!(result.direct_call_matrix["src/a.rs"]["other/b.rs"], 1);
}

#[test]
fn route_limits_stop_enumeration_and_leave_complete_direct_call_counts() {
    use analyzer_core::graph::RelationPathMode;
    let mut graph = graph();
    graph
        .edges
        .extend([edge("a", "b", "src/a.rs", 8), edge("a", "b", "src/a.rs", 9)]);
    graph.reindex();
    let mut options = RelationOptions {
        a: vec!["dir:src".into()],
        b: vec!["dir:other".into()],
        transitive_depth: Some(4),
        path_mode: RelationPathMode::Simple,
        max_paths_per_pair: 1,
        ..Default::default()
    };
    let result = graph.relations_with_config(&options, &Config::default());
    assert!(result.truncated);
    assert!(result
        .truncation_reasons
        .iter()
        .any(|reason| reason == "max_paths_per_pair"));
    assert_eq!(result.a_to_b.len(), 2);
    assert_eq!(result.direct_call_count, 2);
    assert_eq!(result.route_count, 1);
    options.max_paths_per_pair = 100;
    options.max_paths_total = 1;
    let result = graph.relations_with_config(&options, &Config::default());
    assert_eq!(result.paths_a_to_b.len() + result.paths_b_to_a.len(), 1);
    assert!(result
        .truncation_reasons
        .iter()
        .any(|reason| reason == "max_paths_total"));
    options.work_budget = 0;
    let result = graph.relations_with_config(&options, &Config::default());
    assert_eq!(result.work_used, 0);
    assert_eq!(result.route_count, 0);
    assert_eq!(result.direct_call_count, 2);
    assert_eq!(result.matrix["src/a.rs"]["other/b.rs"], 0);
    assert!(result
        .truncation_reasons
        .iter()
        .any(|reason| reason == "work_budget"));
}

#[test]
fn dense_route_search_obeys_work_budget_during_enumeration() {
    use analyzer_core::graph::RelationPathMode;
    let mut graph = graph();
    graph.edges.clear();
    for index in 0..12 {
        graph
            .nodes
            .push(node(&format!("x{index}"), "middle/x.rs", "middle"));
    }
    graph.edges.push(edge("a", "x0", "src/a.rs", 1));
    for left in 0..12 {
        for right in left + 1..12 {
            graph.edges.push(edge(
                &format!("x{left}"),
                &format!("x{right}"),
                "middle/x.rs",
                left + 1,
            ));
        }
        graph
            .edges
            .push(edge(&format!("x{left}"), "b", "middle/x.rs", 20));
    }
    graph.reindex();
    let options = RelationOptions {
        a: vec!["dir:src".into()],
        b: vec!["dir:other".into()],
        transitive_depth: Some(100),
        path_mode: RelationPathMode::Simple,
        work_budget: 40,
        ..Default::default()
    };
    let result = graph.relations_with_config(&options, &Config::default());
    assert!(result.truncated);
    assert!(result.work_used <= 40);
    assert!(
        result
            .paths_a_to_b
            .iter()
            .map(|path| path.edges.len())
            .sum::<usize>()
            <= 40
    );
}

#[test]
fn disconnected_wide_sets_cannot_bypass_default_route_work_budget() {
    let mut graph = CallGraph::default();
    for index in 0..10 {
        graph
            .nodes
            .push(node(&format!("a{index}"), "src/a.rs", "app::a"));
    }
    for index in 0..100 {
        graph
            .nodes
            .push(node(&format!("b{index}"), "other/b.rs", "app::b"));
    }
    graph.reindex();
    let result = graph.relations_with_config(
        &RelationOptions {
            a: vec!["dir:src".into()],
            b: vec!["dir:other".into()],
            transitive_depth: Some(2),
            work_budget: 40,
            ..Default::default()
        },
        &Config::default(),
    );
    assert_eq!(result.a.len(), 10);
    assert_eq!(result.b.len(), 100);
    assert_eq!(result.route_count, 0);
    assert_eq!(result.direct_call_count, 0);
    assert_eq!(result.work_used, 40);
    assert!(result.truncated);
    assert_eq!(result.truncation_reasons, ["work_budget"]);
}
