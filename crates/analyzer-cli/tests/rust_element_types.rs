use analyzer_core::{
    graph::CallGraph, Confidence, Config, FileModel, LanguageRegistry, LanguageStrategy,
};
use std::path::Path;

fn analyze(source: &str) -> (FileModel, CallGraph) {
    let strategy = analyzer_rust::Rust;
    let file = strategy.analyze(source, Path::new("src/lib.rs"));
    assert!(file.parse_errors.is_empty(), "{:?}", file.parse_errors);
    let mut registry = LanguageRegistry::new();
    registry.register(strategy);
    let graph = CallGraph::build(std::slice::from_ref(&file), &registry, &Config::default());
    (file, graph)
}
fn method_targets(file: &FileModel, graph: &CallGraph, method: &str) -> Vec<(String, Confidence)> {
    file.functions
        .iter()
        .flat_map(|function| &function.calls)
        .filter(|call| call.name == method)
        .flat_map(|call| {
            graph
                .edges
                .iter()
                .filter(move |edge| edge.callsite == call.range)
        })
        .map(|edge| {
            (
                graph.node(&edge.to).unwrap().label.clone(),
                edge.confidence.clone(),
            )
        })
        .collect()
}

#[test]
fn borrowed_slice_element_establishes_the_local_method_receiver() {
    let (file, graph) = analyze("struct Pattern; impl Pattern { fn matches_with(&self){} }\nfn fill_todo(patterns: &[Pattern], idx: usize) { let pattern = &patterns[idx]; pattern.matches_with(); }\n");
    assert_eq!(
        method_targets(&file, &graph, "matches_with"),
        vec![(
            "src::lib::Pattern::matches_with".into(),
            Confidence::Resolved
        )]
    );
}

#[test]
fn vec_field_index_uses_the_element_type_and_a_proven_tuple_index() {
    let (file, graph) = analyze("struct Pattern; struct Error; struct Paths { dir_patterns: Vec<Pattern>, todo: Vec<Result<(u8, usize), Error>> }\nimpl Pattern { fn matches_with(&self){} }\nimpl Paths { fn next(&mut self) { let (path, mut idx) = match self.todo.pop().unwrap() { Ok(pair) => pair, Err(e) => return }; self.dir_patterns[idx].matches_with(); self.dir_patterns[idx+1].matches_with(); } }\n");
    assert_eq!(
        method_targets(&file, &graph, "matches_with"),
        vec![
            (
                "src::lib::Pattern::matches_with".into(),
                Confidence::Resolved
            );
            2
        ]
    );
}

#[test]
fn standard_iterator_combinators_type_their_closure_parameters() {
    let (file, graph) = analyze("struct Opt; struct OptGroup; struct Options { grps: Vec<OptGroup> }\nimpl OptGroup { fn long_to_short(&self) -> Opt { Opt } }\nimpl Options { fn parse(&self) { let opts: Vec<Opt> = self.grps.iter().filter(|x| { x.long_to_short(); true }).map(|x| x.long_to_short()).collect(); } }\n");
    assert_eq!(
        method_targets(&file, &graph, "long_to_short"),
        vec![
            (
                "src::lib::OptGroup::long_to_short".into(),
                Confidence::Resolved
            );
            2
        ]
    );
}

#[test]
fn arrays_references_and_known_for_elements_keep_their_type() {
    let (file, graph) = analyze("struct Pattern; impl Pattern { fn matches_with(&self){} }\nfn run(patterns: &[Pattern; 2]) { (&patterns[0]).matches_with(); for pattern in patterns.iter() { pattern.matches_with(); } }\n");
    assert_eq!(
        method_targets(&file, &graph, "matches_with"),
        vec![
            (
                "src::lib::Pattern::matches_with".into(),
                Confidence::Resolved
            );
            2
        ]
    );
}

