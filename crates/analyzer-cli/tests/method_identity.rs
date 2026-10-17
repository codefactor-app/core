use analyzer_core::{
    algorithmic, graph::CallGraph, Confidence, Config, FileModel, LanguageRegistry,
    LanguageStrategy,
};
use std::{collections::BTreeSet, path::Path};

fn registry() -> LanguageRegistry {
    let mut registry = LanguageRegistry::new();
    registry.register(analyzer_javascript::JavaScript);
    registry.register(analyzer_typescript::TypeScript);
    registry.register(analyzer_rust::Rust);
    registry
}

fn model(strategy: &dyn LanguageStrategy, path: &str, source: &str) -> FileModel {
    let model = strategy.analyze(source, Path::new(path));
    assert!(model.parse_errors.is_empty(), "{:?}", model.parse_errors);
    model
}

fn targets(graph: &CallGraph, from: &str) -> BTreeSet<String> {
    graph
        .edges
        .iter()
        .filter(|edge| edge.from.ends_with(from))
        .map(|edge| {
            graph
                .nodes
                .iter()
                .find(|node| node.id == edge.to.as_ref())
                .unwrap()
                .label
                .clone()
        })
        .collect()
}

fn reachable(graph: &CallGraph, from: &str) -> BTreeSet<String> {
    let mut pending = graph
        .nodes
        .iter()
        .filter(|node| node.label == from)
        .map(|node| node.id.clone())
        .collect::<Vec<_>>();
    let mut visited = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if visited.insert(id.clone()) {
            pending.extend(
                graph
                    .edges
                    .iter()
                    .filter(|edge| edge.from.as_ref() == id)
                    .map(|edge| edge.to.to_string()),
            );
        }
    }
    graph
        .nodes
        .iter()
        .filter(|node| visited.contains(&node.id))
        .map(|node| node.label.clone())
        .collect()
}

#[test]
fn js_ts_static_and_instance_bodies_have_distinct_identities_and_reachable_helpers() {
    let source = r#"
function staticHelper() {}
function instanceHelper() {}
class C {
    static ping() { staticHelper(); }
    ping() { instanceHelper(); }
    static forward() { this.ping(); const arrow = () => this.ping(); arrow(); }
    forward() { this.ping(); const arrow = () => this.ping(); arrow(); }
}
function staticEntry() { C.ping(); C.forward(); }
function instanceEntry() { const c = new C(); c.ping(); c.forward(); }
"#;
    for (strategy, path) in [
        (
            Box::new(analyzer_javascript::JavaScript) as Box<dyn LanguageStrategy>,
            "identity.js",
        ),
        (Box::new(analyzer_typescript::TypeScript), "identity.ts"),
    ] {
        let file = model(&*strategy, path, source);
        let pings = file
            .functions
            .iter()
            .filter(|function| function.name == "ping")
            .collect::<Vec<_>>();
        assert_eq!(pings.len(), 2, "{path}");
        assert_ne!(pings[0].fqn, pings[1].fqn, "{path}");
        let graph = CallGraph::build(&[file], &registry(), &Config::default());
        let ids = graph
            .nodes
            .iter()
            .map(|node| &node.id)
            .collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), graph.nodes.len(), "{path}");
        assert_eq!(
            targets(&graph, "identity.staticEntry"),
            BTreeSet::from([
                "identity.C.ping#static".into(),
                "identity.C.forward#static".into()
            ]),
            "{path}: {graph:?}"
        );
        assert!(
            targets(&graph, "identity.instanceEntry").contains("identity.C.ping"),
            "{path}: {graph:?}"
        );
        assert_eq!(
            targets(&graph, "identity.C.forward#static"),
            BTreeSet::from([
                "identity.C.ping#static".into(),
                "identity.C.forward#static.arrow".into()
            ]),
            "{path}: {graph:?}"
        );
        assert_eq!(
            targets(&graph, "identity.C.forward"),
            BTreeSet::from(["identity.C.ping".into(), "identity.C.forward.arrow".into()]),
            "{path}: {graph:?}"
        );
        let static_reachable = reachable(&graph, "identity.staticEntry");
        let instance_reachable = reachable(&graph, "identity.instanceEntry");
        assert!(static_reachable.contains("identity.staticHelper"), "{path}");
        assert!(
            !static_reachable.contains("identity.instanceHelper"),
            "{path}"
        );
        assert!(
            instance_reachable.contains("identity.instanceHelper"),
            "{path}"
        );
        assert!(
            !instance_reachable.contains("identity.staticHelper"),
            "{path}"
        );
    }
}

