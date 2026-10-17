use analyzer_core::graph::{CallGraph, GraphEdge, GraphNode, NodeKind};

fn node(id: &str, label: &str, file: &str, line: usize) -> GraphNode {
    GraphNode {
        id: id.into(),
        label: label.into(),
        kind: NodeKind::Function,
        file: Some(file.into()),
        line: Some(line),
        language: Some("python".into()),
        module: Some("pkg".into()),
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

#[test]
fn selects_exact_wildcard_and_file_line() {
    let mut graph = CallGraph::default();
    graph.nodes = vec![
        node("first", "pkg.service.load", "src/service.py", 7),
        node("second", "pkg.service.save", "src/service.py", 20),
        node("third", "pkg.other.load", "src/other.py", 3),
    ];
    graph.reindex();
    assert_eq!(graph.select("pkg.service.load"), vec!["first"]);
    assert_eq!(graph.select("pkg.service.*"), vec!["first", "second"]);
    assert_eq!(graph.select("pkg.*.load"), vec!["first", "third"]);
    assert_eq!(graph.select("src/service.py:20"), vec!["second"]);
}

fn edge(from: &str, to: &str) -> GraphEdge {
    GraphEdge {
        from: from.into(),
        to: to.into(),
        callsite: analyzer_core::ir::SourceRange::default(),
        confidence: analyzer_core::ir::Confidence::Resolved,
        guard: vec![].into(),
        in_loop: vec![].into(),
        recursive: false,
        hidden_vendor: false,
        argument: None,
    }
}

#[test]
fn cycle_projection_preserves_bounded_reachability_and_is_acyclic() {
    use analyzer_core::{
        graph::{Direction, GraphFilter},
        Config,
    };
    use std::collections::{BTreeSet, VecDeque};
    fn acyclic(nodes: &BTreeSet<String>, edges: &[(String, String)]) -> bool {
        let mut pending = nodes.clone();
        loop {
            let roots = pending
                .iter()
                .filter(|node| {
                    !edges
                        .iter()
                        .any(|(from, to)| to == *node && pending.contains(from))
                })
                .cloned()
                .collect::<Vec<_>>();
            if roots.is_empty() {
                return pending.is_empty();
            }
            for root in roots {
                pending.remove(&root);
            }
        }
    }
    for seed in 0..24usize {
        let mut graph = CallGraph::default();
        graph.nodes = (0..7)
            .map(|i| node(&format!("n{i}"), &format!("n{i}"), "a.py", i + 1))
            .collect();
        for from in 0..7 {
            for to in 0..7 {
                if ((seed + 1) * (from + 3) * 17
                    + (seed * seed + 5) * (to + 7) * 19
                    + from * to * 11)
                    % 13
                    < 4
                {
                    graph
                        .edges
                        .push(edge(&format!("n{from}"), &format!("n{to}")));
                }
            }
        }
        graph.reindex();
        for direction in [Direction::Callees, Direction::Callers, Direction::Both] {
            for depth in [Some(0), Some(1), Some(3), None] {
                let mut reachable = BTreeSet::from([String::from("n0")]);
                let mut selected = BTreeSet::new();
                let directions = if direction == Direction::Both {
                    vec![Direction::Callees, Direction::Callers]
                } else {
                    vec![direction]
                };
                for walk in directions {
                    let mut queue = VecDeque::from([(String::from("n0"), 0usize)]);
                    let mut visited = BTreeSet::from([String::from("n0")]);
                    while let Some((current, distance)) = queue.pop_front() {
                        if depth.is_some_and(|limit| distance >= limit) {
                            continue;
                        }
                        for e in &graph.edges {
                            let (from, to) = if walk == Direction::Callers {
                                (&e.to, &e.from)
                            } else {
                                (&e.from, &e.to)
                            };
                            if from.as_ref() != current {
                                continue;
                            }
                            selected.insert((e.from.to_string(), e.to.to_string()));
                            reachable.insert(to.to_string());
                            if visited.insert(to.to_string()) {
                                queue.push_back((to.to_string(), distance + 1));
                            }
                        }
                    }
                }
                let query = graph.query(
                    "n0",
                    direction,
                    depth,
                    &GraphFilter::all(),
                    &Config::default(),
                );
                let visible = query
                    .edges
                    .iter()
                    .map(|e| (e.from.to_string(), e.to.to_string()))
                    .collect::<Vec<_>>();
                let evidence = query
                    .edges
                    .iter()
                    .chain(&query.cycles)
                    .map(|e| (e.from.to_string(), e.to.to_string()))
                    .collect::<BTreeSet<_>>();
                assert_eq!(
                    query
                        .nodes
                        .iter()
                        .map(|n| n.id.clone())
                        .collect::<BTreeSet<_>>(),
                    reachable
                );
                assert_eq!(evidence, selected, "seed={seed}, {direction:?}, {depth:?}");
                assert!(acyclic(&reachable, &visible));
                assert_eq!(
                    query.cycles.is_empty(),
                    acyclic(&reachable, &selected.into_iter().collect::<Vec<_>>())
                );
            }
        }
    }
}

#[test]
fn shortest_paths_include_zero_length_paths_for_overlapping_selectors() {
    let mut graph = CallGraph::default();
    graph.nodes = vec![node("a", "pkg.a", "a.py", 1), node("b", "pkg.b", "b.py", 1)];
    graph.edges = vec![edge("a", "b")];
    graph.reindex();
    let result = graph.paths("pkg.*", "pkg.b", true, 10);
    assert_eq!(result.paths, vec![vec![String::from("b")]]);
    assert!(!result.truncated);
}

#[test]
fn all_paths_count_symbol_routes_once_with_repeated_calls() {
    let mut graph = CallGraph::default();
    graph.nodes = vec![node("a", "pkg.a", "a.py", 1), node("b", "pkg.b", "b.py", 1)];
    graph.edges = vec![edge("a", "b"), edge("a", "b")];
    graph.reindex();
    let result = graph.paths("pkg.a", "pkg.b", false, 1);
    assert_eq!(
        result.paths,
        vec![vec![String::from("a"), String::from("b")]]
    );
    assert!(
        !result.truncated,
        "repeated call sites are not additional symbol paths"
    );
}

#[test]
fn graph_json_roundtrip_preserves_query_indexes() {
    let mut graph = CallGraph::default();
    graph.nodes = vec![node("a", "pkg.a", "a.py", 1), node("b", "pkg.b", "b.py", 1)];
    graph.edges = vec![edge("a", "b")];
    graph.reindex();
    let json = serde_json::to_string(&graph).unwrap();
    let restored: CallGraph = serde_json::from_str(&json).unwrap();
    assert_eq!(
        restored.paths("pkg.a", "pkg.b", true, 10).paths,
        vec![vec![String::from("a"), String::from("b")]]
    );
    assert_eq!(restored.node("a").unwrap().label, "pkg.a");
    assert_eq!(restored.outgoing_edges("a").count(), 1);
    assert_eq!(restored.outgoing_edges("missing").count(), 0);
}

#[test]
fn outgoing_index_preserves_edge_order_and_refreshes_after_mutation() {
    let mut graph = CallGraph::default();
    graph.nodes = vec![node("a", "pkg.a", "a.py", 1), node("b", "pkg.b", "b.py", 1)];
    graph.edges = vec![
        edge("a", "b"),
        edge("b", "a"),
        edge("a", "a"),
        edge("a", "b"),
    ];
    for round in 0..2 {
        graph.reindex();
        for id in ["a", "b", "missing"] {
            let expected = graph
                .edges
                .iter()
                .filter(|e| e.from.as_ref() == id)
                .collect::<Vec<_>>();
            assert_eq!(
                serde_json::to_value(graph.outgoing_edges(id).collect::<Vec<_>>()).unwrap(),
                serde_json::to_value(expected).unwrap()
            );
        }
        if round == 0 {
            graph.edges = vec![edge("b", "b")];
        }
    }
}

#[test]
fn directory_filter_respects_path_components() {
    use analyzer_core::{
        config::Config,
        graph::{Direction, GraphFilter},
    };
    let mut graph = CallGraph::default();
    graph.nodes = vec![
        node("a", "pkg.a", "src/a.py", 1),
        node("b", "pkg.b", "src-old/b.py", 1),
    ];
    graph.edges = vec![edge("a", "b")];
    graph.reindex();
    let filter = GraphFilter {
        directory: Some("src".into()),
        ..GraphFilter::all()
    };
    let result = graph.query(
        "pkg.a",
        Direction::Callees,
        None,
        &filter,
        &Config::default(),
    );
    assert!(result.edges.is_empty());
}

#[test]
fn class_selector_matches_only_exact_parent_type() {
    let mut graph = CallGraph::default();
    let mut first = node("a", "pkg.Service.run", "a.py", 1);
    first.parent_type = Some("pkg.Service".into());
    let mut second = node("b", "pkg.ServiceOther.run", "a.py", 2);
    second.parent_type = Some("pkg.ServiceOther".into());
    graph.nodes = vec![first, second, node("c", "pkg.Service_helper", "a.py", 3)];
    graph.reindex();
    assert_eq!(graph.select("class:pkg.Service"), vec![String::from("a")]);
    assert!(graph.select("class:missing").is_empty());
}
