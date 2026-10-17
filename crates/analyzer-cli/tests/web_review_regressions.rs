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
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "codefactor-web-review-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("node_modules/demo")).unwrap();
        std::fs::write(root.join("analyzer.yaml"), "cache: {enabled: false}\nsets:\n  app: ['file:app.ts']\n  dependencies: ['vendor:node_modules/demo']\n").unwrap();
        std::fs::write(root.join("app.ts"), "import { work } from 'demo';\nexport function main() { work(); }\nclass Box { run() { function nested() { work(); } nested(); } }\n").unwrap();
        std::fs::write(
            root.join("node_modules/demo/index.ts"),
            "export function work() { before(); }\n",
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
fn request(port: u16, path: &str, host: &str) -> Option<(u16, String)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    write!(
        stream,
        "GET {path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    let (headers, body) = response.split_once("\r\n\r\n")?;
    Some((
        headers.split_whitespace().nth(1)?.parse().ok()?,
        body.into(),
    ))
}
fn get(port: u16, path: &str) -> Value {
    let (status, body) = request(port, path, &format!("127.0.0.1:{port}")).unwrap();
    assert_eq!(status, 200, "{path}: {body}");
    serde_json::from_str(&body).unwrap()
}
fn start(fixture: &Fixture, host: &str, watch: bool) -> (Server, u16) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut command = Command::new(env!("CARGO_BIN_EXE_codefactor"));
    command
        .args([
            "serve",
            fixture.0.to_str().unwrap(),
            "--host",
            host,
            "--port",
            &port.to_string(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if watch {
        command.arg("--watch");
    }
    let server = Server(command.spawn().unwrap());
    let started = Instant::now();
    while request(port, "/api/status", &format!("127.0.0.1:{port}")).is_none() {
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "server failed to start"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    (server, port)
}
fn flatten<'a>(nodes: &'a Value, out: &mut Vec<&'a Value>) {
    for node in nodes.as_array().unwrap() {
        out.push(node);
        flatten(&node["children"], out);
    }
}
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

#[test]
fn fuzzy_search_capabilities_and_optional_internal_relation_edges_are_available() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.join("a.ts"),"import {bridge} from './middle';\nexport function renderWidget(enabled:boolean){ if(enabled)bridge(); helper(); }\nfunction helper(){}\n").unwrap();
    std::fs::write(
        fixture.0.join("middle.ts"),
        "import {finish} from './b';\nexport function bridge(){finish();}\n",
    )
    .unwrap();
    std::fs::write(
        fixture.0.join("b.ts"),
        "import {renderWidget} from './a';\nexport function finish(){renderWidget(false);}\n",
    )
    .unwrap();
    let (_server, port) = start(&fixture, "127.0.0.1", false);
    let search = get(port, "/api/search?q=rndrw");
    assert!(search["results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|node| node["name"] == "renderWidget"));
    let typo = get(port, "/api/search?q=renderWidgte");
    assert_eq!(typo["results"][0]["name"], "renderWidget");
    let tree = get(port, "/api/tree");
    let config = get(port, "/api/config");
    assert_eq!(
        tree["strategy_capabilities"],
        config["strategy_capabilities"]
    );
    assert_eq!(
        tree["strategy_capabilities"]["python"]["static_types"],
        false
    );
    let plain = get(port, "/api/relations?a=file:a.ts&b=file:b.ts&transitive=3");
    assert!(plain.get("internal_a").is_none());
    assert!(plain["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|node| node["file"] == "middle.ts"));
    let full = get(
        port,
        "/api/relations?a=file:a.ts&b=file:b.ts&transitive=3&internal=true&level=set",
    );
    assert_eq!(full["internal_a"].as_array().unwrap().len(), 1);
    assert_eq!(full["matrix"]["A"]["B"], 1);
    assert_eq!(full["direct_call_matrix"]["B"]["A"], 1);
}

#[test]
fn module_initializer_is_navigable_without_enclosing_declarations_or_function_metrics() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.join("app.ts"), "function work() {}\nclass Box { run() { work(); } }\nfunction sibling() { work(); }\nwork();\n").unwrap();
    let (_server, port) = start(&fixture, "127.0.0.1", false);
    let tree = get(port, "/api/tree");
    let mut nodes = Vec::new();
    flatten(&tree["children"], &mut nodes);
    let file = nodes
        .iter()
        .find(|node| node["kind"] == "file" && node["name"] == "app.ts")
        .unwrap();
    let initializer = file["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["kind"] == "module_initializer")
        .unwrap();
    assert_eq!(initializer["name"], "Инициализация модуля");
    assert!(initializer["children"].as_array().unwrap().is_empty());
    assert!(file["children"]
        .as_array()
        .unwrap()
        .iter()
        .any(|node| node["name"] == "Box"));
    assert!(initializer["metrics"]["complexity"].is_null());
    let id = initializer["symbol_id"].as_str().unwrap();
    let graph = get(port, &format!("/api/graph/{}?dir=callees", encode(id)));
    let init = graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["id"] == id)
        .unwrap();
    assert_eq!(init["behavior"], "module_initializer");
    assert!(init["simple_threshold"].is_null());
    assert!(init["metrics"]["parameters"].is_null());
    assert_eq!(init["metrics"]["callsites"], 1);
    assert_eq!(init["metrics"]["fan_out"], 1);
    assert!(graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|node| node["label"] == "app.work"));
}

#[test]
fn wildcard_binding_accepts_literal_interfaces_and_rejects_dns_hosts_and_wrong_ports() {
    let fixture = Fixture::new();
    let (_server, port) = start(&fixture, "0.0.0.0", false);
    assert_eq!(get(port, "/api/status")["revision"], 0);
    for host in [
        format!("untrusted.example:{port}"),
        format!("127.0.0.1:{}", port + 1),
    ] {
        assert_eq!(request(port, "/api/status", &host).unwrap().0, 421);
    }
    let (_loopback, port) = start(&fixture, "127.0.0.1", false);
    assert_eq!(
        request(port, "/api/status", &format!("192.0.2.5:{port}"))
            .unwrap()
            .0,
        421
    );
}

#[test]
fn tree_and_relations_expose_declarations_named_sets_and_exclusions() {
    let fixture = Fixture::new();
    let (_server, port) = start(&fixture, "127.0.0.1", false);
    let tree = get(port, "/api/tree");
    assert_eq!(tree["named_sets"]["app"][0], "file:app.ts");
    let mut nodes = Vec::new();
    flatten(&tree["children"], &mut nodes);
    let class = nodes
        .iter()
        .find(|node| node["name"] == "Box" && node["kind"] == "class")
        .unwrap();
    let method = class["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["name"] == "run")
        .unwrap();
    assert_eq!(method["kind"], "method");
    assert_eq!(method["children"][0]["name"], "nested");
    assert!(method["metrics"]["complexity"].is_number());
    assert!(method["symbol_id"]
        .as_str()
        .unwrap()
        .starts_with("fn:typescript:app.ts:"));
    let relations = get(
        port,
        "/api/relations?a=set:app&b=set:app&exclude_b=file:app.ts",
    );
    assert!(relations["a"].as_array().unwrap().len() >= 3);
    assert!(relations["b"].as_array().unwrap().is_empty());
    assert_eq!(relations["counts_a"]["files"], 1);
}

#[test]
fn expanded_dependency_source_ast_hidden_edges_and_watch_stay_current() {
    let fixture = Fixture::new();
    let (_server, port) = start(&fixture, "127.0.0.1", true);
    assert_eq!(
        request(
            port,
            "/api/file?path=node_modules%2Fdemo%2Findex.ts",
            &format!("127.0.0.1:{port}")
        )
        .unwrap()
        .0,
        404
    );
    get(port, "/api/vendor/node_modules%2Fdemo");
    let vendor = get(port, "/api/file?path=node_modules%2Fdemo%2Findex.ts");
    assert!(vendor["source"].as_str().unwrap().contains("before()"));
    assert!(get(port, "/api/ast?path=node_modules%2Fdemo%2Findex.ts")["root"].is_object());
    let app = get(port, "/api/file?id=app.ts");
    let main = app["functions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|function| function["name"] == "main")
        .unwrap();
    let graph_path = format!(
        "/api/graph/{}?dir=callees&vendor=hidden",
        encode(main["id"].as_str().unwrap())
    );
    let graph = get(port, &graph_path);
    assert!(
        !graph["hidden_vendor_edges"].as_array().unwrap().is_empty(),
        "{graph:#}"
    );
    assert!(!graph["hidden_vendor_nodes"].as_array().unwrap().is_empty());
    let revision = get(port, "/api/status")["revision"].as_u64().unwrap();
    std::fs::write(
        fixture.0.join("node_modules/demo/index.ts"),
        "export function work() { after_update(); }\n",
    )
    .unwrap();
    let changed = Instant::now();
    loop {
        if get(port, "/api/status")["revision"].as_u64().unwrap() > revision {
            let vendor = get(port, "/api/file?path=node_modules%2Fdemo%2Findex.ts");
            if vendor["calls"]
                .as_array()
                .unwrap()
                .iter()
                .any(|call| call["name"] == "after_update")
            {
                break;
            }
        }
        assert!(
            changed.elapsed() < Duration::from_secs(15),
            "watch kept stale vendor graph"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let relations = get(port, "/api/relations?a=set:app&b=set:dependencies");
    assert!(!relations["a_to_b"].as_array().unwrap().is_empty());
    #[cfg(unix)]
    {
        let outside = fixture.0.with_extension("outside.ts");
        std::fs::write(&outside, "export function secret() {}\n").unwrap();
        let source = fixture.0.join("node_modules/demo/index.ts");
        std::fs::remove_file(&source).unwrap();
        std::os::unix::fs::symlink(&outside, &source).unwrap();
        for path in [
            "/api/file?path=node_modules%2Fdemo%2Findex.ts",
            "/api/ast?path=node_modules%2Fdemo%2Findex.ts",
        ] {
            assert_eq!(
                request(port, path, &format!("127.0.0.1:{port}")).unwrap().0,
                403
            );
        }
        std::fs::remove_file(outside).unwrap();
    }
}

#[test]
fn lazy_graph_shallow_evidence_and_frontier_match_filtered_branch_requests() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.join("app.ts"), "function root() { child(); other(); }\nfunction child() { fetch('/api'); grand(); }\nfunction grand() { leaf(); }\nfunction leaf() {}\nfunction other() { outside(); }\n").unwrap();
    let (_server, port) = start(&fixture, "127.0.0.1", false);
    let file = get(port, "/api/file?id=app.ts");
    let functions = file["functions"].as_array().unwrap();
    let id = |name: &str| {
        functions
            .iter()
            .find(|function| function["name"] == name)
            .unwrap()["id"]
            .as_str()
            .unwrap()
    };
    let first = get(
        port,
        &format!(
            "/api/graph/{}?dir=callees&depth=1&lazy=true&external=false",
            encode(id("root"))
        ),
    );
    assert_eq!(first["revision"], get(port, "/api/status")["revision"]);
    let names = first["coverage_nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(names.contains(&"root"));
    assert!(names.contains(&"child"));
    assert!(!names.contains(&"grand"));
    let hints = first["frontier"][id("child")]["callees"]
        .as_array()
        .unwrap();
    let branch = get(
        port,
        &format!(
            "/api/graph/{}?dir=callees&depth=1&lazy=true&external=false",
            encode(id("child"))
        ),
    );
    let edges = branch["coverage_edges"].as_array().unwrap();
    assert_eq!(hints.len(), edges.len());
    assert!(hints.iter().all(|hint| edges
        .iter()
        .any(|edge| edge["load_key"] == hint["key"] && edge["to"] == hint["target"])));
    assert!(edges.iter().all(|edge| edge["load_key"].is_u64()));
    assert!(!branch["coverage_nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|node| node["name"] == "leaf"));
    assert!(
        branch["frontier"][id("grand")]["callees"]
            .as_array()
            .unwrap()
            .len()
            == 1
    );
}

#[test]
fn manifest_only_watch_change_rebuilds_resolution_and_preserves_expanded_vendor() {
    let fixture = Fixture::new();
    let manifest = fixture.0.join("node_modules/demo/package.json");
    std::fs::write(&manifest, r#"{"main":"./index.ts"}"#).unwrap();
    std::fs::write(
        fixture.0.join("node_modules/demo/other.ts"),
        "export function work() { changed_manifest_target(); }\n",
    )
    .unwrap();
    let (_server, port) = start(&fixture, "127.0.0.1", true);
    get(port, "/api/vendor/node_modules/demo");
    let before = get(port, "/api/relations?a=set:app&b=set:dependencies");
    assert!(before["a_to_b"]
        .as_array()
        .unwrap()
        .iter()
        .any(|edge| edge["to"].as_str().unwrap().contains("index.ts")));
    let revision = get(port, "/api/status")["revision"].as_u64().unwrap();
    std::fs::write(&manifest, r#"{"main":"./other.ts"}"#).unwrap();
    let changed = Instant::now();
    loop {
        if get(port, "/api/status")["revision"].as_u64().unwrap() > revision {
            let relation = get(port, "/api/relations?a=set:app&b=set:dependencies");
            if relation["a_to_b"]
                .as_array()
                .unwrap()
                .iter()
                .any(|edge| edge["to"].as_str().unwrap().contains("other.ts"))
            {
                break;
            }
        }
        assert!(
            changed.elapsed() < Duration::from_secs(15),
            "manifest-only change did not rebuild vendor calls"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        get(port, "/api/file?path=node_modules%2Fdemo%2Fother.ts")["functions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|function| function["name"] == "work")
    );
}

#[test]
fn codemap_collects_dependencies_without_architecture_rules_and_preserves_selection() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.0.join("src")).unwrap();
    std::fs::write(fixture.0.join("analyzer.yaml"),"cache: {enabled: false}\narchitecture: {enabled: false}\nalgorithmic_complexity: {enabled: false}\nsets:\n  one: ['file:src/a.ts']\n  overlap: ['file:src/a.ts']\n  function: ['src/a.chosen']\n  empty: ['file:missing.ts']\n  loop: ['set:loop']\n").unwrap();
    std::fs::write(fixture.0.join("src/a.ts"),"import { work, Model } from './b';\nexport function chosen(on: boolean) { if(on) { work(); } }\nexport function other() { work(); }\nexport class Child extends Model {}\n").unwrap();
    std::fs::write(
        fixture.0.join("src/b.ts"),
        "export function work() {}\nexport class Model {}\n",
    )
    .unwrap();
    std::fs::write(
        fixture.0.join("src/empty.ts"),
        "export const answer = 42;\n",
    )
    .unwrap();
    let (_server, port) = start(&fixture, "127.0.0.1", false);
    let data = get(port, "/api/codemap?group=file");
    assert!(data["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b["id"] == "file:src/empty.ts"));
    let links = data["links"].as_array().unwrap();
    let link = links
        .iter()
        .find(|l| l["from"] == "file:src/a.ts" && l["to"] == "file:src/b.ts")
        .unwrap();
    assert_eq!(link["kinds"]["call"], 2);
    assert!(link["kinds"]["import"].as_u64().unwrap() > 0);
    assert!(link["kinds"]["inheritance"].as_u64().unwrap() > 0);
    assert!(!data.to_string().contains("\"witnesses\""));
    let details=get(port,"/api/codemap/evidence?group=file&external=false&expanded=%5B%5D&revision=0&mode=link&from=file:src/a.ts&to=file:src/b.ts&per_page=1");
    assert_eq!(details["witnesses"].as_array().unwrap().len(), 1);
    assert_eq!(details["total"], link["count"]);
    let calls=get(port,"/api/codemap/evidence?group=file&kinds=call&revision=0&mode=link&from=file:src/a.ts&to=file:src/b.ts");
    assert!(calls["witnesses"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| !w["guard"].as_array().unwrap().is_empty()));
    let sets = get(port, "/api/codemap?group=set");
    assert!(sets["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b["id"] == "set:empty" && b["counts"]["entities"] == 0));
    assert!(sets["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w.as_str().unwrap().contains("cyclic")));
    assert!(
        sets["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["id"] == "set:one")
            .unwrap()["shared_entities"]
            .as_u64()
            .unwrap()
            > 0
    );
    let chosen = get(
        port,
        "/api/codemap/evidence?group=set&revision=0&mode=outgoing&block=set:function&kinds=call",
    );
    assert_eq!(chosen["total"], 1);
    assert_eq!(chosen["witnesses"][0]["kind"], "call");
    let none = get(port, "/api/codemap?group=file&kinds=none");
    assert!(none["links"].as_array().unwrap().is_empty());
    assert_eq!(
        request(port, "/api/codemap?group=bad", &format!("127.0.0.1:{port}"))
            .unwrap()
            .0,
        400
    );
    assert_eq!(
        request(
            port,
            "/api/codemap/evidence?revision=99&block=file:src/a.ts",
            &format!("127.0.0.1:{port}")
        )
        .unwrap()
        .0,
        409
    );
}

#[test]
fn codemap_watch_invalidates_revision_and_retains_external_boundaries() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.0.join("analyzer.yaml"),
        "cache: {enabled: false}\narchitecture: {enabled: false}\n",
    )
    .unwrap();
    let (_server, port) = start(&fixture, "127.0.0.1", true);
    let before = get(port, "/api/codemap?group=file&external=true");
    assert!(before["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b["status"] == "package" || b["status"] == "unresolved"));
    let hidden = get(port, "/api/codemap?group=file");
    assert!(hidden["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .all(|b| b["status"] == "project"));
    let rev = before["revision"].as_u64().unwrap();
    std::fs::write(fixture.0.join("app.ts"), "export function changed() {}\n").unwrap();
    let started = Instant::now();
    let after = loop {
        let data = get(port, "/api/codemap?group=file");
        if data["revision"].as_u64().unwrap() > rev
            && data["blocks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|b| b["id"] == "file:app.ts" && b["counts"]["functions"] == 1)
        {
            break data;
        }
        assert!(started.elapsed() < Duration::from_secs(15));
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(
        after["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["id"] == "file:app.ts")
            .unwrap()["counts"]["functions"],
        1
    );
    assert_eq!(
        request(
            port,
            &format!("/api/codemap/evidence?revision={rev}&block=file:app.ts"),
            &format!("127.0.0.1:{port}")
        )
        .unwrap()
        .0,
        409
    );
}

#[test]
fn codemap_dependency_witnesses_cover_all_five_languages_and_isolated_files() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.join("analyzer.yaml"),"cache: {enabled: false}\narchitecture: {enabled: true, exclude: ['**']}\nalgorithmic_complexity: {enabled: false}\nsets:\n  php_domain: ['ns:A', 'ns:B']\n").unwrap();
    let examples=[
        ("php/a.php","<?php namespace A; use B\\Model; use function B\\work; function main() { work(); } function inspect(Model $m): Model { $x=$m->value; return $m; } class Child extends Model {}"),
        ("php/b.php","<?php namespace B; function work() {} class Model { public int $value=1; }"),
        ("py/a.py","from py.b import work, Model\ndef main():\n    work()\ndef inspect(m: Model) -> Model:\n    x = m.value\n    return m\nclass Child(Model):\n    pass\n"),
        ("py/b.py","def work():\n    pass\nclass Model:\n    value: int = 1\n"),
        ("js/a.js","import {work,Model} from './b.js'; export function main(){work();} export class Child extends Model{} export function inspect(){ const m=new Model(); return m.value; }"),
        ("js/b.js","export function work(){} export class Model {value=1;}"),
        ("ts/a.ts","import {work,Model} from './b'; export function main(){work();} export class Child extends Model{} export function inspect(m:Model):Model {const x=m.value;return m;}"),
        ("ts/b.ts","export function work(){} export class Model {value:number=1;}"),
        ("rs/Cargo.toml","[package]\nname='map_fixture'\nversion='0.1.0'\n[lib]\npath='lib.rs'\n"),
        ("rs/lib.rs","mod a; mod b;"),
        ("rs/a.rs","use super::b::{work, Model}; pub fn main(){work();} pub fn inspect(m:Model)->Model {let _x=m.value; m}"),
        ("rs/b.rs","pub fn work(){} pub struct Model {pub value:i32}"),
    ];
    for (file, source) in examples {
        let path = fixture.0.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }
    let (_server, port) = start(&fixture, "127.0.0.1", false);
    let map = get(port, "/api/codemap?group=file");
    for (a, b) in [
        ("php/a.php", "php/b.php"),
        ("py/a.py", "py/b.py"),
        ("js/a.js", "js/b.js"),
        ("ts/a.ts", "ts/b.ts"),
        ("rs/a.rs", "rs/b.rs"),
    ] {
        let link = map["links"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["from"] == format!("file:{a}") && l["to"] == format!("file:{b}"))
            .unwrap_or_else(|| panic!("No {a} -> {b}: {map}"));
        assert!(
            link["kinds"]["call"].as_u64().unwrap_or(0) > 0,
            "{a} call: {link}"
        );
        assert!(
            link["kinds"]["import"].as_u64().unwrap_or(0) > 0,
            "{a} import: {link}"
        );
        if !a.starts_with("js/") {
            assert!(
                link["kinds"]["type"].as_u64().unwrap_or(0) > 0,
                "{a} type: {link}"
            );
            assert!(
                link["kinds"]["data"].as_u64().unwrap_or(0) > 0,
                "{a} data: {link}"
            );
        }
        if !a.starts_with("rs/") {
            assert!(
                link["kinds"]["inheritance"].as_u64().unwrap_or(0) > 0,
                "{a} inheritance: {link}"
            );
        }
    }
    let sets = get(port, "/api/codemap?group=set");
    let domain = sets["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == "set:php_domain")
        .unwrap();
    assert_eq!(domain["counts"]["files"], 2);
    assert!(sets["links"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["from"] == "set:php_domain"
            && l["to"] == "set:php_domain"
            && l["kinds"]["call"].as_u64().unwrap_or(0) > 0));
    let modules = get(port, "/api/codemap?group=module");
    assert!(modules["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b["id"] == "module:php:A"));
}

#[test]
fn codemap_vendor_expansion_replaces_the_package_boundary_with_parsed_members() {
    let fixture = Fixture::new();
    let (_server, port) = start(&fixture, "127.0.0.1", false);
    let before = get(port, "/api/codemap?group=file&external=true");
    let id = "package:typescript:node_modules/demo";
    assert!(before["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b["id"] == id && b["status"] == "package"));
    // A known import and its call share the actual package boundary, not an invented unknown demo node.
    assert!(!before["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b["id"] == "import:typescript:demo"));
    let selected = get(port, "/api/codemap?group=set&external=true");
    assert!(selected["links"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["from"] == "set:app" && l["to"] == "set:dependencies"));
    get(port, "/api/vendor/node_modules/demo");
    let after = get(port, "/api/codemap?group=file&external=true");
    assert!(after["revision"].as_u64().unwrap() > before["revision"].as_u64().unwrap());
    let package = after["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == id)
        .unwrap();
    assert_eq!(package["status"], "vendor");
    assert_eq!(package["counts"]["files"], 1);
    let selected = get(port, "/api/codemap?group=set&external=true");
    assert_eq!(
        selected["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["id"] == "set:dependencies")
            .unwrap()["counts"]["files"],
        1
    );
    let revision = after["revision"].as_u64().unwrap();
    let members=get(port,&format!("/api/codemap/evidence?group=file&external=true&revision={revision}&block=package:typescript:node_modules/demo"));
    assert!(members["entities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["file"] == "node_modules/demo/index.ts"));
}

#[test]
fn compact_navigation_retains_identity_and_fetches_algorithmic_details_separately() {
    let fixture = Fixture::new();
    let (_server, port) = start(&fixture, "127.0.0.1", false);
    let summary = get(port, "/api/tree?summary_only=true&compact=true");
    assert_eq!(summary["summary"]["files"], 1);
    assert!(summary["children"].as_array().unwrap().is_empty());
    let compact = get(port, "/api/tree?compact=true");
    let mut tree = vec![];
    flatten(&compact["children"], &mut tree);
    assert!(tree.iter().any(|node| node["fqn"] == "app.main"));
    let full = get(
        port,
        "/api/graph/fn:typescript:app.ts:app.main?dir=both&depth=1&lazy=true",
    );
    let small = get(
        port,
        "/api/graph/fn:typescript:app.ts:app.main?dir=both&depth=1&lazy=true&compact=true",
    );
    assert_eq!(small["coverage"], full["coverage"]);
    assert_eq!(
        small["nodes"].as_array().unwrap().len(),
        full["nodes"].as_array().unwrap().len()
    );
    assert_eq!(small["compact"], true);
    let raw = if small["coverage_nodes_same_as_nodes"] == true {
        &small["nodes"]
    } else {
        &small["coverage_nodes"]
    };
    assert_eq!(
        raw.as_array().unwrap().len(),
        full["coverage_nodes"].as_array().unwrap().len()
    );
    assert!(small["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .all(|node| node["algorithmic_complexity"]["explanations"].is_null()));
    let symbol = get(port, "/api/symbol/fn:typescript:app.ts:app.main");
    assert!(symbol["algorithmic_complexity"].is_object());
    assert!(symbol["metrics"]["fan_out"].is_number());
    let normal = get(port, "/api/codemap?group=directory");
    let visible = get(port, "/api/codemap?group=directory&compact=true");
    assert_eq!(normal["links"], visible["links"]);
    assert_eq!(visible["hierarchy_scope"], "visible");
}

#[test]
fn codemap_overlap_direction_details_follow_projected_links_without_duplicate_witnesses() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.0.join("src")).unwrap();
    std::fs::write(
        fixture.0.join("analyzer.yaml"),
        "cache: {enabled: false}\narchitecture: {enabled: false}\nalgorithmic_complexity: {enabled: false}\npaths: {include: ['src/**']}\nsets:\n  left: ['file:src/entry.ts']\n  right: ['file:src/entry.ts']\n  alias: ['file:src/entry.ts']\n",
    ).unwrap();
    std::fs::write(
        fixture.0.join("src/entry.ts"),
        "export function first(){second();}\nexport function second(){}\n",
    )
    .unwrap();
    let (_server, port) = start(&fixture, "127.0.0.1", false);
    let map = get(port, "/api/codemap?group=set&kinds=call");
    assert!(map["links"].as_array().unwrap().iter().any(|link| {
        link["from"] == "set:left" && link["to"] == "set:right" && link["count"] == 1
    }));
    for mode in ["incoming", "outgoing", "internal"] {
        let details = get(
            port,
            &format!(
                "/api/codemap/evidence?group=set&kinds=call&revision=0&block=set:left&mode={mode}"
            ),
        );
        assert_eq!(details["total"], 1, "{mode}: {details}");
        assert_eq!(details["witnesses"].as_array().unwrap().len(), 1);
    }
}

#[test]
fn codemap_expanded_parent_direction_details_exclude_internal_child_links() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.0.join("src/group")).unwrap();
    std::fs::write(
        fixture.0.join("analyzer.yaml"),
        "cache: {enabled: false}\narchitecture: {enabled: false}\nalgorithmic_complexity: {enabled: false}\npaths: {include: ['src/**']}\n",
    ).unwrap();
    for (path, source) in [
        ("src/group/entry.ts", "import {inside} from './peer'; import {outside} from '../other'; export function run(){inside();outside();}"),
        ("src/group/peer.ts", "export function inside(){}"),
        ("src/other.ts", "export function outside(){}"),
    ] {
        std::fs::write(fixture.0.join(path), source).unwrap();
    }
    let (_server, port) = start(&fixture, "127.0.0.1", false);
    let base="/api/codemap/evidence?group=directory&kinds=call&expanded=%5B%22dir%3Asrc%22%2C%22dir%3Asrc%2Fgroup%22%5D&revision=0&block=dir:src/group";
    let outgoing = get(port, &format!("{base}&mode=outgoing"));
    assert_eq!(outgoing["total"], 1, "{outgoing}");
    assert!(outgoing["entities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["file"] == "src/other.ts"));
    assert_eq!(get(port, &format!("{base}&mode=incoming"))["total"], 0);
    assert_eq!(get(port, &format!("{base}&mode=internal"))["total"], 1);
}
