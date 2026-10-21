use analyzer_core::config::{Config, ConfigLoad, ConfigLoader};
use analyzer_core::ir::{FileModel, FunctionModel, Parameter};
use analyzer_core::metrics::MetricRegistry;
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Fixture {
    fn new(source: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "codefactor-config-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("analyzer.yaml"), source).unwrap();
        Self(root)
    }
    fn load(&self, verbose: bool) -> ConfigLoad {
        ConfigLoader::new()
            .load(&self.0.join("analyzer.yaml"), false, verbose)
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn unknown_section_keys_preserve_valid_siblings_and_obey_quiet_and_strict_modes() {
    let fixture = Fixture::new(
        "architecture: {enabled: false, misspelled_key: true}\nalgorithmic_complexity: {enabled: false, misspelled_key: true}\n",
    );
    for verbose in [false, true] {
        let load = fixture.load(verbose);
        assert!(!load.config.architecture.enabled);
        assert!(!load.config.algorithmic_complexity.enabled);
        assert_eq!(load.skipped_keys.len(), 2);
        assert_eq!(load.diagnostics.len(), if verbose { 2 } else { 0 });
        for diagnostic in load.diagnostics {
            assert!(diagnostic.path.ends_with(".misspelled_key"));
            assert_eq!(diagnostic.message, "unknown key skipped");
            assert!(diagnostic.source.unwrap().ends_with("analyzer.yaml"));
            assert!(diagnostic.line.is_some());
        }
    }
    let error = ConfigLoader::new()
        .load(&fixture.0.join("analyzer.yaml"), true, false)
        .unwrap_err()
        .to_string();
    assert!(error.contains("architecture.misspelled_key"));
    assert!(error.contains("algorithmic_complexity.misspelled_key"));
}

#[test]
fn algorithmic_invalid_leaves_preserve_all_override_scopes_and_symbolic_error() {
    let fixture = Fixture::new(
        r#"algorithmic_complexity:
  enabled: false
  heuristics: invalid
  unknown: warn
  limits: {warn: 'N^3', error: N, typo: N}
  by_language:
    python: {enabled: true, heuristics: false, unknown: invalid, limits: {warn: N, typo: N}}
    rust: invalid
  by_path:
    - invalid
    - {glob: '[', enabled: false}
    - {glob: 'src/**', enabled: true, heuristics: invalid, limits: {error: 'N^2', typo: N}}
  by_entity:
    pkg.worker.run: {enabled: true, unknown: error, extra: true, limits: {warn: '2^N', error: 'N^3'}}
    pkg.bad: invalid
"#,
    );
    let load = fixture.load(true);
    let config = load.config.algorithmic_complexity;
    assert!(!config.enabled);
    assert!(config.heuristics);
    assert!(config.limits.warn.is_none());
    assert_eq!(config.limits.error.as_deref(), Some("N"));
    assert_eq!(config.by_language["python"].enabled, Some(true));
    assert_eq!(config.by_language["python"].heuristics, Some(false));
    assert!(config.by_language["python"].unknown.is_none());
    assert!(!config.by_language.contains_key("rust"));
    assert_eq!(config.by_path.len(), 1);
    assert_eq!(config.by_path[0].enabled, Some(true));
    assert_eq!(config.by_path[0].limits.error.as_deref(), Some("N^2"));
    assert_eq!(config.by_entity["pkg.worker.run"].enabled, Some(true));
    assert_eq!(
        config.by_entity["pkg.worker.run"].limits.error.as_deref(),
        Some("N^3")
    );
    assert!(!config.by_entity.contains_key("pkg.bad"));
    assert_eq!(
        load.provenance["algorithmic_complexity.by_path.0.glob"].line,
        12
    );
    assert!(load
        .skipped_keys
        .contains(&"algorithmic_complexity.by_entity.pkg.worker.run.extra".into()));
    let invalid = load
        .diagnostics
        .iter()
        .find(|d| d.path == "algorithmic_complexity.heuristics")
        .unwrap();
    assert_eq!(invalid.line, Some(3));
    assert!(invalid
        .source
        .as_deref()
        .unwrap()
        .ends_with("analyzer.yaml"));
    assert!(!load.diagnostics.iter().any(|d| d.path.is_empty()));
}

#[test]
fn architecture_invalid_rules_and_declarations_preserve_other_settings() {
    let fixture = Fixture::new(
        r#"architecture:
  enabled: false
  profile: extended
  max_evidence: 0
  rules:
    arch_dry: {min_tokens: 1, min_statements: 3, min_sites: 2}
    arch_isp: {min_methods: 6, max_usage_ratio: 2}
    unknown_rule: {}
  disabled_rules: [arch_dry, unknown_rule, arch_srp]
  exclude: ['[', 'vendor/**']
  composition_roots: ['bad', 'dir:src']
  layers:
    - invalid
    - {name: app, selectors: ['bad', 'dir:src'], unknown: true}
    - {name: app, selectors: ['dir:duplicate']}
    - {name: empty, selectors: ['bad']}
    - {name: infra, selectors: ['dir:infra']}
  forbidden_dependencies:
    - invalid
    - {from: app, to: infra, unknown: true}
    - {from: app, to: app}
    - {from: app, to: missing}
    - {from: app}
"#,
    );
    let load = fixture.load(true);
    let config = load.config.architecture;
    assert!(!config.enabled);
    assert_eq!(config.max_evidence, 50);
    assert!(config.rules["arch_dry"].min_tokens.is_none());
    assert_eq!(config.rules["arch_dry"].min_statements, Some(3));
    assert_eq!(config.rules["arch_isp"].min_methods, Some(6));
    assert!(config.rules["arch_isp"].max_usage_ratio.is_none());
    assert!(!config.rules.contains_key("unknown_rule"));
    assert_eq!(config.disabled_rules, ["arch_dry", "arch_srp"]);
    assert_eq!(config.exclude, ["vendor/**"]);
    assert_eq!(config.composition_roots, ["dir:src"]);
    assert_eq!(config.layers.len(), 2);
    assert_eq!(config.layers[0].selectors, ["dir:src"]);
    assert_eq!(config.layers[1].name, "infra");
    assert_eq!(config.forbidden_dependencies.len(), 1);
    assert_eq!(config.forbidden_dependencies[0].to, "infra");
    assert_eq!(load.provenance["architecture.layers.1.name"].line, 17);
    assert_eq!(
        load.provenance["architecture.forbidden_dependencies.0.to"].line,
        20
    );
    config.validate().unwrap();
    for unknown in [
        "architecture.rules.arch_dry.min_sites",
        "architecture.rules.unknown_rule",
        "architecture.disabled_rules.1",
    ] {
        assert!(
            load.skipped_keys.contains(&unknown.to_string()),
            "{unknown}"
        );
    }
    assert!(ConfigLoader::new()
        .load(&fixture.0.join("analyzer.yaml"), true, false)
        .is_err());
}

#[test]
fn malformed_section_shapes_do_not_discard_other_config_and_default_values_serialize() {
    for source in [
        "architecture: invalid\nalgorithmic_complexity: {enabled: false}\n",
        "architecture: {enabled: false}\nalgorithmic_complexity: invalid\n",
    ] {
        let fixture = Fixture::new(source);
        let load = fixture.load(false);
        assert!(!load.diagnostics.is_empty());
        assert_eq!(
            load.config.architecture.enabled,
            source.starts_with("architecture: invalid")
        );
        assert_eq!(
            load.config.algorithmic_complexity.enabled,
            !source.starts_with("architecture: invalid")
        );
        assert!(!load.diagnostics.iter().any(|d| d.path.is_empty()));
    }
    assert!(serde_json::to_value(Config::default()).unwrap()["metrics"]
        ["count_variadic_parameters"]
        .as_bool()
        .unwrap());
    let loader = ConfigLoader::new();
    let key = loader
        .keys()
        .into_iter()
        .find(|key| key.name == "metrics.count_variadic_parameters")
        .unwrap();
    assert_eq!(key.default, Some(serde_yaml::Value::Bool(true)));
}

#[test]
fn variadic_counting_is_independent_of_receiver_counting_and_validates_boolean_values() {
    let file = FileModel {
        language: "python".into(),
        functions: vec![FunctionModel {
            fqn: "C.run".into(),
            parameters: vec![
                Parameter {
                    name: "self".into(),
                    implicit_receiver: true,
                    ..Default::default()
                },
                Parameter {
                    name: "value".into(),
                    ..Default::default()
                },
                Parameter {
                    name: "args".into(),
                    variadic: true,
                    ..Default::default()
                },
                Parameter {
                    name: "kwargs".into(),
                    variadic: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }],
        ..Default::default()
    };
    let registry = MetricRegistry::new();
    for (variadic, receiver, expected) in [
        (true, false, 3.0),
        (false, false, 1.0),
        (false, true, 2.0),
        (true, true, 4.0),
    ] {
        let fixture = Fixture::new(&format!("metrics: {{count_variadic_parameters: {variadic}, count_implicit_receiver: {receiver}}}\n"));
        let load = fixture.load(false);
        let params = registry
            .calculate(&file, &load.config)
            .into_iter()
            .find(|m| m.id.as_ref() == "function_params")
            .unwrap();
        assert_eq!(params.value, expected);
    }
    let load = Fixture::new(
        "metrics: {count_variadic_parameters: invalid, count_implicit_receiver: true}\n",
    )
    .load(false);
    assert!(load.config.metrics.count_variadic_parameters);
    assert!(load.config.metrics.count_implicit_receiver);
    assert_eq!(
        load.diagnostics[0].path,
        "metrics.count_variadic_parameters"
    );
}

#[test]
fn core_version_is_registered_validated_and_only_read_from_primary_yaml() {
    let fixture = Fixture::new("include: included.yaml\ncore: {version: '0.2.0'}\n");
    std::fs::write(
        fixture.0.join("included.yaml"),
        "core: {version: '9.9.9'}\npaths: {exclude: ['generated/**']}\n",
    )
    .unwrap();
    let loader = ConfigLoader::new();
    let load = loader
        .load(&fixture.0.join("analyzer.yaml"), true, true)
        .unwrap();
    assert_eq!(load.config.core.version, "0.2.0");
    assert_eq!(load.config.paths.exclude, vec!["generated/**"]);
    assert!(load
        .diagnostics
        .iter()
        .any(|d| d.message.contains("primary config")));
    assert!(loader.keys().iter().any(|key| key.name == "core.version"));
    std::fs::write(fixture.0.join("analyzer.yaml"), "include: included.yaml\n").unwrap();
    assert_eq!(fixture.load(false).config.core.version, "latest");
    for value in ["null", "true", "123", "'^0.1.0'", "'../0.1.0'", "'v0.1.0'"] {
        std::fs::write(
            fixture.0.join("analyzer.yaml"),
            format!("core: {{version: {value}}}\n"),
        )
        .unwrap();
        let load = fixture.load(true);
        assert_eq!(load.config.core.version, "latest");
        assert!(load.diagnostics.iter().any(|d| d.path == "core.version"));
    }
}
