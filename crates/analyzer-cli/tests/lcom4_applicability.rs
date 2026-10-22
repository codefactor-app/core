use analyzer_core::{
    graph::CallGraph,
    metrics::{advanced_metrics, MetricValue},
    Config, FileModel, LanguageRegistry, LanguageStrategy, TypeModel,
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
};

fn analyze(sources: &[(&str, &str)]) -> (Vec<FileModel>, Vec<MetricValue>, CallGraph) {
    let mut registry = LanguageRegistry::new();
    registry.register(analyzer_php::Php);
    registry.register(analyzer_python::Python);
    registry.register(analyzer_javascript::JavaScript);
    registry.register(analyzer_typescript::TypeScript);
    let files = sources
        .iter()
        .map(|(path, source)| {
            let path = Path::new(path);
            registry
                .by_extension(path.extension().unwrap().to_str().unwrap())
                .unwrap()
                .analyze(source, path)
        })
        .collect::<Vec<_>>();
    for file in &files {
        assert!(
            file.parse_errors.is_empty(),
            "{}: {:?}",
            file.path,
            file.parse_errors
        );
    }
    let graph = CallGraph::build(&files, &registry, &Config::default());
    let sources = sources
        .iter()
        .map(|(path, source)| (path.to_string(), source.to_string()))
        .collect::<BTreeMap<_, _>>();
    let metrics = advanced_metrics(&files, &graph, &sources, &Default::default());
    (files, metrics, graph)
}

