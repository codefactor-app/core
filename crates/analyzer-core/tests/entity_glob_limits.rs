use analyzer_core::config::{Config, ConfigLoader, EntityGlobLimits, PathLimits, Threshold};
use analyzer_core::ir::FileModel;
use analyzer_core::metrics::{distribution, AggregateGroup, MetricValue};
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
    BTreeMap::from([(
        "function_params".into(),
        Threshold {
            warn,
            error,
            enabled: None,
        },
    )])
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

#[test]
fn enabled_false_switches_a_rule_off_per_path_and_a_later_override_restores_it() {
    let fqn = r"App\Dto\Payload::__construct";
    let off = Threshold {
        warn: None,
        error: None,
        enabled: Some(false),
    };
    let on = Threshold {
        warn: None,
        error: None,
        enabled: Some(true),
    };
    let mut config = Config::default();
    config.limits.global = limits(Some(4.0), Some(7.0));
    assert!(violation(&config, fqn).is_some());
    config.limits.by_path.push(PathLimits {
        glob: "src/**".into(),
        metrics: BTreeMap::from([("function_params".into(), off.clone())]),
    });
    assert!(violation(&config, fqn).is_none());
    config.limits.by_entity_glob.push(EntityGlobLimits {
        glob: r"App\Dto\*".into(),
        metrics: BTreeMap::from([("function_params".into(), on)]),
    });
    let restored = violation(&config, fqn).expect("re-enabled by a later override");
    assert_eq!(restored.severity, Severity::Error);
    assert_eq!(restored.limit, Some(7.0));
    config.limits.by_entity_glob.clear();
    config.limits.by_path.clear();
    config.limits.global.insert("function_params".into(), off);
    assert!(violation(&config, fqn).is_none());
    let fixture = Fixture::new("limits:\n  by_path:\n    - glob: \"src/Dto/**\"\n      lcom4: {enabled: false}\n      function_params: {enabled: true, warn: 9}\n");
    let loaded = ConfigLoader::new().load(&fixture.0, true, true).unwrap();
    assert!(loaded.diagnostics.is_empty(), "{:?}", loaded.diagnostics);
    let path = &loaded.config.limits.by_path[0];
    assert_eq!(path.metrics["lcom4"].enabled, Some(false));
    assert_eq!(path.metrics["function_params"].enabled, Some(true));
    assert_eq!(path.metrics["function_params"].warn, Some(9.0));
}

#[test]
fn enabled_overrides_follow_all_scopes_and_preserve_inherited_thresholds() {
    let fixture = Fixture::new(
        r#"limits:
  function_params: {enabled: false, warn: 4, error: 7}
  by_language:
    php:
      function_params: {enabled: true, warn: 5}
  by_path:
    - glob: 'src/**'
      function_params: {enabled: false, error: 12}
  by_entity_glob:
    - glob: 'App\Dto\*'
      function_params: {enabled: true}
  by_entity:
    App\Dto\Payload::run:
      function_params: {enabled: false}
"#,
    );
    let fqn = r"App\Dto\Payload::run";
    let mut config = ConfigLoader::new()
        .load(&fixture.0, true, true)
        .unwrap()
        .config;
    assert!(violation(&config, fqn).is_none(), "exact entity disables");
    config
        .limits
        .by_entity
        .get_mut(fqn)
        .unwrap()
        .get_mut("function_params")
        .unwrap()
        .enabled = Some(true);
    let restored = violation(&config, fqn).unwrap();
    assert_eq!(restored.severity, Severity::Error);
    assert_eq!(restored.limit, Some(12.0));
    config.limits.by_entity.clear();
    assert_eq!(violation(&config, fqn).unwrap().limit, Some(12.0));
    config.limits.by_entity_glob.clear();
    assert!(violation(&config, fqn).is_none(), "path disables");
    config.limits.by_path.clear();
    assert_eq!(violation(&config, fqn).unwrap().limit, Some(7.0));
    config.limits.by_language.clear();
    assert!(violation(&config, fqn).is_none(), "global disables");
}

#[test]
fn aggregate_thresholds_can_be_disabled_and_reenabled_without_losing_bounds() {
    let fixture = Fixture::new(
        "limits:\n  aggregates:\n    function_params.avg: {warn: 4, error: 7, enabled: false}\n",
    );
    let mut config = ConfigLoader::new()
        .load(&fixture.0, true, true)
        .unwrap()
        .config;
    let groups = [AggregateGroup {
        name: "project".into(),
        metrics: BTreeMap::from([("function_params".into(), distribution(&[15.0], 1))]),
    }];
    let evaluate_aggregates = |config: &Config| {
        evaluate(
            &[],
            &[],
            &groups,
            config,
            &BTreeSet::new(),
            &BTreeMap::new(),
        )
    };
    assert!(evaluate_aggregates(&config).is_empty());
    config
        .limits
        .aggregates
        .get_mut("function_params.avg")
        .unwrap()
        .enabled = Some(true);
    let violations = evaluate_aggregates(&config);
    assert_eq!(violations.len(), 1);
    assert_eq!(violations[0].rule, "function_params.avg");
    assert_eq!(violations[0].severity, Severity::Error);
    assert_eq!(violations[0].limit, Some(7.0));
}

#[test]
fn enabled_requires_a_boolean_and_thresholds_round_trip_as_mappings() {
    let fixture =
        Fixture::new("limits:\n  function_params: {enabled: 'off', warn: 6}\n  lcom4: off\n");
    let loader = ConfigLoader::new();
    let load = loader.load(&fixture.0, false, true).unwrap();
    assert_eq!(load.config.limits.global["function_params"].warn, Some(6.0));
    assert_eq!(load.config.limits.global["function_params"].enabled, None);
    assert!(!load.config.limits.global.contains_key("lcom4"));
    assert!(load.diagnostics.iter().any(|d| {
        d.path == "limits.function_params.enabled" && d.message.contains("expected boolean")
    }));
    let strict = loader.load(&fixture.0, true, false).unwrap();
    assert_eq!(strict.diagnostics.len(), 2);
    for enabled in [None, Some(false), Some(true)] {
        let threshold = Threshold {
            enabled,
            ..Threshold::new(4.0, 7.0)
        };
        let yaml = serde_yaml::to_string(&threshold).unwrap();
        assert_eq!(yaml.contains("enabled:"), enabled.is_some());
        let restored: Threshold = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(restored.enabled, enabled);
        assert_eq!(restored.warn, Some(4.0));
        assert_eq!(restored.error, Some(7.0));
    }
}
