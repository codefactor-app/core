use serde_json::Value;
use std::{
    path::PathBuf,
    process::{Command, Output},
};

struct Project(PathBuf);
impl Project {
    fn new(other_namespace: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "codefactor-module-depth-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("billing.php"), format!("<?php namespace App\\Billing; class Invoice {{ static function run() {{ \\{other_namespace}\\Repository::run(); }} }}\n")).unwrap();
        std::fs::write(root.join("repository.php"), format!("<?php namespace {other_namespace}; class Repository {{ static function run() {{ \\App\\Billing\\Invoice::run(); }} }}\n")).unwrap();
        let project = Self(root);
        project.configure("");
        project
    }

    fn configure(&self, architecture: &str) {
        std::fs::write(self.0.join("analyzer.yaml"), format!("cache: {{enabled: false}}\nlimits:\n  module_cycle: {{warn: 0, error: 0}}\narchitecture:\n  exclude: []\n{architecture}")).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_codefactor"))
            .current_dir(&self.0)
            .args(args)
            .output()
            .unwrap()
    }

    fn scan(&self) -> Value {
        let output = self.run(&["scan", ".", "--format", "json", "--strict-config"]);
        assert!(matches!(output.status.code(), Some(0 | 2)), "{output:?}");
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(report["parse_errors"].as_array().unwrap().is_empty());
        report
    }
}
impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn cycles<'a>(report: &'a Value, rule: &str) -> Vec<&'a Value> {
    report["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|violation| violation["rule"] == rule)
        .collect()
}

fn source_identities(report: &Value) -> Vec<(String, String, Vec<String>)> {
    report["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| {
            (
                file["path"].as_str().unwrap().into(),
                file["module"].as_str().unwrap().into(),
                file["functions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|function| function["fqn"].as_str().unwrap().into())
                    .collect(),
            )
        })
        .collect()
}

#[test]
fn php_depth_collapses_submodule_cycles_without_changing_source_identities() {
    let project = Project::new("App\\Billing\\Store");
    let before = project.scan();
    assert_eq!(cycles(&before, "module_cycle").len(), 1);
    assert_eq!(cycles(&before, "arch_cycle").len(), 1);
    assert_eq!(
        cycles(&before, "module_cycle")[0]["module_cycle"]["participants"],
        serde_json::json!(["php:App\\Billing", "php:App\\Billing\\Store"])
    );
    project.configure("  module_depth: {php: 2}\n");
    let after = project.scan();
    assert!(cycles(&after, "module_cycle").is_empty());
    assert!(cycles(&after, "arch_cycle").is_empty());
    assert_eq!(source_identities(&before), source_identities(&after));
    let module_metrics = after["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|metric| metric["id"] == "module_cycle")
        .collect::<Vec<_>>();
    assert_eq!(module_metrics.len(), 2);
    for metric in module_metrics {
        let file = after["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == metric["path"])
            .unwrap();
        assert_eq!(metric["entity"], file["module"]);
        assert_eq!(metric["value"], 0.0);
    }
}

#[test]
fn different_top_modules_and_explicit_groups_keep_their_cycles() {
    let project = Project::new("App\\Shipping\\Store");
    project.configure("  module_depth: {php: 2}\n");
    let report = project.scan();
    assert_eq!(cycles(&report, "module_cycle").len(), 1);
    assert_eq!(cycles(&report, "arch_cycle").len(), 1);
    assert_eq!(
        cycles(&report, "module_cycle")[0]["module_cycle"]["participants"],
        serde_json::json!(["php:App\\Billing", "php:App\\Shipping"])
    );

    let grouped = Project::new("App\\Billing\\Store");
    grouped.configure("  module_depth: {php: 1}\n  modules:\n    - {name: billing, selectors: ['file:billing.php']}\n    - {name: storage, selectors: ['file:repository.php']}\n");
    let report = grouped.scan();
    assert_eq!(cycles(&report, "module_cycle").len(), 1);
    assert_eq!(cycles(&report, "arch_cycle").len(), 1);
    assert_eq!(
        cycles(&report, "module_cycle")[0]["module_cycle"]["participants"],
        serde_json::json!(["php:group:billing", "php:group:storage"])
    );
}

#[test]
fn strict_config_accepts_canonical_depth_keys_and_preserves_good_siblings_when_sanitizing() {
    let project = Project::new("App\\Billing\\Store");
    project
        .configure("  module_depth: {php: 2, python: 3, rust: 1, javascript: 2, typescript: 4}\n");
    let checked = project.run(&["config", "check", "--file", "analyzer.yaml", "--strict"]);
    assert!(checked.status.success(), "{checked:?}");
    let keys = project.run(&["config", "keys"]);
    assert!(keys.status.success(), "{keys:?}");
    assert!(String::from_utf8_lossy(&keys.stdout)
        .lines()
        .any(|line| line.starts_with("architecture.module_depth")));
    let shown = project.run(&["config", "show", "--file", "analyzer.yaml", "--resolved"]);
    assert!(shown.status.success(), "{shown:?}");
    let config: serde_yaml::Value = serde_yaml::from_slice(&shown.stdout).unwrap();
    for (language, depth) in [
        ("php", 2),
        ("python", 3),
        ("rust", 1),
        ("javascript", 2),
        ("typescript", 4),
    ] {
        assert_eq!(
            config["architecture"]["module_depth"][language].as_u64(),
            Some(depth)
        );
    }
    for invalid in ["python: 0", "python: -1", "python: '2'", "java: 2"] {
        project.configure(&format!("  module_depth: {{php: 2, {invalid}}}\n"));
        let checked = project.run(&["config", "check", "--file", "analyzer.yaml", "--strict"]);
        assert!(!checked.status.success(), "{invalid}: {checked:?}");
        let shown = project.run(&["config", "show", "--file", "analyzer.yaml", "--resolved"]);
        assert!(shown.status.success(), "{invalid}: {shown:?}");
        let config: serde_yaml::Value = serde_yaml::from_slice(&shown.stdout).unwrap();
        let depths = config["architecture"]["module_depth"].as_mapping().unwrap();
        assert_eq!(depths.len(), 1, "{invalid}: {config:?}");
        assert_eq!(
            config["architecture"]["module_depth"]["php"].as_u64(),
            Some(2)
        );
    }
}
