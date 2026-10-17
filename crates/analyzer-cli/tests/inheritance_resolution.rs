use analyzer_core::{
    graph::CallGraph, Confidence, Config, FileModel, LanguageRegistry, LanguageStrategy,
};
use std::path::Path;

fn graph(strategy: &dyn LanguageStrategy, sources: &[(&str, &str)]) -> (Vec<FileModel>, CallGraph) {
    let files = sources
        .iter()
        .map(|(path, source)| {
            let model = strategy.analyze(source, Path::new(path));
            assert!(
                model.parse_errors.is_empty(),
                "{path}: {:?}",
                model.parse_errors
            );
            model
        })
        .collect::<Vec<_>>();
    let mut registry = LanguageRegistry::new();
    registry.register(analyzer_python::Python);
    registry.register(analyzer_php::Php);
    let graph = CallGraph::build(&files, &registry, &Config::default());
    (files, graph)
}

fn outgoing(graph: &CallGraph, suffix: &str) -> Vec<(String, Confidence)> {
    graph
        .edges
        .iter()
        .filter(|edge| edge.from.ends_with(suffix))
        .map(|edge| (edge.to.to_string(), edge.confidence.clone()))
        .collect()
}

fn exact(graph: &CallGraph, from: &str, suffixes: &[&str]) {
    let edges = outgoing(graph, from);
    assert_eq!(edges.len(), suffixes.len(), "{from}: {edges:?}");
    for suffix in suffixes {
        assert!(
            edges
                .iter()
                .any(|(target, confidence)| target.ends_with(suffix)
                    && *confidence == Confidence::Resolved),
            "{from} → {suffix}: {edges:?}"
        );
    }
}

#[test]
fn python_multiple_inheritance_preserves_base_order() {
    let (_, graph) = graph(&analyzer_python::Python, &[("mro.py", "class A:\n def ping(self): pass\nclass B:\n def ping(self): pass\nclass C(A, B):\n def run(self): self.ping()\nclass D(B, A):\n def run(self): self.ping()\n")]);
    exact(&graph, ".C.run", &[".A.ping"]);
    exact(&graph, ".D.run", &[".B.ping"]);
    assert!(graph.resolution_diagnostics.is_empty());
}

#[test]
fn python_c3_skips_shared_ancestor_until_later_base() {
    let (_, graph) = graph(&analyzer_python::Python, &[("diamond.py", "class Root:\n def ping(self): pass\nclass A(Root): pass\nclass B(Root):\n def ping(self): pass\nclass C(A, B):\n def run(self): self.ping()\n")]);
    exact(&graph, ".C.run", &[".B.ping"]);
}

#[test]
fn python_c3_selects_inherited_constructor() {
    let (_, graph) = graph(&analyzer_python::Python, &[("construct.py", "class Root:\n def __init__(self): pass\nclass A(Root): pass\nclass B(Root):\n def __init__(self): pass\nclass C(A, B): pass\ndef entry(): C()\n")]);
    exact(&graph, ".entry", &[".B.__init__"]);
}

#[test]
fn python_mro_follows_imported_base_aliases() {
    let (_, graph) = graph(&analyzer_python::Python, &[("bases.py", "class Root:\n def ping(self): pass\nclass A(Root): pass\nclass B(Root):\n def ping(self): pass\n"), ("main.py", "from bases import A as First, B as Second\nclass C(First, Second):\n def run(self): self.ping()\n")]);
    exact(&graph, ".C.run", &["bases.B.ping"]);
}

#[test]
fn python_super_uses_c3_after_declaring_class() {
    let (_, graph) = graph(&analyzer_python::Python, &[("super.py", "class Root:\n def ping(self): pass\nclass A(Root): pass\nclass B(Root):\n def ping(self): pass\nclass C(A, B):\n def ping(self): pass\n def run(self): super().ping()\n")]);
    let edges = outgoing(&graph, ".C.run");
    assert!(
        edges
            .iter()
            .any(|(target, confidence)| target.ends_with(".B.ping")
                && *confidence == Confidence::Resolved),
        "{edges:?}"
    );
    assert!(
        !edges
            .iter()
            .any(|(target, _)| target.ends_with(".C.ping") || target.ends_with(".Root.ping")),
        "{edges:?}"
    );
}

