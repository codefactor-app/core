use std::{path::PathBuf, process::Command};

struct Project(PathBuf);
impl Project {
    fn new(config: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "codefactor-thresholds-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("analyzer.yaml"), config).unwrap();
        Self(root)
    }
    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_codefactor"))
            .current_dir(&self.0)
            .args(args)
            .output()
            .unwrap()
    }
}
impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn strict_config_commands_accept_and_expose_enabled_at_every_threshold_scope() {
    let project = Project::new(
        r#"limits:
  function_params: {warn: 4, error: 7, enabled: false}
  by_language:
    php:
      function_params: {enabled: true}
  by_path:
    - glob: 'src/Dto/**'
      function_params: {enabled: false}
  by_entity_glob:
    - glob: '*::run'
      function_params: {enabled: true}
  by_entity:
    App\Dto\Payload::run:
      function_params: {enabled: false}
  aggregates:
    function_params.avg: {warn: 3, enabled: false}
"#,
    );
    let output = project.run(&["config", "check", "--file", "analyzer.yaml", "--strict"]);
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("0 key(s) skipped"));
    let keys = project.run(&["config", "keys"]);
    assert!(keys.status.success());
    let keys = String::from_utf8_lossy(&keys.stdout);
    for key in [
        "limits.*.enabled",
        "limits.by_language.*.*.enabled",
        "limits.by_path.*.*.enabled",
        "limits.by_entity_glob.*.*.enabled",
        "limits.by_entity.*.*.enabled",
        "limits.aggregates.*.enabled",
    ] {
        assert!(keys.contains(key), "missing {key}");
    }
    let shown = project.run(&["config", "show", "--file", "analyzer.yaml", "--resolved"]);
    assert!(shown.status.success());
    let config: serde_yaml::Value = serde_yaml::from_slice(&shown.stdout).unwrap();
    assert_eq!(config["limits"]["function_params"]["enabled"], false);
    assert_eq!(
        config["limits"]["by_language"]["php"]["function_params"]["enabled"],
        true
    );
    assert_eq!(
        config["limits"]["by_path"][0]["function_params"]["enabled"],
        false
    );
    assert_eq!(
        config["limits"]["by_entity_glob"][0]["function_params"]["enabled"],
        true
    );
    assert_eq!(
        config["limits"]["by_entity"]["App\\Dto\\Payload::run"]["function_params"]["enabled"],
        false
    );
    assert_eq!(
        config["limits"]["aggregates"]["function_params.avg"]["enabled"],
        false
    );
}

#[test]
fn strict_config_rejects_string_enabled_values_and_scalar_off() {
    for threshold in ["{warn: 4, enabled: 'off'}", "off"] {
        let project = Project::new(&format!("limits:\n  function_params: {threshold}\n"));
        let output = project.run(&["config", "check", "--file", "analyzer.yaml", "--strict"]);
        assert!(!output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("expected"), "{output:?}");
    }
}
