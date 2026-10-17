use analyzer_core::{
    graph::{Direction, GraphFilter},
    Analyzer, Confidence, Config, LanguageRegistry,
};
use std::{collections::BTreeSet, path::PathBuf};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
                "codefactor-cargo-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )))
    }
    fn write(&self, path: &str, source: &str) {
        let path = self.0.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }
    fn scan(&self, config: Config) -> analyzer_core::AnalysisReport {
        let mut registry = LanguageRegistry::new();
        registry.register(analyzer_rust::Rust);
        Analyzer::new(registry, config).scan(&self.0).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn config() -> Config {
    let mut config = Config::default();
    config.cache.enabled = false;
    config.architecture.enabled = false;
    config.resolution.rust.features = Some(vec![]);
    config.resolution.rust.cfg = Some(vec![]);
    config.resolution.rust.target = Some("x86_64-unknown-linux-gnu".into());
    config
}
fn targets(report: &analyzer_core::AnalysisReport, caller: &str) -> Vec<(String, Confidence)> {
    let caller = report
        .graph
        .nodes
        .iter()
        .find(|node| node.label == caller)
        .unwrap_or_else(|| panic!("missing {caller}: {:?}", report.graph.nodes));
    report
        .graph
        .outgoing_edges(&caller.id)
        .map(|edge| {
            (
                report.graph.node(&edge.to).unwrap().label.clone(),
                edge.confidence.clone(),
            )
        })
        .collect()
}
#[test]
fn path_dependencies_use_rename_keys_and_custom_library_names() {
    let f = Fixture::new();
    f.write(
        "Cargo.toml",
        "[workspace]\nmembers=['backend','renamed','normal']\n",
    );
    f.write("backend/Cargo.toml", "[package]\nname='engine-pkg'\nversion='0.1.0'\n[lib]\nname='wire_api'\npath='engine/start.rs'\n");
    f.write("backend/engine/start.rs", "pub mod child;pub struct Client;impl Client{pub fn run(&self){child::work();}}pub fn work(){}\n");
    f.write("backend/engine/child.rs", "pub fn work(){}\n");
    f.write("renamed/Cargo.toml", "[package]\nname='renamed'\nversion='0.1.0'\n[dependencies]\nstore={package='engine-pkg',path='../backend'}\n");
    f.write(
        "renamed/src/lib.rs",
        "use store::child::work as run;fn flow(c:&store::Client){run();store::work();c.run();}\n",
    );
    f.write("normal/Cargo.toml", "[package]\nname='normal'\nversion='0.1.0'\n[dependencies]\nengine-pkg={path='../backend'}\n");
    f.write(
        "normal/src/main.rs",
        "fn flow(){wire_api::child::work();wire_api::work();}\n",
    );
    let report = f.scan(config());
    let renamed = targets(&report, "renamed::src::lib::flow");
    for target in [
        "backend::engine::child::work",
        "backend::engine::start::work",
        "backend::engine::start::Client::run",
    ] {
        assert!(
            renamed.contains(&(target.into(), Confidence::Resolved)),
            "{renamed:?}"
        );
    }
    let normal = targets(&report, "normal::src::main::flow");
    assert_eq!(normal.len(), 2);
    assert!(
        normal
            .iter()
            .all(|(_, confidence)| *confidence == Confidence::Resolved),
        "{normal:?}"
    );
    assert!(
        report.resolution_diagnostics.is_empty(),
        "{:?}",
        report.resolution_diagnostics
    );
    let context = analyzer_core::resolution::ResolutionContext::load(&f.0, &config());
    assert!(context
        .metadata_paths
        .contains(&f.0.join("backend/Cargo.toml")));
}
#[test]
fn workspace_inherited_dependency_paths_keep_workspace_origin_and_optional_gating() {
    let f = Fixture::new();
    f.write("Cargo.toml", "[workspace]\nmembers=['crates/backend','crates/app']\n[workspace.dependencies]\nstore={package='backend',path='crates/backend'}\n");
    f.write(
        "crates/backend/Cargo.toml",
        "[package]\nname='backend'\nversion='0.1.0'\n",
    );
    f.write("crates/backend/src/lib.rs", "pub fn work(){}\n");
    f.write("crates/app/Cargo.toml", "[package]\nname='app'\nversion='0.1.0'\n[dependencies]\nstore={workspace=true,optional=true}\n[features]\nfast=['dep:store']\n");
    f.write(
        "crates/app/src/lib.rs",
        "#[cfg(feature=\"fast\")]fn flow(){store::work();}\n",
    );
    let off = f.scan(config());
    assert!(off
        .graph
        .nodes
        .iter()
        .all(|node| node.label != "crates::app::src::lib::flow"));
    let mut cfg = config();
    cfg.resolution.rust.features = Some(vec!["fast".into()]);
    let on = f.scan(cfg);
    assert_eq!(
        targets(&on, "crates::app::src::lib::flow"),
        vec![(
            "crates::backend::src::lib::work".into(),
            Confidence::Resolved
        )]
    );
}
#[test]
fn target_dependencies_need_known_inputs_and_select_the_matching_local_package() {
    let f = Fixture::new();
    f.write("app/Cargo.toml", "[package]\nname='app'\nversion='0.1.0'\n[target.'cfg(windows)'.dependencies]\napi={package='windows-api',path='../windows'}\n[target.'cfg(unix)'.dependencies]\napi={package='unix-api',path='../unix'}\n");
    for (directory, name) in [("windows", "windows-api"), ("unix", "unix-api")] {
        f.write(
            &format!("{directory}/Cargo.toml"),
            &format!("[package]\nname='{name}'\nversion='0.1.0'\n"),
        );
        f.write(&format!("{directory}/src/lib.rs"), "pub fn work(){}\n");
    }
    f.write("app/src/lib.rs", "fn flow(){api::work();}\n");
    let report = f.scan(config());
    assert_eq!(
        targets(&report, "app::src::lib::flow"),
        vec![("unix::src::lib::work".into(), Confidence::Resolved)]
    );
    let mut cfg = config();
    cfg.resolution.rust.target = None;
    let unknown = f.scan(cfg);
    assert!(targets(&unknown, "app::src::lib::flow")
        .iter()
        .all(|(_, confidence)| *confidence != Confidence::Resolved));
    assert!(unknown
        .resolution_diagnostics
        .iter()
        .any(|diagnostic| diagnostic.contains("target-specific Cargo dependency")));
}
#[test]
fn unavailable_dependencies_never_bind_an_unrelated_discovered_filename() {
    let f = Fixture::new();
    f.write("Cargo.toml", "[package]\nname='app'\nversion='0.1.0'\n[dependencies]\nexternal='1'\nmissing={path='missing'}\n");
    f.write(
        "src/lib.rs",
        "fn flow(){external::work();missing::work();}\n",
    );
    f.write("external.rs", "pub fn work(){}\n");
    let report = f.scan(config());
    assert!(targets(&report, "src::lib::flow")
        .iter()
        .all(|(_, confidence)| *confidence != Confidence::Resolved));
    assert!(
        report
            .resolution_diagnostics
            .iter()
            .any(|message| message.contains("external or has no analyzed local library")),
        "{:?}",
        report.resolution_diagnostics
    );
    assert!(
        report
            .resolution_diagnostics
            .iter()
            .any(|message| message.contains("missing, unreadable or outside project")),
        "{:?}",
        report.resolution_diagnostics
    );
}
#[test]
fn shared_lib_and_bin_sources_get_separate_graph_contexts_and_one_source_metric() {
    let f = Fixture::new();
    f.write("Cargo.toml", "[package]\nname='app'\nversion='0.1.0'\n[[bin]]\nname='tool'\npath='runner/entry.rs'\nrequired-features=['tool']\n[features]\ntool=[]\n");
    f.write("src/lib.rs", "#[path=\"../shared.rs\"]mod shared;fn private_helper(n:usize){for i in 0..n{}}fn flow(n:usize){shared::run(n);}\n");
    f.write("runner/entry.rs", "#[path=\"../shared.rs\"]mod shared;fn private_helper(n:usize){for i in 0..n{for j in 0..n{}}}fn flow(n:usize){shared::run(n);}\n");
    f.write(
        "shared.rs",
        "pub fn run(n:usize){crate::private_helper(n);}\n",
    );
    let mut cfg = config();
    cfg.resolution.rust.features = Some(vec!["tool".into()]);
    let report = f.scan(cfg.clone());
    let shared = report
        .graph
        .nodes
        .iter()
        .filter(|node| node.source_fqn.as_deref() == Some("shared::run"))
        .collect::<Vec<_>>();
    assert_eq!(shared.len(), 2, "{:?}", report.graph.nodes);
    for (caller, root) in [
        ("src::lib::flow", "src/lib.rs"),
        ("runner::entry::flow", "runner/entry.rs"),
    ] {
        let selected = targets(&report, caller);
        assert_eq!(selected.len(), 1, "{selected:?}");
        let node = report
            .graph
            .nodes
            .iter()
            .find(|node| node.label == selected[0].0)
            .unwrap();
        assert_eq!(node.cargo_target.as_deref(), Some(root));
        let expected = if root == "src/lib.rs" {
            "src::lib::private_helper"
        } else {
            "runner::entry::private_helper"
        };
        assert_eq!(
            targets(&report, &node.label),
            vec![(expected.into(), Confidence::Resolved)]
        );
        let query = report.graph.query(
            caller,
            Direction::Callees,
            None,
            &GraphFilter::default(),
            &cfg,
        );
        let labels = query
            .nodes
            .iter()
            .map(|node| node.label.as_str())
            .collect::<BTreeSet<_>>();
        assert!(labels.contains(expected));
        let excluded = if root == "src/lib.rs" {
            "runner::entry::private_helper"
        } else {
            "src::lib::private_helper"
        };
        assert!(!labels.contains(excluded), "{labels:?}");
    }
    assert_eq!(
        report
            .files
            .iter()
            .filter(|file| file.path == "shared.rs")
            .count(),
        1
    );
    assert_eq!(
        report
            .metrics
            .iter()
            .filter(|metric| metric.path.as_ref() == "shared.rs"
                && metric.id.as_ref() == "function_lines")
            .count(),
        1
    );
    let function = report
        .files
        .iter()
        .find(|file| file.path == "shared.rs")
        .unwrap()
        .functions
        .iter()
        .find(|function| function.name == "run")
        .unwrap();
    let estimate = function.algorithmic_complexity.as_ref().unwrap();
    assert_eq!(
        estimate.status,
        analyzer_core::algorithmic::EstimateStatus::Unknown
    );
    assert!(
        estimate
            .unknown_reasons
            .iter()
            .any(|reason| reason.contains("multiple Cargo targets")),
        "{estimate:?}"
    );
    assert!(shared
        .iter()
        .all(|node| node.algorithmic_complexity.is_some()));
    let off = f.scan(config());
    assert_eq!(
        off.graph
            .nodes
            .iter()
            .filter(|node| node.source_fqn.as_deref() == Some("shared::run"))
            .count(),
        1
    );
    assert!(off
        .graph
        .nodes
        .iter()
        .all(|node| node.cargo_target.as_deref() != Some("runner/entry.rs")));
}
#[test]
fn binary_only_shared_descendants_disappear_when_required_features_are_off() {
    let f = Fixture::new();
    f.write("Cargo.toml", "[package]\nname='app'\nversion='0.1.0'\nautobins=false\n[[bin]]\nname='tool'\npath='runner/entry.rs'\nrequired-features=['tool']\n[features]\ntool=[]\n");
    f.write("runner/entry.rs", "mod child;fn flow(){child::work();}\n");
    f.write("runner/child.rs", "pub fn work(){}\n");
    let off = f.scan(config());
    assert!(
        off.graph
            .nodes
            .iter()
            .all(|node| node.file.as_deref() != Some("runner/child.rs")),
        "{:?}",
        off.graph.nodes
    );
    assert!(off.files.iter().any(|file| file.path == "runner/child.rs"));
    let mut cfg = config();
    cfg.resolution.rust.features = Some(vec!["tool".into()]);
    assert_eq!(
        targets(&f.scan(cfg), "runner::entry::flow"),
        vec![("runner::child::work".into(), Confidence::Resolved)]
    );
}

#[test]
fn same_package_binary_uses_library_crate_name_and_local_modules_shadow_dependencies() {
    let f = Fixture::new();
    f.write("Cargo.toml", "[package]\nname='app-pkg'\nversion='0.1.0'\nautolib=false\n[lib]\nname='app_api'\n[dependencies]\napi={package='backend',path='backend'}\n");
    f.write(
        "src/lib.rs",
        "pub fn work(){}mod api{pub fn work(){}}fn local(){api::work();}\n",
    );
    f.write("src/main.rs", "fn flow(){app_api::work();api::work();}\n");
    f.write(
        "backend/Cargo.toml",
        "[package]\nname='backend'\nversion='0.1.0'\n",
    );
    f.write("backend/src/lib.rs", "pub fn work(){}\n");
    let report = f.scan(config());
    assert_eq!(
        targets(&report, "src::lib::local"),
        vec![("src::lib::api::work".into(), Confidence::Resolved)]
    );
    let binary = targets(&report, "src::main::flow");
    assert_eq!(binary.len(), 2);
    for target in ["src::lib::work", "backend::src::lib::work"] {
        assert!(
            binary.contains(&(target.into(), Confidence::Resolved)),
            "{binary:?}"
        );
    }
}

#[test]
fn cargo_manifest_changes_rebuild_target_bindings_while_source_cache_is_reused() {
    let f = Fixture::new();
    f.write("Cargo.toml", "[package]\nname='app'\nversion='0.1.0'\n[dependencies]\napi={package='backend',path='backend'}\n");
    f.write("src/lib.rs", "fn flow(){api::work();}\n");
    f.write(
        "backend/Cargo.toml",
        "[package]\nname='backend'\nversion='0.1.0'\n[lib]\npath='src/first.rs'\n",
    );
    f.write("backend/src/first.rs", "pub fn work(){}\n");
    f.write("backend/src/second.rs", "pub fn work(){}\n");
    let mut cfg = config();
    cfg.cache.enabled = true;
    let first = f.scan(cfg.clone());
    assert_eq!(
        targets(&first, "src::lib::flow"),
        vec![("backend::src::first::work".into(), Confidence::Resolved)]
    );
    f.write(
        "backend/Cargo.toml",
        "[package]\nname='backend'\nversion='0.1.0'\n[lib]\npath='src/second.rs'\n",
    );
    let second = f.scan(cfg);
    assert_eq!(
        targets(&second, "src::lib::flow"),
        vec![("backend::src::second::work".into(), Confidence::Resolved)]
    );
    assert_eq!(second.summary.cache_hits, second.files.len());
    assert!(second
        .files
        .iter()
        .all(|file| file.cargo_target.is_none() && file.cargo_source_module.is_none()));
}

#[test]
fn equivalent_shared_contexts_share_cost_and_physical_source_selectors_select_all_targets() {
    let f = Fixture::new();
    f.write("Cargo.toml", "[package]\nname='app'\nversion='0.1.0'\n");
    f.write(
        "src/lib.rs",
        "#[path=\"../shared.rs\"]mod shared;fn flow(n:usize){shared::Worker::run(n);}\n",
    );
    f.write(
        "src/main.rs",
        "#[path=\"../shared.rs\"]mod shared;fn flow(n:usize){shared::Worker::run(n);}\n",
    );
    f.write(
        "shared.rs",
        "pub struct Worker;impl Worker{pub fn run(n:usize){for i in 0..n{}}}\n",
    );
    let cfg = config();
    let report = f.scan(cfg.clone());
    let selector = "fn:rust:shared.rs:shared::Worker::run";
    let roots = report.graph.select(selector);
    assert_eq!(roots.len(), 2);
    assert_eq!(report.graph.select("shared::Worker::run"), roots);
    assert_eq!(report.graph.select("class:shared::Worker"), roots);
    for root in roots {
        let node = report.graph.node(&root).unwrap();
        let cost = node.algorithmic_complexity.as_ref().unwrap();
        assert_eq!(cost.formula.as_deref(), Some("Θ(n)"), "{cost:?}");
        assert!(cost.unknown_reasons.is_empty(), "{cost:?}");
    }
    let query = report.graph.query(
        selector,
        Direction::Both,
        None,
        &GraphFilter::default(),
        &cfg,
    );
    assert_eq!(query.roots.len(), 2);
    for caller in ["src::lib::flow", "src::main::flow"] {
        let target = targets(&report, caller);
        assert_eq!(target.len(), 1);
        assert_eq!(target[0].1, Confidence::Resolved);
    }
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(
        json["graph"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|node| node["source_fqn"] == "shared::Worker::run")
            .count(),
        2
    );
    assert_eq!(
        report
            .files
            .iter()
            .find(|file| file.path == "shared.rs")
            .unwrap()
            .functions[0]
            .algorithmic_complexity
            .as_ref()
            .unwrap()
            .formula
            .as_deref(),
        Some("Θ(n)")
    );
}

#[test]
fn local_dependency_feature_requests_and_defaults_follow_manifest_changes_with_cached_ir() {
    let f = Fixture::new();
    f.write("Cargo.toml", "[package]\nname='app'\nversion='0.1.0'\n[dependencies]\napi={package='backend',path='backend',default-features=false,features=['leaf']}\n");
    f.write("src/lib.rs", "fn flow(){api::leaf();api::base();}\n");
    f.write("backend/Cargo.toml", "[package]\nname='backend'\nversion='0.1.0'\n[features]\ndefault=['base']\nbase=[]\nleaf=[]\n");
    f.write(
        "backend/src/lib.rs",
        "#[cfg(feature=\"leaf\")]pub fn leaf(){}#[cfg(feature=\"base\")]pub fn base(){}\n",
    );
    let mut cfg = config();
    cfg.cache.enabled = true;
    let first = f.scan(cfg.clone());
    assert!(targets(&first, "src::lib::flow")
        .contains(&("backend::src::lib::leaf".into(), Confidence::Resolved)));
    assert!(!first
        .graph
        .nodes
        .iter()
        .any(|node| node.label == "backend::src::lib::base"));
    f.write("Cargo.toml", "[package]\nname='app'\nversion='0.1.0'\n[dependencies]\napi={package='backend',path='backend'}\n");
    let second = f.scan(cfg);
    assert!(targets(&second, "src::lib::flow")
        .contains(&("backend::src::lib::base".into(), Confidence::Resolved)));
    assert!(!second
        .graph
        .nodes
        .iter()
        .any(|node| node.label == "backend::src::lib::leaf"));
    assert_eq!(second.summary.cache_hits, second.files.len());
}

#[test]
fn same_name_explicit_feature_does_not_activate_optional_dependency() {
    let f = Fixture::new();
    f.write("Cargo.toml", "[package]\nname='app'\nversion='0.1.0'\n[dependencies]\nstore={package='backend',path='backend',optional=true,default-features=false,features=['leaf']}\n[features]\nstore=[]\nactivate=['dep:store']\n");
    f.write("src/lib.rs", "fn flow(){store::leaf();}\n");
    f.write(
        "backend/Cargo.toml",
        "[package]\nname='backend'\nversion='0.1.0'\n[features]\nleaf=[]\n",
    );
    f.write(
        "backend/src/lib.rs",
        "#[cfg(feature=\"leaf\")]pub fn leaf(){}\n",
    );
    let mut cfg = config();
    cfg.resolution.rust.features = Some(vec!["store".into()]);
    let inactive = f.scan(cfg.clone());
    assert!(!targets(&inactive, "src::lib::flow")
        .iter()
        .any(|(_, confidence)| *confidence == Confidence::Resolved));
    f.write(
        "backend/Cargo.toml",
        "[package]\nname='backend'\nversion='0.1.0'\n[features]\nleaf=[]\nstore=[]\n",
    );
    let inactive = f.scan(cfg.clone());
    assert!(!inactive
        .graph
        .nodes
        .iter()
        .any(|node| node.label == "backend::src::lib::leaf"));
    assert!(!targets(&inactive, "src::lib::flow")
        .iter()
        .any(|(_, confidence)| *confidence == Confidence::Resolved));
    cfg.resolution.rust.features = Some(vec!["activate".into()]);
    assert!(targets(&f.scan(cfg.clone()), "src::lib::flow")
        .contains(&("backend::src::lib::leaf".into(), Confidence::Resolved)));
    f.write("Cargo.toml", "[package]\nname='app'\nversion='0.1.0'\n[dependencies]\nstore={package='backend',path='backend',optional=true,default-features=false,features=['leaf']}\n");
    cfg.resolution.rust.features = Some(vec!["store".into()]);
    assert!(targets(&f.scan(cfg), "src::lib::flow")
        .contains(&("backend::src::lib::leaf".into(), Confidence::Resolved)));
}

#[test]
fn strong_and_weak_forwarding_use_dependency_keys_and_weak_requests_do_not_activate() {
    let f = Fixture::new();
    f.write("Cargo.toml", "[package]\nname='app'\nversion='0.1.0'\n[dependencies]\nstore={package='backend',path='backend',optional=true,default-features=false,features=['leaf']}\n[features]\nweak=['store?/weakleaf']\nstrong=['store/secondary','store?/weakleaf']\nactivate=['dep:store','store?/weakleaf']\n");
    f.write(
        "src/lib.rs",
        "fn flow(){store::work();store::leaf();store::secondary();store::weakleaf();}\n",
    );
    f.write("backend/Cargo.toml", "[package]\nname='backend'\nversion='0.1.0'\n[features]\nleaf=[]\nsecondary=[]\nweakleaf=[]\n");
    f.write("backend/src/lib.rs", "pub fn work(){}#[cfg(feature=\"leaf\")]pub fn leaf(){}#[cfg(feature=\"secondary\")]pub fn secondary(){}#[cfg(feature=\"weakleaf\")]pub fn weakleaf(){}\n");
    let mut cfg = config();
    cfg.resolution.rust.features = Some(vec!["weak".into()]);
    let weak = f.scan(cfg.clone());
    assert!(targets(&weak, "src::lib::flow")
        .iter()
        .all(|(_, confidence)| *confidence != Confidence::Resolved));
    cfg.resolution.rust.features = Some(vec!["strong".into()]);
    let strong = f.scan(cfg.clone());
    for function in ["work", "leaf", "secondary", "weakleaf"] {
        assert!(
            targets(&strong, "src::lib::flow").contains(&(
                format!("backend::src::lib::{function}"),
                Confidence::Resolved
            )),
            "{:?}",
            strong.resolution_diagnostics
        );
    }
    assert!(
        !strong
            .resolution_diagnostics
            .iter()
            .any(|message| message.contains("propagation is not linked")),
        "{:?}",
        strong.resolution_diagnostics
    );
    cfg.resolution.rust.features = Some(vec!["activate".into()]);
    let active = f.scan(cfg);
    assert!(targets(&active, "src::lib::flow")
        .contains(&("backend::src::lib::weakleaf".into(), Confidence::Resolved)));
    assert!(!active
        .graph
        .nodes
        .iter()
        .any(|node| node.label == "backend::src::lib::secondary"));
}

#[test]
fn inherited_feature_requests_are_additive_and_local_dependency_cycles_reach_a_fixed_point() {
    let f = Fixture::new();
    f.write("Cargo.toml", "[workspace]\nmembers=['app','backend','nested']\n[workspace.dependencies]\napi={package='backend',path='backend',default-features=false,features=['inherited']}\n");
    f.write("app/Cargo.toml", "[package]\nname='app'\nversion='0.1.0'\n[dependencies]\napi={workspace=true,features=['member','relay']}\n");
    f.write(
        "app/src/lib.rs",
        "fn flow(){api::inherited();api::member();api::relay();}\n",
    );
    f.write("backend/Cargo.toml", "[package]\nname='backend'\nversion='0.1.0'\n[dependencies]\nnext={package='nested',path='../nested',optional=true,default-features=false,features=['leaf']}\n[features]\ninherited=[]\nmember=[]\nrelay=['dep:next']\n");
    f.write("backend/src/lib.rs", "#[cfg(feature=\"inherited\")]pub fn inherited(){}#[cfg(feature=\"member\")]pub fn member(){}#[cfg(feature=\"relay\")]pub fn relay(){next::work();}\n");
    f.write("nested/Cargo.toml", "[package]\nname='nested'\nversion='0.1.0'\n[dependencies]\nback={package='backend',path='../backend',default-features=false,features=['member']}\n[features]\nleaf=[]\n");
    f.write(
        "nested/src/lib.rs",
        "#[cfg(feature=\"leaf\")]pub fn work(){}\n",
    );
    let report = f.scan(config());
    assert!(targets(&report, "app::src::lib::flow")
        .iter()
        .all(|(_, confidence)| *confidence == Confidence::Resolved));
    assert_eq!(
        targets(&report, "backend::src::lib::relay"),
        vec![("nested::src::lib::work".into(), Confidence::Resolved)]
    );
    assert!(
        report.resolution_diagnostics.is_empty(),
        "{:?}",
        report.resolution_diagnostics
    );
}

#[test]
fn disabled_binary_requests_do_not_enable_dependency_features_and_invalid_requests_stay_uncertain()
{
    let f = Fixture::new();
    f.write("tool/Cargo.toml", "[package]\nname='tool'\nversion='0.1.0'\nautobins=false\n[[bin]]\nname='tool'\npath='runner.rs'\nrequired-features=['tool']\n[features]\ntool=[]\n[dependencies]\napi={package='backend',path='../backend',default-features=false,features=['leaf']}\n");
    f.write("tool/runner.rs", "fn flow(){api::leaf();}\n");
    f.write(
        "backend/Cargo.toml",
        "[package]\nname='backend'\nversion='0.1.0'\n[features]\nleaf=[]\n",
    );
    f.write(
        "backend/src/lib.rs",
        "#[cfg(feature=\"leaf\")]pub fn leaf(){}\n",
    );
    let off = f.scan(config());
    assert!(!off
        .graph
        .nodes
        .iter()
        .any(|node| node.label == "backend::src::lib::leaf"));
    let mut cfg = config();
    cfg.resolution.rust.features = Some(vec!["tool".into()]);
    let on = f.scan(cfg);
    assert_eq!(
        targets(&on, "tool::runner::flow"),
        vec![("backend::src::lib::leaf".into(), Confidence::Resolved)]
    );
    f.write("tool/Cargo.toml", "[package]\nname='tool'\nversion='0.1.0'\n[[bin]]\nname='tool'\npath='runner.rs'\n[dependencies]\napi={package='backend',path='../backend',default-features=false,features=['typo']}\n");
    let invalid = f.scan(config());
    assert!(targets(&invalid, "tool::runner::flow")
        .iter()
        .all(|(_, confidence)| *confidence != Confidence::Resolved));
    assert!(invalid.resolution_diagnostics.iter().any(|message|message.contains("local dependency feature requests cannot be resolved")),"{:?}",invalid.resolution_diagnostics);
}

#[test]
fn local_request_unions_preserve_defaults_requested_by_another_active_package() {
    let f = Fixture::new();
    for (app, request) in [
        ("first", ",default-features=false,features=['leaf']"),
        ("second", ""),
    ] {
        f.write(&format!("{app}/Cargo.toml"),&format!("[package]\nname='{app}'\nversion='0.1.0'\n[dependencies]\napi={{package='backend',path='../backend'{request}}}\n"));
        f.write(
            &format!("{app}/src/lib.rs"),
            "fn flow(){api::base();api::leaf();}\n",
        );
    }
    f.write("backend/Cargo.toml","[package]\nname='backend'\nversion='0.1.0'\n[features]\ndefault=['base']\nbase=[]\nleaf=[]\n");
    f.write(
        "backend/src/lib.rs",
        "#[cfg(feature=\"base\")]pub fn base(){}#[cfg(feature=\"leaf\")]pub fn leaf(){}\n",
    );
    let report = f.scan(config());
    for app in ["first", "second"] {
        assert!(targets(&report, &format!("{app}::src::lib::flow"))
            .iter()
            .all(|(_, confidence)| *confidence == Confidence::Resolved));
    }
}

#[test]
fn unknown_target_requests_and_version_dependent_workspace_defaults_do_not_certify_features() {
    let f = Fixture::new();
    f.write("Cargo.toml","[workspace]\nmembers=['app','backend']\n[workspace.dependencies]\napi={package='backend',path='backend'}\n");
    f.write("app/Cargo.toml","[package]\nname='app'\nversion='0.1.0'\n[dependencies]\napi={workspace=true,default-features=false}\n");
    f.write("app/src/lib.rs", "fn flow(){api::base();}\n");
    f.write(
        "backend/Cargo.toml",
        "[package]\nname='backend'\nversion='0.1.0'\n[features]\ndefault=['base']\nbase=[]\n",
    );
    f.write(
        "backend/src/lib.rs",
        "#[cfg(feature=\"base\")]pub fn base(){}\n",
    );
    let version = f.scan(config());
    assert!(targets(&version, "app::src::lib::flow")
        .iter()
        .all(|(_, confidence)| *confidence != Confidence::Resolved));
    assert!(version
        .resolution_diagnostics
        .iter()
        .any(|message| message.contains("version-dependent workspace override")));
    f.write("app/Cargo.toml","[package]\nname='app'\nversion='0.1.0'\n[target.'cfg(unix)'.dependencies]\napi={package='backend',path='../backend',features=['base'],default-features=false}\n");
    let mut cfg = config();
    cfg.resolution.rust.target = None;
    let unknown = f.scan(cfg);
    assert!(targets(&unknown, "app::src::lib::flow")
        .iter()
        .all(|(_, confidence)| *confidence != Confidence::Resolved));
    assert!(unknown
        .resolution_diagnostics
        .iter()
        .any(|message| message.contains("target-specific Cargo dependency")));
}

#[test]
fn custom_library_root_outside_package_directory_keeps_its_manifest_feature_owner() {
    let f = Fixture::new();
    f.write("app/Cargo.toml","[package]\nname='app'\nversion='0.1.0'\n[dependencies]\napi={package='backend',path='../backend',features=['leaf'],default-features=false}\n");
    f.write("app/src/lib.rs", "fn flow(){api::leaf();}\n");
    f.write("backend/Cargo.toml","[package]\nname='backend'\nversion='0.1.0'\n[lib]\npath='../common.rs'\n[features]\nleaf=[]\n");
    f.write("common.rs", "#[cfg(feature=\"leaf\")]pub fn leaf(){}\n");
    let report = f.scan(config());
    assert_eq!(
        targets(&report, "app::src::lib::flow"),
        vec![("common::leaf".into(), Confidence::Resolved)]
    );
    assert!(
        report.resolution_diagnostics.is_empty(),
        "{:?}",
        report.resolution_diagnostics
    );
}

#[test]
fn malformed_package_metadata_cannot_crash_dependency_feature_linking_or_bind_a_library() {
    let f = Fixture::new();
    f.write("Cargo.toml","[package]\nname='app'\nversion='0.1.0'\n[dependencies]\napi={package='backend',path='backend',features=['leaf']}\n");
    f.write("src/lib.rs", "fn flow(){api::work();}\n");
    f.write(
        "backend/Cargo.toml",
        "[package]\nversion='0.1.0'\n[features]\nleaf=[]\n",
    );
    f.write("backend/src/lib.rs", "pub fn work(){}\n");
    let report = f.scan(config());
    assert!(targets(&report, "src::lib::flow")
        .iter()
        .all(|(_, confidence)| *confidence != Confidence::Resolved));
    assert!(report
        .resolution_diagnostics
        .iter()
        .any(|message| message.contains("Cargo package name is missing or invalid")));
}