#[test]
fn js_ts_static_only_and_instance_only_members_do_not_cross_dispatch_domains() {
    let source = "class C {static staticOnly(){} instanceOnly(){}} function flow(){const c=new C();C.staticOnly();c.instanceOnly();c.staticOnly();C.instanceOnly();}";
    for (strategy, path) in [
        (
            Box::new(analyzer_javascript::JavaScript) as Box<dyn LanguageStrategy>,
            "identity.js",
        ),
        (Box::new(analyzer_typescript::TypeScript), "identity.ts"),
    ] {
        let graph = CallGraph::build(
            &[model(&*strategy, path, source)],
            &registry(),
            &Config::default(),
        );
        let resolved = graph
            .edges
            .iter()
            .filter(|edge| {
                edge.from.ends_with("identity.flow") && edge.confidence == Confidence::Resolved
            })
            .collect::<Vec<_>>();
        assert_eq!(resolved.len(), 2, "{path}: {graph:?}");
        assert_eq!(
            graph
                .edges
                .iter()
                .filter(|edge| edge.from.ends_with("identity.flow")
                    && edge.confidence == Confidence::Dynamic)
                .count(),
            3,
            "{path}: {graph:?}"
        );
    }
}

#[test]
fn literal_and_proven_computed_js_ts_methods_use_decoded_property_names() {
    let source = r#"const method = "computed"; class C { "r\u0075n"() {} static 'load'() {} [method]() {} } function flow(){const c=new C();c.run();C.load();c['computed']();}"#;
    for (strategy, path) in [
        (
            Box::new(analyzer_javascript::JavaScript) as Box<dyn LanguageStrategy>,
            "identity.js",
        ),
        (Box::new(analyzer_typescript::TypeScript), "identity.ts"),
    ] {
        let graph = CallGraph::build(
            &[model(&*strategy, path, source)],
            &registry(),
            &Config::default(),
        );
        let target = targets(&graph, "identity.flow");
        for name in [
            "identity.C.run",
            "identity.C.load#static",
            "identity.C.computed",
        ] {
            assert!(target.contains(name), "{path}: {graph:?}");
        }
    }
}

#[test]
fn typescript_overloads_collapse_within_their_static_or_instance_domain() {
    let source = "class C {static ping(x:string):string;static ping(x:number):number;static ping(x:any):any{return x;}ping(x:string):string;ping(x:number):number;ping(x:any):any{return x;}}function flow(c:C){C.ping(1);c.ping(1);}";
    let file = model(&analyzer_typescript::TypeScript, "identity.ts", source);
    assert_eq!(
        file.functions
            .iter()
            .filter(|function| function.name == "ping")
            .count(),
        2
    );
    let graph = CallGraph::build(&[file], &registry(), &Config::default());
    assert_eq!(
        targets(&graph, "identity.flow"),
        BTreeSet::from(["identity.C.ping".into(), "identity.C.ping#static".into()]),
        "{graph:?}"
    );
    assert!(graph
        .edges
        .iter()
        .all(|edge| edge.confidence == Confidence::Resolved));
}

#[test]
fn rust_inherent_and_trait_impl_bodies_and_nested_helpers_keep_their_identity() {
    let source = r#"
fn a() {} fn b() {} fn own() {}
trait A { fn ping(&self); } trait B { fn ping(&self); }
struct C;
impl A for C {fn ping(&self){fn nested(){a();}nested();}}
impl B for C {fn ping(&self){fn nested(){b();}nested();}}
impl C {fn ping(&self){own();}}
fn inherent(c:&C){c.ping();}
fn trait_a(c:&C){A::ping(c);}
fn trait_b(c:&C){B::ping(c);}
fn ufcs_a(c:&C){<C as A>::ping(c);}
fn ufcs_b(c:&C){<C as B>::ping(c);}
"#;
    let file = model(&analyzer_rust::Rust, "identity.rs", source);
    assert_eq!(
        file.functions
            .iter()
            .filter(|function| function.name == "ping"
                && function.parent_type.as_deref() == Some("identity::C"))
            .count(),
        3
    );
    let graph = CallGraph::build(&[file], &registry(), &Config::default());
    assert_eq!(
        graph
            .nodes
            .iter()
            .map(|node| &node.id)
            .collect::<BTreeSet<_>>()
            .len(),
        graph.nodes.len(),
        "{graph:?}"
    );
    for (entry, target) in [
        ("inherent", "identity::C::ping"),
        ("trait_a", "identity::C::ping#impl[A]"),
        ("ufcs_a", "identity::C::ping#impl[A]"),
        ("trait_b", "identity::C::ping#impl[B]"),
        ("ufcs_b", "identity::C::ping#impl[B]"),
    ] {
        assert_eq!(
            targets(&graph, &format!("identity::{entry}")),
            BTreeSet::from([target.into()]),
            "{entry}: {graph:?}"
        );
        let reached = reachable(&graph, &format!("identity::{entry}"));
        let helper = if entry.ends_with('a') {
            "a"
        } else if entry.ends_with('b') {
            "b"
        } else {
            "own"
        };
        assert!(
            reached.contains(&format!("identity::{helper}")),
            "{entry}: {graph:?}"
        );
        for excluded in ["a", "b", "own"].into_iter().filter(|name| *name != helper) {
            assert!(
                !reached.contains(&format!("identity::{excluded}")),
                "{entry}: {graph:?}"
            );
        }
    }
}

