use serde_json::Value;
use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Output, Stdio},
};

struct Project(PathBuf);

impl Project {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "codefactor-changes-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("project")).unwrap();
        let project = Self(root);
        project.config("architecture: {enabled: false}\n", 100);
        project
    }

    fn root(&self) -> PathBuf {
        self.0.join("project")
    }

    fn config(&self, extra: &str, error_limit: usize) {
        std::fs::write(
            self.root().join("analyzer.yaml"),
            format!(
                "cache: {{enabled: false}}\nalgorithmic_complexity: {{enabled: false}}\nlimits:\n  function_params: {{warn: 0, error: {error_limit}}}\n{extra}"
            ),
        )
        .unwrap();
    }

    fn source(&self, path: &str, source: &str) {
        let path = self.root().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }

    fn command(&self, format: &str, arguments: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_codefactor"));
        command
            .current_dir(&self.0)
            .env_remove("RUST_LOG")
            .arg("scan")
            .arg(self.root())
            .args(["--format", format])
            .args(arguments);
        command
    }

    fn run(&self, format: &str, arguments: &[&str]) -> Output {
        self.command(format, arguments).output().unwrap()
    }

    fn scan(&self, arguments: &[&str]) -> Value {
        let output = self.run("json", arguments);
        success(&output);
        json(&output)
    }

    fn save(&self, name: &str, value: &Value) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
        path
    }

    fn git(&self, arguments: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(self.root())
            .args(arguments)
            .output()
            .unwrap();
        success(&output);
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn success(output: &Output) {
    assert!(
        output.status.success(),
        "status: {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{error}: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn list_file(project: &Project, name: &str, contents: &str) -> PathBuf {
    let path = project.0.join(name);
    std::fs::write(&path, contents).unwrap();
    path
}

#[test]
fn compare_accepts_full_and_violations_reports_and_classifies_the_whole_scan() {
    let project = Project::new();
    project.source(
        "main.py",
        "def old(value):\n    return value\n\ndef keep(value):\n    return value\n",
    );
    let full = project.save("previous.json", &project.scan(&[]));
    let compact = project.save("violations.json", &project.scan(&["--only-violations"]));
    project.source(
        "main.py",
        "def fresh(value):\n    return value\n\ndef keep(value):\n    return value\n",
    );
    for previous in [&full, &compact] {
        let report = project.scan(&["--compare", previous.to_str().unwrap()]);
        let changes = &report["changes"];
        assert_eq!(changes["compared"], true);
        assert_eq!(changes["new"].as_array().unwrap().len(), 1);
        assert_eq!(changes["fixed"].as_array().unwrap().len(), 1);
        assert_eq!(changes["unchanged"], 1);
        assert!(changes["new"][0]["entity"]
            .as_str()
            .unwrap()
            .ends_with("fresh"));
        assert!(changes["fixed"][0]["entity"]
            .as_str()
            .unwrap()
            .ends_with("old"));
        assert_eq!(changes["filtered_out"]["total"], 0);
        assert_eq!(report["summary"]["warnings"], 2);
        assert_eq!(report["violations"].as_array().unwrap().len(), 2);
    }
}

#[test]
fn fail_on_new_is_opt_in_requires_comparison_and_preserves_error_precedence() {
    let project = Project::new();
    project.source("main.py", "def old(value):\n    return value\n");
    let previous = project.save("previous.json", &project.scan(&[]));
    let compare = ["--compare", previous.to_str().unwrap(), "--fail-on-new"];
    success(&project.run("json", &compare));
    let warning = project.run(
        "json",
        &["--compare", previous.to_str().unwrap(), "--fail-on-warn"],
    );
    assert_eq!(warning.status.code(), Some(1));

    project.source("main.py", "def fresh(value):\n    return value\n");
    success(&project.run("json", &["--compare", previous.to_str().unwrap()]));
    assert_eq!(project.run("json", &compare).status.code(), Some(1));
    project.config("architecture: {enabled: false}\n", 1);
    project.source(
        "main.py",
        "def fresh(first, second):\n    return first + second\n",
    );
    assert_eq!(project.run("json", &compare).status.code(), Some(2));

    let invalid = project.run("json", &["--fail-on-new"]);
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("--compare"));
}

#[test]
fn changed_files_stdin_normalizes_paths_and_keeps_complete_source_metrics() {
    let project = Project::new();
    project.source("a.py", "def first(value):\n    return value\n");
    project.source("b.py", "def second(value):\n    return value\n");
    let mut child = project
        .command("json", &["--changed-files", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    write!(
        child.stdin.take().unwrap(),
        "./a.py\nsub/../a.py\n{}\n./deleted.py\n{}\n\n",
        project.root().join("a.py").display(),
        project.root().join("deleted.py").display()
    )
    .unwrap();
    let output = child.wait_with_output().unwrap();
    success(&output);
    let report = json(&output);
    assert_eq!(report["changes"]["compared"], false);
    assert_eq!(
        report["changes"]["changed_files"],
        serde_json::json!(["a.py", "deleted.py"])
    );
    assert!(report["changes"]["new"].as_array().unwrap().is_empty());
    assert_eq!(report["violations"].as_array().unwrap().len(), 1);
    assert_eq!(report["violations"][0]["file"], "a.py");
    assert_eq!(report["summary"]["warnings"], 1);
    assert_eq!(report["changes"]["filtered_out"]["total"], 1);
    assert_eq!(report["changes"]["filtered_out"]["warnings"], 1);
    assert_eq!(report["summary"]["files"], 2);
    assert_eq!(report["files"].as_array().unwrap().len(), 2);
    assert!(report["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|metric| metric["path"] == "b.py"));

    let empty = list_file(&project, "empty.txt", "");
    let report = project.scan(&["--changed-files", empty.to_str().unwrap()]);
    assert!(report["violations"].as_array().unwrap().is_empty());
    assert_eq!(report["summary"]["warnings"], 0);
    assert_eq!(report["changes"]["filtered_out"]["total"], 2);
    assert_eq!(report["changes"]["changed_files"], serde_json::json!([]));
}

#[test]
fn changed_files_preserves_new_findings_outside_the_list_and_compares_before_filtering() {
    let project = Project::new();
    for (file, name) in [("a.py", "first"), ("b.py", "old"), ("c.py", "third")] {
        project.source(file, &format!("def {name}(value):\n    return value\n"));
    }
    let previous = project.save("previous.json", &project.scan(&[]));
    project.source("b.py", "def fresh(value):\n    return value\n");
    let touched = list_file(&project, "touched.txt", "a.py\n");
    let empty = list_file(&project, "empty.txt", "");
    for (list, kept, hidden) in [(&touched, 2, 1), (&empty, 1, 2)] {
        let report = project.scan(&[
            "--compare",
            previous.to_str().unwrap(),
            "--changed-files",
            list.to_str().unwrap(),
        ]);
        assert_eq!(report["changes"]["new"].as_array().unwrap().len(), 1);
        assert_eq!(report["changes"]["fixed"].as_array().unwrap().len(), 1);
        assert_eq!(report["changes"]["unchanged"], 2);
        assert_eq!(report["changes"]["filtered_out"]["total"], hidden);
        assert_eq!(report["violations"].as_array().unwrap().len(), kept);
        assert!(report["violations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|violation| violation["file"] == "b.py"));
        assert_eq!(report["summary"]["warnings"], kept);
    }
}

#[test]
fn git_changed_includes_tracked_untracked_deleted_and_space_containing_paths() {
    let project = Project::new();
    for (file, name) in [
        ("stable.py", "stable"),
        ("with space.py", "changed"),
        ("gone.py", "gone"),
    ] {
        project.source(file, &format!("def {name}(value):\n    return value\n"));
    }
    project.git(&["init", "-q"]);
    project.git(&["add", "."]);
    project.git(&[
        "-c",
        "user.name=Change Tests",
        "-c",
        "user.email=changes@example.test",
        "commit",
        "-qm",
        "Initial sources",
    ]);
    project.source(
        "with space.py",
        "def changed(first, second):\n    return first + second\n",
    );
    project.source("new space.py", "def added(value):\n    return value\n");
    std::fs::remove_file(project.root().join("gone.py")).unwrap();
    let report = project.scan(&["--git-changed", "HEAD"]);
    assert_eq!(
        report["changes"]["changed_files"],
        serde_json::json!(["gone.py", "new space.py", "with space.py"])
    );
    assert_eq!(report["summary"]["warnings"], 2);
    assert_eq!(report["changes"]["filtered_out"]["total"], 1);
    assert!(report["violations"]
        .as_array()
        .unwrap()
        .iter()
        .all(|violation| violation["file"] != "stable.py"));

    let invalid_ref = project.run("json", &["--git-changed", "missing-change-ref"]);
    assert_eq!(invalid_ref.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&invalid_ref.stderr).contains("missing-change-ref"));
    let no_repository = Project::new().run("json", &["--git-changed", "HEAD"]);
    assert_eq!(no_repository.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&no_repository.stderr)
        .to_lowercase()
        .contains("git"));
}

#[test]
fn comparison_read_errors_and_conflicting_file_filters_are_clear() {
    let project = Project::new();
    project.source("main.py", "def keep(value):\n    return value\n");
    let invalid = list_file(&project, "invalid.json", "{invalid-json");
    let missing = project.0.join("missing.json");
    for previous in [&invalid, &missing] {
        let output = project.run("json", &["--compare", previous.to_str().unwrap()]);
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(previous.to_str().unwrap()), "{stderr}");
        assert!(output.stdout.is_empty());
    }
    let list = list_file(&project, "touched.txt", "main.py\n");
    let output = project.run(
        "json",
        &[
            "--changed-files",
            list.to_str().unwrap(),
            "--git-changed",
            "HEAD",
        ],
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--changed-files") && stderr.contains("--git-changed"));
}

#[test]
fn change_sections_and_sarif_states_describe_new_fixed_and_unchanged_findings() {
    let project = Project::new();
    project.source(
        "main.py",
        "def old(value):\n    return value\n\ndef keep(value):\n    return value\n",
    );
    let previous = project.save("previous.json", &project.scan(&[]));
    project.source(
        "main.py",
        "def fresh(value):\n    return value\n\ndef keep(value):\n    return value\n",
    );
    let compare = ["--compare", previous.to_str().unwrap()];
    let sarif_output = project.run("sarif", &compare);
    success(&sarif_output);
    let sarif = json(&sarif_output);
    let results = sarif["runs"][0]["results"].as_array().unwrap();
    for state in ["new", "unchanged"] {
        assert_eq!(
            results
                .iter()
                .filter(|result| result["baselineState"] == state)
                .count(),
            1
        );
    }
    for (format, heading) in [("console", "Анализ изменений"), ("md", "## Changes")]
    {
        let output = project.run(format, &compare);
        success(&output);
        let text = String::from_utf8(output.stdout).unwrap();
        let section = text
            .split_once(heading)
            .unwrap_or_else(|| panic!("{text}"))
            .1;
        assert!(section.contains("fresh"), "{text}");
        assert!(section.contains("old"), "{text}");
    }
}

#[test]
fn duplicate_fingerprints_keep_their_new_occurrence_in_sarif_and_console() {
    let project = Project::new();
    project.source("main.py", "def repeat(value):\n    return value\n");
    let previous = project.save("previous.json", &project.scan(&[]));
    project.source(
        "main.py",
        "def repeat(value):\n    return value\n\ndef repeat(value):\n    return value\n",
    );
    let compare = ["--compare", previous.to_str().unwrap(), "--no-explain"];
    let report = project.scan(&compare);
    let violations = serde_json::from_value::<Vec<analyzer_core::rules::Violation>>(
        report["violations"].clone(),
    )
    .unwrap();
    assert_eq!(violations.len(), 2);
    assert_eq!(violations[0].fingerprint(), violations[1].fingerprint());
    assert_eq!(report["changes"]["unchanged"], 1);
    assert_eq!(report["changes"]["new"].as_array().unwrap().len(), 1);
    assert_eq!(report["changes"]["new"][0]["line"], 4);
    assert!(report["changes"]["fixed"].as_array().unwrap().is_empty());

    let output = project.run("sarif", &compare);
    success(&output);
    let sarif = json(&output);
    let results = sarif["runs"][0]["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    for (line, state) in [(1, "unchanged"), (4, "new")] {
        let result = results
            .iter()
            .find(|result| {
                result["locations"][0]["physicalLocation"]["region"]["startLine"] == line
            })
            .unwrap();
        assert_eq!(result["baselineState"], state);
    }

    let output = project.run("console", &compare);
    success(&output);
    let console = String::from_utf8(output.stdout).unwrap();
    let (changes, remaining) = console
        .split_once("Остальные нарушения по файлам")
        .unwrap_or_else(|| panic!("{console}"));
    let new = changes.split_once("Новые замечания").unwrap().1;
    assert_eq!(new.matches("строка 4").count(), 1, "{console}");
    assert!(!new.contains("строка 1"), "{console}");
    assert_eq!(remaining.matches("строка 1").count(), 1, "{console}");
    assert!(!remaining.contains("строка 4"), "{console}");
}

#[test]
fn architecture_cycles_match_members_omitted_from_evidence_and_new_cycles_fail_ci() {
    let project = Project::new();
    project.config("architecture: {max_evidence: 1}\n", 100);
    let empty = project.save("empty.json", &project.scan(&[]));
    project.source("a.py", "from b import ping_b\ndef ping_a(): ping_b()\n");
    project.source("b.py", "from c import ping_c\ndef ping_b(): ping_c()\n");
    project.source("c.py", "from a import ping_a\ndef ping_c(): ping_a()\n");
    let current = project.scan(&[]);
    let cycle = current["violations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|violation| violation["rule"] == "arch_cycle")
        .unwrap();
    let evidence = cycle["architecture"]["evidence"].as_array().unwrap();
    assert_eq!(evidence.len(), 1);
    let touched = ["a.py", "b.py", "c.py"]
        .into_iter()
        .find(|file| cycle["file"] != *file && evidence[0]["range"]["file"] != *file)
        .unwrap();
    let previous = project.save("current.json", &current);
    let list = list_file(&project, "touched.txt", &format!("{touched}\n"));
    let report = project.scan(&[
        "--compare",
        previous.to_str().unwrap(),
        "--changed-files",
        list.to_str().unwrap(),
    ]);
    assert!(report["changes"]["new"].as_array().unwrap().is_empty());
    assert!(report["violations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|violation| violation["rule"] == "arch_cycle"));
    assert!(report["summary"]["architecture_warnings"].as_u64().unwrap() > 0);
    success(&project.run("json", &["--fail-on-warn"]));
    let new_cycle = project.run(
        "json",
        &["--compare", empty.to_str().unwrap(), "--fail-on-new"],
    );
    assert_eq!(new_cycle.status.code(), Some(1));
}

#[test]
fn forbidden_dependencies_match_targets_omitted_from_public_evidence() {
    let project = Project::new();
    project.config(
        "architecture:\n  max_evidence: 1\n  layers:\n    - {name: domain, selectors: ['dir:src/domain']}\n    - {name: infra, selectors: ['dir:src/infra']}\n  forbidden_dependencies:\n    - {from: domain, to: infra}\n",
        100,
    );
    project.source(
        "src/domain/service.py",
        "from src.infra.first import first\nfrom src.infra.second import second\ndef run():\n    return first() + second()\n",
    );
    project.source("src/infra/first.py", "def first():\n    return 1\n");
    project.source("src/infra/second.py", "def second():\n    return 2\n");
    project.source("src/unrelated.py", "def unrelated():\n    return 0\n");
    let current = project.scan(&[]);
    let findings = current["violations"].as_array().unwrap();
    assert_eq!(findings.len(), 1, "{current}");
    let boundary = &findings[0];
    assert_eq!(boundary["rule"], "arch_boundary");
    let evidence = boundary["architecture"]["evidence"].as_array().unwrap();
    assert_eq!(evidence.len(), 1);
    assert!(boundary["architecture"]["evidence_total"].as_u64().unwrap() >= 2);
    let target = ["src/infra/first.py", "src/infra/second.py"]
        .into_iter()
        .find(|file| {
            boundary["file"] != *file
                && evidence.iter().all(|item| {
                    item["range"]["file"] != *file
                        && item["dependency"]["from_file"] != *file
                        && item["dependency"]["to_file"] != *file
                })
        })
        .unwrap();
    let previous = project.save("current.json", &current);
    for (file, expected) in [(target, 1), ("src/unrelated.py", 0)] {
        let list = list_file(&project, "touched.txt", &format!("{file}\n"));
        let report = project.scan(&[
            "--compare",
            previous.to_str().unwrap(),
            "--changed-files",
            list.to_str().unwrap(),
        ]);
        assert!(report["changes"]["new"].as_array().unwrap().is_empty());
        assert!(report["changes"]["fixed"].as_array().unwrap().is_empty());
        assert_eq!(report["changes"]["unchanged"], 1);
        assert_eq!(report["violations"].as_array().unwrap().len(), expected);
        assert_eq!(report["summary"]["architecture_warnings"], expected);
        assert_eq!(report["architecture"]["warnings"], expected);
        let coverage = report["architecture"]["rules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|rule| rule["rule"] == "arch_boundary")
            .unwrap();
        assert_eq!(coverage["findings"], expected);
        assert_eq!(
            report["changes"]["filtered_out"]["architecture_warnings"],
            1 - expected
        );
    }
}