#[test]
fn python_explicit_super_uses_the_selected_anchor() {
    let (_, graph) = graph(&analyzer_python::Python, &[("explicit.py", "class Root:\n def ping(self): pass\nclass A(Root):\n def ping(self): pass\nclass B(Root):\n def ping(self): pass\nclass C(A,B):\n def first(self): super(C, self).ping()\n def next(self): super(A, self).ping()\n")]);
    for (method, suffix) in [(".C.first", ".A.ping"), (".C.next", ".B.ping")] {
        let edges = outgoing(&graph, method);
        assert!(
            edges
                .iter()
                .any(|(target, confidence)| target.ends_with(suffix)
                    && *confidence == Confidence::Resolved),
            "{method}: {edges:?}"
        );
        assert!(
            !edges
                .iter()
                .any(|(target, _)| target.ends_with(".Root.ping")),
            "{method}: {edges:?}"
        );
    }
}

#[test]
fn python_explicit_object_base_has_known_mro() {
    let (_, graph) = graph(
        &analyzer_python::Python,
        &[(
            "root.py",
            "class A(object):\n def ping(self): pass\nclass C(A):\n def run(self): self.ping()\n",
        )],
    );
    exact(&graph, ".C.run", &[".A.ping"]);
    assert!(
        graph.resolution_diagnostics.is_empty(),
        "{:?}",
        graph.resolution_diagnostics
    );
}

#[test]
fn python_cooperative_super_in_inherited_body_includes_sibling() {
    let (_, graph) = graph(&analyzer_python::Python, &[("cooperative.py", "class Root:\n def ping(self): pass\nclass A(Root):\n def run(self): super().ping()\nclass B(Root):\n def ping(self): pass\nclass C(A, B): pass\n")]);
    let edges = outgoing(&graph, ".A.run");
    for suffix in [".B.ping", ".Root.ping"] {
        assert!(
            edges
                .iter()
                .any(|(target, confidence)| target.ends_with(suffix)
                    && *confidence == Confidence::Ambiguous),
            "{edges:?}"
        );
    }
}

#[test]
fn python_super_retains_runtime_and_project_alternatives() {
    let (_, graph) = graph(&analyzer_python::Python, &[("runtime.py", "class A(list):\n def run(self): super().append(1)\nclass B(list):\n def append(self, item): pass\nclass C(A,B): pass\n")]);
    let edges = outgoing(&graph, ".A.run");
    for suffix in [".B.append", "list::append"] {
        assert!(
            edges
                .iter()
                .any(|(target, confidence)| target.ends_with(suffix)
                    && *confidence == Confidence::Ambiguous),
            "{edges:?}"
        );
    }
}

#[test]
fn hierarchy_diagnostics_survive_graph_serialization() {
    let (_, graph) = graph(
        &analyzer_python::Python,
        &[(
            "unknown.py",
            "class C(Missing):\n def run(self): self.ping()\n",
        )],
    );
    let restored: CallGraph =
        serde_json::from_str(&serde_json::to_string(&graph).unwrap()).unwrap();
    assert_eq!(
        restored.resolution_diagnostics,
        graph.resolution_diagnostics
    );
    assert_eq!(outgoing(&restored, ".C.run"), outgoing(&graph, ".C.run"));
}

#[test]
fn python_invalid_or_unknown_mro_does_not_claim_an_exact_target() {
    for source in [
        "class A:\n def ping(self): pass\nclass B:\n def ping(self): pass\nclass X(A,B): pass\nclass Y(B,A): pass\nclass C(X,Y):\n def run(self): self.ping()\n",
        "class A:\n def ping(self): pass\nclass C(Unknown,A):\n def run(self): self.ping()\n",
        "class A:\n def ping(self): pass\nclass C(A,A):\n def run(self): self.ping()\n",
        "class A:\n def ping(self): pass\nclass C(factory(),A):\n def run(self): self.ping()\n",
        "class A:\n def ping(self): pass\nclass C(A,metaclass=Custom):\n def run(self): self.ping()\n",
        "class A:\n def ping(self): pass\nclass C(A[int]):\n def run(self): self.ping()\n",
        "class A(C):\n def ping(self): pass\nclass C(A):\n def run(self): self.ping()\n",
    ] {
        let (_, graph) = graph(&analyzer_python::Python, &[("invalid.py", source)]);
        let edges = outgoing(&graph, ".C.run");
        assert_eq!(edges.len(), 1, "{source}: {edges:?}");
        assert!(edges[0].0.starts_with("unknown:"), "{source}: {edges:?}");
        assert_eq!(edges[0].1, Confidence::Dynamic);
        assert!(!graph.resolution_diagnostics.is_empty(), "{source}");
    }
}

