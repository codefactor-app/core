use analyzer_core::{
    config::VendorParse, resolution::ResolutionContext, Analyzer, Config, LanguageRegistry,
};

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
                "vendor-tsconfig-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )))
    }
    fn write(&self, path: &str, source: &str) {
        let path = self.0.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }
    fn scan(&self, config: Config) -> analyzer_core::AnalysisReport {
        let mut registry = LanguageRegistry::new();
        registry.register(analyzer_typescript::TypeScript);
        Analyzer::new(registry, config).scan(&self.0).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn config() -> Config {
    let mut config = Config::default();
    config.cache.enabled = false;
    config
}

#[test]
fn unused_vendor_configs_are_quiet_including_custom_and_nested_vendor_paths() {
    let fixture = Fixture::new();
    fixture.write("main.ts", "export function run() {}");
    fixture.write(
        "node_modules/noisy/tsconfig.json",
        r#"{"extends":"@absent/config"}"#,
    );
    fixture.write("external/noisy/jsconfig.json", "invalid JSON");
    fixture.write(
        "nested/node_modules/noisy/tsconfig.json",
        r#"{"extends":"@absent/config"}"#,
    );
    for parse in [VendorParse::None, VendorParse::Lazy, VendorParse::Full] {
        let mut config = config();
        config.vendor.paths = vec!["**/node_modules/**".into(), "external/**".into()];
        config.vendor.parse = parse;
        let report = fixture.scan(config);
        assert!(
            report.resolution_diagnostics.is_empty(),
            "{:?}",
            report.resolution_diagnostics
        );
    }
}

#[test]
fn project_and_explicitly_extended_vendor_configs_keep_diagnostics() {
    let fixture = Fixture::new();
    fixture.write("main.ts", "export function run() {}");
    fixture.write("tsconfig.json", r#"{"extends":"config-package"}"#);
    fixture.write(
        "node_modules/config-package/tsconfig.json",
        r#"{"extends":"@absent/config"}"#,
    );
    let report = fixture.scan(config());
    assert!(report
        .resolution_diagnostics
        .iter()
        .any(|d| d.contains("@absent/config")));
    fixture.write("node_modules/config-package/tsconfig.json", "invalid JSON");
    let report = fixture.scan(config());
    assert!(report
        .resolution_diagnostics
        .iter()
        .any(|d| d.contains("config-package/tsconfig.json")));
    fixture.write("tsconfig.json", r#"{"extends":"@absent/project"}"#);
    assert!(fixture
        .scan(config())
        .resolution_diagnostics
        .iter()
        .any(|d| d.contains("@absent/project")));
}

#[test]
fn configs_for_parsed_vendor_sources_are_loaded_and_tracked_on_demand() {
    let fixture = Fixture::new();
    fixture.write("main.ts", "export function run() {}");
    fixture.write("node_modules/demo/src/index.ts", "export function run() {}");
    fixture.write(
        "node_modules/demo/tsconfig.json",
        r#"{"extends":"./base.json"}"#,
    );
    fixture.write(
        "node_modules/demo/base.json",
        r#"{"extends":"@absent/config"}"#,
    );
    let config = config();
    let initial = ResolutionContext::load(&fixture.0, &config);
    assert!(initial.diagnostics.is_empty());
    assert!(!initial
        .metadata_paths
        .contains(&fixture.0.join("node_modules/demo/tsconfig.json")));
    let expanded = ResolutionContext::load_for_sources(
        &fixture.0,
        &config,
        ["node_modules/demo/src/index.ts"],
    );
    assert!(expanded
        .diagnostics
        .iter()
        .any(|d| d.contains("@absent/config")));
    assert!(expanded
        .metadata_paths
        .contains(&fixture.0.join("node_modules/demo/base.json")));
    let mut full = config.clone();
    full.vendor.parse = VendorParse::Full;
    assert!(fixture
        .scan(full)
        .resolution_diagnostics
        .iter()
        .any(|d| d.contains("@absent/config")));
    let mut registry = LanguageRegistry::new();
    registry.register(analyzer_typescript::TypeScript);
    let analyzer = Analyzer::new(registry, config);
    let mut report = analyzer.scan(&fixture.0).unwrap();
    assert!(report.resolution_diagnostics.is_empty());
    analyzer
        .expand_vendor(&fixture.0, &mut report, "node_modules/demo")
        .unwrap();
    assert!(report
        .resolution_diagnostics
        .iter()
        .any(|d| d.contains("@absent/config")));
}

#[test]
fn parsed_vendor_sources_only_load_the_nearest_preferred_config() {
    let fixture = Fixture::new();
    fixture.write("main.ts", "export function run() {}");
    fixture.write("node_modules/demo/src/index.ts", "export function run() {}");
    fixture.write("node_modules/demo/src/tsconfig.json", "{}");
    fixture.write("node_modules/demo/src/jsconfig.json", "invalid JSON");
    fixture.write(
        "node_modules/demo/tsconfig.json",
        r#"{"extends":"@absent/dev-config"}"#,
    );
    let mut config = config();
    config.vendor.parse = VendorParse::Full;
    let report = fixture.scan(config.clone());
    assert!(
        report.resolution_diagnostics.is_empty(),
        "{:?}",
        report.resolution_diagnostics
    );
    let context = ResolutionContext::load_for_sources(
        &fixture.0,
        &config,
        ["node_modules/demo/src/index.ts"],
    );
    assert!(context
        .metadata_paths
        .contains(&fixture.0.join("node_modules/demo/src/tsconfig.json")));
    assert!(!context
        .metadata_paths
        .contains(&fixture.0.join("node_modules/demo/src/jsconfig.json")));
    assert!(!context
        .metadata_paths
        .contains(&fixture.0.join("node_modules/demo/tsconfig.json")));
    fixture.write(
        "node_modules/demo/src/tsconfig.json",
        r#"{"extends":"../tsconfig.json"}"#,
    );
    assert!(fixture
        .scan(config)
        .resolution_diagnostics
        .iter()
        .any(|d| d.contains("@absent/dev-config")));
}