#[test]
fn rust_trait_dispatch_uses_argument_type_and_preserves_ambiguity_without_inherent_method() {
    let source = "trait A{fn ping(&self);}trait B{fn ping(&self);}struct C;struct D;impl A for C{fn ping(&self){}}impl B for C{fn ping(&self){}}impl A for D{fn ping(&self){}}fn qualified(c:&C){A::ping(c);}fn ambiguous(c:&C){c.ping();}";
    let graph = CallGraph::build(
        &[model(&analyzer_rust::Rust, "identity.rs", source)],
        &registry(),
        &Config::default(),
    );
    assert_eq!(
        targets(&graph, "identity::qualified"),
        BTreeSet::from(["identity::C::ping#impl[A]".into()]),
        "{graph:?}"
    );
    assert_eq!(
        targets(&graph, "identity::ambiguous"),
        BTreeSet::from([
            "identity::C::ping#impl[A]".into(),
            "identity::C::ping#impl[B]".into()
        ]),
        "{graph:?}"
    );
    assert!(graph
        .edges
        .iter()
        .filter(|edge| edge.from.ends_with("identity::ambiguous"))
        .all(|edge| edge.confidence == Confidence::Ambiguous));
}

#[test]
fn separate_dispatch_bodies_propagate_distinct_symbolic_cost_and_graph_summaries() {
    let source = "class C{static ping(n:number){for(let i=0;i<n;i++){}}ping(n:number){for(let i=0;i<n;i++){for(let j=0;j<n;j++){}}}}function staticEntry(n:number){C.ping(n);}function instanceEntry(c:C,n:number){c.ping(n);}";
    let mut files = vec![model(
        &analyzer_typescript::TypeScript,
        "identity.ts",
        source,
    )];
    let config = Config::default();
    let mut graph = CallGraph::build(&files, &registry(), &config);
    algorithmic::analyze(&mut files, &mut graph, &config);
    for (name, formula) in [
        ("identity.C.ping#static", "O(n)"),
        ("identity.C.ping", "O(n^2)"),
        ("identity.staticEntry", "O(n)"),
        ("identity.instanceEntry", "O(n^2)"),
    ] {
        let function = files[0]
            .functions
            .iter()
            .find(|function| function.fqn == name)
            .unwrap();
        let estimate = function.algorithmic_complexity.as_ref().unwrap();
        assert_eq!(
            estimate.known_formula.as_deref(),
            Some(formula),
            "{name}: {estimate:?}"
        );
        let node = graph.nodes.iter().find(|node| node.label == name).unwrap();
        assert!(std::sync::Arc::ptr_eq(
            function.algorithmic_complexity.as_ref().unwrap(),
            node.algorithmic_complexity.as_ref().unwrap()
        ));
    }
    let serialized = serde_json::to_value(&graph).unwrap();
    assert_eq!(
        serialized["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|node| node["label"]
                .as_str()
                .unwrap()
                .starts_with("identity.C.ping"))
            .count(),
        2
    );
}

#[test]
fn rust_trait_default_bodies_remain_callable_when_not_overridden() {
    let source = "fn helper(){}trait A{fn ping(&self){helper();}}struct C;impl A for C{}fn flow(c:&C){A::ping(c);}";
    let graph = CallGraph::build(
        &[model(&analyzer_rust::Rust, "identity.rs", source)],
        &registry(),
        &Config::default(),
    );
    assert_eq!(
        targets(&graph, "identity::flow"),
        BTreeSet::from(["identity::A::ping".into()]),
        "{graph:?}"
    );
    assert!(reachable(&graph, "identity::flow").contains("identity::helper"));
}

