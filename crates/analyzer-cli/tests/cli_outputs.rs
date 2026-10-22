use std::path::PathBuf;
use std::process::Command;

fn fixture() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "codefactor-cli-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/leaf.py"), "def leaf(x):\n    return x\n").unwrap();
    std::fs::write(root.join("src/main.py"),"from leaf import leaf\nimport requests\ndef root(x):\n    if x:\n        requests.get('https://example.test')\n        mystery()\n        nested(x)\n        return leaf(x)\n    return 0\ndef nested(x):\n    if x > 1:\n        return leaf(x)\n    return 0\n").unwrap();
    std::fs::write(
        root.join("src/helper.js"),
        "function foreign() { leaf(); }\n",
    )
    .unwrap();
    root
}
fn run(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_codefactor"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "command {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn strategy_capabilities_are_in_every_scan_report_and_legacy_reports_still_deserialize() {
    let root = fixture();
    let root_str = root.to_str().unwrap();
    let mut json: serde_json::Value = serde_json::from_str(&run(&[
        "scan",
        root_str,
        "--format",
        "json",
        "--no-architecture",
    ]))
    .unwrap();
    assert_eq!(
        json["strategy_capabilities"]["python"]["static_types"],
        false
    );
    assert_eq!(json["strategy_capabilities"]["rust"]["static_types"], true);
    assert_eq!(
        json["strategy_capabilities"]["typescript"]["generics"],
        true
    );
    let limited: serde_json::Value = serde_json::from_str(&run(&[
        "scan",
        root_str,
        "--format",
        "json",
        "--only-violations",
    ]))
    .unwrap();
    assert_eq!(
        limited["strategy_capabilities"],
        json["strategy_capabilities"]
    );
    assert!(run(&["scan", root_str, "--format", "md"]).contains("## Strategy capabilities"));
    assert!(run(&["scan", root_str, "--format", "console"]).contains("Возможности стратегий"));
    let sarif: serde_json::Value =
        serde_json::from_str(&run(&["scan", root_str, "--format", "sarif"])).unwrap();
    assert_eq!(
        sarif["runs"][0]["properties"]["strategy_capabilities"],
        json["strategy_capabilities"]
    );
    json.as_object_mut()
        .unwrap()
        .remove("strategy_capabilities");
    let old: analyzer_core::AnalysisReport = serde_json::from_value(json).unwrap();
    assert!(old.strategy_capabilities.is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn logging_respects_levels_module_filters_and_cli_precedence_without_polluting_json() {
    let root = fixture();
    let invoke = |level: Option<&str>, filter: &str, verbose: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_codefactor"));
        command
            .args([
                "scan",
                root.to_str().unwrap(),
                "--format",
                "json",
                "--no-architecture",
            ])
            .env("RUST_LOG", filter);
        if let Some(level) = level {
            command.args(["--log-level", level]);
        }
        if verbose {
            command.arg("--verbose");
        }
        let output = command.output().unwrap();
        assert!(output.status.success());
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();
        String::from_utf8(output.stderr).unwrap()
    };
    assert!(invoke(Some("off"), "trace", true).is_empty());
    let info = invoke(Some("info"), "", false);
    assert!(info.contains("INFO analyzer_core::report: scan completed"));
    assert!(!info.contains("DEBUG"));
    let trace = invoke(None, "error,analyzer_core::report=trace", false);
    assert!(trace.contains("TRACE analyzer_core::report: analyzing file="));
    assert!(trace.contains("DEBUG analyzer_core::report: discovery"));
    assert!(invoke(None, "", true).contains("DEBUG analyzer_core::report"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn ast_function_and_metric_groups_have_real_content() {
    let root = fixture();
    let file = root.join("src/main.py");
    let file = file.to_str().unwrap();
    let root_str = root.to_str().unwrap();
    let ast = run(&["ast", file, "--function", "root", "--format", "json"]);
    let value: serde_json::Value = serde_json::from_str(&ast).unwrap();
    assert_eq!(value["node"]["kind"], "function");
    assert_eq!(value["node"]["range"]["start_line"], 3);
    let dot = run(&["ast", file, "--function", "root", "--format", "dot"]);
    assert!(dot.contains("digraph ast"));
    let project = run(&["metrics", root_str, "--group-by", "project"]);
    assert!(
        project.contains("Проект")
            && project.contains("Длина функции")
            && project.contains("function_lines")
    );
    assert!(project.contains("p90"));
    let directory = run(&["metrics", root_str, "--group-by", "directory"]);
    assert!(directory.contains("Каталог: src") && directory.contains("Длина функции"));
    let module = run(&["metrics", root_str, "--group-by", "module"]);
    assert!(module.contains("Модуль: src.main") && module.contains("Длина функции"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn graph_filters_selectors_and_shortest_paths_change_results() {
    let root = fixture();
    let root_str = root.to_str().unwrap();
    let query = |args: &[&str]| -> serde_json::Value {
        let mut argv = vec![
            "graph", "callees", "root", "--root", root_str, "--format", "json",
        ];
        argv.extend_from_slice(args);
        serde_json::from_str(&run(&argv)).unwrap()
    };
    let unfiltered = query(&[]);
    assert!(unfiltered["edges"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["to"].as_str().unwrap_or("").contains("src.leaf.leaf")));
    let by_module = query(&["--module", "src.leaf"]);
    assert_eq!(by_module["edges"].as_array().unwrap().len(), 1);
    let by_directory = query(&["--directory", "src/leaf.py"]);
    assert_eq!(by_directory["edges"].as_array().unwrap().len(), 1);
    let by_language = query(&["--language", "javascript"]);
    assert_eq!(by_language["edges"].as_array().unwrap().len(), 0);
    let excluded = query(&["--exclude", "leaf"]);
    assert!(excluded["edges"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| !e["to"].as_str().unwrap_or("").contains("leaf")));
    let dynamic = query(&["--confidence", "dynamic"]);
    assert!(dynamic["edges"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["confidence"] == "dynamic"));
    let http = query(&["--external-category", "http"]);
    assert!(http["edges"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["to"].as_str().unwrap_or("").starts_with("external:http:")));
    let exact = run(&[
        "graph",
        "callees",
        "src.main.root",
        "--root",
        root_str,
        "--summary",
    ]);
    assert!(exact.contains("functions="));
    let wildcard = run(&[
        "graph",
        "callees",
        "src.main.*",
        "--root",
        root_str,
        "--summary",
    ]);
    assert!(wildcard.contains("functions="));
    let file_line = run(&[
        "graph",
        "callees",
        "src/main.py:3",
        "--root",
        root_str,
        "--summary",
    ]);
    assert!(file_line.contains("functions="));
    let paths = run(&[
        "graph",
        "paths",
        "--from",
        "root",
        "--to",
        "leaf",
        "--root",
        root_str,
        "--shortest",
        "--format",
        "json",
    ]);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&paths).unwrap()["paths"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "{paths}"
    );
    let js = run(&[
        "graph", "callees", "foreign", "--root", root_str, "--format", "json",
    ]);
    assert!(!js.contains("fn:python:"));
    let with_conditions = run(&[
        "graph",
        "callees",
        "root",
        "--root",
        root_str,
        "--format",
        "json",
        "--with-conditions",
    ]);
    let with_conditions: serde_json::Value = serde_json::from_str(&with_conditions).unwrap();
    let nested_to_leaf = with_conditions["edges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| {
            e["from"]
                .as_str()
                .unwrap_or("")
                .ends_with("src.main.nested")
                && e["to"].as_str().unwrap_or("").ends_with("src.leaf.leaf")
        })
        .unwrap();
    assert!(nested_to_leaf["accumulated_conditions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|path| {
            let path = path.as_array().unwrap();
            path.iter().any(|g| g["raw"] == "x") && path.iter().any(|g| g["raw"] == "x > 1")
        }));
    let with_conditions_md = run(&[
        "graph",
        "callees",
        "root",
        "--root",
        root_str,
        "--format",
        "md",
        "--with-conditions",
    ]);
    assert!(with_conditions_md.contains("## Accumulated path conditions"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn graph_paths_relations_and_multi_format_reports_are_readable() {
    let root = fixture();
    let root_str = root.to_str().unwrap();
    let graph = run(&[
        "graph", "callees", "root", "--root", root_str, "--format", "console",
    ]);
    assert!(graph.contains("## Call chains"));
    assert!(graph.contains("src.main.root"));
    assert!(graph.contains("src.leaf.leaf"));
    let paths = run(&[
        "graph", "paths", "--from", "root", "--to", "leaf", "--root", root_str, "--format", "md",
    ]);
    assert!(paths.contains("# Call paths"));
    assert!(paths.contains("callsite"), "{paths}");
    let relations = run(&[
        "relations",
        "--a",
        "file:src/main.py",
        "--b",
        "file:src/leaf.py",
        "--root",
        root_str,
        "--format",
        "md",
    ]);
    assert!(relations.contains("# Relations between sets"));
    assert!(relations.contains("## Matrix"));
    assert!(relations.contains("src/main.py"));
    let out = root.join("report.md");
    let out_str = out.to_str().unwrap();
    let _ = run(&["scan", root_str, "--format", "md,json", "--out", out_str]);
    let md = std::fs::read_to_string(&out).unwrap();
    let json = std::fs::read_to_string(root.join("report.json")).unwrap();
    assert!(md.contains("## Source hierarchy"));
    assert!(md.contains("## Aggregates"));
    assert!(serde_json::from_str::<serde_json::Value>(&json).is_ok());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn project_config_loads_implicitly_and_explicit_config_wins() {
    let root = fixture();
    let root_str = root.to_str().unwrap();
    let project = root.join("analyzer.yaml");
    std::fs::write(
        &project,
        "config_version: 1\nlimits:\n  function_lines: {warn: 1}\n",
    )
    .unwrap();
    let implicit: serde_json::Value =
        serde_json::from_str(&run(&["scan", root_str, "--format", "json"])).unwrap();
    assert!(implicit["violations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["rule"] == "function_lines"));
    assert_eq!(implicit["config_path"], project.to_str().unwrap());
    let override_path = root.join("override.yaml");
    std::fs::write(
        &override_path,
        "config_version: 1\nlimits:\n  function_lines: {warn: 1000}\n",
    )
    .unwrap();
    let explicit: serde_json::Value = serde_json::from_str(&run(&[
        "scan",
        root_str,
        "--format",
        "json",
        "--config",
        override_path.to_str().unwrap(),
    ]))
    .unwrap();
    assert!(!explicit["violations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["rule"] == "function_lines"));
    assert_eq!(explicit["config_path"], override_path.to_str().unwrap());
    std::fs::write(&project, "limits: [\n").unwrap();
    let malformed = Command::new(env!("CARGO_BIN_EXE_codefactor"))
        .args(["scan", root_str, "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(malformed.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&malformed.stderr).contains("syntax"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn sarif_omits_location_for_project_aggregate_violations() {
    let root = fixture();
    std::fs::write(
        root.join("analyzer.yaml"),
        "config_version: 1\nlimits:\n  aggregates:\n    function_lines.avg: {warn: 0}\n",
    )
    .unwrap();
    let sarif: serde_json::Value =
        serde_json::from_str(&run(&["scan", root.to_str().unwrap(), "--format", "sarif"])).unwrap();
    let results = sarif["runs"][0]["results"].as_array().unwrap();
    assert!(results
        .iter()
        .any(|result| result["ruleId"] == "function_lines.avg"));
    for result in results
        .iter()
        .filter(|result| result["ruleId"] == "function_lines.avg")
    {
        assert!(result.get("locations").is_none(), "{result}");
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn scan_defaults_to_bounded_human_output_and_machine_export_stays_complete() {
    let root = fixture();
    let source = (0..60)
        .map(|i| format!("def extra_{i}(x, y):\n    pass\n"))
        .collect::<String>();
    std::fs::write(root.join("src/excess.py"), source).unwrap();
    std::fs::write(
        root.join("analyzer.yaml"),
        "limits:\n  function_params: {warn: 0, error: 0}\n",
    )
    .unwrap();
    let execute = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_codefactor"))
            .arg("scan")
            .arg(&root)
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        String::from_utf8(output.stdout).unwrap()
    };
    let json: serde_json::Value = serde_json::from_str(&execute(&["--format", "json"])).unwrap();
    let count = json["violations"].as_array().unwrap().len();
    assert!(count > 50);
    let console = execute(&[]);
    assert!(console.contains("Codefactor · Анализ кода"));
    assert!(console.contains(&format!("Показано 50 из {count}")));
    assert!(console.contains("Что означают метрики"));
    assert!(!console.contains('\x1b'));
    let full = execute(&["--all", "--no-explain", "--color", "always"]);
    assert!(!full.contains("Показано 50 из"));
    assert!(!full.contains("Что означают метрики"));
    assert!(full.contains("\x1b[1;31m"));
    let out = root.join("report.txt");
    execute(&["--out", out.to_str().unwrap(), "--color", "always"]);
    assert!(!std::fs::read_to_string(out).unwrap().contains('\x1b'));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn package_core_version_is_visible_and_accepted_by_strict_config_commands() {
    let root = fixture();
    let config = root.join("analyzer.yaml");
    std::fs::write(&config, "core: {version: '0.1.2'}\n").unwrap();
    let file = config.to_str().unwrap();
    run(&["config", "check", "--file", file, "--strict"]);
    assert!(run(&["config", "keys"]).contains("core.version"));
    assert!(run(&["config", "show", "--file", file, "--resolved"]).contains("0.1.2"));
    run(&[
        "scan",
        root.to_str().unwrap(),
        "--strict-config",
        "--format",
        "json",
    ]);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn compact_console_folds_noise_and_full_restores_it() {
    let root = fixture();
    for (name, source) in [
        ("a.py", "from b import ping_b\ndef ping_a(): ping_b()\n"),
        ("b.py", "from c import ping_c\ndef ping_b(): ping_c()\n"),
        ("c.py", "from a import ping_a\ndef ping_c(): ping_a()\n"),
    ] {
        std::fs::write(root.join("src").join(name), source).unwrap();
    }
    std::fs::write(
        root.join("analyzer.yaml"),
        "cache: {enabled: false}\nalgorithmic_complexity: {enabled: false}\nlimits:\n  module_cycle: {warn: 0}\n  function_params: {warn: 0, error: 3}\n  function_lines: {warn: 1, error: 50}\n",
    )
    .unwrap();
    let scan = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_codefactor"))
            .arg("scan")
            .arg(&root)
            .args(args)
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap()
    };
    let compact = scan(&[]);
    assert!(compact.contains("отключено: "), "{compact}");
    assert!(compact.contains("Оценки не вычислялись"), "{compact}");
    assert!(!compact.contains("анализ отключён"), "{compact}");
    assert!(
        compact.contains("правил: skipped — Доступно в extended"),
        "{compact}"
    );
    assert!(!compact.contains("Границы слоёв: skipped"), "{compact}");
    assert!(
        compact.contains("Подробности: Архитектура · ПРЕДУПРЕЖДЕНИЕ #1"),
        "{compact}"
    );
    assert!(!compact.contains("Файлы компоненты:"), "{compact}");
    assert!(compact.contains("(×"), "{compact}");
    assert!(compact.contains("Что делать:"), "{compact}");
    assert!(!compact.contains("скрыто baseline"), "{compact}");
    let full = scan(&["--full"]);
    assert!(full.contains("анализ отключён"), "{full}");
    assert!(full.contains("Границы слоёв: skipped"), "{full}");
    assert!(full.contains("Файлы компоненты:"), "{full}");
    assert!(!full.contains("Подробности: Архитектура"), "{full}");

    run(&["baseline", "create", "--root", root.to_str().unwrap()]);
    let hidden = scan(&[]);
    assert!(hidden.contains("скрыто baseline: "), "{hidden}");
    assert!(!hidden.contains("устаревших записей"), "{hidden}");
    std::fs::write(
        root.join("analyzer.yaml"),
        "cache: {enabled: false}\nalgorithmic_complexity: {enabled: false}\nlimits:\n  module_cycle: {warn: 0}\n  function_params: {warn: 100, error: 200}\n  function_lines: {warn: 100, error: 200}\n",
    )
    .unwrap();
    let stale = scan(&["--format", "json"]);
    let json: serde_json::Value = serde_json::from_str(&stale).unwrap();
    assert!(
        json["summary"]["baseline_stale"].as_u64().unwrap() > 0,
        "{stale}"
    );
    assert!(json["summary"]["baseline_suppressed"].as_u64().unwrap() > 0);
    assert!(scan(&[]).contains("устаревших записей baseline: "));
    std::fs::remove_dir_all(root).unwrap();
}
