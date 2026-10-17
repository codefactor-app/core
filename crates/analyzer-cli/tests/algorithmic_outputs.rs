use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

const SOURCE:&str="fn linear(n:usize){for i in 0..n {}}\nfn quadratic(n:usize){for i in 0..n {linear(n);}}\nfn sorted(items:&mut [usize]){items.sort_unstable();}\nfn unknown(){println!(\"hello\");}\n";
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "codefactor-cost-output-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("lib.rs"), SOURCE).unwrap();
        std::fs::write(root.join("analyzer.yaml"),"cache: {enabled: false}\nalgorithmic_complexity:\n  limits: {warn: N, error: 'N*log(N)'}\n").unwrap();
        Self(root)
    }
    fn scan(&self, format: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_codefactor"))
            .args(["scan", self.0.to_str().unwrap(), "--format", format])
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn symbolic_reports_preserve_severity_and_have_no_fake_numeric_values() {
    let fixture = Fixture::new();
    let output = fixture.scan("json");
    assert_eq!(output.status.code(), Some(2));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], "3");
    let violations = report["violations"].as_array().unwrap();
    let exact = violations
        .iter()
        .find(|v| v["entity"] == "lib::quadratic")
        .unwrap();
    assert_eq!(exact["severity"], "error");
    assert!(exact.get("value").is_none());
    assert!(exact.get("limit").is_none());
    assert_eq!(
        exact["algorithmic_complexity"]["estimate"]["formula"],
        "Θ(n^2)"
    );
    assert_eq!(
        violations
            .iter()
            .find(|v| v["entity"] == "lib::sorted")
            .unwrap()["severity"],
        "warn"
    );
    assert!(!violations.iter().any(|v| v["entity"] == "lib::unknown"));
    for format in ["text", "md"] {
        let output = fixture.scan(format);
        assert_eq!(output.status.code(), Some(2));
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("Θ(n^2)"), "{format}");
        assert!(text.contains("верхняя граница"));
        assert!(text.contains("macro expansion") || text.contains("macro"));
    }
    let output = fixture.scan("sarif");
    assert_eq!(output.status.code(), Some(2));
    let sarif: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(sarif["runs"][0]["results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["level"] == "error"
            && r["properties"]["algorithmic_complexity"]["estimate"]["formula"] == "Θ(n^2)"));
    for format in ["json", "mermaid", "dot", "md"] {
        let output = Command::new(env!("CARGO_BIN_EXE_codefactor"))
            .args([
                "graph",
                "callees",
                "quadratic",
                "--root",
                fixture.0.to_str().unwrap(),
                "--format",
                format,
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("Θ(n^2)"), "{format}: {text}");
    }
    std::fs::write(
        fixture.0.join("analyzer.yaml"),
        "cache: {enabled: false}\nalgorithmic_complexity: {unknown: error}\n",
    )
    .unwrap();
    let output = fixture.scan("json");
    assert_eq!(output.status.code(), Some(2));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report["violations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["rule"] == "algorithmic_time_unknown" && v["severity"] == "error"));
}

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn get(port: u16, path: &str) -> Option<serde_json::Value> {
    let mut socket = TcpStream::connect(("127.0.0.1", port)).ok()?;
    socket.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    write!(
        socket,
        "GET {path} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut bytes = String::new();
    socket.read_to_string(&mut bytes).ok()?;
    if !bytes.starts_with("HTTP/1.1 200") && !bytes.starts_with("HTTP/1.0 200") {
        return None;
    }
    serde_json::from_str(bytes.split_once("\r\n\r\n")?.1).ok()
}
#[test]
fn web_metadata_and_watch_refresh_share_the_scan_estimates() {
    let fixture = Fixture::new();
    let port = TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let _server = Server(
        Command::new(env!("CARGO_BIN_EXE_codefactor"))
            .args([
                "serve",
                fixture.0.to_str().unwrap(),
                "--port",
                &port.to_string(),
                "--watch",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let started = Instant::now();
    while get(port, "/api/status").is_none() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "server did not start"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let file = get(port, "/api/file?id=lib.rs").unwrap();
    let quadratic = file["functions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "quadratic")
        .unwrap();
    assert_eq!(quadratic["algorithmic_complexity"]["formula"], "Θ(n^2)");
    let path = "/api/graph/fn%3Arust%3Alib.rs%3Alib%3A%3Aquadratic?dir=callees";
    let graph = get(port, path).unwrap();
    assert!(graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["algorithmic_complexity"]["formula"] == "Θ(n^2)"));
    let violations = get(port, "/api/violations?rule=algorithmic_time").unwrap();
    assert_eq!(violations["total"], 2);
    std::fs::write(
        fixture.0.join("lib.rs"),
        SOURCE.replace("for i in 0..n {linear(n);}", "linear(n);"),
    )
    .unwrap();
    let changed = Instant::now();
    loop {
        let graph = get(port, path).unwrap();
        let estimate = graph["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == "fn:rust:lib.rs:lib::quadratic")
            .unwrap();
        if estimate["algorithmic_complexity"]["formula"] == "Θ(n)" {
            break;
        }
        assert!(
            changed.elapsed() < Duration::from_secs(10),
            "watch kept a stale cost summary"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        get(port, "/api/violations?rule=algorithmic_time").unwrap()["total"],
        1
    );
}

#[test]
fn web_api_and_watch_expose_costs_for_php_python_javascript_and_typescript() {
    let fixture = Fixture::new();
    std::fs::remove_file(fixture.0.join("lib.rs")).unwrap();
    let sources = [
        ("cost.php", include_str!("fixtures/cost/basic.php")),
        ("cost.py", include_str!("fixtures/cost/basic.py")),
        ("cost.js", include_str!("fixtures/cost/basic.js")),
        ("cost.ts", include_str!("fixtures/cost/basic.ts")),
    ];
    for (path, source) in sources {
        std::fs::write(fixture.0.join(path), source).unwrap();
    }
    let port = TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let _server = Server(
        Command::new(env!("CARGO_BIN_EXE_codefactor"))
            .args([
                "serve",
                fixture.0.to_str().unwrap(),
                "--port",
                &port.to_string(),
                "--watch",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let start = Instant::now();
    while get(port, "/api/status").is_none() {
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(25));
    }
    for (path, source) in sources {
        let file = get(port, &format!("/api/file?id={path}")).unwrap();
        let f = file["functions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == "quadratic")
            .unwrap();
        let formula = if path.ends_with(".php") {
            "Θ(n^2)"
        } else {
            "O(n^2)"
        };
        assert_eq!(f["algorithmic_complexity"]["formula"], formula, "{path}");
        let language = file["language"].as_str().unwrap();
        let id = format!("fn:{language}:{path}:{}", f["fqn"].as_str().unwrap());
        let encoded = id.bytes().map(|b| format!("%{b:02X}")).collect::<String>();
        let graph = get(port, &format!("/api/graph/{encoded}?dir=callees")).unwrap();
        assert!(graph["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["algorithmic_complexity"]["formula"] == formula));
        let changed = if path.ends_with(".py") {
            source.replace("for i in range(n): linear(n)", "linear(n)")
        } else {
            source
                .replace("for($i=0;$i<$n;$i++){linear($n);}", "linear($n);")
                .replace("for(let i=0;i<n;i++){linear(n);}", "linear(n);")
        };
        assert_ne!(changed, source);
        std::fs::write(fixture.0.join(path), changed).unwrap();
        let start = Instant::now();
        loop {
            let graph = get(port, &format!("/api/graph/{encoded}?dir=callees")).unwrap();
            let f = graph["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|n| n["id"] == id)
                .unwrap();
            if matches!(
                f["algorithmic_complexity"]["formula"].as_str(),
                Some("Θ(n)" | "O(n)")
            ) {
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "stale {path} graph"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