#[test]
fn unknown_indexes_ranges_custom_vec_and_custom_iterators_do_not_guess_an_element() {
    for source in [
        "struct Pattern; impl Pattern { fn matches_with(&self){} } fn run(patterns: &[Pattern], index: Mystery) { patterns[index].matches_with(); patterns[..].matches_with(); }",
        "struct Pattern; struct Custom; impl Pattern { fn matches_with(&self){} } fn run(patterns: Custom) { patterns[0].matches_with(); }",
        "struct Pattern; struct Vec<T> { item: T } struct Custom; impl Pattern { fn matches_with(&self){} } impl<T> Vec<T> { fn iter(&self) -> Custom { Custom } } fn run(patterns: Vec<Pattern>) { patterns.iter().map(|x| x.matches_with()); patterns[0].matches_with(); }",
        "struct Pattern; struct Custom; impl Pattern { fn matches_with(&self){} } impl Custom { fn iter(&self) -> Self { Custom } } fn run(patterns: Custom) { patterns.iter().map(|x| x.matches_with()); }",
        "struct T; impl T { fn matches_with(&self){} } fn run<T>(patterns: Vec<T>) { patterns[0].matches_with(); patterns.iter().map(|x| x.matches_with()); }",
        "struct T; struct Custom; impl T { fn matches_with(&self){} } impl Custom { fn value<T>(&self) -> T { panic!() } } fn run(custom: Custom) { custom.value().matches_with(); }",
    ] {
        let (file, graph) = analyze(source);
        let targets = method_targets(&file, &graph, "matches_with");
        assert!(!targets.is_empty());
        assert!(targets.iter().all(|(_, confidence)| *confidence != Confidence::Resolved), "{source}: {targets:?}");
    }
}

#[test]
fn collection_field_elements_resolve_in_their_declaration_namespace() {
    let (file, graph) = analyze("struct Pattern; impl Pattern { fn matches_with(&self){} }\nmod nested { pub struct Pattern; impl Pattern { pub fn matches_with(&self){} } pub struct Patterns { pub items: Vec<Pattern> } }\nfn run(patterns: nested::Patterns, idx: usize) { patterns.items[idx].matches_with(); }\n");
    assert_eq!(
        method_targets(&file, &graph, "matches_with"),
        vec![(
            "src::lib::nested::Pattern::matches_with".into(),
            Confidence::Resolved
        )]
    );
}

#[test]
fn frozen_getopts_and_glob_element_receivers_are_resolved() {
    for (source, cases) in [
        (
            include_str!("call_resolution_corpus/projects/getopts/src/lib.rs"),
            vec![(441, "long_to_short", "src::lib::OptGroup::long_to_short")],
        ),
        (
            include_str!("call_resolution_corpus/projects/glob/src/lib.rs"),
            vec![
                (460, "matches_with", "src::lib::Pattern::matches_with"),
                (956, "matches_with", "src::lib::Pattern::matches_with"),
            ],
        ),
    ] {
        let strategy = analyzer_rust::Rust;
        let file = strategy.analyze(source, Path::new("src/lib.rs"));
        assert!(file.parse_errors.is_empty(), "{:?}", file.parse_errors);
        let mut config = Config::default();
        config.resolution.rust.target = Some("x86_64-unknown-linux-gnu".into());
        config.resolution.rust.features = Some(vec![]);
        config.resolution.rust.cfg = Some(vec![]);
        let mut registry = LanguageRegistry::new();
        registry.register(strategy);
        let graph = CallGraph::build(std::slice::from_ref(&file), &registry, &config);
        for (line, method, target) in cases {
            let call = file
                .functions
                .iter()
                .flat_map(|function| &function.calls)
                .find(|call| call.range.start_line == line && call.name == method)
                .unwrap();
            let edges = graph
                .edges
                .iter()
                .filter(|edge| edge.callsite == call.range)
                .map(|edge| {
                    (
                        graph.node(&edge.to).unwrap().label.as_str(),
                        edge.confidence.clone(),
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(
                edges,
                vec![(target, Confidence::Resolved)],
                "line {line}: {call:?}"
            );
        }
    }
}
