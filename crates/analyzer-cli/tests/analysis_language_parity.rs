use analyzer_core::{
    graph::CallGraph,
    metrics::{advanced_metrics, MetricValue},
    Config, FileModel, LanguageRegistry,
};
use std::{collections::BTreeMap, path::Path};

fn registry() -> LanguageRegistry {
    let mut registry = LanguageRegistry::new();
    registry.register(analyzer_php::Php);
    registry.register(analyzer_python::Python);
    registry.register(analyzer_javascript::JavaScript);
    registry.register(analyzer_typescript::TypeScript);
    registry.register(analyzer_rust::Rust);
    registry
}

fn analyze(sources: &[(&str, &str)]) -> (Vec<FileModel>, Vec<MetricValue>) {
    let registry = registry();
    let files = sources
        .iter()
        .map(|(path, source)| {
            let path = Path::new(path);
            let strategy = registry
                .by_extension(path.extension().unwrap().to_str().unwrap())
                .unwrap();
            let file = strategy.analyze(source, path);
            assert!(file.parse_errors.is_empty(), "{:?}", file.parse_errors);
            file
        })
        .collect::<Vec<_>>();
    let graph = CallGraph::build(&files, &registry, &Config::default());
    let sources = sources
        .iter()
        .map(|(path, source)| (path.to_string(), source.to_string()))
        .collect::<BTreeMap<_, _>>();
    let metrics = advanced_metrics(&files, &graph, &sources);
    (files, metrics)
}

fn metric(metrics: &[MetricValue], path: &str, id: &str) -> f64 {
    metrics
        .iter()
        .find(|metric| metric.path.as_ref() == path && metric.id.as_ref() == id)
        .unwrap()
        .value
}

#[test]
fn source_operators_private_members_and_lifetimes_do_not_hide_work_from_scores() {
    for (path, small, large) in [
        (
            "work.py",
            "def work(x):\n return x // 2\n",
            "def work(x):\n return x // 2 + x * 3 + x * 4\n",
        ),
        (
            "work.js",
            "class Value { #field=0; work(x){return this.#field + x;} }",
            "class Value { #field=0; work(x){return this.#field + x * 3 + x * 4;} }",
        ),
        (
            "work.ts",
            "class Value { #field=0; work(x:number){return this.#field + x;} }",
            "class Value { #field=0; work(x:number){return this.#field + x * 3 + x * 4;} }",
        ),
        (
            "work.rs",
            "fn work(x: &'static i32) -> i32 { *x }",
            "fn work(x: &'static i32) -> i32 { *x + *x * 3 + *x * 4 }",
        ),
    ] {
        let (_, small) = analyze(&[(path, small)]);
        let (_, large) = analyze(&[(path, large)]);
        assert!(
            metric(&large, path, "halstead_volume") > metric(&small, path, "halstead_volume"),
            "{path}: executable work must increase token volume"
        );
        assert!(
            metric(&large, path, "maintainability_index")
                < metric(&small, path, "maintainability_index"),
            "{path}: maintainability must reflect the additional work"
        );
    }
}

#[test]
fn duplicate_detection_keeps_python_floor_division_operands() {
    let (_, metrics) = analyze(&[(
        "work.py",
        "def first(x):\n a=x+1; b=a+2; c=b+3; d=c+4; e=d+5; return e // 2\n\
         def second(x):\n a=x+1; b=a+2; c=b+3; d=c+4; e=d+5; return e // 7\n",
    )]);
    assert!(
        !metrics
            .iter()
            .any(|metric| metric.id.as_ref() == "duplicate_functions"),
        "different divisors change executable behavior"
    );
}

#[test]
fn module_cycle_scores_do_not_join_independent_language_graphs() {
    let (_, metrics) = analyze(&[
        ("a.py", "from b import work\ndef run():\n work()\n"),
        ("b.py", "def work():\n pass\n"),
        ("a.js", "export function work() {}"),
        (
            "b.js",
            "import {work} from './a.js'; export function run() {work();}",
        ),
    ]);
    for path in ["a.py", "b.py", "a.js", "b.js"] {
        assert_eq!(metric(&metrics, path, "module_cycle"), 0.0, "{path}");
    }
}

#[test]
fn real_javascript_typescript_interop_cycles_keep_distinct_module_identities() {
    for sources in [
        vec![
            (
                "a.js",
                "import {right} from './b.ts'; export function left(){right();}",
            ),
            (
                "b.ts",
                "import {left} from './a.js'; export function right(){left();}",
            ),
        ],
        // Equal stems are still two physical modules linked by actual imports.
        vec![
            (
                "a.js",
                "import {right} from './a.ts'; export function left(){right();}",
            ),
            (
                "a.ts",
                "import {left} from './a.js'; export function right(){left();}",
            ),
        ],
    ] {
        let (_, metrics) = analyze(&sources);
        for (path, _) in &sources {
            assert_eq!(metric(&metrics, path, "module_cycle"), 1.0, "{path}");
        }
    }
}

