use serde_json::Value;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
struct Fixture(PathBuf);
impl Fixture {
    fn new(source: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "codefactor-coverage-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("node_modules/demo")).unwrap();
        std::fs::write(root.join("analyzer.yaml"), "cache: {enabled: false}\n").unwrap();
        std::fs::write(root.join("app.ts"), source).unwrap();
        std::fs::write(
            root.join("node_modules/demo/index.ts"),
            "export function unparsed(){}\n",
        )
        .unwrap();
        Self(root)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn request(port: u16, path: &str) -> Option<Value> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    write!(
        stream,
        "GET {path} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    let (headers, body) = response.split_once("\r\n\r\n")?;
    assert!(
        headers.starts_with("HTTP/1.0 200") || headers.starts_with("HTTP/1.1 200"),
        "{response}"
    );
    serde_json::from_str(body).ok()
}
fn start(fixture: &Fixture) -> (Server, u16) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let server = Server(
        Command::new(env!("CARGO_BIN_EXE_codefactor"))
            .args([
                "serve",
                fixture.0.to_str().unwrap(),
                "--port",
                &port.to_string(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let now = Instant::now();
    while request(port, "/api/status").is_none() {
        assert!(now.elapsed() < Duration::from_secs(15));
        std::thread::sleep(Duration::from_millis(25));
    }
    (server, port)
}
#[test]
fn requested_coverage_is_complete_for_indexed_graph_independently_of_shallow_nodes() {
    let fixture=Fixture::new("function caller(){root();}\nfunction root(){child();}\nfunction child(){fetch('/api');grand();}\nfunction grand(){leaf();}\nfunction leaf(){}\n");
    let (_server, port) = start(&fixture);
    let get = |suffix: &str| {
        request(
            port,
            &format!("/api/graph/app.root?lazy=true&depth=1&external=false&{suffix}"),
        )
        .unwrap()
    };
    let full = get("dir=callees&coverage_unbounded=true");
    assert_eq!(full["coverage_nodes"].as_array().unwrap().len(), 2);
    assert_eq!(
        full["requested_coverage"]["functions"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert_eq!(full["coverage"]["functions"].as_array().unwrap().len(), 4);
    assert_eq!(
        full["loaded_coverage"]["functions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        full["requested_coverage"]["external"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(full["coverage_scope"], "requested");
    assert_eq!(full["revision"], full["coverage_revision"]);
    assert_eq!(full["coverage_complete"], false);
    assert_eq!(full["coverage_unparsed_vendor_files"], 1);
    assert_eq!(full["requested_coverage"]["vendor_boundary"], true);
    let limited = get("dir=callees&coverage_depth=2");
    assert_eq!(
        limited["requested_coverage"]["functions"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(limited["coverage_depth"], 2);
    let filtered = get("dir=callees&coverage_unbounded=true&exclude=grand");
    assert_eq!(
        filtered["requested_coverage"]["functions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let both = get("dir=both&coverage_unbounded=true");
    assert_eq!(
        both["requested_coverage"]["functions"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    let callers = request(
        port,
        "/api/graph/app.leaf?lazy=true&depth=1&dir=callers&coverage_unbounded=true&external=false",
    )
    .unwrap();
    assert_eq!(callers["coverage_nodes"].as_array().unwrap().len(), 2);
    assert_eq!(
        callers["requested_coverage"]["functions"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
}
#[test]
fn api_and_cli_expose_multiple_routes_limits_and_distinct_matrix_counts() {
    let fixture=Fixture::new("function root(){child();child();other();}\nfunction child(){leaf();other();}\nfunction other(){if(true){leaf();}}\nfunction leaf(){}\n");
    let (_server, port) = start(&fixture);
    let all = request(
        port,
        "/api/relations?a=app.root&b=app.leaf&transitive=4&path_mode=all_shortest",
    )
    .unwrap();
    assert_eq!(all["paths_a_to_b"].as_array().unwrap().len(), 3);
    assert_eq!(all["route_count"], 3);
    assert_eq!(all["direct_call_count"], 0);
    assert_eq!(all["truncated"], false);
    assert_eq!(
        all["revision"],
        request(port, "/api/status").unwrap()["revision"]
    );
    assert!(all["paths_a_to_b"]
        .as_array()
        .unwrap()
        .iter()
        .any(|path| !path["edges"][1]["guard"].as_array().unwrap().is_empty()));
    let capped = request(
        port,
        "/api/relations?a=app.root&b=app.leaf&transitive=4&path_mode=simple&max_paths_per_pair=1",
    )
    .unwrap();
    assert_eq!(capped["paths_a_to_b"].as_array().unwrap().len(), 1);
    assert_eq!(capped["truncated"], true);
    let output = Command::new(env!("CARGO_BIN_EXE_codefactor"))
        .args([
            "relations",
            "--root",
            fixture.0.to_str().unwrap(),
            "--a",
            "app.root",
            "--b",
            "app.leaf",
            "--transitive",
            "4",
            "--path-mode",
            "all_shortest",
            "--max-paths-total",
            "2",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cli: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(cli["paths_a_to_b"].as_array().unwrap().len(), 2);
    assert_eq!(cli["truncated"], true);
    assert_eq!(cli["options"]["max_paths_total"], 2);
}