#[test]
fn rust_ufcs_cost_substitution_consumes_explicit_self_before_size_parameters() {
    let source = "trait A{fn ping(&self,n:usize);}struct C;impl A for C{fn ping(&self,n:usize){for i in 0..n{for j in 0..n{}}}}impl C{fn ping(&self,n:usize){for i in 0..n{}}}fn inherent(c:&C,n:usize){c.ping(n);}fn qualified(c:&C,n:usize){A::ping(c,n);}fn ufcs(c:&C,n:usize){<C as A>::ping(c,n);}";
    let mut files = vec![model(&analyzer_rust::Rust, "identity.rs", source)];
    let config = Config::default();
    let mut graph = CallGraph::build(&files, &registry(), &config);
    algorithmic::analyze(&mut files, &mut graph, &config);
    for (name, formula) in [
        ("inherent", "O(n)"),
        ("qualified", "O(n^2)"),
        ("ufcs", "O(n^2)"),
    ] {
        let function = files[0]
            .functions
            .iter()
            .find(|function| function.name == name)
            .unwrap();
        let estimate = function.algorithmic_complexity.as_ref().unwrap();
        assert_eq!(
            estimate.known_formula.as_deref(),
            Some(formula),
            "{name}: {estimate:?}"
        );
    }
}

#[test]
fn literal_method_names_cannot_collide_with_static_identity_suffixes() {
    let source = r#"class C{static run(){} "run#static"(){} "run%23static"(){}}function flow(){const c=new C();C.run();c['run#static']();c['run%23static']();}"#;
    for (strategy, path) in [
        (
            Box::new(analyzer_javascript::JavaScript) as Box<dyn LanguageStrategy>,
            "identity.js",
        ),
        (Box::new(analyzer_typescript::TypeScript), "identity.ts"),
    ] {
        let graph = CallGraph::build(
            &[model(&*strategy, path, source)],
            &registry(),
            &Config::default(),
        );
        assert_eq!(
            graph
                .nodes
                .iter()
                .map(|node| &node.id)
                .collect::<BTreeSet<_>>()
                .len(),
            graph.nodes.len(),
            "{path}: {graph:?}"
        );
        assert_eq!(
            graph
                .edges
                .iter()
                .filter(|edge| edge.from.ends_with("identity.flow")
                    && edge.confidence == Confidence::Resolved)
                .count(),
            3,
            "{path}: {graph:?}"
        );
        let target = targets(&graph, "identity.flow");
        for name in [
            "identity.C.run#static",
            "identity.C.run%23static",
            "identity.C.run%2523static",
        ] {
            assert!(target.contains(name), "{path}: {graph:?}");
        }
    }
}

#[test]
fn rust_generic_trait_specializations_select_separate_bodies() {
    let source = "trait A<T>{fn ping(&self);}struct C;impl A<u8> for C{fn ping(&self){}}impl A<u16> for C{fn ping(&self){}}fn first(c:&C){<C as A<u8>>::ping(c);}fn second(c:&C){<C as A<u16>>::ping(c);}";
    let graph = CallGraph::build(
        &[model(&analyzer_rust::Rust, "identity.rs", source)],
        &registry(),
        &Config::default(),
    );
    assert_eq!(
        targets(&graph, "identity::first"),
        BTreeSet::from(["identity::C::ping#impl[A<u8>]".into()]),
        "{graph:?}"
    );
    assert_eq!(
        targets(&graph, "identity::second"),
        BTreeSet::from(["identity::C::ping#impl[A<u16>]".into()]),
        "{graph:?}"
    );
}

#[test]
fn rust_trait_object_calls_do_not_treat_an_ordinary_argument_as_explicit_self() {
    let source = "trait A{fn ping(&self,n:usize);}struct C;impl A for C{fn ping(&self,n:usize){}}fn ordinary(c:&dyn A,n:usize){c.ping(n);}fn qualified(c:&dyn A,n:usize){A::ping(c,n);}";
    let graph = CallGraph::build(
        &[model(&analyzer_rust::Rust, "identity.rs", source)],
        &registry(),
        &Config::default(),
    );
    for name in ["ordinary", "qualified"] {
        assert_eq!(
            targets(&graph, &format!("identity::{name}")),
            BTreeSet::from(["identity::C::ping".into()]),
            "{name}: {graph:?}"
        );
        assert!(graph
            .edges
            .iter()
            .filter(|edge| edge.from.ends_with(&format!("identity::{name}")))
            .all(|edge| edge.confidence == Confidence::Ambiguous));
    }
}
