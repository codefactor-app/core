use analyzer_core::{
    algorithmic::{self, ComplexityEstimate, EstimateStatus},
    graph::CallGraph,
    Config, FileModel, LanguageRegistry,
};
use std::path::Path;
fn registry() -> LanguageRegistry {
    let mut r = LanguageRegistry::default();
    r.register(analyzer_php::Php);
    r.register(analyzer_python::Python);
    r.register(analyzer_javascript::JavaScript);
    r.register(analyzer_typescript::TypeScript);
    r.register(analyzer_rust::Rust);
    r
}
fn analyze(path: &str, source: &str, config: &Config) -> FileModel {
    let registry = registry();
    let strategy = registry
        .by_extension(Path::new(path).extension().unwrap().to_str().unwrap())
        .unwrap();
    let mut files = vec![strategy.analyze(source, Path::new(path))];
    assert!(
        files[0].parse_errors.is_empty(),
        "{path}: {:?}",
        files[0].parse_errors
    );
    let mut graph = CallGraph::build(&files, &registry, config);
    algorithmic::analyze(&mut files, &mut graph, config);
    for f in &files[0].functions {
        let n = graph.nodes.iter().find(|n| n.label == f.fqn).unwrap();
        if f.is_module_initializer() {
            assert!(f.algorithmic_complexity.is_none());
            assert!(n.algorithmic_complexity.is_none());
            continue;
        }
        assert!(std::sync::Arc::ptr_eq(
            f.algorithmic_complexity.as_ref().unwrap(),
            n.algorithmic_complexity.as_ref().unwrap()
        ));
    }
    files.remove(0)
}
fn estimate<'a>(file: &'a FileModel, name: &str) -> &'a ComplexityEstimate {
    file.functions
        .iter()
        .find(|f| f.name == name)
        .unwrap()
        .algorithmic_complexity
        .as_ref()
        .unwrap()
}
const FIXTURES: &[(&str, &str)] = &[
    ("cost.rs", include_str!("fixtures/cost/basic.rs")),
    ("cost.php", include_str!("fixtures/cost/basic.php")),
    ("cost.py", include_str!("fixtures/cost/basic.py")),
    ("cost.js", include_str!("fixtures/cost/basic.js")),
    ("cost.ts", include_str!("fixtures/cost/basic.ts")),
];
#[test]
fn every_language_models_growth_and_project_calls_without_certifying_unknowns() {
    let mut failures = Vec::new();
    for &(path, source) in FIXTURES {
        let file = analyze(path, source, &Config::default());
        for (name, growth) in [
            ("constant", "1"),
            ("fixed", "1"),
            ("linear", "n"),
            ("sequential", "m + n"),
            ("nested", "m·n"),
            ("triangular", "n^2"),
            ("quadratic", "n^2"),
            ("traversal", "items.len()"),
            ("logarithmic", "log(n)"),
            ("zero", "1"),
        ] {
            let e = estimate(&file, name);
            let expected = if name == "constant"
                || (matches!(name, "fixed" | "zero") && !path.ends_with(".py"))
                || path.ends_with(".rs")
                || (path.ends_with(".php") && name != "logarithmic")
            {
                EstimateStatus::Tight
            } else if path.ends_with(".php") {
                EstimateStatus::UpperBound
            } else {
                EstimateStatus::Heuristic
            };
            let notation = if expected == EstimateStatus::Tight {
                "Θ"
            } else {
                "O"
            };
            if e.formula.as_deref() != Some(format!("{notation}({growth})").as_str())
                || e.status != expected
            {
                failures.push(format!(
                    "{path} {name}: {:?} {:?}; {:?}",
                    e.status, e.formula, e.unknown_reasons
                ));
            }
            if e.status == EstimateStatus::Heuristic {
                assert!(!e.assumptions.is_empty());
            }
        }
        for name in ["unresolved", "recursive", "changed", "conditional_step"] {
            let e = estimate(&file, name);
            if e.status != EstimateStatus::Unknown
                || e.formula.is_some()
                || e.unknown_reasons.is_empty()
            {
                failures.push(format!(
                    "{path} {name} incorrectly certified: {:?} {:?}",
                    e.status, e.formula
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn disabling_dynamic_assumptions_produces_unknown_not_unsupported() {
    let mut config = Config::default();
    config.algorithmic_complexity.heuristics = false;
    for &(path, source) in FIXTURES.iter().filter(|(p, _)| {
        matches!(
            Path::new(p).extension().unwrap().to_str().unwrap(),
            "py" | "js" | "ts"
        )
    }) {
        let file = analyze(path, source, &config);
        let e = estimate(&file, "linear");
        assert_eq!(e.status, EstimateStatus::Unknown, "{path}");
        assert!(e.unknown_reasons.iter().any(|r| r.contains("heuristic")));
        assert_eq!(estimate(&file, "constant").status, EstimateStatus::Tight);
    }
}

#[test]
fn unsafe_dynamic_semantics_and_nonprogressing_loops_have_no_complete_formula() {
    let cases = [
        ("destructured.js", "function f({n}){return 1;}"),
        ("destructured.ts", "function f([n]:number[]){return 1;}"),
        ("declared.ts", "declare function operation(n:number):void; function f(n:number){operation(n);}"),
        ("decorator.py", "@decorate\ndef f(n:int):\n return 1\n"),
        ("interpolation.py", "def f(n:int):\n return f'{foreign(n)}'\n"),
        ("power.py", "def f(n:int):\n return 2**n\n"),
        ("shadow.py", "def f(n: int, range):\n for i in range(n):\n  pass\n"),
        ("hooks.py", "def f(items: Custom):\n for i in items:\n  pass\n"),
        ("getter.js", "function f(object){return object.value;}"),
        ("typed.ts", "function f(n: Custom){return n+1;}"),
        ("shadow.ts", "function f(n:number,Math:any){let i=n;while(i>0){i=Math.floor(i/2);}}"),
        ("sort.js", "function f(items){items.sort();}"),
        ("recursive.php", "<?php function f(array $items){return count($items,COUNT_RECURSIVE);}"),
        ("namespace.php", "<?php namespace App; function f(array $items){return count($items);}"),
        ("continue.py", "def f(n:int):\n i=0\n while i<n:\n  if i==0:\n   continue\n  i+=1\n"),
        ("continue.js", "function f(n){let i=0;while(i<n){continue;i++;}}"),
        ("nonprogress.php", "<?php function f(int $n){$i=$n;while($i>0){$i=intdiv($i,1);}}"),
        ("zero_boundary.py", "def f(n:int):\n i=n\n while i>=0:\n  i//=2\n"),
        ("else.py", "def f(n:int):\n for i in range(n):\n  pass\n else:\n  foreign()\n"),
        ("while_else.py", "def f(n:int):\n i=0\n while i<n:\n  i+=1\n else:\n  foreign()\n"),
        ("default.py", "def f():\n def inner(n=foreign()):\n  pass\n return 1\n"),
        ("inclusive.php", "<?php function f(){$i=0;while($i<=0){foreign();$i++;}}"),
        ("inclusive.js", "function f(){let i=0;while(i>=0){foreign();i--;}}"),
        ("keywords.py", "def costly(n:int,m:int):\n for i in range(n):\n  pass\ndef f(n:int,m:int):\n costly(m=n,n=m)\n"),
    ];
    let mut failures = Vec::new();
    for (path, source) in cases {
        let file = analyze(path, source, &Config::default());
        let e = estimate(&file, "f");
        if e.status != EstimateStatus::Unknown || e.formula.is_some() {
            failures.push(format!("{path}: {:?} {:?}", e.status, e.formula));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn native_lengths_receiver_names_and_language_specific_loop_forms_are_modeled() {
    for (path, source, name, formula, status) in [
        (
            "absolute.php",
            "<?php namespace App; function f(array $items){for($i=0;$i<\\count($items);$i++){}}",
            "f",
            "Θ(items.len())",
            EstimateStatus::Tight,
        ),
        (
            "bytes.php",
            "<?php function f(string $items){for($i=0;$i<strlen($items);$i++){}}",
            "f",
            "Θ(items.len())",
            EstimateStatus::Tight,
        ),
        (
            "float.php",
            "<?php function f(float $n){for($i=0;$i<$n;$i++){}}",
            "f",
            "O(n)",
            EstimateStatus::Heuristic,
        ),
        (
            "self.py",
            "def costly(self:int):\n for i in range(self):\n  pass\ndef f(n:int):\n costly(n)\n",
            "f",
            "O(n)",
            EstimateStatus::Heuristic,
        ),
        (
            "length.py",
            "def f(items:list[int]):\n for i in range(len(items)):\n  pass\n",
            "f",
            "O(items.len())",
            EstimateStatus::Heuristic,
        ),
        (
            "space.ts",
            "function f(items:number[]){for(const item\nof\nitems){}}",
            "f",
            "O(items.len())",
            EstimateStatus::Heuristic,
        ),
        (
            "skip.php",
            "<?php function f(){ $i=5;while($i<1){foreign();$i++;}}",
            "f",
            "Θ(1)",
            EstimateStatus::Tight,
        ),
        (
            "break.php",
            "<?php function f(int $n){for($i=0;$i<$n;$i++){break;}}",
            "f",
            "O(n)",
            EstimateStatus::UpperBound,
        ),
    ] {
        let file = analyze(path, source, &Config::default());
        let e = estimate(&file, name);
        assert_eq!(e.status, status, "{path}: {:?}", e.unknown_reasons);
        assert_eq!(e.formula.as_deref(), Some(formula), "{path}");
    }
}

#[test]
fn php_native_syntax_and_unused_object_parameters_do_not_obscure_costs() {
    let file = analyze(
        "native.php",
        r#"<?php
class Value {
    public function receiver() { return $this; }
}
function unused(Request $request): bool { return true; }
function literal() { return ['name' => 1, 2, 3]; }
function fixedArray() { foreach ([1, 2, 3] as $key => $value) { $copy = $key; } }
function keyed(array $items) { foreach ($items as $key => $value) { $copy = $value; } }
function unary(int $n) { return -$n; }
"#,
        &Config::default(),
    );
    for (name, formula) in [
        ("receiver", "Θ(1)"),
        ("unused", "Θ(1)"),
        ("literal", "Θ(1)"),
        ("fixedArray", "Θ(1)"),
        ("keyed", "Θ(items.len())"),
        ("unary", "Θ(1)"),
    ] {
        let e = estimate(&file, name);
        assert_eq!(e.status, EstimateStatus::Tight, "{name}: {e:?}");
        assert_eq!(e.formula.as_deref(), Some(formula), "{name}");
    }
}

#[test]
fn php_declared_property_costs_require_explicit_storage_assumptions() {
    let source = r#"<?php
class Value {
    private ?string $title = null;
    private bool $enabled = false;
    private array $items = [];
    public function getTitle() { return $this->title; }
    public function setEnabled(bool $enabled) { $this->enabled = $enabled; return $this; }
    public function setItems(array $items) { $this->items = $items; }
    public function missing() { return $this->missing; }
    public function dynamic(string $name) { return $this->$name; }
    public static function invalidReceiver() { return $this; }
}
class Hooks {
    public string $title { get => foreign(); }
    public function hooked() { return $this->title; }
}
class Derived extends Value {
    public function inherited() { return $this->title; }
}
trait Shared {
    private bool $enabled = false;
    public function traitRead() { return $this->enabled; }
}
function custom(Request $request) { return $request->value; }
function foreignCall(Request $request) { return $request->work(); }
function unpacked(array $items) { return [...$items]; }
function referenced(array $items) { foreach ($items as $key => &$value) {} }
function dynamicKey(string $key) { return [$key => 1]; }
"#;
    let file = analyze("storage.php", source, &Config::default());
    for name in ["getTitle", "setEnabled"] {
        let e = estimate(&file, name);
        assert_eq!(e.status, EstimateStatus::Heuristic, "{name}: {e:?}");
        assert_eq!(e.formula.as_deref(), Some("O(1)"));
        assert!(e.assumptions.iter().any(|a| a.contains("property hooks")));
    }
    for name in [
        "setItems",
        "missing",
        "dynamic",
        "invalidReceiver",
        "hooked",
        "inherited",
        "traitRead",
        "custom",
        "foreignCall",
        "unpacked",
        "referenced",
        "dynamicKey",
    ] {
        let e = estimate(&file, name);
        assert_eq!(e.status, EstimateStatus::Unknown, "{name}: {e:?}");
        assert!(e.formula.is_none());
        assert!(!e.unknown_reasons.is_empty());
    }
    let mut config = Config::default();
    config.algorithmic_complexity.heuristics = false;
    let strict = analyze("storage.php", source, &config);
    for name in ["getTitle", "setEnabled"] {
        assert_eq!(estimate(&strict, name).status, EstimateStatus::Unknown);
    }
}

struct Workspace(std::path::PathBuf);
impl Workspace {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "codefactor-five-costs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        for &(path, source) in FIXTURES {
            std::fs::write(root.join(path), source).unwrap();
        }
        std::fs::write(
            root.join("analyzer.yaml"),
            "cache: {enabled: true, path: .cache}\nalgorithmic_complexity:\n  limits: {error: N}\n",
        )
        .unwrap();
        Self(root)
    }
    fn scan(&self, format: &str) -> std::process::Output {
        std::process::Command::new(env!("CARGO_BIN_EXE_codefactor"))
            .args(["scan", self.0.to_str().unwrap(), "--format", format])
            .output()
            .unwrap()
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn five_language_cli_cache_graph_and_thresholds_preserve_estimates() {
    let workspace = Workspace::new();
    let cold = workspace.scan("json");
    assert_eq!(cold.status.code(), Some(2));
    let report: serde_json::Value = serde_json::from_slice(&cold.stdout).unwrap();
    assert_eq!(report["summary"]["files"], 5);
    assert_eq!(report["summary"]["cache_hits"], 0);
    assert!(!String::from_utf8_lossy(&cold.stdout).contains("cost_ir"));
    let warm = workspace.scan("json");
    let cached: serde_json::Value = serde_json::from_slice(&warm.stdout).unwrap();
    assert_eq!(cached["summary"]["cache_hits"], 5);
    assert_eq!(report["files"], cached["files"]);
    assert_eq!(report["graph"], cached["graph"]);
    for file in report["files"].as_array().unwrap() {
        let language = file["language"].as_str().unwrap();
        let f = file["functions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == "quadratic")
            .unwrap();
        let v = report["violations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["rule"] == "algorithmic_time" && v["entity"] == f["fqn"])
            .unwrap();
        let tight = matches!(language, "rust" | "php");
        assert_eq!(
            v["severity"],
            if tight { "error" } else { "warn" },
            "{language}"
        );
        assert_eq!(
            f["algorithmic_complexity"]["formula"],
            if tight { "Θ(n^2)" } else { "O(n^2)" }
        );
        let graph = std::process::Command::new(env!("CARGO_BIN_EXE_codefactor"))
            .args([
                "graph",
                "callees",
                f["fqn"].as_str().unwrap(),
                "--root",
                workspace.0.to_str().unwrap(),
                "--format",
                "json",
            ])
            .output()
            .unwrap();
        assert!(
            graph.status.success(),
            "{}",
            String::from_utf8_lossy(&graph.stderr)
        );
        let graph: serde_json::Value = serde_json::from_slice(&graph.stdout).unwrap();
        assert!(graph["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["algorithmic_complexity"] == f["algorithmic_complexity"]));
    }
    for format in ["md", "sarif"] {
        let output = workspace.scan(format);
        assert_eq!(output.status.code(), Some(2));
        let body = String::from_utf8(output.stdout).unwrap();
        assert!(body.contains("Θ(n^2)"));
        assert!(body.contains("O(n^2)"));
    }
    // Changing a callee after a cache hit must recompute its caller on every adapter.
    for &(path, source) in FIXTURES {
        let start = source.find("linear(").unwrap();
        let end = source[start..]
            .find('}')
            .map(|v| start + v)
            .unwrap_or_else(|| {
                source[start..]
                    .find("def sequential")
                    .map(|v| start + v)
                    .unwrap()
            });
        let changed = if path.ends_with(".py") {
            source.replace(
                "def linear(n: int):\n    for i in range(n): pass",
                "def linear(n: int): pass",
            )
        } else {
            let begin = source[..end].rfind("for").unwrap();
            format!("{}{}", &source[..begin], &source[end + 1..])
        };
        std::fs::write(workspace.0.join(path), changed).unwrap();
    }
    let changed = workspace.scan("json");
    let changed: serde_json::Value = serde_json::from_slice(&changed.stdout).unwrap();
    for file in changed["files"].as_array().unwrap() {
        let f = file["functions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == "quadratic")
            .unwrap();
        assert!(
            matches!(
                f["algorithmic_complexity"]["formula"].as_str(),
                Some("Θ(n)" | "O(n)")
            ),
            "{}: {:?}",
            file["language"],
            f["algorithmic_complexity"]
        );
    }
    std::fs::write(
        workspace.0.join("analyzer.yaml"),
        "cache: {enabled: true, path: .cache}\nalgorithmic_complexity: {enabled: false}\n",
    )
    .unwrap();
    let off = workspace.scan("json");
    let off: serde_json::Value = serde_json::from_slice(&off.stdout).unwrap();
    assert!(off["files"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|f| f["functions"].as_array().unwrap())
        .all(|f| f["algorithmic_complexity"]["status"] == "disabled"));
}