struct Project(PathBuf);
impl Project {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "codefactor-lcom4-applicability-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("analyzer.yaml"),
            "cache: {enabled: false}\nlimits:\n  lcom4: {warn: 1, error: 4}\n",
        )
        .unwrap();
        Self(root)
    }
}
impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn cli_omits_inapplicable_php_types_from_metrics_findings_and_aggregates() {
    let project = Project::new();
    std::fs::write(
        project.0.join("models.php"),
        r#"<?php
abstract class AbstractModel {
    function left() { return $this->a; }
    function right() { return $this->b; }
}
trait SharedBehavior {
    function left() { return $this->a; }
    function right() { return $this->b; }
}
interface Contract { function left(); function right(); }
enum Status {
    case Ready;
    function left() { return 1; }
    function right() { return 2; }
}
class NullObject {
    function left() { return null; }
    function right() { return null; }
}
class StaticUtility {
    static function left() { return 1; }
    static function right() { return 2; }
}
class SingleMethod { function only() { return $this->a; } }
class SingleInstanceMethod {
    function only() { return $this->a; }
    static function helper() { return 1; }
}
class Connected {
    function __construct($a, $b) { $this->a = $a; $this->b = $b; }
    function left() { return $this->a; }
    function right() { return $this->b; }
}
class Split {
    function left() { return $this->a; }
    function right() { return $this->b; }
}
"#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_codefactor"))
        .current_dir(&project.0)
        .args(["scan", ".", "--no-architecture", "--format", "json"])
        .output()
        .unwrap();
    assert!(matches!(output.status.code(), Some(0 | 2)), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report["parse_errors"].as_array().unwrap().is_empty());
    let lcom = report["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|metric| metric["id"] == "lcom4")
        .map(|metric| {
            (
                metric["entity"].as_str().unwrap(),
                metric["value"].as_f64().unwrap(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(lcom, BTreeMap::from([("Connected", 1.0), ("Split", 2.0)]));
    let findings = report["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|violation| violation["rule"] == "lcom4")
        .collect::<Vec<_>>();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0]["entity"], "Split");
    let project_group = report["aggregates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["name"] == "project")
        .unwrap();
    assert_eq!(project_group["metrics"]["lcom4"]["avg"], 1.5);
    assert_eq!(project_group["metrics"]["lcom4"]["max"], 2.0);
    let samples = project_group["metrics"]["lcom4"]["histogram"]
        .as_array()
        .unwrap()
        .iter()
        .map(|bin| bin["count"].as_u64().unwrap())
        .sum::<u64>();
    assert_eq!(
        samples, 2,
        "skipped classes must not contribute zero samples"
    );
}

#[test]
fn implicit_fields_constructor_bridges_and_resolved_calls_stay_measurable() {
    let (files, metrics, graph) = analyze(&[
        ("models.py", "class Implicit:\n def left(self): return self.a\n def right(self): return self.b\nclass Connected:\n def __init__(self, a, b):\n  self.a = a\n  self.b = b\n def left(self): return self.a\n def right(self): return self.b\nclass Calls:\n def left(self): return self.right()\n def right(self): return 1\n"),
        ("models.js", "class Implicit { left() { return this.a; } right() { return this.b; } }\nclass Connected { constructor(a, b) { this.a = a; this.b = b; } left() { return this.a; } right() { return this.b; } }\nclass Calls { left() { return this.right(); } right() { return 1; } }\n"),
        ("models.ts", "class Implicit { left() { return this.a; } right() { return this.b; } }\nclass Connected { constructor(a: number, b: number) { this.a = a; this.b = b; } left() { return this.a; } right() { return this.b; } }\nclass Calls { left() { return this.right(); } right() { return 1; } }\n"),
        ("models.php", "<?php class Implicit { function left() { return $this->a; } function right() { return $this->b; } }\nclass Connected { function __construct($a, $b) { $this->a = $a; $this->b = $b; } function left() { return $this->a; } function right() { return $this->b; } }\nclass Calls { function left() { return $this->right(); } function right() { return 1; } }\n"),
    ]);
    for file in files {
        for (name, expected) in [("Implicit", 2.0), ("Connected", 1.0), ("Calls", 1.0)] {
            let ty = file.types.iter().find(|ty| ty.name == name).unwrap();
            let metric = metrics
                .iter()
                .find(|metric| {
                    metric.id.as_ref() == "lcom4"
                        && metric.path.as_ref() == file.path
                        && metric.entity.as_ref() == ty.fqn
                })
                .unwrap_or_else(|| panic!("{}: {} was skipped", file.path, ty.fqn));
            assert_eq!(metric.value, expected, "{}: {name}", file.path);
            if name != "Connected" {
                assert!(
                    ty.fields.is_empty(),
                    "{}: {name}: {:?}",
                    file.path,
                    ty.fields
                );
            }
            if name == "Calls" {
                let left = file
                    .functions
                    .iter()
                    .find(|function| {
                        function.parent_type.as_deref() == Some(ty.fqn.as_str())
                            && function.name == "left"
                    })
                    .unwrap();
                let right = file
                    .functions
                    .iter()
                    .find(|function| {
                        function.parent_type.as_deref() == Some(ty.fqn.as_str())
                            && function.name == "right"
                    })
                    .unwrap();
                assert!(
                    graph.edges.iter().any(|edge| edge.from.ends_with(&left.fqn)
                        && edge.to.ends_with(&right.fqn)
                        && edge.confidence == analyzer_core::Confidence::Resolved),
                    "{}: same-type call must be resolved",
                    file.path
                );
            }
        }
    }
}

#[test]
fn python_static_class_and_abstract_methods_do_not_add_cohesion_components() {
    let (files, metrics, _) = analyze(&[(
        "mixed.py",
        r#"from abc import abstractmethod
class Mixed:
 def left(self): return self.a
 def right(self): return self.b
 @staticmethod
 def utility(): return 1
 @classmethod
 def factory(cls): return 2
 @abstractmethod
 def contract(self): pass
"#,
    )]);
    let ty = &files[0].types[0];
    let lcom = metrics
        .iter()
        .find(|metric| metric.id.as_ref() == "lcom4" && metric.entity.as_ref() == ty.fqn)
        .unwrap();
    assert_eq!(lcom.value, 2.0);
}

#[test]
fn parsed_abstract_flag_round_trips_and_old_type_models_default_to_concrete() {
    let cases: Vec<(Box<dyn LanguageStrategy>, &str, &str)> = vec![
        (Box::new(analyzer_php::Php), "abstract.php", "<?php abstract class Base { function left() { return $this->a; } function right() { return $this->b; } } class Concrete {}"),
        (Box::new(analyzer_php::Php), "uppercase.php", "<?php ABSTRACT class Base { function left() { return $this->a; } function right() { return $this->b; } } class Concrete {}"),
        (Box::new(analyzer_typescript::TypeScript), "abstract.ts", "abstract class Base { left() { return this.a; } right() { return this.b; } } class Concrete {}"),
    ];
    for (strategy, path, source) in cases {
        let model = strategy.analyze(source, Path::new(path));
        assert!(
            model.parse_errors.is_empty(),
            "{path}: {:?}",
            model.parse_errors
        );
        let abstract_type = model.types.iter().find(|ty| ty.name == "Base").unwrap();
        let concrete_type = model.types.iter().find(|ty| ty.name == "Concrete").unwrap();
        assert!(abstract_type.is_abstract, "{path}");
        assert!(!concrete_type.is_abstract, "{path}");
        let mut serialized = serde_json::to_value(abstract_type).unwrap();
        assert_eq!(serialized["is_abstract"], true);
        let restored: TypeModel = serde_json::from_value(serialized.clone()).unwrap();
        assert!(restored.is_abstract, "{path}");
        serialized.as_object_mut().unwrap().remove("is_abstract");
        let legacy: TypeModel = serde_json::from_value(serialized).unwrap();
        assert!(!legacy.is_abstract, "{path}");
    }
}

#[test]
fn javascript_abstract_identifiers_do_not_make_concrete_classes_abstract() {
    let (files, metrics, _) = analyze(&[(
        "names.js",
        "class abstract { left() { return this.a; } right() { return this.b; } }\nclass Derived extends abstract { left() { return this.a; } right() { return this.b; } }\n",
    )]);
    assert_eq!(files[0].types.len(), 2);
    for ty in &files[0].types {
        assert!(!ty.is_abstract, "{}", ty.name);
        let lcom = metrics
            .iter()
            .find(|metric| metric.id.as_ref() == "lcom4" && metric.entity.as_ref() == ty.fqn)
            .unwrap_or_else(|| panic!("concrete class {} was skipped", ty.name));
        assert_eq!(lcom.value, 2.0, "{}", ty.name);
    }
}
