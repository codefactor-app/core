use analyzer_core::{Analyzer, Config, LanguageRegistry};
use serde_json::Value;
use std::{
    path::PathBuf,
    process::{Command, Output},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "codefactor-audit-regression-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn write(&self, name: &str, source: &str) {
        let path = self.0.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }
    fn run(&self, command: &str, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_codefactor"))
            .arg(command)
            .arg(&self.0)
            .args(args)
            .output()
            .unwrap()
    }
    fn json(&self, args: &[&str]) -> Value {
        let output = self.run("scan", args);
        assert!(
            matches!(output.status.code(), Some(0 | 2)),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn analyzer(&self, mut config: Config) -> Analyzer {
        config.cache.enabled = false;
        let mut registry = LanguageRegistry::new();
        registry.register(analyzer_python::Python);
        registry.register(analyzer_javascript::JavaScript);
        registry.register(analyzer_typescript::TypeScript);
        registry.register(analyzer_php::Php);
        registry.register(analyzer_rust::Rust);
        Analyzer::new(registry, config)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn module_initializer_keeps_source_metrics_and_mixed_language_costs_aligned() {
    let fixture = Fixture::new();
    fixture.write("entry.js", "function work() {}\nclass Box { run() { work(); } }\nfunction sibling() { work(); }\nwork();\n");
    fixture.write("lib.rs", "pub fn sum(items: &[u32]) -> u32 { let mut total = 0; for item in items { total += item; } total }\n");
    let mut config = Config::default();
    config.algorithmic_complexity.enabled = true;
    let report = fixture.analyzer(config).scan(&fixture.0).unwrap();
    assert_eq!(report.summary.functions, 4);
    let file = report.file("entry.js").unwrap();
    let initializer = file
        .functions
        .iter()
        .find(|f| f.is_module_initializer())
        .unwrap();
    assert!(initializer.algorithmic_complexity.is_none());
    assert!(report.file("lib.rs").unwrap().functions[0]
        .algorithmic_complexity
        .is_some());
    assert_eq!(
        report
            .metrics
            .iter()
            .find(|m| m.path.as_ref() == "entry.js" && m.id.as_ref() == "functions_per_file")
            .unwrap()
            .value,
        3.0
    );
    assert!(!report
        .metrics
        .iter()
        .any(|m| m.scope.as_ref() == "function" && m.entity.as_ref() == initializer.fqn));
    let init_id = format!("fn:javascript:entry.js:{}", initializer.fqn);
    let work_id = "fn:javascript:entry.js:entry.work";
    assert!(report
        .graph
        .edges
        .iter()
        .any(|edge| edge.from.as_ref() == init_id
            && edge.to.as_ref() == work_id
            && edge.confidence == analyzer_core::Confidence::Resolved));
    assert_eq!(
        report
            .metrics
            .iter()
            .find(|m| m.entity.as_ref() == "entry.work" && m.id.as_ref() == "fan_in")
            .unwrap()
            .value,
        3.0
    );
}

#[test]
fn only_real_comments_suppress_rules_across_all_languages() {
    let fixture = Fixture::new();
    fixture.write(
        "analyzer.yaml",
        "cache: {enabled: false}\nlimits:\n  function_params: {error: 2}\n",
    );
    let cases = [
        (
            "js",
            "const text = `first\n// analyzer-ignore: function_params `;\n",
            "// analyzer-ignore: function_params\n",
            "function work(a,b,c) { return a; }\n",
        ),
        (
            "ts",
            "const text = '// analyzer-ignore: function_params ';\n",
            "/* analyzer-ignore: function_params */\n",
            "function work(a:number,b:number,c:number) { return a; }\n",
        ),
        (
            "py",
            "text = '# analyzer-ignore: function_params '\n",
            "# analyzer-ignore: function_params\n",
            "def work(a,b,c):\n    return a\n",
        ),
        (
            "rs",
            "const TEXT: &str = r#\"// analyzer-ignore: function_params \"#;\n",
            "// analyzer-ignore: function_params\n",
            "fn work(a:i32,b:i32,c:i32)->i32 { a }\n",
        ),
        (
            "php",
            "<?php\n$text = '// analyzer-ignore: function_params ';\n",
            "<?php\n// analyzer-ignore: function_params\n",
            "function work($a,$b,$c) { return $a; }\n",
        ),
    ];
    for (extension, literal, comment, function) in cases {
        fixture.write(
            &format!("literal.{extension}"),
            &format!("{literal}{function}"),
        );
        fixture.write(
            &format!("comment.{extension}"),
            &format!("{comment}{function}"),
        );
    }
    let report = fixture.json(&["--format", "json"]);
    let violations = report["violations"].as_array().unwrap();
    for extension in ["js", "ts", "py", "rs", "php"] {
        assert!(
            violations
                .iter()
                .any(|v| v["rule"] == "function_params"
                    && v["file"] == format!("literal.{extension}")),
            "{extension}: {violations:?}"
        );
        assert!(
            !violations
                .iter()
                .any(|v| v["rule"] == "function_params"
                    && v["file"] == format!("comment.{extension}")),
            "{extension}: {violations:?}"
        );
    }
    assert_eq!(report["parse_errors"].as_array().unwrap().len(), 0);
}

#[test]
fn metric_filters_use_accessors_and_visibility_and_skip_nested_methods() {
    let fixture = Fixture::new();
    fixture.write("model.ts", "class Model { value() { return this.field; } get_work() { perform(); } public field = 1; nested() { function local() {} local(); } }\nfunction perform() {}\n");
    fixture.write("lib.rs", "struct Model { field: i32 } impl Model { fn hidden(&self) { work(); } pub(crate) fn restricted(&self) { work(); } pub fn public(&self) { work(); } } fn work() {}\n");
    let mut config = Config::default();
    config.metrics.count_accessors = false;
    config.metrics.count_private_methods = false;
    let report = fixture.analyzer(config).scan(&fixture.0).unwrap();
    let methods = |path: &str| {
        report
            .metrics
            .iter()
            .find(|m| m.id.as_ref() == "methods_per_class" && m.path.as_ref() == path)
            .unwrap()
            .value
    };
    assert_eq!(
        methods("model.ts"),
        2.0,
        "get_work and nested are methods; its local is a function"
    );
    assert_eq!(
        methods("lib.rs"),
        1.0,
        "only the public inherent method counts"
    );
}

#[test]
fn cognitive_scores_do_not_depend_on_call_count() {
    let fixture = Fixture::new();
    fixture.write("model.js", "function quiet(x) { if (x) { if (x > 1) { return 1; } } return 0; }\nfunction noisy(x) { if (x) { first(); second(); if (x > 1) { third(); return 1; } } return 0; }\nfunction first(){} function second(){} function third(){}\n");
    let report = fixture
        .analyzer(Config::default())
        .scan(&fixture.0)
        .unwrap();
    let score = |name: &str| {
        report
            .metrics
            .iter()
            .find(|m| m.id.as_ref() == "cognitive_complexity" && m.entity.ends_with(name))
            .unwrap()
            .value
    };
    assert_eq!(score("quiet"), 3.0);
    assert_eq!(score("quiet"), score("noisy"));
}

#[test]
fn scan_reuses_tsx_syntax_for_comment_line_counts() {
    let fixture = Fixture::new();
    fixture.write("view.tsx", "const view = <div>\n/* JSX text, not a JavaScript comment */\n</div>;\n// real comment\nfunction work() {}\n");
    let report = fixture
        .analyzer(Config::default())
        .scan(&fixture.0)
        .unwrap();
    assert!(report.parse_errors.is_empty(), "{:?}", report.parse_errors);
    assert_eq!(report.files[0].loc.total, 5);
    assert_eq!(report.files[0].loc.code, 4);
    assert_eq!(report.files[0].loc.comment, 1);
}

#[test]
fn duplicate_detection_compares_bodies_with_different_function_names() {
    let fixture = Fixture::new();
    let body = "let total = 0; for (let i = 0; i < n; i++) { total += i * 2; } if (total > 10) { total -= 1; } return total;";
    fixture.write("model.js", &format!("function alpha(n) {{ {body} }}\nfunction beta(n) {{ {body} }}\nfunction unique(n) {{ return n; }}\n"));
    let report = fixture
        .analyzer(Config::default())
        .scan(&fixture.0)
        .unwrap();
    let duplicates = report
        .metrics
        .iter()
        .filter(|m| m.id.as_ref() == "duplicate_functions")
        .collect::<Vec<_>>();
    assert_eq!(duplicates.len(), 2, "{duplicates:?}");
    assert!(duplicates
        .iter()
        .all(|m| m.value == 1.0 && (m.entity.ends_with("alpha") || m.entity.ends_with("beta"))));
}

#[test]
fn output_defaults_follow_configuration_and_only_violations_is_compact() {
    let fixture = Fixture::new();
    fixture.write("model.py", "def work(a,b,c):\n    return a\n");
    fixture.write("analyzer.yaml", "cache: {enabled: false}\noutput: {formats: [json]}\nlimits:\n  function_params: {error: 2}\n");
    let report = fixture.json(&[]);
    assert!(report.get("files").is_some());
    let compact = fixture.json(&["--only-violations"]);
    assert_eq!(compact["report_type"], "violations");
    assert_eq!(compact["violations"], report["violations"]);
    assert_eq!(compact["summary"], report["summary"]);
    for field in ["files", "metrics", "aggregates", "graph", "deferred_vendor"] {
        assert!(compact.get(field).is_none(), "{field}");
    }
    let output = fixture.run("metrics", &[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(serde_json::from_slice::<Value>(&output.stdout)
        .unwrap()
        .is_array());
    let console = fixture.run("scan", &["--format", "console"]);
    assert!(String::from_utf8_lossy(&console.stdout).contains("Codefactor · Анализ кода"));
    fixture.write(
        "analyzer.yaml",
        "cache: {enabled: false}\noutput: {formats: [md,json]}\n",
    );
    let out = fixture.0.join("report.md");
    let output = fixture.run("scan", &["--out", out.to_str().unwrap()]);
    assert!(output.status.success());
    assert!(std::fs::read_to_string(out).unwrap().contains("#"));
    assert!(serde_json::from_str::<Value>(
        &std::fs::read_to_string(fixture.0.join("report.json")).unwrap()
    )
    .is_ok());
}

#[test]
fn cli_relations_use_named_sets_exclusions_and_real_indirect_sources() {
    let fixture = Fixture::new();
    fixture.write(
        "src/a.py",
        "from middle import middle\ndef entry():\n    middle()\n",
    );
    fixture.write(
        "src/middle.py",
        "from b import leaf\ndef middle():\n    leaf()\n",
    );
    fixture.write("src/b.py", "def leaf():\n    pass\n");
    fixture.write(
        "analyzer.yaml",
        "cache: {enabled: false}\nsets:\n  start: [file:src/a.py]\n  end: [file:src/b.py]\n",
    );
    let query = |extra: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_codefactor"));
        command.args([
            "relations",
            "--root",
            fixture.0.to_str().unwrap(),
            "--a",
            "set:start",
            "--b",
            "end",
            "--transitive",
            "3",
        ]);
        command.args(extra);
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let relation: Value = serde_json::from_str(&query(&[])).unwrap();
    assert!(relation["a_to_b"].as_array().unwrap().is_empty());
    let path = &relation["paths_a_to_b"][0];
    assert!(path["from"].as_str().unwrap().ends_with("src.a.entry"));
    assert_eq!(path["edges"].as_array().unwrap().len(), 2);
    assert_eq!(relation["matrix"]["src.a.entry"]["src.b.leaf"], 1);
    let excluded: Value = serde_json::from_str(&query(&["--exclude-a", "file:src/a.py"])).unwrap();
    assert!(excluded["a"].as_array().unwrap().is_empty());
    let markdown = query(&["--format", "md"]);
    assert!(markdown.contains("## Indirect A → B"));
    assert!(markdown.contains("src/middle.py"));
}