#[test]
fn python_builtin_base_import_alias_has_known_mro() {
    let (_, graph) = graph(&analyzer_python::Python, &[("root.py", "from builtins import object as Root\nclass A(Root):\n def ping(self): pass\nclass C(A):\n def run(self): self.ping()\n")]);
    exact(&graph, ".C.run", &[".A.ping"]);
    assert!(
        graph.resolution_diagnostics.is_empty(),
        "{:?}",
        graph.resolution_diagnostics
    );
}

#[test]
fn python_inherited_field_types_obey_c3_precedence() {
    let (_, graph) = graph(&analyzer_python::Python, &[("fields.py", "class One:\n def ping(self): pass\nclass Two:\n def ping(self): pass\nclass A:\n def __init__(self, repo: One): self.repo=repo\nclass B:\n def __init__(self, repo: Two): self.repo=repo\nclass C(A,B):\n def run(self): self.repo.ping()\n")]);
    exact(&graph, ".C.run", &[".One.ping"]);
}

#[test]
fn php_simple_traits_are_composed_and_inherited() {
    let (files, graph) = graph(&analyzer_php::Php, &[("traits.php", "<?php trait T { public function ping() {} } class C { use T; function run() { $this->ping(); } } class D extends C { function next() { $this->ping(); } }")]);
    assert_eq!(
        files[0]
            .types
            .iter()
            .find(|ty| ty.name == "C")
            .unwrap()
            .trait_uses[0]
            .traits,
        ["T"]
    );
    exact(&graph, "C::run", &["T::ping"]);
    exact(&graph, "D::next", &["T::ping"]);
}

#[test]
fn php_trait_precedence_and_aliases_preserve_original_bodies() {
    let (_, graph) = graph(&analyzer_php::Php, &[("traits.php", "<?php trait T1 { public function ping() {} } trait T2 { public function ping() {} } trait T3 { public function ping() {} } class C { use T1, T2, T3 { T1::ping insteadof T2; T1::ping insteadof T3; T2::ping as pong; ping as same; } function run() { $this->ping(); $this->pong(); $this->same(); } }")]);
    exact(&graph, "C::run", &["T1::ping", "T2::ping", "T1::ping"]);
    assert!(
        graph.resolution_diagnostics.is_empty(),
        "{:?}",
        graph.resolution_diagnostics
    );
}

#[test]
fn php_nested_traits_can_select_a_shared_original_body() {
    let (_, graph) = graph(&analyzer_php::Php, &[("shared.php", "<?php trait Inner { function ping() {} } trait First { use Inner; } trait Second { use Inner; } class C { use First, Second { First::ping insteadof Second; } function run() { $this->ping(); } }")]);
    exact(&graph, "C::run", &["Inner::ping"]);
    assert!(graph.resolution_diagnostics.is_empty());
}

#[test]
fn php_trait_precedence_accepts_multiple_exclusions_in_one_clause() {
    let (_, graph) = graph(&analyzer_php::Php, &[("traits.php", "<?php trait T1 { function ping() {} } trait T2 { function ping() {} } trait T3 { function ping() {} } class C { use T1, T2, T3 { T1::ping insteadof T2, T3; T3::ping as pong; } function run() { $this->ping(); $this->pong(); } }")]);
    exact(&graph, "C::run", &["T1::ping", "T3::ping"]);
    assert!(graph.resolution_diagnostics.is_empty());
}

#[test]
fn php_nested_traits_and_class_overrides_take_precedence_over_parent() {
    let (_, graph) = graph(&analyzer_php::Php, &[("nested.php", "<?php trait Inner { function ping() {} } trait Outer { use Inner; function own() {} } class Base { function ping() {} } class C extends Base { use Outer; function run() { $this->ping(); $this->own(); } } class D extends C { use Outer; function ping() {} function next() { $this->ping(); } }")]);
    exact(&graph, "C::run", &["Inner::ping", "Outer::own"]);
    exact(&graph, "D::next", &["D::ping"]);
}

