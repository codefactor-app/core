use analyzer_core::{
    graph::{Direction, GraphFilter},
    Analyzer, Confidence, Config, LanguageRegistry,
};
use std::{collections::BTreeSet, path::PathBuf};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
                "codefactor-shared-root-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )))
    }
    fn write(&self, path: &str, text: &str) {
        let path = self.0.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    fn scan(&self, features: &[&str], cache: bool) -> analyzer_core::AnalysisReport {
        self.scan_with_features(Some(features), cache)
    }
    fn scan_with_features(
        &self,
        features: Option<&[&str]>,
        cache: bool,
    ) -> analyzer_core::AnalysisReport {
        let mut registry = LanguageRegistry::new();
        registry.register(analyzer_rust::Rust);
        let mut cfg = Config::default();
        cfg.cache.enabled = cache;
        cfg.architecture.enabled = false;
        cfg.resolution.rust.features =
            features.map(|features| features.iter().map(|s| s.to_string()).collect());
        cfg.resolution.rust.cfg = Some(vec![]);
        Analyzer::new(registry, cfg).scan(&self.0).unwrap()
    }
    fn shared(&self) {
        self.write("Cargo.toml","[package]\nname='app'\nversion='0.1.0'\nedition='2021'\nautobins=false\n[lib]\npath='shared.rs'\n[[bin]]\nname='tool'\npath='shared.rs'\nrequired-features=['tool']\n[features]\ntool=[]\n");
        self.write(
            "shared.rs",
            "mod child;pub fn work(){}pub fn linear(n:usize){for i in 0..n{}}fn main(){work();child::run();}\n",
        );
        self.write("child.rs", "pub fn run(){crate::work();}\n");
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn shared_library_root_stays_active_when_its_binary_is_disabled_and_contexts_stay_separate() {
    let f = Fixture::new();
    f.shared();
    let off = f.scan(&[], false);
    assert_eq!(
        off.graph.select("shared::work").len(),
        1,
        "{:?}",
        off.graph.nodes
    );
    assert_eq!(off.graph.select("child::run").len(), 1);
    let on = f.scan(&["tool"], false);
    for symbol in ["shared::work", "shared::main", "child::run"] {
        assert_eq!(on.graph.select(symbol).len(), 2, "{:?}", on.graph.nodes);
    }
    let roots = on.graph.select("shared::main");
    let mut targets = BTreeSet::new();
    for root in roots {
        let caller = on.graph.node(&root).unwrap();
        assert_eq!(caller.cargo_target.as_deref(), Some("shared.rs"));
        targets.insert(caller.cargo_target_id.clone().unwrap());
        for edge in on.graph.outgoing_edges(&root) {
            assert_eq!(edge.confidence, Confidence::Resolved);
            assert_eq!(
                on.graph.node(&edge.to).unwrap().cargo_target_id,
                caller.cargo_target_id
            );
        }
        let query = on.graph.query(
            &root,
            Direction::Callees,
            None,
            &GraphFilter::default(),
            &Config::default(),
        );
        assert!(query
            .nodes
            .iter()
            .filter(|node| node.kind == analyzer_core::graph::NodeKind::Function)
            .all(|node| node.cargo_target_id == caller.cargo_target_id));
    }
    assert_eq!(targets.len(), 2);
    assert!(targets.iter().any(|target| target.contains("lib")));
    assert!(targets
        .iter()
        .any(|target| target.contains("bin") && target.contains("tool")));
    for node in on.graph.select("shared::linear") {
        let estimate = on
            .graph
            .node(&node)
            .unwrap()
            .algorithmic_complexity
            .as_ref()
            .unwrap();
        assert_eq!(estimate.formula.as_deref(), Some("Θ(n)"));
        assert!(estimate.unknown_reasons.is_empty(), "{estimate:?}");
    }
    assert_eq!(
        on.files
            .iter()
            .filter(|file| file.path == "shared.rs")
            .count(),
        1
    );
    assert_eq!(
        on.metrics
            .iter()
            .filter(|metric| metric.path.as_ref() == "shared.rs"
                && metric.id.as_ref() == "function_lines"
                && metric.entity.as_ref() == "shared::work")
            .count(),
        1
    );
    let json = serde_json::to_value(&on).unwrap();
    let serialized = json["graph"]["nodes"].as_array().unwrap();
    assert_eq!(
        serialized
            .iter()
            .filter(|node| node["source_fqn"] == "shared::work"
                && node["cargo_target"] == "shared.rs"
                && node["cargo_target_id"].is_string())
            .count(),
        2
    );
}

#[test]
fn cached_sources_follow_target_features_and_manifest_only_changes() {
    let f = Fixture::new();
    f.shared();
    let off = f.scan(&[], true);
    let library = off.graph.select("shared::work")[0].clone();
    let on = f.scan(&["tool"], true);
    assert_eq!(on.graph.select("shared::work").len(), 2);
    assert_eq!(on.summary.cache_hits, on.files.len());
    assert!(on.graph.node(&library).is_some());
    let off_again = f.scan(&[], true);
    assert_eq!(off_again.graph.select("shared::work"), vec![library]);
    assert_eq!(off_again.summary.cache_hits, off_again.files.len());
    let context = analyzer_core::resolution::ResolutionContext::load(&f.0, &Config::default());
    assert!(context.metadata_paths.contains(&f.0.join("Cargo.toml")));
    let manifest = std::fs::read_to_string(f.0.join("Cargo.toml")).unwrap();
    f.write(
        "Cargo.toml",
        &manifest.replace("required-features=['tool']", "required-features=[]"),
    );
    let unguarded = f.scan(&[], true);
    assert_eq!(unguarded.graph.select("shared::work").len(), 2);
    assert_eq!(unguarded.summary.cache_hits, unguarded.files.len());
    assert!(unguarded
        .files
        .iter()
        .all(|file| file.cargo_target.is_none() && file.cargo_source_module.is_none()));
}

#[test]
fn shared_root_binaries_keep_independent_name_and_feature_requirements() {
    let f = Fixture::new();
    f.shared();
    let manifest = std::fs::read_to_string(f.0.join("Cargo.toml")).unwrap();
    f.write("Cargo.toml", &format!("{manifest}other=[]\n[[bin]]\nname='other'\npath='shared.rs'\nrequired-features=['other']\n"));
    for (features, expected) in [
        (&[][..], 1),
        (&["tool"][..], 2),
        (&["other"][..], 2),
        (&["tool", "other"][..], 3),
    ] {
        let report = f.scan(features, false);
        assert_eq!(report.graph.select("shared::work").len(), expected);
        let ids = report
            .graph
            .select("shared::work")
            .iter()
            .map(|id| {
                report
                    .graph
                    .node(id)
                    .unwrap()
                    .cargo_target_id
                    .clone()
                    .unwrap()
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), expected);
        assert!(ids.contains("shared.rs#target[lib:app@.]"));
        assert_eq!(
            ids.contains("shared.rs#target[bin:tool@.]"),
            features.contains(&"tool")
        );
        assert_eq!(
            ids.contains("shared.rs#target[bin:other@.]"),
            features.contains(&"other")
        );
    }
}

#[test]
fn local_dependency_binds_only_the_library_when_its_root_is_shared_with_a_binary() {
    let f = Fixture::new();
    f.shared();
    f.write("consumer/Cargo.toml", "[package]\nname='consumer'\nversion='0.1.0'\nedition='2021'\n[dependencies]\napi={package='app',path='..',features=['tool']}\n");
    f.write("consumer/src/lib.rs", "pub fn flow(){api::work();}\n");
    let report = f.scan(&[], false);
    assert_eq!(report.graph.select("shared::work").len(), 2);
    let flow = report.graph.select("consumer::src::lib::flow");
    assert_eq!(flow.len(), 1);
    let edges = report.graph.outgoing_edges(&flow[0]).collect::<Vec<_>>();
    assert_eq!(edges.len(), 1, "{edges:?}");
    assert_eq!(edges[0].confidence, Confidence::Resolved);
    let target = report.graph.node(&edges[0].to).unwrap();
    assert_eq!(target.source_fqn.as_deref(), Some("shared::work"));
    assert_eq!(target.cargo_target.as_deref(), Some("shared.rs"));
    assert_eq!(
        target.cargo_target_id.as_deref(),
        Some("shared.rs#target[lib:app@.]")
    );
}

#[test]
fn unknown_binary_requirements_are_reported_even_with_an_active_shared_library() {
    let f = Fixture::new();
    f.shared();
    let report = f.scan_with_features(None, false);
    assert_eq!(report.graph.select("shared::work").len(), 2);
    assert!(
        report.resolution_diagnostics.iter().any(|message| {
            message.contains("shared.rs#target[bin:tool@.]")
                && message.contains("required-features cannot be determined")
        }),
        "{:?}",
        report.resolution_diagnostics
    );
}
