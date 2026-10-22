use std::{path::PathBuf, process::Command};

struct Project(PathBuf);
impl Project {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "codefactor-strict-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("analyzer.yaml"),
            "cache: {enabled: false}\nalgorithmic_complexity: {enabled: false}\n",
        )
        .unwrap();
        Self(root)
    }
    fn run(&self, arguments: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_codefactor"))
            .current_dir(&self.0)
            .args(arguments)
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
fn unknown_languages_fail_before_discovery_and_output_creation() {
    let project = Project::new();
    // An invalid root proves language validation happens before discovery.
    for language in ["ruts", "rust,ruts", "rust,,js"] {
        let output = project.run(&[
            "scan",
            "missing",
            "--lang",
            language,
            "--format",
            "json",
            "--out",
            "result.json",
        ]);
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("Unknown language"), "{stderr}");
        assert!(
            stderr.contains("rust") && stderr.contains("javascript"),
            "{stderr}"
        );
        assert!(!project.0.join("result.json").exists());
    }
    for language in [
        "rust",
        "javascript",
        "typescript",
        "python",
        "php",
        "js",
        "ts",
    ] {
        assert!(project
            .run(&["scan", ".", "--lang", language, "--format", "json"])
            .status
            .success());
    }
}

#[test]
fn parse_errors_are_sarif_notifications_and_strict_exit_is_opt_in() {
    for (name, source) in [
        ("bad.rs", "fn broken( {"),
        ("bad.py", "def broken(:\n pass"),
        ("bad.js", "function broken( {"),
        ("bad.ts", "function broken( {"),
        ("bad.php", "<?php function broken( {"),
    ] {
        let project = Project::new();
        std::fs::write(project.0.join(name), source).unwrap();
        let normal = project.run(&["scan", ".", "--format", "json"]);
        assert!(normal.status.success(), "{name}: {:?}", normal.stderr);
        let json: serde_json::Value = serde_json::from_slice(&normal.stdout).unwrap();
        assert!(
            json["summary"]["parse_errors"].as_u64().unwrap() > 0,
            "{name}"
        );
        for command in ["scan", "metrics"] {
            assert_eq!(
                project
                    .run(&[command, ".", "--format", "json", "--fail-on-parse-errors"])
                    .status
                    .code(),
                Some(1)
            );
        }
        let sarif = project.run(&["scan", ".", "--format", "sarif"]);
        assert!(sarif.status.success());
        let json: serde_json::Value = serde_json::from_slice(&sarif.stdout).unwrap();
        let invocation = &json["runs"][0]["invocations"][0];
        assert_eq!(invocation["executionSuccessful"], false);
        let notes = invocation["toolExecutionNotifications"].as_array().unwrap();
        assert!(!notes.is_empty());
        assert_eq!(notes[0]["level"], "error");
        assert_eq!(
            notes[0]["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
            name
        );
    }
}

#[test]
fn strict_parse_exit_preserves_rule_and_config_error_precedence() {
    let project = Project::new();
    std::fs::write(project.0.join("bad.rs"), "fn broken( {").unwrap();
    std::fs::write(
        project.0.join("good.rs"),
        "pub fn good(first: u32, second: u32) -> u32 { first + second }",
    )
    .unwrap();
    std::fs::write(project.0.join("analyzer.yaml"), "cache: {enabled: false}\nalgorithmic_complexity: {enabled: false}\nlimits:\n  function_params: {warn: 0, error: 1}\n").unwrap();
    assert_eq!(
        project
            .run(&["scan", ".", "--format", "json", "--fail-on-parse-errors"])
            .status
            .code(),
        Some(2)
    );
    std::fs::write(project.0.join("analyzer.yaml"), "limits: [").unwrap();
    assert_eq!(
        project
            .run(&["scan", ".", "--fail-on-parse-errors"])
            .status
            .code(),
        Some(3)
    );
}
