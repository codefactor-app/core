use analyzer_core::{metrics::MetricRegistry, Config, LanguageStrategy};
use serde_json::Value;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

struct Project(PathBuf);
impl Project {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "codefactor-constructor-params-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("analyzer.yaml"), "cache: {enabled: false}\n").unwrap();
        Self(root)
    }

    fn write(&self, path: &str, source: &str) {
        std::fs::write(self.0.join(path), source).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_codefactor"))
            .current_dir(&self.0)
            .args(args)
            .output()
            .unwrap()
    }

    fn scan_json(&self) -> Value {
        let output = self.run(&["scan", ".", "--no-architecture", "--format", "json"]);
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

#[test]
fn only_member_constructors_get_their_own_parameter_metric_in_every_language() {
    let cases: Vec<(Box<dyn LanguageStrategy>, &str, &str, &str)> = vec![
        (
            Box::new(analyzer_php::Php),
            "model.php",
            "<?php class Model {\n function __CoNsTrUcT($a, $b, $c) {}\n function run($value) { function __construct($a, $b) {} }\n}\nfunction __construct($a, $b) {}\n",
            "__CoNsTrUcT",
        ),
        (
            Box::new(analyzer_python::Python),
            "model.py",
            "class Model:\n def __init__(self, a, b, c): pass\n def run(self, value):\n  def __init__(a, b): pass\ndef __init__(a, b): pass\n",
            "__init__",
        ),
        (
            Box::new(analyzer_javascript::JavaScript),
            "model.js",
            "class Model {\n constructor(a, b, c) {}\n run(value) { function constructor(a, b) {} }\n}\nclass Factory { static constructor(a, b) {} }\nfunction constructor(a, b) {}\n",
            "constructor",
        ),
        (
            Box::new(analyzer_typescript::TypeScript),
            "model.ts",
            "class Model {\n constructor(a: number, b: number, c: number) {}\n run(value: number) { function constructor(a: number, b: number) {} }\n}\nclass Factory { static constructor(a: number, b: number) {} }\nfunction constructor(a: number, b: number) {}\n",
            "constructor",
        ),
        (
            Box::new(analyzer_rust::Rust),
            "model.rs",
            "struct Model;\nimpl Model {\n fn new(a: i32, b: i32, c: i32) -> Self { Self }\n fn run(&self, value: i32) { fn new(a: i32, b: i32) {} }\n}\nfn new(a: i32, b: i32) {}\n",
            "new",
        ),
    ];
    for (strategy, path, source, name) in cases {
        let model = strategy.analyze(source, Path::new(path));
        assert!(model.parse_errors.is_empty(), "{path}: {model:?}");
        let functions = model
            .functions
            .iter()
            .filter(|f| !f.is_module_initializer())
            .collect::<Vec<_>>();
        let expected_functions = if matches!(model.language.as_str(), "javascript" | "typescript") {
            5
        } else {
            4
        };
        assert_eq!(functions.len(), expected_functions, "{path}: {functions:?}");
        let constructor = functions.iter().copied().find(|f| f.name == name).unwrap();
        assert!(constructor.parent_type.is_some(), "{path}");
        assert!(constructor.parent_function.is_none(), "{path}");
        assert!(constructor.is_constructor(&model.language), "{path}");
        let metrics = MetricRegistry::new().calculate(&model, &Config::default());
        for function in functions {
            let parameter_metrics = metrics
                .iter()
                .filter(|m| {
                    m.entity.as_ref() == function.fqn
                        && matches!(m.id.as_ref(), "constructor_params" | "function_params")
                })
                .collect::<Vec<_>>();
            assert_eq!(parameter_metrics.len(), 1, "{path}: {function:?}");
            let (id, count) = if function.fqn == constructor.fqn {
                ("constructor_params", Some(3.0))
            } else if function.name == "run" {
                ("function_params", Some(1.0))
            } else {
                assert!(
                    !function.is_constructor(&model.language),
                    "{path}: {function:?}"
                );
                (
                    "function_params",
                    function.parent_function.is_none().then_some(2.0),
                )
            };
            assert_eq!(parameter_metrics[0].id.as_ref(), id, "{path}: {function:?}");
            if let Some(count) = count {
                assert_eq!(parameter_metrics[0].value, count, "{path}: {function:?}");
            }
        }
    }
}

#[test]
fn constructors_keep_receiver_and_variadic_counting_settings_in_every_applicable_language() {
    let cases: Vec<(Box<dyn LanguageStrategy>, &str, &str, usize, usize)> = vec![
        (
            Box::new(analyzer_python::Python),
            "model.py",
            "class Model:\n def __init__(self, value, *args, **kwargs): pass\n",
            2,
            1,
        ),
        (
            Box::new(analyzer_php::Php),
            "model.php",
            "<?php class Model { function __construct($value, ...$args) {} }",
            1,
            0,
        ),
        (
            Box::new(analyzer_javascript::JavaScript),
            "model.js",
            "class Model { constructor(value, ...args) {} }",
            1,
            0,
        ),
        (
            Box::new(analyzer_typescript::TypeScript),
            "model.ts",
            "class Model { constructor(value: number, ...args: number[]) {} }",
            1,
            0,
        ),
    ];
    for (strategy, path, source, variadics, receivers) in cases {
        let model = strategy.analyze(source, Path::new(path));
        assert!(model.parse_errors.is_empty(), "{path}");
        for count_variadics in [false, true] {
            for count_receivers in [false, true] {
                let mut config = Config::default();
                config.metrics.count_variadic_parameters = count_variadics;
                config.metrics.count_implicit_receiver = count_receivers;
                let metrics = MetricRegistry::new().calculate(&model, &config);
                let metric = metrics
                    .iter()
                    .find(|m| m.id.as_ref() == "constructor_params")
                    .unwrap();
                let expected = 1
                    + if count_variadics { variadics } else { 0 }
                    + if count_receivers { receivers } else { 0 };
                assert_eq!(metric.value, expected as f64, "{path}");
                assert!(
                    !metrics.iter().any(|m| m.id.as_ref() == "function_params"),
                    "{path}"
                );
            }
        }
    }
}

fn php_parameters(count: usize) -> String {
    (1..=count)
        .map(|i| format!("$p{i}"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[test]
fn cli_default_constructor_limits_are_eight_and_twelve_while_functions_keep_four_and_seven() {
    let project = Project::new();
    let mut source = String::from("<?php\n");
    for (name, count) in [("Within", 8), ("Warn", 9), ("AtError", 12), ("Error", 13)] {
        source.push_str(&format!(
            "class {name} {{ function __construct({}) {{}} }}\n",
            php_parameters(count)
        ));
    }
    source.push_str(&format!("function work({}) {{}}\n", php_parameters(8)));
    project.write("model.php", &source);
    let report = project.scan_json();
    let violations = report["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| {
            matches!(
                v["rule"].as_str(),
                Some("constructor_params" | "function_params")
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(violations.len(), 4, "{violations:?}");
    for (suffix, rule, severity, value, limit) in [
        ("Warn::__construct", "constructor_params", "warn", 9.0, 8.0),
        (
            "AtError::__construct",
            "constructor_params",
            "warn",
            12.0,
            8.0,
        ),
        (
            "Error::__construct",
            "constructor_params",
            "error",
            13.0,
            12.0,
        ),
        ("work", "function_params", "error", 8.0, 7.0),
    ] {
        let violation = violations
            .iter()
            .find(|v| v["entity"].as_str().unwrap().rsplit('\\').next() == Some(suffix))
            .unwrap();
        assert_eq!(violation["rule"], rule);
        assert_eq!(violation["severity"], severity);
        assert_eq!(violation["value"], value);
        assert_eq!(violation["limit"], limit);
    }
}

#[test]
fn strict_config_accepts_constructor_parameters_at_every_scope_and_lists_the_metric_key() {
    let project = Project::new();
    project.write(
        "analyzer.yaml",
        r#"limits:
  constructor_params: {warn: 8, error: 12}
  by_language:
    php:
      constructor_params: {warn: 9}
  by_path:
    - glob: 'src/Dto/**'
      constructor_params: {enabled: false}
  by_entity_glob:
    - glob: '*::__construct'
      constructor_params: {error: 14}
  by_entity:
    Model::__construct:
      constructor_params: {enabled: true}
  aggregates:
    constructor_params.avg: {warn: 10}
"#,
    );
    let checked = project.run(&["config", "check", "--file", "analyzer.yaml", "--strict"]);
    assert!(checked.status.success(), "{checked:?}");
    assert!(String::from_utf8_lossy(&checked.stdout).contains("0 key(s) skipped"));
    let keys = project.run(&["config", "keys"]);
    assert!(keys.status.success(), "{keys:?}");
    assert!(String::from_utf8_lossy(&keys.stdout)
        .lines()
        .any(|line| line.starts_with("limits.constructor_params\t")));
    let shown = project.run(&["config", "show", "--file", "analyzer.yaml", "--resolved"]);
    assert!(shown.status.success(), "{shown:?}");
    let config: serde_yaml::Value = serde_yaml::from_slice(&shown.stdout).unwrap();
    assert_eq!(
        config["limits"]["constructor_params"]["error"].as_f64(),
        Some(12.0)
    );
    assert_eq!(
        config["limits"]["by_path"][0]["constructor_params"]["enabled"],
        false
    );
}

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn catalog(port: u16) -> Option<Value> {
    let mut socket = TcpStream::connect(("127.0.0.1", port)).ok()?;
    socket.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    write!(
        socket,
        "GET /api/metrics/catalog HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut response = String::new();
    socket.read_to_string(&mut response).ok()?;
    if !response.starts_with("HTTP/1.1 200") && !response.starts_with("HTTP/1.0 200") {
        return None;
    }
    serde_json::from_str(response.split_once("\r\n\r\n")?.1).ok()
}

#[test]
fn console_glossary_and_web_catalog_describe_constructor_parameters() {
    let project = Project::new();
    project.write(
        "model.php",
        &format!(
            "<?php class Model {{ function __construct({}) {{}} }}\n",
            php_parameters(13)
        ),
    );
    let output = project.run(&["scan", ".", "--no-architecture", "--color", "never"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let console = String::from_utf8(output.stdout).unwrap();
    assert!(console.contains("Параметры конструктора (constructor_params)"));
    assert!(console.contains("Пороги по умолчанию"));
    let port = TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let _server = Server(
        Command::new(env!("CARGO_BIN_EXE_codefactor"))
            .current_dir(&project.0)
            .args([
                "serve",
                ".",
                "--no-architecture",
                "--port",
                &port.to_string(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let started = Instant::now();
    let catalog = loop {
        if let Some(catalog) = catalog(port) {
            break catalog;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "server did not start"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    let entries = catalog
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["id"] == "constructor_params")
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["title"], "Параметры конструктора");
    assert!(entries[0]["description"].as_str().unwrap().contains("Rust"));
    assert!(!entries[0]["action"].as_str().unwrap().is_empty());
}
