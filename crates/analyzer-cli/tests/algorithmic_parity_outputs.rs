use serde_json::Value;
use std::{path::PathBuf, process::Command};

const SOURCES: &[(&str, &str)] = &[
    ("counter.php", "<?php class Counter { private int $value=0; public function read(){return $this->value;} public function run(int $n){for($i=0;$i<$n;$i++){$this->read();}} }"),
    ("counter.py", "class Counter:\n value: int = 0\n def read(self):\n  return self.value\n def run(self, n: int):\n  for i in range(n):\n   self.read()\n"),
    ("counter.js", "class Counter { value=0; read(){return this.value;} run(n){for(let i=0;i<n;i++){this.read();}} }"),
    ("counter.ts", "class Counter { value:number=0; read(){return this.value;} run(n:number){for(let i=0;i<n;i++){this.read();}} }"),
    ("counter.rs", "struct Counter { value:usize } impl Counter { fn read(&self)->usize{self.value} fn run(&self,n:usize){for _i in 0..n{self.read();}} }"),
];

struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "codefactor-receiver-costs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        for &(name, source) in SOURCES {
            std::fs::write(path.join(name), source).unwrap();
        }
        Self(path)
    }
    fn configure(&self, heuristics: bool) {
        std::fs::write(
            self.0.join("analyzer.yaml"),
            format!("cache: {{enabled: true, path: .cache}}\nalgorithmic_complexity:\n  heuristics: {heuristics}\n  unknown: error\n  limits: {{error: '1'}}\n"),
        )
        .unwrap();
    }
    fn scan(&self) -> Value {
        let output = Command::new(env!("CARGO_BIN_EXE_codefactor"))
            .args(["scan", self.0.to_str().unwrap(), "--format", "json"])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["summary"]["parse_errors"], 0);
        assert_eq!(report["summary"]["files"], 5);
        report
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn receiver_costs_propagate_through_calls_cache_graph_and_policy_in_every_language() {
    let workspace = Workspace::new();
    workspace.configure(true);
    let cold = workspace.scan();
    assert_eq!(cold["summary"]["cache_hits"], 0);
    let warm = workspace.scan();
    assert_eq!(warm["summary"]["cache_hits"], 5);
    assert_eq!(cold["files"], warm["files"]);
    assert_eq!(cold["graph"], warm["graph"]);
    assert_eq!(cold["violations"], warm["violations"]);
    for file in warm["files"].as_array().unwrap() {
        let language = file["language"].as_str().unwrap();
        let tight = language == "rust";
        for (name, growth) in [("read", "1"), ("run", "n")] {
            let f = file["functions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["name"] == name)
                .unwrap();
            let cost = &f["algorithmic_complexity"];
            assert_eq!(
                cost["status"],
                if tight { "tight" } else { "heuristic" },
                "{language} {name}: {cost}"
            );
            assert_eq!(
                cost["formula"],
                format!("{}({growth})", if tight { "Θ" } else { "O" }),
                "{language} {name}"
            );
            let node = warm["graph"]["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|n| n["language"] == language && n["label"] == f["fqn"])
                .unwrap();
            assert_eq!(node["algorithmic_complexity"], *cost);
            if name == "run" {
                let violation = warm["violations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|v| {
                        v["file"] == file["path"]
                            && v["entity"] == f["fqn"]
                            && v["rule"] == "algorithmic_time"
                    })
                    .unwrap();
                assert_eq!(violation["severity"], if tight { "error" } else { "warn" });
                assert_eq!(violation["algorithmic_complexity"]["estimate"], *cost);
            }
        }
    }
    // Reuse parsed models while applying stricter policy to inferred field storage.
    workspace.configure(false);
    let strict = workspace.scan();
    assert_eq!(strict["summary"]["cache_hits"], 5);
    for file in strict["files"].as_array().unwrap() {
        for f in file["functions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| matches!(f["name"].as_str(), Some("read" | "run")))
        {
            let cost = &f["algorithmic_complexity"];
            if file["language"] == "rust" {
                assert_eq!(cost["status"], "tight");
            } else {
                assert_eq!(cost["status"], "unknown", "{}", file["language"]);
                assert!(cost["formula"].is_null());
                assert!(strict["violations"].as_array().unwrap().iter().any(|v| {
                    v["file"] == file["path"]
                        && v["entity"] == f["fqn"]
                        && v["rule"] == "algorithmic_time_unknown"
                        && v["severity"] == "error"
                }));
            }
        }
    }
}
