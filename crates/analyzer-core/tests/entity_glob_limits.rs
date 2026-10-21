use analyzer_core::config::{Config, ConfigLoader, EntityGlobLimits, PathLimits, Threshold};
use analyzer_core::ir::FileModel;
use analyzer_core::metrics::MetricValue;
use analyzer_core::rules::{evaluate, Severity};
use std::collections::{BTreeMap, BTreeSet};

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new(source: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "entity-glob-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("analyzer.yaml");
        std::fs::write(&path, source).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}
fn limits(warn: Option<f64>, error: Option<f64>) -> BTreeMap<String, Threshold> {
    BTreeMap::from([("function_params".into(), Threshold { warn, error })])
}
fn violation(config: &Config, fqn: &str) -> Option<analyzer_core::rules::Violation> {
    let file = FileModel {
        path: "src/View.php".into(),
        language: "php".into(),
        ..Default::default()
    };
    let metric = MetricValue {
        id: "function_params".into(),
        scope: "function".into(),
        entity: fqn.into(),
        path: file.path.clone().into(),
        line: 2,
        value: 15.0,
    };
    evaluate(
        &[metric],
        &[file],
        &[],
        config,
        &BTreeSet::new(),
        &BTreeMap::new(),
    )
    .into_iter()
    .next()
}

#[test]
fn fqn_globs_span_php_separators_and_overlay_paths_before_exact_entities() {
    let fqn = r"App\Billing\Nested\InvoiceView::__construct";
    let mut config = Config::default();
    config.limits.global = limits(Some(4.0), Some(7.0));
    config
        .limits
        .by_language
        .insert("php".into(), limits(Some(5.0), Some(20.0)));
    config.limits.by_path.push(PathLimits {
        glob: "src/**".into(),
        metrics: limits(Some(6.0), Some(18.0)),
    });
    config.limits.by_entity_glob = vec![
        EntityGlobLimits {
            glob: r"App\*View::__construct".into(),
            metrics: limits(Some(10.0), None),
        },
        EntityGlobLimits {
            glob: r"App\Billing\*".into(),
            metrics: limits(None, Some(12.0)),
        },
    ];
    let first = violation(&config, fqn).unwrap();
    assert_eq!(first.severity, Severity::Error);
    assert_eq!(first.limit, Some(12.0));
    config
        .limits
        .by_entity
        .insert(fqn.into(), limits(None, Some(16.0)));
    let exact = violation(&config, fqn).unwrap();
    assert_eq!(exact.severity, Severity::Warn);
    assert_eq!(exact.limit, Some(10.0));
    assert_eq!(
        violation(&config, &fqn.to_lowercase()).unwrap().limit,
        Some(6.0)
    );
    assert_eq!(
        violation(&config, r"App\InvoiceView::__construct")
            .unwrap()
            .limit,
        Some(10.0)
    );
}

#[test]
fn strict_loader_accepts_fqn_patterns_and_records_provenance() {
    let fixture = Fixture::new("limits:\n  by_entity_glob:\n    - glob: 'App\\*View::__construct'\n      function_params: {warn: 12, error: 24}\n");
    let load = ConfigLoader::new().load(&fixture.0, true, true).unwrap();
    assert_eq!(load.config.limits.by_entity_glob.len(), 1);
    assert_eq!(
        load.config.limits.by_entity_glob[0].glob,
        r"App\*View::__construct"
    );
    assert_eq!(
        load.provenance["limits.by_entity_glob.0.function_params.warn"].line,
        4
    );
}

#[test]
fn invalid_fqn_globs_are_reported_without_discarding_valid_siblings() {
    let fixture = Fixture::new("limits:\n  by_entity_glob:\n    - glob: '['\n      function_params: {warn: 12}\n    - function_params: {warn: 12}\n    - glob: 'app.*'\n      function_params: {warn: 8, error: nope}\n      typo_metric: {warn: 1}\n");
    let load = ConfigLoader::new().load(&fixture.0, false, true).unwrap();
    assert_eq!(load.config.limits.by_entity_glob.len(), 1);
    assert_eq!(
        load.config.limits.by_entity_glob[0].metrics["function_params"].warn,
        Some(8.0)
    );
    assert!(
        load.config.limits.by_entity_glob[0].metrics["function_params"]
            .error
            .is_none()
    );
    assert!(!load.config.limits.by_entity_glob[0]
        .metrics
        .contains_key("typo_metric"));
    assert!(ConfigLoader::new().load(&fixture.0, true, false).is_err());
    assert!(load.diagnostics.iter().any(|d| d.path.ends_with(".error")));
}
