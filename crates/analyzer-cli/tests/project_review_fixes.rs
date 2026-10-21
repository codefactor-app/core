use analyzer_core::{
    graph::{Direction, GraphFilter},
    Analyzer, Config, LanguageRegistry,
};
use std::{path::PathBuf, process::Command};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "codefactor-review-fixes-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("analyzer.yaml"),
            "cache: {enabled: false}\narchitecture: {enabled: false}\nalgorithmic_complexity: {enabled: false}\n",
        )
        .unwrap();
        Self(root)
    }
    fn write(&self, path: &str, source: &str) {
        let path = self.0.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }
    fn analyzer(&self) -> Analyzer {
        let mut registry = LanguageRegistry::new();
        registry.register(analyzer_python::Python);
        registry.register(analyzer_javascript::JavaScript);
        registry.register(analyzer_typescript::TypeScript);
        registry.register(analyzer_php::Php);
        registry.register(analyzer_rust::Rust);
        let mut config = Config::default();
        config.cache.enabled = false;
        config.architecture.enabled = false;
        config.algorithmic_complexity.enabled = false;
        Analyzer::new(registry, config)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn sibling_cycles_are_reported_without_losing_reachable_functions() {
    let fixture = Fixture::new();
    fixture.write("cycles.py", "def entry():\n    left()\n    right()\ndef left():\n    right()\ndef right():\n    left()\n");
    let analyzer = fixture.analyzer();
    let report = analyzer.scan(&fixture.0).unwrap();
    for direction in [Direction::Callees, Direction::Callers, Direction::Both] {
        let root = if direction == Direction::Callers {
            "cycles.left"
        } else {
            "cycles.entry"
        };
        let query =
            report.graph_query(root, direction, None, &GraphFilter::all(), &analyzer.config);
        assert_eq!(query.coverage.functions.len(), 3);
        assert!(!query.cycles.is_empty(), "{direction:?}");
        assert!(
            !(query
                .edges
                .iter()
                .any(|e| e.from.ends_with("cycles.left") && e.to.ends_with("cycles.right"))
                && query
                    .edges
                    .iter()
                    .any(|e| e.from.ends_with("cycles.right") && e.to.ends_with("cycles.left")))
        );
    }
}

#[test]
fn inheritance_depth_resolves_local_and_imported_bases_in_four_languages() {
    let fixture = Fixture::new();
    fixture.write("base.py", "class A:\n    pass\nclass B(A):\n    pass\n");
    fixture.write(
        "derived.py",
        "from base import B as Parent\nclass C(Parent):\n    pass\n",
    );
    for extension in ["js", "ts"] {
        fixture.write(
            &format!("base.{extension}"),
            "export class A {}\nexport class B extends A {}\n",
        );
        fixture.write(&format!("derived.{extension}"), &format!("import {{ B as Parent }} from './base.{extension}';\nclass C extends Parent {{}}\n"));
    }
    fixture.write(
        "base.php",
        "<?php\nnamespace App;\nclass A {}\nclass B extends A {}\n",
    );
    fixture.write(
        "derived.php",
        "<?php\nnamespace Other;\nuse App\\B as Parent;\nclass C extends Parent {}\n",
    );
    let report = fixture.analyzer().scan(&fixture.0).unwrap();
    assert!(report.parse_errors.is_empty(), "{:?}", report.parse_errors);
    for extension in ["py", "js", "ts", "php"] {
        let metric = report
            .metrics
            .iter()
            .find(|m| {
                m.path.as_ref() == format!("derived.{extension}")
                    && m.id.as_ref() == "inheritance_depth"
            })
            .unwrap();
        assert_eq!(metric.value, 2.0, "{extension}: {}", metric.entity);
    }
}

#[test]
fn lcom_uses_actual_method_targets_and_shared_fields() {
    let fixture = Fixture::new();
    fixture.write("cohesion.py", "class Other:\n    def right(self):\n        return 0\nclass Separate:\n    def left(self, other: Other):\n        return self.a + other.right()\n    def right(self):\n        return self.b\nclass Connected:\n    def left(self):\n        return self.a + self.right()\n    def right(self):\n        return self.b\nclass Fields:\n    def left(self):\n        return self.a\n    def right(self):\n        return self.a\n");
    let report = fixture.analyzer().scan(&fixture.0).unwrap();
    for (class, expected) in [("Separate", 2.0), ("Connected", 1.0), ("Fields", 1.0)] {
        let metric = report
            .metrics
            .iter()
            .find(|m| m.entity.as_ref() == format!("cohesion.{class}") && m.id.as_ref() == "lcom4")
            .unwrap();
        assert_eq!(metric.value, expected, "{class}");
    }
}

#[test]
fn halstead_is_independent_of_other_functions_on_the_same_line() {
    let fixture = Fixture::new();
    let small = "function small(){return 'число';}";
    let large =
        "function large(n){ let total=0; for(let i=0;i<n;i++){ total += i*i; } return total; }";
    fixture.write("together.js", &format!("{small} {large}\n"));
    fixture.write("separate.js", &format!("{small}\r\n{large}\r\n"));
    let report = fixture.analyzer().scan(&fixture.0).unwrap();
    for function in ["small", "large"] {
        for metric in ["halstead_volume", "maintainability_index"] {
            let values = ["together", "separate"].map(|module| {
                report
                    .metrics
                    .iter()
                    .find(|m| {
                        m.entity.as_ref() == format!("{module}.{function}")
                            && m.id.as_ref() == metric
                    })
                    .unwrap()
                    .value
            });
            assert_eq!(values[0], values[1], "{function}: {metric}");
        }
    }
}

#[test]
fn negated_float_comparisons_retain_the_original_condition() {
    let fixture = Fixture::new();
    fixture.write(
        "conditions.py",
        "def hit():\n    pass\ndef entry(x: float):\n    if not (x > 0):\n        hit()\n",
    );
    let report = fixture.analyzer().scan(&fixture.0).unwrap();
    let guard = &report
        .graph
        .edges
        .iter()
        .find(|e| e.from.ends_with("conditions.entry") && e.to.ends_with("conditions.hit"))
        .unwrap()
        .guard[0];
    assert_eq!(guard.raw, "not (x > 0)");
    assert_eq!(guard.normalized, guard.raw);
    assert!(f64::NAN.partial_cmp(&0.0).is_none());
}

#[test]
fn external_query_arguments_belong_to_the_selected_callsite() {
    let fixture = Fixture::new();
    fixture.write("external.py", "import requests\ndef one():\n    requests.get('https://one.example/')\ndef two():\n    requests.get('https://two.example/')\n");
    let analyzer = fixture.analyzer();
    let report = analyzer.scan(&fixture.0).unwrap();
    for name in ["one", "two"] {
        let query = report.graph_query(
            &format!("external.{name}"),
            Direction::Callees,
            None,
            &GraphFilter::all(),
            &analyzer.config,
        );
        let external = query
            .nodes
            .iter()
            .find(|n| n.category.as_deref() == Some("http"))
            .unwrap();
        assert_eq!(
            external.argument.as_deref(),
            Some(format!("https://{name}.example/").as_str())
        );
    }
    let http = report
        .graph
        .nodes
        .iter()
        .find(|n| n.category.as_deref() == Some("http"))
        .unwrap();
    assert!(
        http.argument.is_none(),
        "a shared node must not choose one resource"
    );
    let restored: analyzer_core::graph::CallGraph =
        serde_json::from_value(serde_json::to_value(&report.graph).unwrap()).unwrap();
    let query = restored.query(
        "category:http",
        Direction::Callers,
        None,
        &GraphFilter::all(),
        &analyzer.config,
    );
    assert_eq!(query.coverage.external.len(), 1);
    assert_eq!(
        query
            .edges
            .iter()
            .filter_map(|edge| edge.argument.as_deref())
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::from(["https://one.example/", "https://two.example/"])
    );
    assert!(analyzer_core::report_markdown::graph_markdown(&query).contains("https://two.example/"));

    let mut redacted = fixture.analyzer();
    redacted.config.external.redact = vec!["two".into()];
    let report = redacted.scan(&fixture.0).unwrap();
    let edge = report
        .graph
        .edges
        .iter()
        .find(|e| e.from.ends_with("external.two"))
        .unwrap();
    assert_eq!(
        edge.argument.as_deref(),
        Some("https://[REDACTED].example/")
    );
    redacted.config.external.capture_args = analyzer_core::config::CaptureArgs::None;
    let report = redacted.scan(&fixture.0).unwrap();
    assert!(report.graph.edges.iter().all(|e| e.argument.is_none()));
    assert!(report.graph.nodes.iter().all(|n| n.argument.is_none()));
}

#[test]
fn accumulated_conditions_finish_with_explicit_limits_on_branching_chains() {
    let fixture = Fixture::new();
    let mut source = String::new();
    for i in 0..16 {
        source.push_str(&format!(
            "def f{i}(x):\n    if x:\n        f{}(x)\n    else:\n        f{}(x)\n",
            i + 1,
            i + 1
        ));
    }
    source.push_str("def f16(x):\n    pass\n");
    fixture.write("chain.py", &source);
    let stdout = fixture.0.join("graph.json");
    let mut child = Command::new(env!("CARGO_BIN_EXE_codefactor"))
        .args(["graph", "callees", "chain.f0", "--root"])
        .arg(&fixture.0)
        .args(["--format", "json", "--with-conditions"])
        .stdout(std::fs::File::create(&stdout).unwrap())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > std::time::Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("bounded accumulation did not finish within 10 seconds");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(stdout).unwrap()).unwrap();
    assert_eq!(value["coverage"]["functions"].as_array().unwrap().len(), 17);
    assert_eq!(value["accumulated_conditions_metadata"]["truncated"], true);
    assert!(value["edges"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| !e["accumulated_conditions"].as_array().unwrap().is_empty()));
    let metadata = &value["accumulated_conditions_metadata"];
    assert!(metadata["work_used"].as_u64().unwrap() <= metadata["work_budget"].as_u64().unwrap());
    let total = value["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|edge| edge["accumulated_conditions"].as_array().unwrap().len())
        .sum::<usize>();
    assert!(total <= metadata["max_paths_total"].as_u64().unwrap() as usize);
}

#[test]
fn small_accumulation_preserves_both_branch_alternatives() {
    let fixture = Fixture::new();
    fixture.write("branches.py", "def entry(x):\n    if x:\n        left()\n    else:\n        right()\ndef left():\n    finish()\ndef right():\n    finish()\ndef finish():\n    leaf()\ndef leaf():\n    pass\n");
    let output = Command::new(env!("CARGO_BIN_EXE_codefactor"))
        .args(["graph", "callees", "branches.entry", "--root"])
        .arg(&fixture.0)
        .args(["--format", "json", "--with-conditions"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["accumulated_conditions_metadata"]["truncated"], false);
    let edge = value["edges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| {
            e["from"].as_str().unwrap().ends_with("branches.finish")
                && e["to"].as_str().unwrap().ends_with("branches.leaf")
        })
        .unwrap();
    let alternatives = edge["accumulated_conditions"].as_array().unwrap();
    assert_eq!(alternatives.len(), 2);
    assert!(alternatives.iter().any(|path| path[0]["branch"] == "then"));
    assert!(alternatives.iter().any(|path| path[0]["branch"] == "else"));
}

#[test]
fn typescript_declaration_files_are_not_analyzed_as_sources() {
    use analyzer_core::LanguageStrategy;
    use analyzer_typescript::TypeScript;
    for (name, declaration) in [
        ("a.d.ts", true),
        ("a.d.mts", true),
        ("a.d.cts", true),
        ("styles.d.css.ts", true),
        ("a.ts", false),
        ("a.mts", false),
        ("a.d.tsx", false),
        ("d.ts", false),
        ("a.dts", false),
    ] {
        assert_eq!(
            TypeScript.is_declaration_file(std::path::Path::new(name)),
            declaration,
            "{name}"
        );
    }

    let fixture = Fixture::new();
    fixture.write(
        "src/main.ts",
        "export function run(id: number) { return id; }\n",
    );
    fixture.write(
        "src/view.d.tsx",
        "export function View() { return null; }\n",
    );
    fixture.write(
        "src/types/api.d.ts",
        "export declare function listUsers(a: string, b: string, c: string, d: string, e: string, f: string): void;\nexport declare class Client { get(p: string): Promise<unknown>; }\n",
    );
    fixture.write(
        "src/env.d.mts",
        "declare function env(name: string): string;\n",
    );
    fixture.write(
        "src/legacy.d.cts",
        "declare function legacy(): void;\nexport = legacy;\n",
    );
    fixture.write(
        "src/styles.d.css.ts",
        "declare const s: Record<string, string>;\nexport default s;\n",
    );
    fixture.write(
        "node_modules/axios/index.d.ts",
        "export declare function get(url: string): Promise<unknown>;\n",
    );
    let mut analyzer = fixture.analyzer();
    analyzer.config.vendor.parse = analyzer_core::config::VendorParse::Full;
    let report = analyzer.scan(&fixture.0).unwrap();
    let files = report
        .files
        .iter()
        .map(|f| f.path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(files, ["src/main.ts", "src/view.d.tsx"]);
    assert!(report.deferred_vendor.is_empty());
    assert_eq!(report.summary.functions, 2);
    let declaration = |path: &str| {
        ["api.d", "env.d", "legacy.d", "styles.d", "axios"]
            .iter()
            .any(|stem| path.contains(stem))
    };
    assert!(report.metrics.iter().all(|m| !declaration(&m.path)));
    assert!(report.violations.iter().all(|v| !declaration(&v.file)));
    assert!(report.graph.nodes.iter().all(|n| !declaration(&n.id)));
}

#[test]
fn lazy_vendor_declaration_files_still_link_typescript_callers() {
    let fixture = Fixture::new();
    fixture.write(
        "src/main.ts",
        "import pad from 'leftpadx';\nexport function main() { return pad('a', 3); }\n",
    );
    fixture.write(
        "node_modules/leftpadx/index.js",
        "module.exports = function pad(s, n) { return s.padStart(n); };\n",
    );
    fixture.write(
        "node_modules/leftpadx/index.d.ts",
        "export default function pad(s: string, n: number): string;\n",
    );
    let analyzer = fixture.analyzer();
    assert_eq!(
        analyzer.config.vendor.parse,
        analyzer_core::config::VendorParse::Lazy
    );
    let mut report = analyzer.scan(&fixture.0).unwrap();
    assert!(report
        .deferred_vendor
        .iter()
        .any(|d| d.path == "node_modules/leftpadx/index.d.ts" && d.language == "typescript"));
    assert!(report.graph.edges.iter().any(|e| {
        &*e.from == "fn:typescript:src/main.ts:src/main.main"
            && &*e.to == "vendor:node_modules/leftpadx"
    }));

    analyzer
        .expand_vendor(&fixture.0, &mut report, "node_modules/leftpadx")
        .unwrap();
    let files = report
        .files
        .iter()
        .map(|f| f.path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(files, ["node_modules/leftpadx/index.js", "src/main.ts"]);
}
