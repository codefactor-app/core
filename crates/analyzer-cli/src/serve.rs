use analyzer_core::graph::{Direction, GraphFilter, RelationOptions, RelationPathMode};
use analyzer_core::{AnalysisReport, Analyzer};
use axum::{
    extract::{Path as AxumPath, Query, Request, State},
    http::{header, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
    time::{Duration, UNIX_EPOCH},
};

struct AppState {
    analyzer: Arc<Analyzer>,
    report: RwLock<AnalysisReport>,
    root: PathBuf,
    single_file: bool,
    revision: AtomicU64,
    expanded_vendor: Mutex<std::collections::BTreeSet<String>>,
    operation_lock: tokio::sync::Mutex<()>,
    config_snapshot: Value,
    watch_error: RwLock<Option<String>>,
    function_info: RwLock<BTreeMap<String, Value>>,
    codemap_index: RwLock<Option<(u64, Arc<analyzer_core::codemap::DependencyIndex>)>>,
}
type SourceSnapshot = BTreeMap<PathBuf, (u64, u128)>;
#[derive(Deserialize)]
struct FileQuery {
    id: Option<String>,
    path: Option<String>,
}
#[derive(Deserialize)]
struct AstQuery {
    path: String,
    function: Option<String>,
}
#[derive(Deserialize)]
struct GraphQuery {
    compact: Option<bool>,
    dir: Option<String>,
    depth: Option<usize>,
    vendor: Option<String>,
    external: Option<bool>,
    category: Option<String>,
    language: Option<String>,
    module: Option<String>,
    directory: Option<String>,
    confidence: Option<String>,
    exclude: Option<String>,
    lazy: Option<bool>,
    coverage_depth: Option<usize>,
    coverage_unbounded: Option<bool>,
}
#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}
#[derive(Deserialize)]
struct RelationQuery {
    a: String,
    b: String,
    exclude_a: Option<String>,
    exclude_b: Option<String>,
    transitive: Option<usize>,
    path_mode: Option<RelationPathMode>,
    max_paths_per_pair: Option<usize>,
    max_paths_total: Option<usize>,
    work_budget: Option<usize>,
    level: Option<String>,
    internal: Option<bool>,
}

pub async fn run(
    analyzer: Analyzer,
    mut report: AnalysisReport,
    root: PathBuf,
    loaded: Option<analyzer_core::config::ConfigLoad>,
    host: String,
    port: u16,
    open_browser: bool,
    watch: bool,
) -> anyhow::Result<()> {
    let root = std::fs::canonicalize(root)?;
    let single_file = root.is_file();
    let ip: IpAddr = host.parse()?;
    if !ip.is_loopback() {
        log::warn!("codefactor is listening on {ip}; project source may be read by clients that can reach this address");
    }
    let addr = SocketAddr::new(ip, port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let actual = listener.local_addr()?;
    let allowed_hosts = allowed_hosts(ip, actual.port());
    let analyzer = Arc::new(analyzer);
    let config_snapshot = configuration_snapshot(&analyzer, report.config_path.as_deref(), loaded);
    let function_info = super::function_info::prepare(&mut report);
    let state = Arc::new(AppState {
        analyzer: analyzer.clone(),
        report: RwLock::new(report),
        root: root.clone(),
        single_file,
        revision: AtomicU64::new(0),
        expanded_vendor: Mutex::new(std::collections::BTreeSet::new()),
        operation_lock: tokio::sync::Mutex::new(()),
        config_snapshot,
        watch_error: RwLock::new(None),
        function_info: RwLock::new(function_info),
        codemap_index: RwLock::new(None),
    });
    let app = Router::new()
        .route("/", get(index))
        .route("/assets/app.css", get(css))
        .route("/assets/app.js", get(js))
        .route("/assets/ui-utils.js", get(ui_utils))
        .route("/assets/graph-model.js", get(graph_model))
        .route("/assets/tree-model.js", get(tree_model))
        .route("/assets/relations-model.js", get(relations_model))
        .route("/assets/codemap-model.js", get(codemap_model))
        .route("/assets/codemap.css", get(codemap_css))
        .route("/assets/codemap.js", get(codemap_js))
        .route("/assets/codemap-controls.js", get(codemap_controls))
        .route("/api/tree", get(tree))
        .route("/api/status", get(status))
        .route("/api/file", get(file))
        .route("/api/ast", get(ast))
        .route("/api/symbol/*id", get(symbol))
        .route("/api/graph/*id", get(graph))
        .route("/api/search", get(search))
        .route("/api/violations", get(violations))
        .route("/api/architecture", get(architecture))
        .route("/api/config", get(configuration))
        .route("/api/metrics/catalog", get(metric_catalog))
        .route("/api/relations", get(relations))
        .route("/api/codemap", get(codemap))
        .route("/api/codemap/evidence", get(codemap_evidence))
        .route("/api/vendor/*package", get(expand_vendor))
        .fallback(get(index))
        .layer(middleware::from_fn_with_state(allowed_hosts, validate_host))
        .with_state(state.clone());
    if watch {
        let state = state.clone();
        let analyzer = analyzer.clone();
        let root = root.clone();
        tokio::spawn(async move {
            // The initial scan predates this snapshot; reconcile it on the first tick.
            let mut last = None;
            let mut interval = tokio::time::interval(Duration::from_secs(2));
            loop {
                interval.tick().await;
                let snapshot_root = root.clone();
                let snapshot_analyzer = analyzer.clone();
                let source_paths = state
                    .report
                    .read()
                    .unwrap()
                    .files
                    .iter()
                    .map(|file| file.path.clone())
                    .collect::<Vec<_>>();
                let current = match tokio::task::spawn_blocking(move || {
                    source_snapshot_for_paths(&snapshot_root, &snapshot_analyzer, &source_paths)
                })
                .await
                {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        log::error!("watch snapshot failed: {error}");
                        *state.watch_error.write().unwrap() = Some(error.to_string());
                        continue;
                    }
                };
                reconcile_watch(&state, &mut last, current).await;
            }
        });
    }
    let browser_addr = if ip.is_unspecified() {
        SocketAddr::new(
            if ip.is_ipv4() {
                "127.0.0.1".parse()?
            } else {
                "::1".parse()?
            },
            actual.port(),
        )
    } else {
        actual
    };
    let url = format!("http://{browser_addr}/");
    log::info!("HTTP server started address={url} watch={watch}");
    println!("Serving read-only analysis at {url}");
    if open_browser {
        open_url(&url);
    }
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
async fn reconcile_watch(
    state: &Arc<AppState>,
    last: &mut Option<SourceSnapshot>,
    current: SourceSnapshot,
) -> bool {
    if last.as_ref() == Some(&current) {
        *state.watch_error.write().unwrap() = None;
        return false;
    }
    let _operation = state.operation_lock.lock().await;
    let root = state.root.clone();
    let analyzer = state.analyzer.clone();
    let expanded = state
        .expanded_vendor
        .lock()
        .unwrap()
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    match tokio::task::spawn_blocking(move || {
        let mut report = analyzer.scan(&root)?;
        for package in expanded {
            analyzer.expand_vendor(&root, &mut report, &package)?;
        }
        Ok::<_, analyzer_core::analysis::AnalysisError>(report)
    })
    .await
    {
        Ok(Ok(mut report)) => {
            let mut current_report = state.report.write().unwrap();
            report.config_path = current_report.config_path.clone();
            *state.function_info.write().unwrap() = super::function_info::prepare(&mut report);
            *current_report = report;
            *state.watch_error.write().unwrap() = None;
            state.revision.fetch_add(1, Ordering::Release);
            drop(current_report);
            // Failed scans leave the prior committed snapshot intact for retry.
            *last = Some(current);
            true
        }
        Ok(Err(error)) => {
            log::error!("watch reanalysis failed: {error}");
            *state.watch_error.write().unwrap() = Some(error.to_string());
            false
        }
        Err(error) => {
            log::error!("watch task failed: {error}");
            *state.watch_error.write().unwrap() = Some(error.to_string());
            false
        }
    }
}
struct AllowedHosts {
    exact: std::collections::BTreeSet<String>,
    wildcard: bool,
    port: u16,
}
impl AllowedHosts {
    fn contains(&self, host: &str) -> bool {
        self.exact.contains(host)
            || self.wildcard
                && host.parse::<SocketAddr>().is_ok_and(|address| {
                    address.port() == self.port
                        && !address.ip().is_unspecified()
                        && !address.ip().is_multicast()
                })
    }
}
fn allowed_hosts(bound: IpAddr, port: u16) -> Arc<AllowedHosts> {
    let mut hosts = std::collections::BTreeSet::new();
    hosts.insert(
        SocketAddr::new(bound, port)
            .to_string()
            .to_ascii_lowercase(),
    );
    if bound.is_loopback() || bound.is_unspecified() {
        hosts.insert(format!("localhost:{port}"));
        hosts.insert(format!("127.0.0.1:{port}"));
        hosts.insert(format!("[::1]:{port}"));
    }
    Arc::new(AllowedHosts {
        exact: hosts,
        wildcard: bound.is_unspecified(),
        port,
    })
}
async fn validate_host(
    State(allowed): State<Arc<AllowedHosts>>,
    request: Request,
    next: Next,
) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::to_ascii_lowercase);
    if host.as_ref().is_some_and(|host| allowed.contains(host)) {
        let method = request.method().clone();
        let path = request.uri().path().to_string();
        let response = next.run(request).await;
        log::debug!("HTTP {method} {path} status={}", response.status());
        response
    } else {
        (StatusCode::MISDIRECTED_REQUEST, "unexpected Host header").into_response()
    }
}
#[cfg(test)]
fn source_snapshot(root: &Path, analyzer: &Analyzer, _single_file: bool) -> SourceSnapshot {
    let sources = analyzer_core::discovery::discover(root, &analyzer.config, &analyzer.registry)
        .into_iter()
        .filter(|file| {
            !file.vendor || analyzer.config.vendor.parse == analyzer_core::config::VendorParse::Full
        })
        .map(|file| file.relative)
        .collect::<Vec<_>>();
    source_snapshot_for_paths(root, analyzer, &sources)
}
fn source_snapshot_for_paths(
    root: &Path,
    analyzer: &Analyzer,
    sources: &[String],
) -> SourceSnapshot {
    // Use the scanner's discovery rules, including vendor directories and language
    // overrides. This also detects added/deleted dependency files after expansion.
    let metadata = analyzer_core::resolution::ResolutionContext::load_for_sources(
        root,
        &analyzer.config,
        sources.iter().map(String::as_str),
    );
    analyzer_core::discovery::discover(root, &analyzer.config, &analyzer.registry)
        .into_iter()
        .map(|file| file.absolute)
        .chain(metadata.metadata_paths)
        .filter_map(|path| {
            let meta = std::fs::metadata(&path).ok()?;
            if !meta.is_file() {
                return None;
            }
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .unwrap_or_default()
                .as_nanos();
            Some((path, (meta.len(), modified)))
        })
        .collect()
}
fn open_url(url: &str) {
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = std::process::Command::new("xdg-open").arg(url).spawn();
    if let Err(e) = result {
        log::warn!("could not open browser: {e}")
    }
}
async fn index() -> Response {
    let mut response = Html(include_str!("../web/index.html")).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
async fn css() -> Response {
    asset(include_str!("../web/app.css"), "text/css; charset=utf-8")
}
async fn graph_model() -> Response {
    asset(
        include_str!("../web/graph-model.js"),
        "text/javascript; charset=utf-8",
    )
}
async fn tree_model() -> Response {
    asset(
        include_str!("../web/tree-model.js"),
        "text/javascript; charset=utf-8",
    )
}
async fn relations_model() -> Response {
    asset(
        include_str!("../web/relations-model.js"),
        "text/javascript; charset=utf-8",
    )
}
async fn codemap_model() -> Response {
    asset(
        include_str!("../web/codemap-model.js"),
        "text/javascript; charset=utf-8",
    )
}
async fn codemap_css() -> Response {
    asset(
        include_str!("../web/codemap.css"),
        "text/css; charset=utf-8",
    )
}
async fn codemap_controls() -> Response {
    asset(
        include_str!("../web/codemap-controls.js"),
        "text/javascript; charset=utf-8",
    )
}
async fn codemap_js() -> Response {
    asset(
        include_str!("../web/codemap.js"),
        "text/javascript; charset=utf-8",
    )
}
async fn js() -> Response {
    asset(
        include_str!("../web/app.js"),
        "text/javascript; charset=utf-8",
    )
}
async fn ui_utils() -> Response {
    asset(
        include_str!("../web/ui-utils.js"),
        "text/javascript; charset=utf-8",
    )
}
fn asset(body: &'static str, content_type: &'static str) -> Response {
    let mut r = body.into_response();
    r.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    r.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}
#[derive(Clone, serde::Serialize)]
struct TreeNode {
    id: String,
    name: String,
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fqn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    loc: Option<analyzer_core::Loc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metrics: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    violations: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    symbol_id: Option<String>,
    vendor: bool,
    children: Vec<TreeNode>,
}
fn empty_node(id: String, name: String, kind: &str) -> TreeNode {
    TreeNode {
        id,
        name,
        kind: kind.into(),
        path: None,
        language: None,
        fqn: None,
        file_id: None,
        line: None,
        loc: None,
        metrics: None,
        violations: None,
        symbol_id: None,
        vendor: false,
        children: vec![],
    }
}
#[derive(Default, Deserialize)]
struct TreeQuery {
    compact: Option<bool>,
    summary_only: Option<bool>,
}
async fn tree(State(state): State<Arc<AppState>>, Query(q): Query<TreeQuery>) -> Json<Value> {
    let report = state.report.read().unwrap();
    if q.summary_only.unwrap_or(false) {
        return Json(
            json!({"project":state.root.file_name().and_then(|s|s.to_str()).unwrap_or("Project"),"root":state.root,"summary":report.summary,"strategy_capabilities":report.strategy_capabilities,"named_sets":state.analyzer.config.sets,"children":[],"summary_only":true}),
        );
    }
    let info = state.function_info.read().unwrap();
    let mut roots: BTreeMap<String, TreeBuilder> = BTreeMap::new();
    let mut entity_violations = BTreeMap::<(&str, &str), usize>::new();
    for issue in &report.violations {
        *entity_violations
            .entry((&issue.file, &issue.entity))
            .or_default() += 1;
    }
    let violations: BTreeMap<String, usize> = {
        let mut map = BTreeMap::new();
        for v in &report.violations {
            let mut paths = std::collections::BTreeSet::from([v.file.clone()]);
            if let Some(d) = &v.architecture {
                paths.extend(d.affected_ranges().map(|r| r.file.to_string()));
            }
            if let Some(d) = &v.module_cycle {
                paths.extend(d.files.iter().cloned());
            }
            for path in paths {
                *map.entry(path).or_insert(0) += 1;
            }
        }
        map
    };
    for file in &report.files {
        let parts = Path::new(&file.path)
            .components()
            .filter_map(|p| p.as_os_str().to_str())
            .collect::<Vec<_>>();
        if parts.is_empty() {
            continue;
        }
        let filename = parts[parts.len() - 1];
        let file_id = file.path.clone();
        let file_node = TreeNode {
            id: file_id.clone(),
            name: filename.into(),
            kind: "file".into(),
            path: Some(file.path.clone()),
            language: Some(file.language.clone()),
            fqn: None,
            file_id: Some(file_id.clone()),
            line: None,
            loc: Some(file.loc.clone()),
            metrics: None,
            violations: violations.get(&file.path).copied(),
            symbol_id: None,
            vendor: file.vendor,
            children: declaration_tree(file, &entity_violations, &info),
        };
        insert_path(&mut roots, &parts, file_node);
    }
    let mut value = json!({"project":state.root.file_name().and_then(|s|s.to_str()).unwrap_or("Project"),"root":state.root,"summary":report.summary,"strategy_capabilities":report.strategy_capabilities,"named_sets":state.analyzer.config.sets,"children":roots.into_values().map(TreeBuilder::finish).collect::<Vec<_>>()});
    if q.compact.unwrap_or(false) {
        fn compact(nodes: &mut Value) {
            if let Some(nodes) = nodes.as_array_mut() {
                for node in nodes {
                    if let Some(o) = node.as_object_mut() {
                        o.retain(|_, v| !v.is_null());
                        if let Some(m) = o.get_mut("metrics").and_then(Value::as_object_mut) {
                            m.retain(|key, _| {
                                [
                                    "parameters",
                                    "complexity",
                                    "fan_in",
                                    "fan_out",
                                    "errors",
                                    "warnings",
                                    "methods",
                                    "fields",
                                ]
                                .contains(&key.as_str())
                            });
                        }
                    }
                    compact(&mut node["children"]);
                }
            }
        }
        compact(&mut value["children"]);
    }
    Json(value)
}
fn declaration_tree(
    file: &analyzer_core::FileModel,
    violations: &BTreeMap<(&str, &str), usize>,
    info: &BTreeMap<String, Value>,
) -> Vec<TreeNode> {
    struct Declaration<'a> {
        node: TreeNode,
        range: &'a analyzer_core::SourceRange,
        parent_function: Option<&'a str>,
        parent_type: Option<&'a str>,
        is_type: bool,
        is_initializer: bool,
    }
    let issue_count = |fqn: &str| {
        violations
            .get(&(file.path.as_str(), fqn))
            .copied()
            .unwrap_or(0)
    };
    let mut declarations = Vec::new();
    for model in &file.types {
        let mut node = empty_node(
            format!(
                "decl:{}:{}:type:{}:{}:{}",
                file.language,
                file.path,
                model.fqn,
                model.range.start_line,
                model.range.start_column
            ),
            model.name.clone(),
            &format!("{:?}", model.kind).to_lowercase(),
        );
        node.symbol_id = Some(format!("type:{}", model.fqn));
        node.fqn = Some(model.fqn.clone());
        node.violations = Some(issue_count(&model.fqn));
        node.metrics = Some(json!({"methods": model.methods.len(), "fields": model.fields.len()}));
        declarations.push(Declaration {
            node,
            range: &model.range,
            parent_function: None,
            parent_type: None,
            is_type: true,
            is_initializer: false,
        });
    }
    for model in &file.functions {
        let symbol = format!("fn:{}:{}:{}", file.language, file.path, model.fqn);
        let mut node = empty_node(
            format!(
                "decl:{symbol}:{}:{}",
                model.range.start_line, model.range.start_column
            ),
            if model.is_module_initializer() {
                "Инициализация модуля".into()
            } else {
                model.name.clone()
            },
            if model.is_module_initializer() {
                "module_initializer"
            } else if model.parent_type.is_some() && model.parent_function.is_none() {
                "method"
            } else {
                "function"
            },
        );
        node.symbol_id = Some(symbol.clone());
        node.fqn = Some(model.fqn.clone());
        node.violations = Some(issue_count(&model.fqn));
        node.metrics = info
            .get(&symbol)
            .and_then(|value| value.get("metrics"))
            .cloned();
        declarations.push(Declaration {
            node,
            range: &model.range,
            parent_function: model.parent_function.as_deref(),
            parent_type: model.parent_type.as_deref(),
            is_type: false,
            is_initializer: model.is_module_initializer(),
        });
    }
    let contains = |outer: &analyzer_core::SourceRange, child: &analyzer_core::SourceRange| {
        let start = (outer.start_line, outer.start_column);
        let end = (outer.end_line, outer.end_column);
        let child_start = (child.start_line, child.start_column);
        let child_end = (child.end_line, child.end_column);
        start <= child_start && child_end <= end && (start != child_start || end != child_end)
    };
    // Parent lookup remains bounded for large files: explicit FQNs use indexes,
    // and a source-order stack handles lexical nesting without a quadratic scan.
    let mut function_index = BTreeMap::<&str, Vec<usize>>::new();
    let mut type_index = BTreeMap::<&str, Vec<usize>>::new();
    for (index, declaration) in declarations.iter().enumerate() {
        let fqn = declaration.node.fqn.as_deref().unwrap_or("");
        if declaration.is_type {
            type_index.entry(fqn).or_default().push(index);
            if declaration.node.name != fqn {
                type_index
                    .entry(&declaration.node.name)
                    .or_default()
                    .push(index);
            }
        } else {
            function_index.entry(fqn).or_default().push(index);
        }
    }
    let mut source_order = (0..declarations.len()).collect::<Vec<_>>();
    source_order.sort_by_key(|index| {
        let range = declarations[*index].range;
        (
            range.start_line,
            range.start_column,
            std::cmp::Reverse((range.end_line, range.end_column)),
        )
    });
    let mut lexical_parents = vec![None; declarations.len()];
    let mut stack = Vec::<usize>::new();
    for index in source_order {
        // The synthetic entry spans the file without lexically containing its declarations.
        if declarations[index].is_initializer {
            continue;
        }
        while stack
            .last()
            .is_some_and(|outer| !contains(declarations[*outer].range, declarations[index].range))
        {
            stack.pop();
        }
        lexical_parents[index] = stack.last().copied();
        stack.push(index);
    }
    let parents = declarations
        .iter()
        .enumerate()
        .map(|(index, declaration)| {
            let explicit = declaration
                .parent_function
                .and_then(|name| function_index.get(name))
                .or_else(|| {
                    declaration
                        .parent_type
                        .and_then(|name| type_index.get(name))
                });
            explicit
                .and_then(|candidates| {
                    candidates
                        .iter()
                        .copied()
                        .filter(|candidate| {
                            *candidate != index
                                && (declaration.parent_function.is_none()
                                    || contains(declarations[*candidate].range, declaration.range))
                        })
                        .min_by_key(|candidate| {
                            let range = declarations[*candidate].range;
                            (
                                range.end_line.saturating_sub(range.start_line),
                                range.end_column.saturating_sub(range.start_column),
                            )
                        })
                })
                .or(lexical_parents[index])
        })
        .collect::<Vec<_>>();
    let mut nodes = declarations
        .into_iter()
        .map(|mut declaration| {
            declaration.node.file_id = Some(file.path.clone());
            declaration.node.path = Some(file.path.clone());
            declaration.node.language = Some(file.language.clone());
            declaration.node.line = Some(declaration.range.start_line);
            declaration.node.vendor = file.vendor;
            Some(declaration.node)
        })
        .collect::<Vec<_>>();
    let mut children = vec![Vec::<usize>::new(); nodes.len()];
    let mut root_indices = Vec::new();
    for (index, parent) in parents.iter().enumerate() {
        if let Some(parent) = parent {
            children[*parent].push(index);
        } else {
            root_indices.push(index);
        }
    }
    fn finish(index: usize, nodes: &mut [Option<TreeNode>], children: &[Vec<usize>]) -> TreeNode {
        let mut node = nodes[index].take().expect("declaration emitted once");
        for child in &children[index] {
            node.children.push(finish(*child, nodes, children));
        }
        node.children.sort_by_key(|child| child.line);
        node
    }
    let mut roots = root_indices
        .into_iter()
        .map(|index| finish(index, &mut nodes, &children))
        .collect::<Vec<_>>();
    roots.sort_by_key(|node| node.line);
    roots
}
#[derive(Default)]
struct TreeBuilder {
    node: Option<TreeNode>,
    children: BTreeMap<String, TreeBuilder>,
}
impl TreeBuilder {
    fn finish(self) -> TreeNode {
        let mut node = self.node.expect("tree builder node initialized");
        node.children
            .extend(self.children.into_values().map(Self::finish));
        node.children.sort_by(|a, b| a.id.cmp(&b.id));
        node
    }
}
fn insert_path(nodes: &mut BTreeMap<String, TreeBuilder>, parts: &[&str], node: TreeNode) {
    if parts.len() <= 1 {
        let key = node.id.clone();
        nodes.insert(
            key,
            TreeBuilder {
                node: Some(node),
                children: BTreeMap::new(),
            },
        );
        return;
    }
    let directory = parts[0];
    let key = format!("dir:{directory}");
    let dir = nodes.entry(key.clone()).or_insert_with(|| TreeBuilder {
        node: Some(empty_node(key, directory.into(), "directory")),
        children: BTreeMap::new(),
    });
    insert_path(&mut dir.children, &parts[1..], node);
}
fn call_target_details(node: &analyzer_core::graph::GraphNode) -> Value {
    let mut value = json!({"id":node.id,"label":node.label,"file":node.file,"line":node.line,"kind":format!("{:?}",node.kind).to_lowercase()});
    if let Some(target) = &node.cargo_target {
        value["cargo_target"] = json!(target);
    }
    if let Some(target) = &node.cargo_target_id {
        value["cargo_target_id"] = json!(target);
    }
    if let Some(source) = &node.source_fqn {
        value["source_fqn"] = json!(source);
    }
    value
}
async fn file(State(state): State<Arc<AppState>>, Query(q): Query<FileQuery>) -> Response {
    let requested = q.path.or(q.id).unwrap_or_default();
    let report = state.report.read().unwrap();
    let Some(model) = report.files.iter().find(|f| f.path == requested) else {
        return (StatusCode::NOT_FOUND, "file not in analyzed project").into_response();
    };
    let Some(path) = confined_file(&state.root, &model.path, state.single_file) else {
        return (
            StatusCode::FORBIDDEN,
            "file path escapes project root or is not a regular file",
        )
            .into_response();
    };
    match std::fs::read_to_string(path) {
        Ok(source) => {
            let info = state.function_info.read().unwrap();
            let functions = model.functions.iter().map(|f| {
                let id=format!("fn:{}:{}:{}",model.language,model.path,f.fqn);
                let mut value=json!({"id":id,"fqn":f.fqn,"name":f.name,"kind":if f.is_module_initializer(){"module_initializer"}else if f.parent_type.is_some(){"method"}else{"function"},"range":f.range,"line":f.range.start_line,"signature":f.signature,"parent_type":f.parent_type,"algorithmic_complexity":f.algorithmic_complexity});
                if let Some(metadata)=info.get(&id) {for (key,item) in metadata.as_object().unwrap() { value[key]=item.clone(); }}
                value
            }).collect::<Vec<_>>();
            let types=model.types.iter().map(|t|json!({"id":format!("type:{}",t.fqn),"fqn":t.fqn,"name":t.name,"kind":format!("{:?}",t.kind).to_lowercase(),"range":t.range,"line":t.range.start_line})).collect::<Vec<_>>();
            // A file request needs only this file's callers, using the graph's existing index.
            // Sorted, unique IDs preserve global edge order even for duplicate declarations.
            let callers = report
                .graph
                .nodes
                .iter()
                .filter(|node| {
                    node.kind == analyzer_core::graph::NodeKind::Function
                        && node.file.as_deref() == Some(model.path.as_str())
                })
                .map(|node| node.id.clone())
                .collect::<std::collections::BTreeSet<_>>();
            let mut edges_by_callsite = BTreeMap::new();
            for id in callers {
                for edge in report.graph.outgoing_edges(&id) {
                    edges_by_callsite
                        .entry((
                            edge.callsite.file.as_ref(),
                            edge.callsite.start_line,
                            edge.callsite.start_column,
                        ))
                        .or_insert_with(Vec::new)
                        .push(edge);
                }
            }
            let calls=model.functions.iter().flat_map(|f|f.calls.iter().map(|c|{let edges=edges_by_callsite.get(&(c.range.file.as_ref(),c.range.start_line,c.range.start_column)).map(Vec::as_slice).unwrap_or(&[]);let targets=edges.iter().map(|e|e.to.clone()).collect::<Vec<_>>();let target_details=edges.iter().filter_map(|e|report.graph.node(e.to.as_ref()).map(call_target_details)).collect::<Vec<_>>();json!({"name":c.name,"targets":c.targets,"resolved_targets":targets,"resolved_target_details":target_details,"confidence":c.confidence,"range":c.range,"file_id":model.path})})).collect::<Vec<_>>();
            let issues = report
                .violations
                .iter()
                .filter(|v| issue_in_file(v, &model.path))
                .collect::<Vec<_>>();
            let lines = issues
                .iter()
                .flat_map(|v| {
                    let mut lines = Vec::new();
                    if v.file == model.path {
                        lines.push(v.line);
                    }
                    if let Some(d) = &v.architecture {
                        lines.extend(
                            d.affected_ranges()
                                .filter(|r| r.file.as_ref() == model.path)
                                .map(|r| r.start_line),
                        );
                    }
                    if let Some(d) = &v.module_cycle {
                        lines.extend(
                            d.affected_ranges()
                                .filter(|r| r.file.as_ref() == model.path)
                                .map(|r| r.start_line),
                        );
                        if d.files.contains(&model.path) && lines.is_empty() {
                            lines.push(1);
                        }
                    }
                    lines
                })
                .collect::<std::collections::BTreeSet<_>>();
            Json(json!({"id":model.path,"path":model.path,"language":model.language,"source":source,"loc":model.loc,"functions":functions,"types":types,"calls":calls,"violations":issues.iter().take(50).collect::<Vec<_>>(),"violation_lines":lines,"violation_count":issues.len(),"parse_error_count":model.parse_errors.len()})).into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, "source file unavailable").into_response(),
    }
}
/// Detailed AST is parsed on demand and never retained in the project report.
async fn ast(State(state): State<Arc<AppState>>, Query(q): Query<AstQuery>) -> Response {
    let (path, selected_range) = {
        let report = state.report.read().unwrap();
        let Some(model) = report.files.iter().find(|file| file.path == q.path) else {
            return (StatusCode::NOT_FOUND, "file not in analyzed project").into_response();
        };
        let Some(path) = confined_file(&state.root, &model.path, state.single_file) else {
            return (StatusCode::FORBIDDEN, "file path escapes project root").into_response();
        };
        let selected_range = if let Some(selector) = q.function.as_deref() {
            let Some(function) = model
                .functions
                .iter()
                .find(|f| f.fqn == selector || f.name == selector)
            else {
                return (StatusCode::NOT_FOUND, "function not found").into_response();
            };
            Some(function.range.clone())
        } else {
            None
        };
        (path, selected_range)
    };
    let Ok(source) = std::fs::read_to_string(&path) else {
        return (StatusCode::NOT_FOUND, "source file unavailable").into_response();
    };
    let Some(strategy) = state
        .analyzer
        .registry
        .language_for_path(&path, &state.analyzer.config.languages.extensions)
    else {
        return (StatusCode::BAD_REQUEST, "unsupported file").into_response();
    };
    let tree = strategy.parse(source.trim_start_matches('\u{feff}'), Path::new(&q.path));
    if let Some(range) = selected_range {
        let mut found = Vec::new();
        tree.root.find(
            &|node| {
                node.kind == analyzer_core::AstKind::Function
                    && node.range.start_line == range.start_line
                    && node.range.start_column == range.start_column
            },
            &mut found,
        );
        let Some(node) = found.first() else {
            return (StatusCode::NOT_FOUND, "AST function node not found").into_response();
        };
        Json(json!({"function":q.function,"node":node,"parse_errors":tree.errors})).into_response()
    } else {
        Json(json!(tree)).into_response()
    }
}
fn confined_file(root: &Path, relative: &str, single_file: bool) -> Option<PathBuf> {
    let requested = Path::new(relative);
    if requested.is_absolute()
        || requested.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return None;
    }
    let base = if single_file { root.parent()? } else { root };
    let canonical = std::fs::canonicalize(base.join(requested)).ok()?;
    let confined = if single_file {
        canonical == root
    } else {
        canonical.starts_with(root)
    };
    if !confined || !canonical.is_file() {
        return None;
    }
    Some(canonical)
}
async fn symbol(State(state): State<Arc<AppState>>, AxumPath(id): AxumPath<String>) -> Response {
    let report = state.report.read().unwrap();
    let source = report
        .graph
        .node(&id)
        .map(|node| node.source_id().into_owned())
        .unwrap_or_else(|| id.clone());
    let Some((file, f)) = report.symbol(&source) else {
        return (StatusCode::NOT_FOUND, "symbol not found").into_response();
    };
    let info = state.function_info.read().unwrap();
    let mut value = info
        .get(&id)
        .or_else(|| info.get(&source))
        .cloned()
        .unwrap_or_else(|| json!({"id":id}));
    value["id"] = json!(id);
    value["fqn"] = json!(f.fqn);
    value["name"] = json!(f.name);
    value["file"] = json!(file.path);
    value["line"] = json!(f.range.start_line);
    value["range"] = json!(f.range);
    value["signature"] = json!(f.signature);
    value["parameters"] = json!(f.parameters);
    value["algorithmic_complexity"] = json!(f.algorithmic_complexity);
    value["metrics"]["params"] = json!(f.parameters.len());
    value["metrics"]["return_points"] = json!(f.return_points);
    Json(value).into_response()
}
async fn graph(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<GraphQuery>,
) -> Response {
    let report = state.report.read().unwrap();
    let mut filter = GraphFilter::all();
    filter.with_external = q.external.unwrap_or(true);
    if let Some(category) = q.category {
        filter.external_categories = category.split(',').map(str::to_string).collect();
    }
    filter.language = q.language;
    filter.module = q.module;
    filter.directory = q.directory;
    if let Some(levels) = q.confidence {
        filter.confidence = levels
            .split(',')
            .filter_map(|c| match c {
                "resolved" => Some(analyzer_core::Confidence::Resolved),
                "ambiguous" => Some(analyzer_core::Confidence::Ambiguous),
                "dynamic" => Some(analyzer_core::Confidence::Dynamic),
                _ => None,
            })
            .collect();
    }
    if let Some(exclude) = q.exclude {
        filter.exclude = exclude.split(',').map(str::to_string).collect();
    }
    let direction = match q.dir.as_deref().unwrap_or("callees") {
        "callers" => Direction::Callers,
        "both" => Direction::Both,
        _ => Direction::Callees,
    };
    let mut config = state.analyzer.config.clone();
    if let Some(mode) = q.vendor.as_deref() {
        config.vendor.mode = match mode {
            "expanded" => analyzer_core::config::VendorMode::Expanded,
            "hidden" => analyzer_core::config::VendorMode::Hidden,
            _ => analyzer_core::config::VendorMode::Collapsed,
        };
    }
    let selector = id
        .strip_prefix("type:")
        .map(|fqn| format!("class:{fqn}"))
        .unwrap_or(id);
    let query = report.graph_query(&selector, direction, q.depth, &filter, &config);
    let mut value = serde_json::to_value(query).unwrap_or_else(|_| json!({"nodes":[],"edges":[]}));
    let revision = state.revision.load(Ordering::Acquire);
    value["revision"] = json!(revision);
    value["coverage_complete"] = json!(report.deferred_vendor.is_empty());
    value["coverage_unparsed_vendor_files"] = json!(report.deferred_vendor.len());
    value["coverage_unparsed_vendor_packages"] = json!(report
        .deferred_vendor
        .iter()
        .map(|file| &file.package)
        .collect::<std::collections::BTreeSet<_>>());
    if q.lazy.unwrap_or(false) {
        let mut raw_config = config.clone();
        raw_config.vendor.mode = analyzer_core::config::VendorMode::Expanded;
        let requested_depth = if q.coverage_unbounded.unwrap_or(false) {
            None
        } else {
            q.coverage_depth.or(q.depth)
        };
        let mut requested = report
            .graph_query(&selector, direction, requested_depth, &filter, &raw_config)
            .coverage;
        requested.vendor_boundary |= !report.deferred_vendor.is_empty()
            || config.vendor.mode != analyzer_core::config::VendorMode::Expanded
                && !requested.vendor_functions.is_empty();
        value["loaded_coverage"] = value["coverage"].clone();
        value["coverage"] = json!(requested);
        value["requested_coverage"] = value["coverage"].clone();
        value["coverage_scope"] = json!("requested");
        value["coverage_direction"] = json!(direction);
        value["coverage_depth"] = json!(requested_depth);
        value["requested_depth"] = json!(requested_depth);
        value["coverage_revision"] = json!(revision);
        value["coverage_vendor_view"] = json!(config.vendor.mode);
        value["coverage_filters"] = json!({"external":filter.with_external,"category":filter.external_categories,"language":filter.language,"module":filter.module,"directory":filter.directory,"confidence":filter.confidence,"exclude":filter.exclude});
        let raw = report.graph_query(&selector, direction, q.depth, &filter, &raw_config);
        let ids = raw
            .nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let mut frontier = ids
            .iter()
            .map(|id| ((*id).to_owned(), json!({"callers":[],"callees":[]})))
            .collect::<BTreeMap<_, _>>();
        let mut identifiers = BTreeMap::<String, usize>::new();
        for (index, edge) in report.graph.edges.iter().enumerate() {
            if !ids.contains(edge.from.as_ref()) && !ids.contains(edge.to.as_ref()) {
                continue;
            }
            // Canonical edge ordinals are exact within this revision. Identical
            // edges share an ordinal, matching the UI's duplicate suppression.
            let key = *identifiers
                .entry(serde_json::to_string(edge).unwrap())
                .or_insert(index);
            for (source, target, dir) in [
                (&edge.from, &edge.to, "callees"),
                (&edge.to, &edge.from, "callers"),
            ] {
                if let Some(hints) = frontier.get_mut(source.as_ref()) {
                    if report
                        .graph
                        .node(target)
                        .is_some_and(|node| graph_filter_accepts(&filter, node, edge))
                    {
                        let hint = json!({"target":target,"key":key});
                        let entries = hints[dir].as_array_mut().unwrap();
                        entries.push(hint);
                    }
                }
            }
        }
        let evidence = |edge: &analyzer_core::graph::GraphEdge| {
            let mut item = serde_json::to_value(edge).unwrap();
            item["load_key"] = json!(identifiers.get(&serde_json::to_string(edge).unwrap()));
            item
        };
        value["coverage_nodes"] = json!(raw.nodes);
        value["coverage_edges"] = json!(raw.edges.iter().map(evidence).collect::<Vec<_>>());
        value["coverage_cycles"] = json!(raw.cycles.iter().map(evidence).collect::<Vec<_>>());
        value["frontier"] = json!(frontier);
        value["lazy"] = json!(true);
    }
    let hidden_ids = value["hidden_vendor_edges"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|edge| [edge["from"].as_str(), edge["to"].as_str()])
        .flatten()
        .collect::<std::collections::BTreeSet<_>>();
    value["hidden_vendor_nodes"] = json!(hidden_ids
        .iter()
        .filter_map(|id| report.graph.node(id))
        .filter(|node| node.vendor)
        .collect::<Vec<_>>());
    value["root"] = value["roots"]
        .as_array()
        .and_then(|x| x.first())
        .cloned()
        .unwrap_or(Value::Null);
    let info = state.function_info.read().unwrap();
    for key in ["nodes", "coverage_nodes"] {
        if let Some(nodes) = value[key].as_array_mut() {
            for node in nodes {
                if let Some(metadata) = node["id"].as_str().and_then(|id| info.get(id)) {
                    for (key, item) in metadata.as_object().unwrap() {
                        node[key] = item.clone();
                    }
                }
            }
        }
    }
    if q.compact.unwrap_or(false) {
        for key in ["nodes", "coverage_nodes", "hidden_vendor_nodes"] {
            if let Some(nodes) = value[key].as_array_mut() {
                for node in nodes {
                    if let Some(estimate) = node
                        .get_mut("algorithmic_complexity")
                        .and_then(Value::as_object_mut)
                    {
                        estimate.retain(|key, _| {
                            ["status", "formula", "normalized"].contains(&key.as_str())
                        });
                        node["details_available"] = json!(true);
                    }
                }
            }
        }
        if let Some(frontier) = value["frontier"].as_object_mut() {
            for hints in frontier.values_mut() {
                for dir in ["callers", "callees"] {
                    if let Some(items) = hints[dir].as_array_mut() {
                        for item in items {
                            if let Some(o) = item.as_object_mut() {
                                o.remove("target");
                            }
                        }
                    }
                }
            }
        }
        for (raw, visible, marker) in [
            ("coverage_nodes", "nodes", "coverage_nodes_same_as_nodes"),
            ("coverage_edges", "edges", "coverage_edges_same_as_edges"),
            (
                "coverage_cycles",
                "cycles",
                "coverage_cycles_same_as_cycles",
            ),
        ] {
            let mut full = value[raw].take();
            let same = match (full.as_array_mut(), value[visible].as_array()) {
                (Some(full), Some(shown)) if full.len() == shown.len() => {
                    full.iter_mut().zip(shown).all(|(node, visible)| {
                        let key = node.as_object_mut().and_then(|o| o.remove("load_key"));
                        let same = node == visible;
                        if let Some(key) = key {
                            node.as_object_mut().unwrap().insert("load_key".into(), key);
                        }
                        same
                    })
                }
                _ => false,
            };
            if same {
                value[visible] = full;
                value.as_object_mut().unwrap().remove(raw);
                value[marker] = json!(true);
            } else {
                value[raw] = full;
            }
        }
        for (key, marker) in [
            ("requested_coverage", "requested_coverage_same_as_coverage"),
            ("loaded_coverage", "loaded_coverage_same_as_coverage"),
        ] {
            if value[key] == value["coverage"] {
                value.as_object_mut().unwrap().remove(key);
                value[marker] = json!(true);
            }
        }
        value["compact"] = json!(true);
    }
    Json(value).into_response()
}
// Mirror GraphFilter's target predicate for the one-hop continuation metadata.
// No traversal is performed here; canonical edges are visited once per response.
fn graph_filter_accepts(
    filter: &GraphFilter,
    node: &analyzer_core::graph::GraphNode,
    edge: &analyzer_core::graph::GraphEdge,
) -> bool {
    use analyzer_core::graph::NodeKind;
    (filter.with_runtime || node.category.as_deref() != Some("runtime"))
        && (node.kind != NodeKind::External
            || filter.with_external
                && (filter.external_categories.is_empty()
                    || node
                        .category
                        .as_ref()
                        .is_some_and(|category| filter.external_categories.contains(category))))
        && filter
            .language
            .as_ref()
            .is_none_or(|language| node.language.as_ref() == Some(language))
        && filter
            .module
            .as_ref()
            .is_none_or(|module| node.module.as_ref() == Some(module))
        && filter.directory.as_ref().is_none_or(|directory| {
            let directory = directory.trim_end_matches('/');
            node.file.as_ref().is_some_and(|file| {
                directory.is_empty()
                    || file == directory
                    || file
                        .strip_prefix(directory)
                        .is_some_and(|rest| rest.starts_with('/'))
            })
        })
        && !filter
            .exclude
            .iter()
            .any(|item| node.id.contains(item) || node.label.contains(item))
        && (filter.confidence.is_empty() || filter.confidence.contains(&edge.confidence))
}
async fn search(State(state): State<Arc<AppState>>, Query(q): Query<SearchQuery>) -> Json<Value> {
    let report = state.report.read().unwrap();
    let query = q.q.as_str();
    let mut matches = report
        .files
        .iter()
        .flat_map(|file| {
            file.functions.iter().filter_map(move |f| {
                [f.fqn.as_str(), f.name.as_str(), file.path.as_str()]
                    .into_iter()
                    .filter_map(|candidate| analyzer_core::search::fuzzy_score(candidate, query))
                    .min()
                    .map(|score| (score, file, f))
            })
        })
        .collect::<Vec<_>>();
    matches.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.2.fqn.cmp(&b.2.fqn))
            .then(a.1.path.cmp(&b.1.path))
    });
    let total = matches.len();
    let results = matches.into_iter().take(200).map(|(score, file, f)| json!({"id":format!("fn:{}:{}:{}",file.language,file.path,f.fqn),"fqn":f.fqn,"name":f.name,"line":f.range.start_line,"file":file.path,"score":score})).collect::<Vec<_>>();
    Json(json!({"results":results,"total":total,"truncated":total>200}))
}
#[derive(Deserialize, Default)]
struct ViolationsQuery {
    page: Option<usize>,
    per_page: Option<usize>,
    severity: Option<String>,
    rule: Option<String>,
    q: Option<String>,
    file: Option<String>,
    kind: Option<String>,
    principle: Option<String>,
}
async fn architecture(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!(state.report.read().unwrap().architecture))
}
fn issue_in_file(v: &analyzer_core::rules::Violation, path: &str) -> bool {
    v.file == path
        || v.module_cycle
            .as_ref()
            .is_some_and(|d| d.files.iter().any(|file| file == path))
        || v.architecture
            .as_ref()
            .is_some_and(|d| d.affected_ranges().any(|r| r.file.as_ref() == path))
}
async fn violations(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ViolationsQuery>,
) -> Json<Value> {
    let report = state.report.read().unwrap();
    let mut rules = BTreeMap::<String, usize>::new();
    let scoped = report
        .violations
        .iter()
        .filter(|v| q.file.as_deref().is_none_or(|path| issue_in_file(v, path)))
        .filter(|v| match q.kind.as_deref() {
            Some("architecture") => v.architecture.is_some(),
            Some("metrics") => v.architecture.is_none(),
            _ => true,
        })
        .collect::<Vec<_>>();
    for v in &scoped {
        *rules.entry(v.rule.clone()).or_default() += 1;
    }
    let parse_errors = report
        .parse_errors
        .iter()
        .filter(|e| {
            q.file
                .as_deref()
                .is_none_or(|path| path == e.range.file.as_ref())
        })
        .collect::<Vec<_>>();
    let mut summary = json!(report.summary);
    summary["errors"] = json!(scoped
        .iter()
        .filter(|v| v.severity == analyzer_core::rules::Severity::Error)
        .count());
    summary["warnings"] = json!(scoped
        .iter()
        .filter(|v| v.severity == analyzer_core::rules::Severity::Warn)
        .count());
    summary["architecture_warnings"] =
        json!(scoped.iter().filter(|v| v.architecture.is_some()).count());
    summary["parse_errors"] = json!(parse_errors.len());
    if q.file.is_some() {
        summary["files"] = json!(report
            .files
            .iter()
            .filter(|f| Some(f.path.as_str()) == q.file.as_deref())
            .count());
    }
    let needle = q.q.as_deref().unwrap_or("").to_lowercase();
    let mut selected = scoped
        .iter()
        .copied()
        .filter(|v| {
            let severity = if v.severity == analyzer_core::rules::Severity::Error {
                "error"
            } else {
                "warn"
            };
            q.severity
                .as_deref()
                .filter(|s| !s.is_empty())
                .is_none_or(|s| s == severity)
                && q.rule
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .is_none_or(|r| r == v.rule)
                && q.principle
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .is_none_or(|p| v.architecture.as_ref().is_some_and(|d| d.principle == p))
                && (needle.is_empty()
                    || v.file.to_lowercase().contains(&needle)
                    || v.entity.to_lowercase().contains(&needle)
                    || v.rule.to_lowercase().contains(&needle)
                    || v.module_cycle.as_ref().is_some_and(|d| {
                        d.files
                            .iter()
                            .chain(&d.participants)
                            .any(|text| text.to_lowercase().contains(&needle))
                    })
                    || v.architecture.as_ref().is_some_and(|d| {
                        [&d.title, &d.principle, &d.fact, &d.hypothesis]
                            .iter()
                            .any(|s| s.to_lowercase().contains(&needle))
                            || d.affected_ranges()
                                .any(|r| r.file.to_lowercase().contains(&needle))
                            || d.recommendations.iter().any(|r| {
                                r.pattern.to_lowercase().contains(&needle)
                                    || r.proposal.to_lowercase().contains(&needle)
                            })
                    }))
        })
        .collect::<Vec<_>>();
    selected.sort_by_key(|v| {
        (
            if v.severity == analyzer_core::rules::Severity::Error {
                0
            } else {
                1
            },
            &v.file,
            v.line,
            &v.rule,
        )
    });
    let per_page = q.per_page.unwrap_or(50).clamp(1, 200);
    let pages = selected.len().div_ceil(per_page).max(1);
    let page = q.page.unwrap_or(1).clamp(1, pages);
    Json(
        json!({"violations":selected.iter().skip((page-1)*per_page).take(per_page).collect::<Vec<_>>(), "total":selected.len(), "all_total":scoped.len(), "page":page, "pages":pages, "per_page":per_page, "rules":rules, "summary":summary,
        "parse_errors":parse_errors.iter().take(200).collect::<Vec<_>>(), "parse_error_total":parse_errors.len(), "architecture": report.architecture}),
    )
}
async fn metric_catalog() -> Json<Value> {
    Json(json!(super::presentation::metric_catalog()))
}
fn configuration_snapshot(
    analyzer: &Analyzer,
    config_path: Option<&str>,
    loaded: Option<analyzer_core::config::ConfigLoad>,
) -> Value {
    let mut sources = Vec::new();
    let mut diagnostics = Vec::new();
    let mut provenance = json!({});
    let mut paths = std::collections::BTreeSet::new();
    let canonical = |path: &str| {
        std::fs::canonicalize(path)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.into())
    };
    if let Some(path) = config_path {
        paths.insert(canonical(path));
    }
    if let Some(loaded) = loaded {
        paths.extend(loaded.provenance.values().map(|s| canonical(&s.file)));
        provenance = json!(loaded.provenance);
        diagnostics = loaded.diagnostics;
    }
    for path in paths {
        match std::fs::read_to_string(&path) {
            Ok(source) => sources.push(json!({"path":path,"source":source})),
            Err(error) => sources.push(json!({"path":path,"error":error.to_string()})),
        }
    }
    json!({"path":config_path,"effective":analyzer.config,"strategy_capabilities":analyzer.registry.capabilities(),"yaml":serde_yaml::to_string(&analyzer.config).unwrap_or_default(),"sources":sources,"provenance":provenance,"diagnostics":diagnostics})
}
async fn configuration(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(state.config_snapshot.clone())
}
async fn status(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(
        json!({"revision":state.revision.load(Ordering::Acquire), "watch_error":*state.watch_error.read().unwrap()}),
    )
}
fn map_snapshot(state: &AppState) -> (u64, Arc<analyzer_core::codemap::DependencyIndex>) {
    // Lock the report before reading its revision: witnesses and revision are one snapshot.
    let report = state.report.read().unwrap();
    let revision = state.revision.load(Ordering::Acquire);
    let mut cached = state.codemap_index.write().unwrap();
    if let Some((rev, index)) = cached.as_ref() {
        if *rev == revision {
            return (revision, index.clone());
        }
    }
    let index = Arc::new(analyzer_core::codemap::DependencyIndex::build(
        &report,
        &state.analyzer.config,
        &state.analyzer.registry,
    ));
    *cached = Some((revision, index.clone()));
    (revision, index)
}
async fn codemap(
    State(state): State<Arc<AppState>>,
    Query(q): Query<analyzer_core::codemap::MapOptions>,
) -> Response {
    if let Err(error) = q.validate() {
        return (StatusCode::BAD_REQUEST, error).into_response();
    }
    match tokio::task::spawn_blocking(move || {
        let (revision, index) = map_snapshot(&state);
        let view=index.cached_view(&q);
        let blocks=view.blocks.iter().map(|b|(&b.id,b)).collect::<BTreeMap<_,_>>();
        let mut visible=view.frontier.iter().collect::<std::collections::BTreeSet<_>>();
        if q.compact {
            for id in &view.frontier {
                let mut current=blocks.get(id).copied();
                while let Some(parent)=current.and_then(|b|b.parent.as_ref()) {
                    visible.insert(parent);current=blocks.get(parent).copied();
                }
            }
        }
        let selected=view.blocks.iter().filter(|b| !q.compact || visible.contains(&b.id)).collect::<Vec<_>>();
        let mut value=json!({"blocks":selected,"frontier":view.frontier,"links":view.links,"kinds":view.kinds,"coverage":view.coverage,"warnings":view.warnings,"hierarchy_scope":if q.compact {"visible"} else {"complete"}});
        value["revision"] = json!(revision);
        value["options"] = json!(q);
        Json(value).into_response()
    })
    .await
    {
        Ok(response) => response,
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}
#[derive(Deserialize)]
struct MapEvidenceQuery {
    #[serde(default)]
    group: String,
    #[serde(default)]
    expanded: String,
    #[serde(default)]
    kinds: String,
    #[serde(default)]
    external: bool,
    revision: u64,
    block: Option<String>,
    from: Option<String>,
    to: Option<String>,
    mode: Option<String>,
    page: Option<usize>,
    per_page: Option<usize>,
}
async fn codemap_evidence(
    State(state): State<Arc<AppState>>,
    Query(q): Query<MapEvidenceQuery>,
) -> Response {
    let options = analyzer_core::codemap::MapOptions {
        group: q.group.clone(),
        expanded: q.expanded.clone(),
        kinds: q.kinds.clone(),
        external: q.external,
        compact: false,
    };
    if let Err(error) = options.validate() {
        return (StatusCode::BAD_REQUEST, error).into_response();
    }
    let mode = q.mode.as_deref().unwrap_or("members");
    if !["members", "incoming", "outgoing", "internal", "link"].contains(&mode) {
        return (StatusCode::BAD_REQUEST, "Unknown evidence mode").into_response();
    }
    match tokio::task::spawn_blocking(move || {
        let (revision,index)=map_snapshot(&state);
        if revision!=q.revision { return (StatusCode::CONFLICT,Json(json!({"error":"revision_changed","revision":revision}))).into_response(); }
        let view=index.cached_view(&options);
        let page=q.page.unwrap_or(1).max(1);
        let per_page=q.per_page.unwrap_or(50).clamp(1,200);
        let start=page.saturating_sub(1).saturating_mul(per_page);
        let mode=q.mode.as_deref().unwrap_or("members");
        if mode=="link" {
            let Some(link)=view.links.iter().find(|l|Some(&l.from)==q.from.as_ref() && Some(&l.to)==q.to.as_ref()) else {
                return (StatusCode::NOT_FOUND,"Link not found").into_response();
            };
            let ids=link.witnesses.iter().copied().collect::<std::collections::BTreeSet<_>>();
            return map_witness_page(&index,revision,&ids,page,per_page,start);
        }
        let Some(members)=q.block.as_ref().and_then(|id|view.members.get(id)) else {
            return (StatusCode::NOT_FOUND,"Block not found").into_response();
        };
        if mode=="members" {
            let items=members.iter().skip(start).take(per_page).map(|id| &index.entities[id]).collect::<Vec<_>>();
            return Json(json!({"revision":revision,"page":page,"per_page":per_page,"total":members.len(),"entities":items})).into_response();
        }
        if mode=="incoming" || mode=="outgoing" {
            // Follow the same projected links as the map/matrix. Overlapping
            // configured sets can share physical witnesses across their boundary.
            let blocks=view.blocks.iter().map(|b|(b.id.as_str(),b)).collect::<BTreeMap<_,_>>();
            let mut selected=std::collections::BTreeSet::new();
            let mut pending=vec![q.block.as_deref().unwrap()];
            while let Some(id)=pending.pop() {
                if selected.insert(id) {
                    if let Some(block)=blocks.get(id) {
                        pending.extend(block.children.iter().map(String::as_str));
                    }
                }
            }
            let ids=view.links.iter()
                .filter(|link| if mode=="incoming" {
                    !selected.contains(link.from.as_str()) && selected.contains(link.to.as_str())
                } else {
                    selected.contains(link.from.as_str()) && !selected.contains(link.to.as_str())
                })
                .flat_map(|link|link.witnesses.iter().copied()).collect();
            return map_witness_page(&index,revision,&ids,page,per_page,start);
        }
        let ids=index.witnesses.iter().filter(|w| options.kinds.is_empty() || options.kinds.split(',').any(|k|k==w.kind))
            .filter(|w| options.external || [&w.from,&w.to].iter().all(|id| !index.entities[*id].vendor && index.entities[*id].kind!="boundary"))
            .filter(|w|match mode {"internal"=>members.contains(&w.from)&&members.contains(&w.to),"incoming"=>!members.contains(&w.from)&&members.contains(&w.to),_=>members.contains(&w.from)&&!members.contains(&w.to)})
            .map(|w|w.id).collect();
        map_witness_page(&index,revision,&ids,page,per_page,start)
    }).await {
        Ok(response)=>response,
        Err(error)=>(StatusCode::INTERNAL_SERVER_ERROR,error.to_string()).into_response(),
    }
}
fn map_witness_page(
    index: &analyzer_core::codemap::DependencyIndex,
    revision: u64,
    ids: &std::collections::BTreeSet<usize>,
    page: usize,
    per_page: usize,
    start: usize,
) -> Response {
    let witnesses = ids
        .iter()
        .skip(start)
        .take(per_page)
        .map(|id| &index.witnesses[*id])
        .collect::<Vec<_>>();
    let entity_ids = witnesses
        .iter()
        .flat_map(|w| [&w.from, &w.to])
        .collect::<std::collections::BTreeSet<_>>();
    let entities = entity_ids
        .into_iter()
        .map(|id| &index.entities[id])
        .collect::<Vec<_>>();
    Json(json!({"revision":revision,"page":page,"per_page":per_page,"total":ids.len(),"witnesses":witnesses,"entities":entities})).into_response()
}
async fn relations(
    State(state): State<Arc<AppState>>,
    Query(q): Query<RelationQuery>,
) -> Json<Value> {
    let report = state.report.read().unwrap();
    let selectors = |value: &str| {
        value
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    let options = RelationOptions {
        a: selectors(&q.a),
        b: selectors(&q.b),
        exclude_a: selectors(q.exclude_a.as_deref().unwrap_or("")),
        exclude_b: selectors(q.exclude_b.as_deref().unwrap_or("")),
        transitive_depth: q.transitive,
        path_mode: q.path_mode.unwrap_or_default(),
        max_paths_per_pair: q.max_paths_per_pair.unwrap_or(100),
        max_paths_total: q.max_paths_total.unwrap_or(10_000),
        work_budget: q.work_budget.unwrap_or(1_000_000),
    };
    let mut result = report
        .graph
        .relations_with_config(&options, &state.analyzer.config);
    let level = q.level.as_deref().unwrap_or("function");
    result.regroup_matrix(level);
    let mut value = serde_json::to_value(result)
        .unwrap_or_else(|_| json!({"a_to_b":[],"b_to_a":[],"matrix":{}}));
    value["level"] = json!(level);
    value["revision"] = json!(state.revision.load(Ordering::Acquire));
    value["vendor_unparsed_files"] = json!(report.deferred_vendor.len());
    if q.internal.unwrap_or(false) {
        for side in ["a", "b"] {
            let ids = value[side]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .collect::<std::collections::BTreeSet<_>>();
            let edges = report
                .graph
                .edges
                .iter()
                .filter(|edge| ids.contains(edge.from.as_ref()) && ids.contains(edge.to.as_ref()))
                .collect::<Vec<_>>();
            value[format!("internal_{side}")] = json!(edges);
        }
    }
    Json(value)
}
async fn expand_vendor(
    State(state): State<Arc<AppState>>,
    AxumPath(package): AxumPath<String>,
) -> Response {
    let _operation = state.operation_lock.lock().await;
    let analyzer = state.analyzer.clone();
    let root = state.root.clone();
    let state_for_task = state.clone();
    let package_for_task = package.clone();
    match tokio::task::spawn_blocking(move || {
        let mut report = state_for_task.report.read().unwrap().clone();
        analyzer.expand_vendor(&root, &mut report, &package_for_task)?;
        Ok::<_, analyzer_core::analysis::AnalysisError>(report)
    })
    .await
    {
        Ok(Ok(mut report)) => {
            let files = report
                .files
                .iter()
                .filter(|f| f.vendor && f.path.starts_with(&package))
                .count();
            let mut current_report = state.report.write().unwrap();
            *state.function_info.write().unwrap() = super::function_info::prepare(&mut report);
            *current_report = report;
            *state.watch_error.write().unwrap() = None;
            state.revision.fetch_add(1, Ordering::Release);
            drop(current_report);
            state
                .expanded_vendor
                .lock()
                .unwrap()
                .insert(package.clone());
            Json(json!({"package":package,"expanded":true,"files":files})).into_response()
        }
        Ok(Err(error)) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

#[cfg(test)]
mod ast_tests {
    use super::*;
    #[test]
    fn snapshots_track_used_vendor_configs_without_discovering_unrelated_ones() {
        let root = std::env::temp_dir().join(format!(
            "codefactor-vendor-config-watch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for package in ["used", "unrelated"] {
            std::fs::create_dir_all(root.join(format!("node_modules/{package}"))).unwrap();
            std::fs::write(
                root.join(format!("node_modules/{package}/tsconfig.json")),
                "{}",
            )
            .unwrap();
        }
        std::fs::write(root.join("app.ts"), "export function app() {}\n").unwrap();
        std::fs::write(
            root.join("node_modules/used/index.ts"),
            "export function run() {}\n",
        )
        .unwrap();
        let mut registry = analyzer_core::LanguageRegistry::new();
        registry.register(analyzer_typescript::TypeScript);
        let analyzer = Analyzer::new(registry, Default::default());
        let sources = vec!["app.ts".into(), "node_modules/used/index.ts".into()];
        let before = source_snapshot_for_paths(&root, &analyzer, &sources);
        assert!(before.contains_key(&root.join("node_modules/used/tsconfig.json")));
        assert!(!before.contains_key(&root.join("node_modules/unrelated/tsconfig.json")));
        std::fs::write(
            root.join("node_modules/unrelated/tsconfig.json"),
            "{\"extends\":\"missing\"}",
        )
        .unwrap();
        assert_eq!(
            source_snapshot_for_paths(&root, &analyzer, &sources),
            before
        );
        std::fs::write(
            root.join("node_modules/used/tsconfig.json"),
            "{\"compilerOptions\":{\"baseUrl\":\".\"}}",
        )
        .unwrap();
        assert_ne!(
            source_snapshot_for_paths(&root, &analyzer, &sources),
            before
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn watch_snapshot_tracks_requirements_for_targets_sharing_a_source_root() {
        let root = std::env::temp_dir().join(format!(
            "codefactor-cargo-shared-watch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let manifest = "[package]\nname='app'\nversion='0.1.0'\nautobins=false\n[lib]\npath='shared.rs'\n[[bin]]\nname='tool'\npath='shared.rs'\nrequired-features=['tool']\n[features]\ntool=[]\n";
        std::fs::write(root.join("Cargo.toml"), manifest).unwrap();
        std::fs::write(
            root.join("shared.rs"),
            "pub fn work(){}fn main(){work();}\n",
        )
        .unwrap();
        let mut config = analyzer_core::Config::default();
        config.cache.enabled = false;
        config.resolution.rust.features = Some(vec![]);
        let analyzer = Analyzer::new(crate::registry::registry(), config);
        let before = source_snapshot(&root, &analyzer, false);
        assert!(before.contains_key(&root.join("Cargo.toml")));
        assert_eq!(
            analyzer
                .scan(&root)
                .unwrap()
                .graph
                .select("shared::work")
                .len(),
            1
        );
        std::fs::write(
            root.join("Cargo.toml"),
            manifest.replace("required-features=['tool']", "required-features=[]"),
        )
        .unwrap();
        assert_ne!(source_snapshot(&root, &analyzer, false), before);
        assert_eq!(
            analyzer
                .scan(&root)
                .unwrap()
                .graph
                .select("shared::work")
                .len(),
            2
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn cargo_file_api_keeps_all_source_contexts_and_watches_ignored_dependency_manifests() {
        let root = std::env::temp_dir().join(format!(
            "codefactor-cargo-api-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("ignored/src")).unwrap();
        std::fs::write(root.join(".gitignore"), "ignored/\n").unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname='app'\nversion='0.1.0'\n[dependencies]\nbackend={path='ignored'}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("ignored/Cargo.toml"),
            "[package]\nname='backend'\nversion='0.1.0'\n",
        )
        .unwrap();
        std::fs::write(root.join("ignored/src/lib.rs"), "pub fn work(){}\n").unwrap();
        for entry in ["lib", "main"] {
            std::fs::write(root.join(format!("src/{entry}.rs")), "#[path=\"../shared.rs\"]mod shared;fn helper(){}fn flow(){shared::run();backend::work();}\n").unwrap();
        }
        std::fs::write(root.join("shared.rs"), "pub fn run(){crate::helper();}\n").unwrap();
        let mut config = analyzer_core::Config::default();
        config.cache.enabled = false;
        let analyzer = Arc::new(Analyzer::new(crate::registry::registry(), config));
        let mut report = analyzer.scan(&root).unwrap();
        assert!(!report
            .files
            .iter()
            .any(|file| file.path == "ignored/src/lib.rs"));
        assert!(report
            .resolution_diagnostics
            .iter()
            .any(|message| message.contains("no analyzed local library")));
        let info = super::super::function_info::prepare(&mut report);
        let state = Arc::new(AppState {
            analyzer,
            report: RwLock::new(report),
            root: std::fs::canonicalize(&root).unwrap(),
            single_file: false,
            revision: AtomicU64::new(0),
            expanded_vendor: Mutex::new(Default::default()),
            operation_lock: tokio::sync::Mutex::new(()),
            config_snapshot: Value::Null,
            watch_error: RwLock::new(None),
            function_info: RwLock::new(info),
            codemap_index: RwLock::new(None),
        });
        let response = file(
            State(state.clone()),
            Query(FileQuery {
                path: Some("shared.rs".into()),
                id: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["functions"].as_array().unwrap().len(), 1);
        let details = value["calls"][0]["resolved_target_details"]
            .as_array()
            .unwrap();
        assert_eq!(details.len(), 2);
        assert_eq!(
            details
                .iter()
                .filter_map(|node| node["cargo_target"].as_str())
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from(["src/lib.rs", "src/main.rs"])
        );
        let before = source_snapshot(&root, &state.analyzer, false);
        assert!(before.contains_key(&root.join("ignored/Cargo.toml")));
        std::fs::write(
            root.join("ignored/Cargo.toml"),
            "[package]\nname='backend'\nversion='0.2.0'\ndescription='changed metadata only'\n",
        )
        .unwrap();
        assert_ne!(source_snapshot(&root, &state.analyzer, false), before);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn watch_fixture(name: &str) -> (PathBuf, Arc<AppState>, String) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/bug-review-round2-web")
            .join(format!(
                "{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("copy.py");
        let body = (0..20)
            .map(|i| format!("    v{i} = x + {i}\n"))
            .collect::<String>()
            + "    return v19\n";
        std::fs::write(
            &source,
            format!("def first(x):\n{body}\ndef second(x):\n{body}"),
        )
        .unwrap();
        let updated = format!(
            "def first(x):\n{body}\ndef second(x):\n{}# updated\n",
            body.replace("x + 19", "x - 19")
        );
        let mut config = analyzer_core::Config::default();
        config.cache.enabled = false;
        let analyzer = Arc::new(Analyzer::new(crate::registry::registry(), config));
        let mut report = analyzer.scan(&source).unwrap();
        assert_eq!(report.architecture.warnings, 1);
        let info = super::super::function_info::prepare(&mut report);
        let state = Arc::new(AppState {
            analyzer,
            report: RwLock::new(report),
            root: std::fs::canonicalize(source).unwrap(),
            single_file: true,
            revision: AtomicU64::new(0),
            expanded_vendor: Mutex::new(Default::default()),
            operation_lock: tokio::sync::Mutex::new(()),
            config_snapshot: Value::Null,
            watch_error: RwLock::new(None),
            function_info: RwLock::new(info),
            codemap_index: RwLock::new(None),
        });
        (root, state, updated)
    }

    #[tokio::test]
    async fn first_watch_snapshot_reconciles_changes_after_the_initial_scan() {
        let (root, state, updated) = watch_fixture("startup-reconcile");
        // Edit after the initial report, before watch has ever sampled the source.
        std::fs::write(&state.root, updated).unwrap();
        let current = source_snapshot(&state.root, &state.analyzer, true);
        let mut last = None;
        assert!(reconcile_watch(&state, &mut last, current.clone()).await);
        assert_eq!(state.report.read().unwrap().architecture.warnings, 0);
        assert_eq!(state.revision.load(Ordering::Acquire), 1);
        assert_eq!(last.as_ref(), Some(&current));
        assert!(!reconcile_watch(&state, &mut last, current).await);
        assert_eq!(state.revision.load(Ordering::Acquire), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn five_language_file_api_and_watch_recompute_algorithmic_cost_without_sockets() {
        let root = std::env::temp_dir().join(format!(
            "codefactor-cost-watch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let sources = [
            ("cost.rs", include_str!("../tests/fixtures/cost/basic.rs")),
            ("cost.php", include_str!("../tests/fixtures/cost/basic.php")),
            ("cost.py", include_str!("../tests/fixtures/cost/basic.py")),
            ("cost.js", include_str!("../tests/fixtures/cost/basic.js")),
            ("cost.ts", include_str!("../tests/fixtures/cost/basic.ts")),
        ];
        for (path, source) in sources {
            std::fs::write(root.join(path), source).unwrap();
        }
        let mut config = analyzer_core::Config::default();
        config.cache.enabled = false;
        let analyzer = Arc::new(Analyzer::new(crate::registry::registry(), config));
        let mut report = analyzer.scan(&root).unwrap();
        let info = super::super::function_info::prepare(&mut report);
        let state = Arc::new(AppState {
            analyzer,
            report: RwLock::new(report),
            root: std::fs::canonicalize(&root).unwrap(),
            single_file: false,
            revision: AtomicU64::new(0),
            expanded_vendor: Mutex::new(Default::default()),
            operation_lock: tokio::sync::Mutex::new(()),
            config_snapshot: Value::Null,
            watch_error: RwLock::new(None),
            function_info: RwLock::new(info),
            codemap_index: RwLock::new(None),
        });
        let mut last = Some(source_snapshot(&state.root, &state.analyzer, false));
        for (path, source) in sources {
            let response = file(
                State(state.clone()),
                Query(FileQuery {
                    path: None,
                    id: Some(path.into()),
                }),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            let f = value["functions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["name"] == "quadratic")
                .unwrap();
            assert_eq!(
                f["algorithmic_complexity"]["formula"],
                if path.ends_with(".rs") || path.ends_with(".php") {
                    "Θ(n^2)"
                } else {
                    "O(n^2)"
                }
            );
            let changed = source
                .replace("for i in 0..n{linear(n);}", "linear(n);")
                .replace("for($i=0;$i<$n;$i++){linear($n);}", "linear($n);")
                .replace("for i in range(n): linear(n)", "linear(n)")
                .replace("for(let i=0;i<n;i++){linear(n);}", "linear(n);");
            assert_ne!(changed, source);
            std::fs::write(root.join(path), changed).unwrap();
            let current = source_snapshot(&state.root, &state.analyzer, false);
            assert!(reconcile_watch(&state, &mut last, current).await);
            let response = file(
                State(state.clone()),
                Query(FileQuery {
                    path: None,
                    id: Some(path.into()),
                }),
            )
            .await;
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            let f = value["functions"]
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
                "{path}"
            );
            let report = state.report.read().unwrap();
            let f = report
                .files
                .iter()
                .find(|f| f.path == path)
                .unwrap()
                .functions
                .iter()
                .find(|f| f.name == "quadratic")
                .unwrap();
            let language = &report
                .files
                .iter()
                .find(|file| file.path == path)
                .unwrap()
                .language;
            let id = format!("fn:{language}:{path}:{}", f.fqn);
            let node = report.graph.nodes.iter().find(|n| n.id == id).unwrap();
            assert!(Arc::ptr_eq(
                f.algorithmic_complexity.as_ref().unwrap(),
                node.algorithmic_complexity.as_ref().unwrap()
            ));
        }
        assert_eq!(state.revision.load(Ordering::Acquire), 5);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn failed_watch_reconciliation_keeps_the_committed_snapshot_for_retry() {
        let (root, state, updated) = watch_fixture("watch-retry");
        let previous = source_snapshot(&state.root, &state.analyzer, true);
        let mut last = Some(previous.clone());
        std::fs::write(&state.root, &updated).unwrap();
        let current = source_snapshot(&state.root, &state.analyzer, true);
        assert_ne!(current, previous);
        // A transient failure after sampling must not acknowledge this snapshot.
        std::fs::remove_file(&state.root).unwrap();
        assert!(!reconcile_watch(&state, &mut last, current.clone()).await);
        assert!(state.watch_error.read().unwrap().is_some());
        assert_eq!(last.as_ref(), Some(&previous));
        assert_eq!(state.revision.load(Ordering::Acquire), 0);
        assert_eq!(state.report.read().unwrap().architecture.warnings, 1);
        std::fs::write(&state.root, updated).unwrap();
        assert!(reconcile_watch(&state, &mut last, current.clone()).await);
        assert_eq!(last.as_ref(), Some(&current));
        assert!(state.watch_error.read().unwrap().is_none());
        assert_eq!(state.revision.load(Ordering::Acquire), 1);
        assert_eq!(state.report.read().unwrap().architecture.warnings, 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn file_call_targets_match_global_graph_with_nested_and_duplicate_functions() {
        let root =
            std::env::temp_dir().join(format!("codefactor-file-index-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("source.py"), "def outer():\n    def inner():\n        leaf()\n        missing()\n    inner()\n    leaf()\ndef leaf():\n    first()\ndef leaf():\n    second()\n").unwrap();
        std::fs::write(root.join("other.py"), "def unrelated():\n    missing()\n").unwrap();
        let mut config = analyzer_core::Config::default();
        config.cache.enabled = false;
        let analyzer = Arc::new(Analyzer::new(crate::registry::registry(), config));
        let report = analyzer.scan(&root).unwrap();
        let model = report.file("source.py").unwrap();
        let expected = model.functions.iter().flat_map(|f| &f.calls).map(|call| {
            let edges = report.graph.edges.iter().filter(|e| e.callsite.file == call.range.file && e.callsite.start_line == call.range.start_line && e.callsite.start_column == call.range.start_column).collect::<Vec<_>>();
            let details = edges.iter().filter_map(|e| report.graph.nodes.iter().rev().find(|n| n.id == e.to.as_ref())).map(|n| json!({"id":n.id,"label":n.label,"file":n.file,"line":n.line,"kind":format!("{:?}",n.kind).to_lowercase()})).collect::<Vec<_>>();
            json!({"name":call.name,"targets":call.targets,"resolved_targets":edges.iter().map(|e| &e.to).collect::<Vec<_>>(),"resolved_target_details":details,"confidence":call.confidence,"range":call.range,"file_id":model.path})
        }).collect::<Vec<_>>();
        let state = Arc::new(AppState {
            analyzer,
            report: RwLock::new(report),
            root: std::fs::canonicalize(&root).unwrap(),
            single_file: false,
            revision: AtomicU64::new(0),
            expanded_vendor: Mutex::new(std::collections::BTreeSet::new()),
            operation_lock: tokio::sync::Mutex::new(()),
            config_snapshot: Value::Null,
            watch_error: RwLock::new(None),
            function_info: RwLock::new(BTreeMap::new()),
            codemap_index: RwLock::new(None),
        });
        let response = file(
            State(state.clone()),
            Query(FileQuery {
                path: Some("source.py".into()),
                id: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let actual: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(actual["calls"], json!(expected));
        let Json(result) = search(State(state), Query(SearchQuery { q: "OUTER".into() })).await;
        assert_eq!(result["results"].as_array().unwrap().len(), 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn detailed_ast_is_requested_from_confined_source_without_report_storage() {
        let root = std::env::temp_dir().join(format!("codefactor-ast-api-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let source =
            "function outer(flag: boolean) { function inner() { if (flag) { sink(); } } inner(); }";
        std::fs::write(root.join("nested.ts"), source).unwrap();
        let mut config = analyzer_core::Config::default();
        config.cache.enabled = false;
        let analyzer = Arc::new(Analyzer::new(crate::registry::registry(), config));
        let report = analyzer.scan(&root).unwrap();
        assert!(report.files[0]
            .functions
            .iter()
            .all(|f| f.normalized_ast.is_none()));
        let expected = analyzer
            .registry
            .get("typescript")
            .unwrap()
            .parse(source, Path::new("nested.ts"));
        let state = Arc::new(AppState {
            analyzer,
            report: RwLock::new(report),
            root: std::fs::canonicalize(&root).unwrap(),
            single_file: false,
            revision: AtomicU64::new(0),
            expanded_vendor: Mutex::new(std::collections::BTreeSet::new()),
            operation_lock: tokio::sync::Mutex::new(()),
            config_snapshot: Value::Null,
            watch_error: RwLock::new(None),
            function_info: RwLock::new(BTreeMap::new()),
            codemap_index: RwLock::new(None),
        });
        let response = ast(
            State(state.clone()),
            Query(AstQuery {
                path: "nested.ts".into(),
                function: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let actual: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(actual, serde_json::to_value(expected).unwrap());
        let response = ast(
            State(state.clone()),
            Query(AstQuery {
                path: "nested.ts".into(),
                function: Some("inner".into()),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let actual: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(actual["node"]["kind"], "function");
        assert!(actual["node"]["children"].as_array().unwrap().len() > 1);
        let response = ast(
            State(state),
            Query(AstQuery {
                path: "../outside.ts".into(),
                function: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn large_violation_reports_are_filtered_and_bounded_before_serialization() {
        let root = std::env::temp_dir().join(format!("codefactor-pages-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("source.py"),
            "def caller():\n    leaf()\ndef leaf():\n    pass\n",
        )
        .unwrap();
        let mut registry = analyzer_core::LanguageRegistry::new();
        registry.register(analyzer_python::Python);
        let analyzer = Arc::new(Analyzer::new(registry, Default::default()));
        let mut report = analyzer.scan(&root).unwrap();
        report.violations = (0..100_000)
            .map(|i| analyzer_core::rules::Violation {
                rule: if i % 4 < 2 {
                    "function_lines"
                } else {
                    "function_params"
                }
                .into(),
                severity: if i % 2 == 0 {
                    analyzer_core::rules::Severity::Error
                } else {
                    analyzer_core::rules::Severity::Warn
                },
                file: format!("src/{}.py", i % 100),
                line: i + 1,
                entity: format!("f{i}"),
                value: Some(100.0),
                limit: Some(40.0),
                scope: "function".into(),
                algorithmic_complexity: None,
                architecture: None,
                module_cycle: None,
            })
            .collect();
        let state = Arc::new(AppState {
            analyzer,
            report: RwLock::new(report),
            root: root.clone(),
            single_file: false,
            revision: AtomicU64::new(0),
            expanded_vendor: Mutex::new(Default::default()),
            operation_lock: tokio::sync::Mutex::new(()),
            config_snapshot: Value::Null,
            watch_error: RwLock::new(None),
            function_info: RwLock::new(BTreeMap::new()),
            codemap_index: RwLock::new(None),
        });
        let Json(page) = violations(
            State(state.clone()),
            Query(ViolationsQuery {
                per_page: Some(100_000),
                ..Default::default()
            }),
        )
        .await;
        assert_eq!(page["total"], 100_000);
        assert_eq!(page["pages"], 500);
        assert_eq!(page["violations"].as_array().unwrap().len(), 200);
        assert!(page["violations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["severity"] == "error"));
        let Json(filtered) = violations(
            State(state.clone()),
            Query(ViolationsQuery {
                page: Some(2),
                severity: Some("warn".into()),
                rule: Some("function_params".into()),
                ..Default::default()
            }),
        )
        .await;
        assert_eq!(filtered["total"], 25_000);
        assert_eq!(filtered["page"], 2);
        assert_eq!(filtered["violations"].as_array().unwrap().len(), 50);
        assert!(filtered["violations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["severity"] == "warn" && v["rule"] == "function_params"));
        let Json(found) = violations(
            State(state.clone()),
            Query(ViolationsQuery {
                q: Some("f99999".into()),
                page: Some(99),
                ..Default::default()
            }),
        )
        .await;
        assert_eq!(found["total"], 1);
        assert_eq!(found["page"], 1);
        assert_eq!(found["violations"][0]["entity"], "f99999");
        let Json(files) = tree(State(state), Query(TreeQuery::default())).await;
        assert_eq!(files["children"][0]["kind"], "file");
        assert_eq!(
            files["children"][0]["children"].as_array().unwrap().len(),
            2
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