#[test]
fn php_trait_method_bodies_dispatch_through_consuming_class() {
    let (_, graph) = graph(&analyzer_php::Php, &[("dispatch.php", "<?php trait T { function ping() {} function run() { $this->ping(); self::ping(); } } class C { use T; function ping() {} } class D extends C {}")]);
    exact(&graph, "T::run", &["C::ping", "C::ping"]);
}

#[test]
fn php_trait_body_can_call_alias_and_parent_of_consumer() {
    let (_, graph) = graph(&analyzer_php::Php, &[("dispatch.php", "<?php trait T { function ping() {} function run() { $this->hidden(); parent::ping(); } } class Base { function ping() {} } class C extends Base { use T { ping as private hidden; } }")]);
    exact(&graph, "T::run", &["T::ping", "Base::ping"]);
}

#[test]
fn php_inherited_trait_body_retains_private_alias_and_lexical_parent_scope() {
    let (_, graph) = graph(&analyzer_php::Php, &[("scope.php", "<?php trait T { function ping() {} function run() { $this->hidden(); parent::ping(); self::ping(); } } class Base { function ping() {} } class C extends Base { use T { ping as private hidden; } } class D extends C { function ping() {} function hidden() {} }")]);
    exact(&graph, "T::run", &["T::ping", "Base::ping", "T::ping"]);
}

#[test]
fn php_trait_body_retains_all_distinct_consumer_overrides() {
    let (_, graph) = graph(&analyzer_php::Php, &[("dispatch.php", "<?php trait T { function run() { $this->ping(); } } class C { use T; function ping() {} } class D { use T; function ping() {} }")]);
    let edges = outgoing(&graph, "T::run");
    assert_eq!(edges.len(), 2, "{edges:?}");
    for suffix in ["C::ping", "D::ping"] {
        assert!(
            edges
                .iter()
                .any(|(target, confidence)| target.ends_with(suffix)
                    && *confidence == Confidence::Ambiguous),
            "{edges:?}"
        );
    }
}

#[test]
fn php_trait_composition_follows_namespaces_and_import_aliases() {
    let (_, graph) = graph(&analyzer_php::Php, &[("traits.php", "<?php namespace Dep; trait T { function ping() {} } trait U { function ping() {} }"), ("main.php", "<?php namespace App; use Dep\\{T as First, U as Second}; class C { use First, Second { First::ping insteadof Second; Second::ping as pong; } function run() { $this->ping(); $this->pong(); } }")]);
    exact(&graph, "C::run", &["Dep\\T::ping", "Dep\\U::ping"]);
}

#[test]
fn php_visibility_adaptation_keeps_private_alias_internal() {
    let (_, graph) = graph(&analyzer_php::Php, &[("visibility.php", "<?php trait T { public function ping() {} } class C { use T { ping as private hidden; ping as protected; } function run() { $this->hidden(); $this->ping(); } } class D extends C { function next() { $this->ping(); } } function outside(C $c) { $c->hidden(); $c->ping(); }")]);
    exact(&graph, "C::run", &["T::ping", "T::ping"]);
    exact(&graph, "D::next", &["T::ping"]);
    assert!(outgoing(&graph, "outside")
        .iter()
        .all(|(target, confidence)| target.starts_with("unknown:")
            && *confidence == Confidence::Dynamic));
}

#[test]
fn php_conflicting_traits_are_ambiguous_and_unknown_traits_are_dynamic() {
    let (_, graph) = graph(&analyzer_php::Php, &[("conflict.php", "<?php trait T1 { function ping() {} } trait T2 { function ping() {} } class C { use T1, T2; function run() { $this->ping(); } } class D { use Missing; function run() { $this->ping(); } }")]);
    let edges = outgoing(&graph, "C::run");
    assert_eq!(edges.len(), 2, "{edges:?}");
    assert!(edges
        .iter()
        .all(|(_, confidence)| *confidence == Confidence::Ambiguous));
    assert!(outgoing(&graph, "D::run")
        .iter()
        .all(|(target, confidence)| target.starts_with("unknown:")
            && *confidence == Confidence::Dynamic));
    assert_eq!(graph.resolution_diagnostics.len(), 2);
}
