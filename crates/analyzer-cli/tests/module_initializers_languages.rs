use analyzer_core::graph::NodeKind;
use analyzer_core::{Analyzer, Config, LanguageRegistry};
use std::path::PathBuf;

fn scan(name: &str, source: &str) -> (PathBuf, analyzer_core::AnalysisReport) {
    let root = std::env::temp_dir().join(format!(
        "codefactor-module-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join(name), source).unwrap();
    let mut registry = LanguageRegistry::new();
    registry.register(analyzer_python::Python);
    registry.register(analyzer_php::Php);
    let mut config = Config::default();
    config.cache.enabled = false;
    config.architecture.enabled = false;
    config.algorithmic_complexity.enabled = false;
    let report = Analyzer::new(registry, config).scan(&root).unwrap();
    assert!(report.parse_errors.is_empty(), "{:?}", report.parse_errors);
    (root, report)
}

#[test]
fn python_module_tracks_loading_and_calls_without_executing_method_bodies() {
    let (root, report) = scan(
        "main.py",
        "def leaf(): pass\ndef decorate(fn): return fn\n@decorate\ndef entry(value=leaf()): pass\nclass Worker:\n    value = leaf()\n    def run(self): leaf()\nx = Worker()\nx.run()\nif enabled:\n    leaf()\n",
    );
    let initializer = report.files[0]
        .functions
        .iter()
        .find(|f| f.is_module_initializer())
        .unwrap();
    let edges = report
        .graph
        .edges
        .iter()
        .filter(|e| e.from.ends_with(&initializer.fqn))
        .collect::<Vec<_>>();
    let targets = edges
        .iter()
        .filter_map(|e| report.graph.nodes.iter().find(|n| n.id == e.to.as_ref()))
        .collect::<Vec<_>>();
    assert_eq!(targets.iter().filter(|n| n.label == "main.leaf").count(), 3);
    assert!(targets.iter().any(|n| n.label == "main.decorate"));
    assert!(targets.iter().any(|n| n.label == "main.Worker.run"));
    assert!(!targets.iter().any(|n| n.label == "main.entry"));
    assert!(edges.iter().any(|e| !e.guard.is_empty()));
    assert_eq!(report.summary.functions, 4);
    assert!(report
        .metrics
        .iter()
        .all(|m| !m.entity.contains("<module_initializer>")));
    assert!(report
        .graph
        .nodes
        .iter()
        .any(|n| n.kind == NodeKind::Function && n.label == "main.<module_initializer>"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn php_module_regions_keep_namespace_and_source_order_for_receivers() {
    let (root, report) = scan(
        "main.php",
        "<?php\nnamespace One;\nfunction leaf() {}\nclass Worker { public function run() { leaf(); } }\nleaf();\n$x = new Worker();\n$x->run();\nnamespace Two;\nfunction leaf() {}\nleaf();\n",
    );
    let initializers = report.files[0]
        .functions
        .iter()
        .filter(|f| f.is_module_initializer())
        .collect::<Vec<_>>();
    assert_eq!(initializers.len(), 2);
    for (namespace, line) in [("One", 5), ("Two", 10)] {
        let e = report
            .graph
            .edges
            .iter()
            .find(|e| e.callsite.start_line == line)
            .unwrap();
        assert!(e
            .from
            .ends_with(&format!("{namespace}\\<module_initializer>")));
        assert!(e.to.ends_with(&format!("{namespace}\\leaf")));
        assert_eq!(e.confidence, analyzer_core::Confidence::Resolved);
    }
    let method = report
        .graph
        .edges
        .iter()
        .find(|e| e.callsite.start_line == 7)
        .unwrap();
    assert!(method.to.ends_with("One\\Worker::run"), "{method:?}");
    assert_eq!(report.summary.functions, 3);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn loading_a_python_decorator_factory_keeps_its_result_dynamic() {
    let (root, report) = scan(
        "main.py",
        "def factory(): return lambda fn: fn\n@factory()\ndef entry(): pass\n",
    );
    let edges = report
        .graph
        .edges
        .iter()
        .filter(|e| e.callsite.start_line == 2)
        .collect::<Vec<_>>();
    assert_eq!(edges.len(), 2);
    assert!(edges.iter().any(|e| e.to.ends_with("main.factory")));
    assert!(edges
        .iter()
        .any(|e| e.confidence == analyzer_core::Confidence::Dynamic));
    std::fs::remove_dir_all(root).unwrap();
}
