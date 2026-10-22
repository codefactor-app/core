use serde_json::Value;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    process::{Command, Output},
};

struct Project {
    directory: PathBuf,
    root: PathBuf,
}

impl Project {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "codefactor-baseline-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = directory.join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("analyzer.yaml"),
            "cache: {enabled: false}\nalgorithmic_complexity: {enabled: false}\nlimits:\n  function_params: {warn: 0, error: 100}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("module.py"),
            "def keep(value):\n    return value\n",
        )
        .unwrap();
        Self { directory, root }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_codefactor"));
        command.current_dir(&self.directory).env_remove("RUST_LOG");
        command
    }

    fn analyze(&self, kind: &str, root: &Path, arguments: &[&str]) -> Output {
        self.command()
            .arg(kind)
            .arg(root)
            .args(["--format", "json", "--no-architecture", "--config"])
            .arg(self.root.join("analyzer.yaml"))
            .args(arguments)
            .output()
            .unwrap()
    }

    fn scan(&self, arguments: &[&str]) -> Value {
        json(self.analyze("scan", &self.root, arguments))
    }

    fn baseline(&self, kind: &str, root: &Path, arguments: &[&str]) -> Output {
        self.command()
            .args(["baseline", kind, "--root"])
            .arg(root)
            .arg("--config")
            .arg(self.root.join("analyzer.yaml"))
            .args(arguments)
            .output()
            .unwrap()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn json(output: Output) -> Value {
    success(&output);
    serde_json::from_slice(&output.stdout).unwrap()
}

fn entries(path: &Path) -> BTreeSet<String> {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn diagnostics(report: &Value) -> &[Value] {
    report["diagnostics"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
}

#[test]
fn custom_baseline_paths_are_relative_to_scan_root_and_replace_the_default() {
    let project = Project::new();
    std::fs::create_dir(project.root.join("policy")).unwrap();
    let custom = project.root.join("policy/accepted.json");
    success(&project.baseline(
        "create",
        &project.root,
        &["--baseline", "policy/accepted.json"],
    ));
    assert_eq!(entries(&custom).len(), 1);
    assert!(!project.directory.join("policy/accepted.json").exists());
    std::fs::write(
        project.root.join(".analyzer-baseline.json"),
        r#"["obsolete:function_params:warn"]"#,
    )
    .unwrap();

    for path in ["policy/accepted.json", custom.to_str().unwrap()] {
        let report = project.scan(&["--baseline", path]);
        assert_eq!(report["baseline_path"], custom.to_str().unwrap());
        assert_eq!(report["summary"]["baseline_suppressed"], 1);
        assert_eq!(report["summary"]["baseline_stale"], 0);
        assert!(report["violations"].as_array().unwrap().is_empty());
        assert!(diagnostics(&report).is_empty());
    }

    let console = project
        .command()
        .arg("scan")
        .arg(&project.root)
        .args([
            "--baseline",
            "policy/accepted.json",
            "--no-architecture",
            "--no-explain",
        ])
        .output()
        .unwrap();
    success(&console);
    let console = String::from_utf8(console.stdout).unwrap();
    assert!(console.contains("Baseline:"), "{console}");
    assert!(console.contains(custom.to_str().unwrap()), "{console}");
    assert!(console.contains("скрыто 1"), "{console}");

    // The shared metrics options must affect CI exit status even though its JSON
    // contains metric aggregates rather than the complete scan report.
    let unsuppressed = project.analyze("metrics", &project.root, &["--fail-on-warn"]);
    assert_eq!(unsuppressed.status.code(), Some(1));
    let suppressed = project.analyze(
        "metrics",
        &project.root,
        &["--baseline", custom.to_str().unwrap(), "--fail-on-warn"],
    );
    assert!(json(suppressed).is_array());
}

#[test]
fn no_baseline_bypasses_suppression_and_conflicts_with_custom_paths() {
    let project = Project::new();
    success(&project.baseline("create", &project.root, &[]));
    assert!(project.scan(&[])["violations"]
        .as_array()
        .unwrap()
        .is_empty());
    let report = project.scan(&["--no-baseline"]);
    assert_eq!(report["violations"].as_array().unwrap().len(), 1);
    assert_eq!(report["violations"][0]["rule"], "function_params");
    assert!(report["baseline_path"].is_null());
    assert_eq!(report["summary"]["baseline_suppressed"], 0);
    assert_eq!(report["summary"]["baseline_stale"], 0);
    for kind in ["scan", "metrics"] {
        let bypassed = project.analyze(kind, &project.root, &["--no-baseline", "--fail-on-warn"]);
        assert_eq!(bypassed.status.code(), Some(1));
        let conflict = project.analyze(
            kind,
            &project.root,
            &["--baseline", "accepted.json", "--no-baseline"],
        );
        assert!(!conflict.status.success());
        let stderr = String::from_utf8_lossy(&conflict.stderr);
        assert!(
            stderr.contains("--baseline") && stderr.contains("--no-baseline"),
            "{stderr}"
        );
    }
}

#[test]
fn update_creates_a_missing_baseline_and_reports_new_and_removed_entries() {
    let project = Project::new();
    let path = project.root.join("accepted.json");
    let first = project.baseline("update", &project.root, &["--baseline", "accepted.json"]);
    success(&first);
    let stdout = String::from_utf8(first.stdout).unwrap();
    assert!(stdout.contains("+1 новых"), "{stdout}");
    assert!(stdout.contains("−0 удалено"), "{stdout}");
    let mut old = entries(&path);
    old.insert("removed.py:removed:function_params:warn".into());
    std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
    std::fs::write(
        project.root.join("module.py"),
        "def keep(value):\n    return value\n\ndef added(value):\n    return value\n",
    )
    .unwrap();
    let before = project.scan(&["--baseline", "accepted.json"]);
    assert_eq!(before["summary"]["baseline_suppressed"], 1);
    assert_eq!(before["summary"]["baseline_stale"], 1);
    assert_eq!(before["violations"].as_array().unwrap().len(), 1);

    let updated = project.baseline("update", &project.root, &["--baseline", "accepted.json"]);
    success(&updated);
    let stdout = String::from_utf8(updated.stdout).unwrap();
    assert!(stdout.contains("+1 новых"), "{stdout}");
    assert!(stdout.contains("−1 удалено"), "{stdout}");
    let expected = serde_json::from_value::<Vec<analyzer_core::rules::Violation>>(
        project.scan(&["--no-baseline"])["violations"].clone(),
    )
    .unwrap()
    .iter()
    .map(analyzer_core::rules::Violation::fingerprint)
    .collect::<BTreeSet<_>>();
    assert_eq!(entries(&path), expected);
    let after = project.scan(&["--baseline", "accepted.json"]);
    assert_eq!(after["summary"]["baseline_suppressed"], 2);
    assert_eq!(after["summary"]["baseline_stale"], 0);
    assert!(after["violations"].as_array().unwrap().is_empty());
}

#[test]
fn malformed_and_unreadable_baselines_warn_without_interrupting_analysis() {
    let project = Project::new();
    std::fs::write(project.root.join("invalid.json"), "{not-json").unwrap();
    // A directory produces a read error regardless of the invoking user's UID.
    std::fs::create_dir(project.root.join("unreadable.json")).unwrap();
    for name in ["invalid.json", "unreadable.json"] {
        let output = project.analyze("scan", &project.root, &["--baseline", name]);
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let report = json(output);
        assert_eq!(report["summary"]["files"], 1);
        assert_eq!(report["violations"].as_array().unwrap().len(), 1);
        assert_eq!(report["summary"]["baseline_suppressed"], 0);
        assert_eq!(report["summary"]["baseline_stale"], 0);
        assert_eq!(
            report["baseline_path"],
            project.root.join(name).to_str().unwrap()
        );
        let notes = diagnostics(&report);
        assert_eq!(notes.len(), 1, "{report}");
        let note = notes[0].as_str().unwrap();
        assert!(note.to_lowercase().contains("baseline"), "{note}");
        assert!(note.contains(name), "{note}");
        assert!(stderr.contains(note), "{stderr}");
    }
}

#[test]
fn missing_default_is_silent_and_disabled_baseline_does_not_read_invalid_files() {
    let project = Project::new();
    let missing = project.analyze("scan", &project.root, &[]);
    assert!(missing.stderr.is_empty(), "{:?}", missing.stderr);
    let report = json(missing);
    assert!(diagnostics(&report).is_empty());
    assert_eq!(report["summary"]["baseline_suppressed"], 0);
    assert_eq!(report["summary"]["baseline_stale"], 0);
    std::fs::write(project.root.join(".analyzer-baseline.json"), "not json").unwrap();
    let disabled = project.analyze("scan", &project.root, &["--no-baseline"]);
    assert!(disabled.stderr.is_empty(), "{:?}", disabled.stderr);
    let report = json(disabled);
    assert!(diagnostics(&report).is_empty());
    assert!(report["baseline_path"].is_null());
    assert_eq!(report["violations"].as_array().unwrap().len(), 1);
}

#[test]
fn single_file_baselines_resolve_against_the_source_parent() {
    let project = Project::new();
    let directory = project.root.join("nested");
    std::fs::create_dir(&directory).unwrap();
    let selected = directory.join("selected.py");
    std::fs::write(&selected, "def selected(value):\n    return value\n").unwrap();
    std::fs::write(
        directory.join("sibling.py"),
        "def sibling(value):\n    return value\n",
    )
    .unwrap();
    success(&project.baseline("create", &selected, &["--baseline", "accepted.json"]));
    let custom = directory.join("accepted.json");
    assert_eq!(entries(&custom).len(), 1);
    assert!(!project.root.join("accepted.json").exists());
    let report = json(project.analyze("scan", &selected, &["--baseline", "accepted.json"]));
    assert_eq!(report["summary"]["files"], 1);
    assert_eq!(report["baseline_path"], custom.to_str().unwrap());
    assert_eq!(report["summary"]["baseline_suppressed"], 1);
    assert!(report["violations"].as_array().unwrap().is_empty());

    success(&project.baseline("create", &selected, &[]));
    assert!(directory.join(".analyzer-baseline.json").is_file());
    let report = json(project.analyze("scan", &selected, &[]));
    assert_eq!(report["summary"]["baseline_suppressed"], 1);
    assert_eq!(report["summary"]["baseline_stale"], 0);
}