#[test]
fn real_module_cycle_scores_are_preserved_in_every_language() {
    for sources in [
        vec![
            ("a.php", "<?php namespace A; function left() { \\B\\right(); }"),
            ("b.php", "<?php namespace B; function right() { \\A\\left(); }"),
        ],
        vec![
            (
                "a.php",
                "<?php namespace A; use B\\Right; class Left {function run(Right $b){$b->run($this);}}",
            ),
            (
                "b.php",
                "<?php namespace B; use A\\Left; class Right {function run(Left $a){$a->run($this);}}",
            ),
        ],
        vec![
            ("a.py", "from b import right\ndef left():\n right()\n"),
            ("b.py", "from a import left\ndef right():\n left()\n"),
        ],
        vec![
            (
                "a.js",
                "import {right} from './b'; export function left(){right();}",
            ),
            (
                "b.js",
                "import {left} from './a'; export function right(){left();}",
            ),
        ],
        vec![
            (
                "a.ts",
                "import {right} from './b'; export function left(){right();}",
            ),
            (
                "b.ts",
                "import {left} from './a'; export function right(){left();}",
            ),
        ],
        vec![
            ("lib.rs", "pub mod a; pub mod b;"),
            ("a.rs", "pub fn left(){crate::b::right();}"),
            ("b.rs", "pub fn right(){crate::a::left();}"),
        ],
    ] {
        let (_, metrics) = analyze(&sources);
        for (path, _) in sources.iter().filter(|(path, _)| *path != "lib.rs") {
            assert_eq!(metric(&metrics, path, "module_cycle"), 1.0, "{path}");
        }
    }
}

#[test]
fn php_absolute_function_names_resolve_without_namespace_or_variable_shadowing() {
    let (files, _) = analyze(&[
        ("library.php", "<?php namespace Library; function work() {}"),
        ("global.php", "<?php function work() {}"),
        (
            "caller.php",
            r#"<?php
namespace App;
function work() {}
function invoke($work) {
    \Library\work();
    \work();
    \Missing\work();
    $work();
}
function bare() { work(); }
"#,
        ),
    ]);
    let graph = CallGraph::build(&files, &registry(), &Config::default());
    for (line, target) in [(5, "Library\\work"), (6, "work"), (10, "App\\work")] {
        let edges = graph
            .edges
            .iter()
            .filter(|edge| {
                edge.callsite.file.as_ref() == "caller.php" && edge.callsite.start_line == line
            })
            .collect::<Vec<_>>();
        assert_eq!(edges.len(), 1, "line {line}: {edges:?}");
        let edge = edges[0];
        assert_eq!(
            edge.confidence,
            analyzer_core::Confidence::Resolved,
            "line {line}: {edge:?}"
        );
        assert_eq!(graph.node(&edge.to).unwrap().label, target);
    }
    for line in [7, 8] {
        let edge = graph
            .edges
            .iter()
            .find(|edge| {
                edge.callsite.file.as_ref() == "caller.php" && edge.callsite.start_line == line
            })
            .unwrap();
        assert_eq!(
            edge.confidence,
            analyzer_core::Confidence::Dynamic,
            "line {line}: {edge:?}"
        );
        assert_eq!(
            graph.node(&edge.to).unwrap().kind,
            analyzer_core::graph::NodeKind::Unknown
        );
    }
}

#[test]
fn python_field_and_cohesion_scores_use_the_actual_implicit_receiver() {
    let (files, metrics) = analyze(&[(
        "receivers.py",
        r#"class Conventional:
 def __init__(self, value: int):
  self.value = value
 def first(self): return self.value
 def second(self): return self.value
class Custom:
 def __init__(me, value: int):
  me.value = value
  def local(me): me.hidden = 0
  me.nested.member = 1
 def first(me): return me.value
 def second(me): return me.value
class Static:
 @staticmethod
 def first(self): return self.value
 @staticmethod
 def second(self): return self.value
"#,
    )]);
    for class in ["Conventional", "Custom"] {
        let ty = files[0].types.iter().find(|ty| ty.name == class).unwrap();
        assert_eq!(ty.fields.len(), 1, "{class}: {:?}", ty.fields);
        assert_eq!(ty.fields[0].name, "value");
        assert_eq!(ty.fields[0].type_name.as_deref(), Some("int"));
        assert_eq!(
            metrics
                .iter()
                .find(|metric| metric.entity.as_ref() == ty.fqn && metric.id.as_ref() == "lcom4")
                .unwrap()
                .value,
            1.0,
            "{class}"
        );
    }
    let static_lcom = metrics
        .iter()
        .find(|metric| {
            metric.entity.as_ref() == "receivers.Static" && metric.id.as_ref() == "lcom4"
        })
        .unwrap();
    assert_eq!(
        static_lcom.value, 2.0,
        "a parameter named self is not an implicit receiver in a static method"
    );
}
