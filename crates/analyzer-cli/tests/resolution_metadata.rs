use analyzer_core::{graph::NodeKind, Analyzer, Confidence, Config, LanguageRegistry};
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
                "codefactor-resolver-{}-{}",
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
        registry.register(analyzer_typescript::TypeScript);
        registry.register(analyzer_javascript::JavaScript);
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
    config.vendor.parse = analyzer_core::config::VendorParse::Full;
    config
}
fn targets(report: &analyzer_core::AnalysisReport, caller: &str) -> Vec<(String, Confidence)> {
    let node = report
        .graph
        .nodes
        .iter()
        .find(|node| node.label == caller)
        .unwrap_or_else(|| panic!("missing {caller}"));
    report
        .graph
        .outgoing_edges(&node.id)
        .map(|edge| {
            (
                report.graph.node(&edge.to).unwrap().label.clone(),
                edge.confidence.clone(),
            )
        })
        .collect()
}

#[test]
fn package_tsconfig_extends_resolves_entries_and_preserves_path_origins() {
    let f = Fixture::new();
    f.write("tsconfig.json", r#"{"extends":"config-package"}"#);
    f.write(
        "node_modules/config-package/package.json",
        r#"{"tsconfig":"base.json"}"#,
    );
    f.write(
        "node_modules/config-package/base.json",
        r#"{"compilerOptions":{"baseUrl":"../../src","paths":{"@lib":["leaf"]}}}"#,
    );
    f.write("src/leaf.ts", "export function work(){}");
    f.write(
        "src/main.ts",
        "import {work} from '@lib'; function run(){work();}",
    );
    let report = f.scan(config());
    assert_eq!(
        targets(&report, "src/main.run"),
        vec![("src/leaf.work".into(), Confidence::Resolved)]
    );
    assert!(
        report.resolution_diagnostics.is_empty(),
        "{:?}",
        report.resolution_diagnostics
    );
    let context = analyzer_core::resolution::ResolutionContext::load(&f.0, &config());
    assert!(context
        .metadata_paths
        .contains(&f.0.join("node_modules/config-package/base.json")));
    f.write(
        "node_modules/config-package/base.json",
        r#"{"compilerOptions":{"baseUrl":"../../src","paths":{"@lib":["replacement"]}}}"#,
    );
    f.write("src/replacement.ts", "export function work(){}");
    assert_eq!(
        targets(&f.scan(config()), "src/main.run"),
        vec![("src/replacement.work".into(), Confidence::Resolved)]
    );
}

#[test]
fn scoped_package_tsconfig_subpaths_and_ordered_extends_are_resolved() {
    let f = Fixture::new();
    f.write(
        "src/nested/tsconfig.json",
        r#"{"extends":["@team/base/shared","@team/override"]}"#,
    );
    f.write(
        "node_modules/@team/base/shared.json",
        r#"{"compilerOptions":{"baseUrl":"../../../src","paths":{"@lib":["wrong"]}}}"#,
    );
    f.write(
        "node_modules/@team/override/tsconfig.json",
        r#"{"compilerOptions":{"baseUrl":"../../../src","paths":{"@lib":["leaf"]}}}"#,
    );
    f.write("src/leaf.ts", "export function work(){}");
    f.write("src/wrong.ts", "export function work(){}");
    f.write(
        "src/nested/main.ts",
        "import {work} from '@lib'; function run(){work();}",
    );
    let report = f.scan(config());
    assert_eq!(
        targets(&report, "src/nested/main.run"),
        vec![("src/leaf.work".into(), Confidence::Resolved)]
    );
    assert!(
        report.resolution_diagnostics.is_empty(),
        "{:?}",
        report.resolution_diagnostics
    );
}

#[test]
fn missing_and_cyclic_config_packages_cannot_certify_aliases() {
    for extends in ["absent", "loop"] {
        let f = Fixture::new();
        f.write(
            "tsconfig.json",
            &format!(
                r#"{{"extends":"{extends}","compilerOptions":{{"paths":{{"@lib":["leaf"]}}}}}}"#
            ),
        );
        f.write("node_modules/loop/tsconfig.json", r#"{"extends":"loop"}"#);
        f.write("leaf.ts", "export function work(){}");
        f.write(
            "main.ts",
            "import {work} from '@lib'; function run(){work();}",
        );
        let report = f.scan(config());
        assert!(targets(&report, "main.run")
            .iter()
            .all(|(_, c)| *c == Confidence::Dynamic));
        assert!(!report.resolution_diagnostics.is_empty());
    }
}

#[cfg(unix)]
#[test]
fn tsconfig_package_symlinks_remain_confined_to_project() {
    let f = Fixture::new();
    let outside = Fixture::new();
    outside.write(
        "tsconfig.json",
        r#"{"compilerOptions":{"paths":{"@lib":["leaf"]}}}"#,
    );
    f.write("tsconfig.json", r#"{"extends":"escaped"}"#);
    f.write("leaf.ts", "export function work(){}");
    f.write(
        "main.ts",
        "import {work} from '@lib'; function run(){work();}",
    );
    std::fs::create_dir_all(f.0.join("node_modules")).unwrap();
    std::os::unix::fs::symlink(&outside.0, f.0.join("node_modules/escaped")).unwrap();
    let report = f.scan(config());
    assert!(targets(&report, "main.run")
        .iter()
        .all(|(_, c)| *c == Confidence::Dynamic));
    assert!(report
        .resolution_diagnostics
        .iter()
        .any(|d| d.contains("symlink outside")));
}
#[test]
fn package_main_exports_conditions_patterns_and_blocked_subpaths() {
    let f = Fixture::new();
    f.write("app.ts", "import {work} from 'main'; import {work as exp} from 'demo'; import {work as sub} from 'demo/features/x'; import {work as denied} from 'demo/private'; import api = require('demo'); function run(){work();exp();sub();denied();api.work();}");
    f.write(
        "node_modules/main/package.json",
        r#"{"main":"./lib/entry.js"}"#,
    );
    f.write("node_modules/main/lib/entry.js", "export function work(){}");
    f.write("node_modules/demo/package.json", r#"{"main":"./wrong.js","exports":{".":{"import":"./esm.js","require":"./cjs.js","default":"./wrong.js"},"./features/*":"./lib/*.js","./private":null}}"#);
    for path in ["esm.js", "cjs.js", "wrong.js", "private.js", "lib/x.js"] {
        f.write(
            &format!("node_modules/demo/{path}"),
            "export function work(){}",
        );
    }
    let report = f.scan(config());
    let targets = targets(&report, "app.run");
    for expected in [
        "node_modules/main/lib/entry.work",
        "node_modules/demo/esm.work",
        "node_modules/demo/lib/x.work",
        "node_modules/demo/cjs.work",
    ] {
        assert!(
            targets.contains(&(expected.into(), Confidence::Resolved)),
            "{targets:?}"
        );
    }
    assert_eq!(targets.len(), 5, "{targets:?}");
    assert!(targets
        .iter()
        .any(|(name, confidence)| name == "denied" && *confidence == Confidence::Dynamic));
    assert!(!targets
        .iter()
        .any(|(name, _)| name.contains("wrong") || name == "node_modules/demo/private.work"));
}
#[test]
fn installed_package_does_not_hide_builtin_external_operations() {
    let f = Fixture::new();
    f.write("app.ts", "import axios from 'axios'; import * as ns from 'axios'; import tr = require('axios'); function d(){axios.get();} function n(){ns.get();} function t(){tr.get();}");
    f.write(
        "web.js",
        "const cj = require('axios'); function c(){cj.get();}",
    );
    f.write(
        "node_modules/axios/package.json",
        r#"{"main":"index.js","exports":{".":{"require":"./dist/axios.cjs","default":"./index.js"}}}"#,
    );
    f.write("node_modules/axios/index.js", "export default {};");
    f.write("node_modules/axios/dist/axios.cjs", "module.exports = {};");
    let mut deferred = config();
    deferred.vendor.parse = Config::default().vendor.parse;
    for config in [deferred, config()] {
        let report = f.scan(config);
        for caller in ["app.d", "app.n", "app.t", "web.c"] {
            let targets = targets(&report, caller);
            assert!(
                targets.iter().any(|(name, _)| name == "http · axios · get"),
                "{caller}: {targets:?}"
            );
            assert!(
                !targets
                    .iter()
                    .any(|(name, _)| name.contains("<unresolved>")),
                "{caller}: {targets:?}"
            );
        }
    }
}
#[test]
fn closest_manifest_is_authoritative_and_condition_key_order_is_preserved() {
    let f = Fixture::new();
    f.write(
        "app.ts",
        "import {work} from 'demo'; function run(){work();}",
    );
    f.write(
        "child/app.ts",
        "import {work} from 'demo'; function run(){work();}",
    );
    f.write(
        "node_modules/demo/package.json",
        r#"{"exports":{"default":"./fallback.js","import":"./never.js"}}"#,
    );
    f.write("node_modules/demo/fallback.js", "export function work(){}");
    f.write("node_modules/demo/never.js", "export function work(){}");
    f.write(
        "child/node_modules/demo/package.json",
        r#"{"exports":"./entry.js"}"#,
    );
    f.write(
        "child/node_modules/demo/entry.js",
        "export function work(){}",
    );
    let report = f.scan(config());
    assert_eq!(
        targets(&report, "app.run"),
        vec![(
            "node_modules/demo/fallback.work".into(),
            Confidence::Resolved
        )]
    );
    assert_eq!(
        targets(&report, "child/app.run"),
        vec![(
            "child/node_modules/demo/entry.work".into(),
            Confidence::Resolved
        )]
    );
}
#[test]
fn tsconfig_jsonc_paths_extends_origins_and_nearest_scope() {
    let f = Fixture::new();
    f.write("configs/base.json", "{ // source config owns its relative values\n\"compilerOptions\":{\"baseUrl\":\"../src\",\"paths\":{\"@app/*\":[\"missing/*\",\"lib/*\"],},},}");
    f.write(
        "tsconfig.json",
        r#"{"extends":"./configs/base.json","compilerOptions":{"baseUrl":"./decoy"}}"#,
    );
    f.write(
        "app.ts",
        "import {work} from '@app/api'; function run(){work();}",
    );
    f.write("src/lib/api.ts", "export function work(){}");
    f.write("decoy/lib/api.ts", "export function work(){}");
    f.write(
        "nested/tsconfig.json",
        r#"{"compilerOptions":{"paths":{"@app/*":["./local/*"]}}}"#,
    );
    f.write(
        "nested/app.ts",
        "import {work} from '@app/api'; function run(){work();}",
    );
    f.write("nested/local/api.ts", "export function work(){}");
    let report = f.scan(config());
    assert_eq!(
        targets(&report, "app.run"),
        vec![("src/lib/api.work".into(), Confidence::Resolved)]
    );
    assert_eq!(
        targets(&report, "nested/app.run"),
        vec![("nested/local/api.work".into(), Confidence::Resolved)]
    );
    let context = analyzer_core::resolution::ResolutionContext::load(&f.0, &config());
    assert!(context
        .metadata_paths
        .contains(&f.0.join("configs/base.json")));
    assert!(context.diagnostics.is_empty(), "{:?}", context.diagnostics);
}
#[test]
fn cyclic_tsconfig_does_not_bind_an_accidental_package() {
    let f = Fixture::new();
    f.write("tsconfig.json", r#"{"extends":"./other.json"}"#);
    f.write(
        "other.json",
        r#"{"extends":"./tsconfig.json","compilerOptions":{"paths":{"demo":["./api.ts"]}}}"#,
    );
    f.write(
        "app.ts",
        "import {work} from 'demo'; function run(){work();}",
    );
    f.write("api.ts", "export function work(){}");
    f.write("node_modules/demo/index.ts", "export function work(){}");
    let report = f.scan(config());
    assert_eq!(targets(&report, "app.run")[0].1, Confidence::Dynamic);
    let context = analyzer_core::resolution::ResolutionContext::load(&f.0, &config());
    assert!(context
        .diagnostics
        .iter()
        .any(|message| message.contains("cyclic")));
}
#[test]
fn cargo_custom_lib_binary_and_workspace_roots_resolve_crate_modules() {
    let f = Fixture::new();
    f.write("Cargo.toml", "[workspace]\nmembers=['library', 'tool']\n");
    f.write(
        "library/Cargo.toml",
        "[package]\nname='library'\nversion='0.1.0'\n[lib]\npath='engine/start.rs'\n",
    );
    f.write(
        "library/engine/start.rs",
        "mod child; fn run(){crate::child::work();}",
    );
    f.write("library/engine/child.rs", "pub fn work(){}");
    f.write(
        "tool/Cargo.toml",
        "[package]\nname='tool'\nversion='0.1.0'\n[[bin]]\nname='tool'\npath='runner/entry.rs'\n",
    );
    f.write(
        "tool/runner/entry.rs",
        "mod child; fn run(){crate::child::work();}",
    );
    f.write("tool/runner/child.rs", "pub fn work(){}");
    let report = f.scan(config());
    assert_eq!(
        targets(&report, "library::engine::start::run"),
        vec![("library::engine::child::work".into(), Confidence::Resolved)]
    );
    assert_eq!(
        targets(&report, "tool::runner::entry::run"),
        vec![("tool::runner::child::work".into(), Confidence::Resolved)]
    );
}
#[test]
fn rust_cfg_selects_local_declarations_inline_ancestors_and_conditional_paths() {
    let f = Fixture::new();
    f.write("src/lib.rs", "#[cfg_attr(windows, path=\"win.rs\")]\n#[cfg_attr(not(windows), path=\"linux.rs\")]\nmod platform;\n#[cfg(windows)]\nfn local(){wrong();}\n#[cfg(not(windows))]\nfn local(){right();}\nfn wrong(){} fn right(){} fn run(){local();platform::work();}\n#[cfg(test)] mod tests {fn hidden(){wrong();}}\n");
    f.write("src/win.rs", "pub fn work(){}");
    f.write("src/linux.rs", "pub fn work(){}");
    let mut config = config();
    config.resolution.rust.target = Some("x86_64-unknown-linux-gnu".into());
    config.resolution.rust.cfg = Some(vec![]);
    config.resolution.rust.features = Some(vec![]);
    let report = f.scan(config);
    assert_eq!(
        targets(&report, "src::lib::local"),
        vec![("src::lib::right".into(), Confidence::Resolved)]
    );
    let run = targets(&report, "src::lib::run");
    assert!(
        run.contains(&("src::linux::work".into(), Confidence::Resolved)),
        "{run:?}"
    );
    assert!(report
        .graph
        .nodes
        .iter()
        .all(|node| node.label != "src::lib::tests::hidden"));
}
#[test]
fn rust_test_only_items_are_excluded_from_metrics_unless_test_cfg_is_selected() {
    let f = Fixture::new();
    f.write("src/lib.rs", "pub struct Counter{value:u32}\nimpl Counter{pub fn new()->Self{Self{value:0}} pub fn inc(&mut self){self.value+=1;}}\n#[cfg(test)] fn helper()->Counter{Counter::new()}\n#[cfg(test)] impl Counter{fn peek(&self)->u32{self.value}}\n#[test] fn bare(){Counter::new();}\n#[tokio::test] async fn harness(){}\n#[cfg(not(test))] fn prod_only(){}\n#[cfg(any(test, feature=\"x\"))] fn shared(){}\n#[cfg(test)] mod tests{use super::*; struct H; impl H{fn make()->Counter{Counter::new()}} #[test] fn t(){H::make().inc();} mod nested{#[test] fn n(){}}}\n");
    let metric = |report: &analyzer_core::AnalysisReport, id: &str, entity: &str| {
        report
            .metrics
            .iter()
            .find(|m| &*m.id == id && &*m.entity == entity)
            .map(|m| m.value)
    };
    for cfg in [None, Some(vec![])] {
        let mut config = config();
        config.resolution.rust.cfg = cfg;
        let report = f.scan(config);
        assert_eq!(report.summary.functions, 4);
        assert_eq!(
            metric(&report, "functions_per_file", "src/lib.rs"),
            Some(4.0)
        );
        assert_eq!(metric(&report, "types_per_file", "src/lib.rs"), Some(1.0));
        assert_eq!(
            metric(&report, "fan_in", "src::lib::Counter::new"),
            Some(0.0)
        );
        for hidden in ["bare", "harness", "helper", "Counter::peek", "tests"] {
            let hidden = format!("src::lib::{hidden}");
            assert!(
                report
                    .metrics
                    .iter()
                    .all(|m| !m.entity.starts_with(&hidden)),
                "{hidden}"
            );
            assert!(report
                .graph
                .nodes
                .iter()
                .all(|node| !node.label.starts_with(&hidden)));
        }
        assert!(report.files[0]
            .imports
            .iter()
            .all(|import| !import.path.starts_with("super")));
    }
    let mut config = config();
    config.resolution.rust.cfg = Some(vec!["test".into()]);
    let report = f.scan(config);
    assert_eq!(report.summary.functions, 11);
    assert_eq!(
        metric(&report, "fan_in", "src::lib::Counter::new"),
        Some(3.0)
    );
}
#[test]
fn rust_out_of_line_test_modules_are_excluded_with_their_files() {
    let f = Fixture::new();
    f.write(
        "Cargo.toml",
        "[package]\nname='b'\nversion='0.1.0'\nedition='2021'\n",
    );
    f.write("src/lib.rs", "pub fn real()->u32{1}\npub fn caller()->u32{real()}\n#[cfg(test)]\nmod tests;\n#[cfg(test)]\nmod support;\n");
    f.write("src/tests.rs", "use super::*;\nfn helper()->u32{real()+real()}\n#[test] fn it_works(){assert_eq!(helper(),2);}\n");
    f.write(
        "src/support/mod.rs",
        "mod fixtures;\npub fn make()->u32{fixtures::one()}\n",
    );
    f.write(
        "src/support/fixtures.rs",
        "pub fn one()->u32{crate::real()}\n",
    );
    let paths = |report: &analyzer_core::AnalysisReport| {
        report
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>()
    };
    for cfg in [None, Some(vec![])] {
        let mut config = config();
        config.resolution.rust.cfg = cfg;
        let report = f.scan(config);
        assert_eq!(paths(&report), ["src/lib.rs"]);
        assert_eq!(report.summary.functions, 2);
        assert!(
            report
                .graph
                .nodes
                .iter()
                .all(|node| node.file.as_deref() == Some("src/lib.rs")),
            "{:?}",
            report.graph.nodes
        );
        assert!(report
            .metrics
            .iter()
            .all(|m| m.path.is_empty() || &*m.path == "src/lib.rs"));
        assert!(report.files[0]
            .types
            .iter()
            .all(|ty| !["tests", "support"].contains(&ty.name.as_str())));
    }
    let mut config = config();
    config.resolution.rust.cfg = Some(vec!["test".into()]);
    let report = f.scan(config);
    assert_eq!(paths(&report).len(), 4);
    assert!(report
        .graph
        .nodes
        .iter()
        .any(|node| node.label == "src::tests::helper"));
}
#[test]
fn unknown_rust_condition_does_not_claim_certain_resolution() {
    let f = Fixture::new();
    f.write("src/lib.rs","#[cfg(feature=\"extra\")] fn maybe(){} fn run(){maybe();}\n#[cfg_attr(feature=\"extra\",path=\"other.rs\")] mod child; fn branch(){child::work();}");
    f.write("src/child.rs", "pub fn work(){}");
    f.write("src/other.rs", "pub fn work(){}");
    let report = f.scan(config());
    assert_eq!(
        targets(&report, "src::lib::run")[0].1,
        Confidence::Ambiguous
    );
    assert_ne!(
        targets(&report, "src::lib::branch")[0].1,
        Confidence::Resolved
    );
}
#[test]
fn export_star_chains_cycles_conflicts_and_explicit_override() {
    let f = Fixture::new();
    f.write(
        "app.ts",
        "import {work, clash, chosen} from './barrel'; function run(){work();clash();chosen();}",
    );
    f.write(
        "barrel.ts",
        "export * from './a'; export * from './b'; export {chosen} from './a';",
    );
    f.write("a.ts","export * from './cycle'; export function work(){} export function clash(){} export function chosen(){}");
    f.write(
        "b.ts",
        "export function clash(){} export function chosen(){} export default function nope(){}",
    );
    f.write("cycle.ts", "export * from './barrel';");
    let report = f.scan(config());
    let calls = targets(&report, "app.run");
    assert!(
        calls.contains(&("a.work".into(), Confidence::Resolved)),
        "{calls:?}"
    );
    assert!(
        calls.contains(&("a.chosen".into(), Confidence::Resolved)),
        "{calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|(_, confidence)| *confidence == Confidence::Ambiguous),
        "{calls:?}"
    );
    assert!(report
        .graph
        .nodes
        .iter()
        .any(|node| node.kind == NodeKind::Unknown));
}
#[test]
fn manifest_change_rebuilds_graph_while_source_ir_cache_is_reused() {
    let f = Fixture::new();
    f.write(
        "app.ts",
        "import {work} from 'demo'; function run(){work();}",
    );
    f.write("node_modules/demo/package.json", r#"{"main":"./a.js"}"#);
    f.write("node_modules/demo/a.js", "export function work(){}");
    f.write("node_modules/demo/b.js", "export function work(){}");
    let mut cfg = config();
    cfg.cache.enabled = true;
    let before = f.scan(cfg.clone());
    assert_eq!(targets(&before, "app.run")[0].0, "node_modules/demo/a.work");
    f.write("node_modules/demo/package.json", r#"{"main":"./b.js"}"#);
    let after = f.scan(cfg);
    assert_eq!(targets(&after, "app.run")[0].0, "node_modules/demo/b.work");
    assert_eq!(after.summary.cache_hits, after.files.len());
}

#[test]
fn literal_require_namespace_uses_require_conditions_and_shadowing_is_conservative() {
    let f = Fixture::new();
    f.write("app.js","const api=require('demo'); function run(){api.work();} function shadow(require){const api=require('demo');api.work();}");
    f.write(
        "node_modules/demo/package.json",
        r#"{"exports":{"import":"./esm.js","require":"./cjs.js"}}"#,
    );
    f.write("node_modules/demo/esm.js", "export function work(){}");
    f.write("node_modules/demo/cjs.js", "export function work(){}");
    let report = f.scan(config());
    assert_eq!(
        targets(&report, "app.run"),
        vec![("node_modules/demo/cjs.work".into(), Confidence::Resolved)]
    );
    assert!(targets(&report, "app.shadow")
        .iter()
        .all(|(_, confidence)| *confidence != Confidence::Resolved));
}

#[test]
fn cfg_truth_tables_unknowns_and_empty_inputs_are_explicit() {
    use analyzer_core::resolution::evaluate_cfg;
    let mut cfg = analyzer_core::config::RustResolutionConfig::default();
    assert_eq!(evaluate_cfg("feature=\"x\"", &cfg), None);
    assert_eq!(evaluate_cfg("all()", &cfg), Some(true));
    assert_eq!(evaluate_cfg("any()", &cfg), Some(false));
    cfg.target = Some("x86_64-unknown-linux-gnu".into());
    cfg.features = Some(vec![]);
    cfg.cfg = Some(vec!["custom".into(), "build=\"production\"".into()]);
    for condition in [
        "not(windows)",
        "unix",
        "target_os=\"linux\"",
        "all(custom, build=\"production\")",
        "not(feature=\"x\")",
    ] {
        assert_eq!(evaluate_cfg(condition, &cfg), Some(true), "{condition}");
    }
    assert_eq!(
        evaluate_cfg("any(windows, feature=\"x\")", &cfg),
        Some(false)
    );
    assert_eq!(evaluate_cfg("target_pointer_width=\"64\"", &cfg), None);
}

#[test]
fn cfg_requires_test_only_for_predicates_false_without_test() {
    use analyzer_core::resolution::cfg_requires_test;
    for condition in [
        "test",
        "all(test, unix)",
        "all(unix, test)",
        "not(not(test))",
    ] {
        assert!(cfg_requires_test(condition), "{condition}");
    }
    for condition in [
        "not(test)",
        "any(test, feature = \"x\")",
        "unix",
        "any(not(test), feature = \"y\")",
        "all(test",
    ] {
        assert!(!cfg_requires_test(condition), "{condition}");
    }
}

#[test]
fn malformed_manifests_and_cfg_unknowns_have_separate_bounded_diagnostics() {
    let f = Fixture::new();
    f.write(
        "app.ts",
        "import {work} from 'demo'; function run(){work();}",
    );
    f.write("node_modules/demo/package.json", "{bad}");
    f.write("node_modules/demo/index.ts", "export function work(){}");
    f.write(
        "src/lib.rs",
        "#[cfg(feature=\"x\")] fn hidden(){} fn run(){hidden();}",
    );
    let report = f.scan(config());
    assert!(report.parse_errors.is_empty());
    assert!(report
        .resolution_diagnostics
        .iter()
        .any(|message| message.contains("package.json")));
    assert!(report
        .resolution_diagnostics
        .iter()
        .any(|message| message.contains("cfg(feature=\"x\")")));
    assert_eq!(targets(&report, "app.run")[0].1, Confidence::Dynamic);
}

#[test]
fn relative_alias_and_extends_cannot_escape_the_project_or_become_root_decoys() {
    let f = Fixture::new();
    f.write(
        "tsconfig.json",
        r#"{"compilerOptions":{"paths":{"outside":["../outside.ts"]}}}"#,
    );
    f.write(
        "app.ts",
        "import {work} from 'outside'; function run(){work();}",
    );
    f.write("outside.ts", "export function work(){}");
    let report = f.scan(config());
    assert_eq!(targets(&report, "app.run")[0].1, Confidence::Dynamic);
}

#[test]
fn named_import_has_precedence_over_unrelated_object_method_in_same_file() {
    let f = Fixture::new();
    f.write(
        "app.ts",
        "import {has} from './barrel'; const traps={has(){}}; function run(){has();}",
    );
    f.write("barrel.ts", "export * from './common';");
    f.write("common.ts", "export function has(){}");
    let report = f.scan(config());
    assert_eq!(
        targets(&report, "app.run"),
        vec![("common.has".into(), Confidence::Resolved)]
    );
}

#[test]
fn rust_inner_cfg_filters_the_entire_file_for_an_explicit_context() {
    let f = Fixture::new();
    f.write(
        "src/lib.rs",
        "#![cfg(feature=\"extra\")]\nfn hidden(){} fn run(){hidden();}",
    );
    let mut cfg = config();
    cfg.resolution.rust.features = Some(vec![]);
    let report = f.scan(cfg);
    assert!(report
        .graph
        .nodes
        .iter()
        .all(|node| !node.label.contains("hidden") && !node.label.ends_with("run")));
}

#[test]
fn export_arrays_skip_invalid_targets_but_do_not_choose_a_missing_files_fallback() {
    let f = Fixture::new();
    f.write("app.ts","import {work} from 'good'; import {work as absent} from 'absent'; function run(){work();absent();}");
    f.write(
        "node_modules/good/package.json",
        r#"{"exports":["../invalid.js","./entry.js"]}"#,
    );
    f.write("node_modules/good/entry.js", "export function work(){}");
    f.write(
        "node_modules/absent/package.json",
        r#"{"exports":["./missing.js","./entry.js"]}"#,
    );
    f.write("node_modules/absent/entry.js", "export function work(){}");
    let report = f.scan(config());
    let calls = targets(&report, "app.run");
    assert!(calls.contains(&("node_modules/good/entry.work".into(), Confidence::Resolved)));
    assert!(calls
        .iter()
        .any(|(name, confidence)| name == "absent" && *confidence == Confidence::Dynamic));
    assert!(!calls
        .iter()
        .any(|(name, _)| name == "node_modules/absent/entry.work"));
}

#[test]
fn target_triple_architecture_and_omitted_vendor_are_not_literal_components() {
    use analyzer_core::resolution::evaluate_cfg;
    let mut cfg = analyzer_core::config::RustResolutionConfig::default();
    cfg.target = Some("riscv64gc-unknown-linux-gnu".into());
    assert_eq!(evaluate_cfg("target_arch=\"riscv64\"", &cfg), Some(true));
    cfg.target = Some("x86_64-linux-android".into());
    assert_eq!(evaluate_cfg("target_vendor=\"unknown\"", &cfg), Some(true));
    assert_eq!(evaluate_cfg("target_vendor=\"linux\"", &cfg), Some(false));
    cfg.target = Some("custom-target.json".into());
    assert_eq!(evaluate_cfg("target_arch=\"x86_64\"", &cfg), None);
}

#[test]
fn selected_cargo_features_expand_transitively_for_functions_and_conditional_module_paths() {
    let f = Fixture::new();
    f.write(
        "Cargo.toml",
        "[package]\nname='demo'\nversion='0.1.0'\n[features]\nfast=['safe']\nsafe=[]\n",
    );
    f.write("src/lib.rs","#[cfg(feature=\"safe\")] fn selected(){safe_leaf();}\n#[cfg(not(feature=\"safe\"))] fn selected(){fallback();}\nfn safe_leaf(){} fn fallback(){} fn run(){selected();platform::work();}\n#[cfg_attr(feature=\"safe\",path=\"safe.rs\")] mod platform;\n");
    f.write("src/safe.rs", "pub fn work(){}");
    f.write("src/platform.rs", "pub fn work(){}");
    let mut cfg = config();
    cfg.resolution.rust.features = Some(vec!["fast".into()]);
    let report = f.scan(cfg);
    assert_eq!(
        targets(&report, "src::lib::selected"),
        vec![("src::lib::safe_leaf".into(), Confidence::Resolved)]
    );
    assert!(targets(&report, "src::lib::run")
        .contains(&("src::safe::work".into(), Confidence::Resolved)));
    assert!(
        report.resolution_diagnostics.is_empty(),
        "{:?}",
        report.resolution_diagnostics
    );
}

#[test]
fn cargo_feature_cycles_and_explicit_default_are_local_and_empty_selection_stays_empty() {
    let f = Fixture::new();
    f.write("Cargo.toml","[package]\nname='demo'\nversion='0.1.0'\n[features]\ndefault=['fast']\nfast=['safe']\nsafe=['fast']\n");
    f.write("src/lib.rs","#[cfg(feature=\"safe\")] fn selected(){enabled();}\n#[cfg(not(feature=\"safe\"))] fn selected(){disabled();}\nfn enabled(){} fn disabled(){} fn run(){selected();}\n");
    for selected in [vec!["fast".into()], vec!["default".into()], vec![]] {
        let active = !selected.is_empty();
        let mut cfg = config();
        cfg.resolution.rust.features = Some(selected);
        let report = f.scan(cfg);
        assert_eq!(
            targets(&report, "src::lib::selected"),
            vec![(
                format!("src::lib::{}", if active { "enabled" } else { "disabled" }),
                Confidence::Resolved
            )]
        );
        assert!(
            report.resolution_diagnostics.is_empty(),
            "{:?}",
            report.resolution_diagnostics
        );
    }
    let report = f.scan(config());
    assert!(targets(&report, "src::lib::run")
        .iter()
        .all(|(_, confidence)| *confidence == Confidence::Ambiguous));
}

#[test]
fn nearest_cargo_package_owns_its_feature_dependency_closure() {
    let f = Fixture::new();
    for (prefix, feature) in [("", "safe"), ("nested/", "other")] {
        f.write(&format!("{prefix}Cargo.toml"),&format!("[package]\nname='demo'\nversion='0.1.0'\n[features]\nfast=['{feature}']\n{feature}=[]\n"));
        f.write(&format!("{prefix}src/lib.rs"),"#[cfg(feature=\"safe\")] fn selected(){safe();}\n#[cfg(not(feature=\"safe\"))] fn selected(){other();}\nfn safe(){} fn other(){} fn run(){selected();}\n");
    }
    let mut cfg = config();
    cfg.resolution.rust.features = Some(vec!["fast".into()]);
    let report = f.scan(cfg);
    assert_eq!(
        targets(&report, "src::lib::selected"),
        vec![("src::lib::safe".into(), Confidence::Resolved)]
    );
    assert_eq!(
        targets(&report, "nested::src::lib::selected"),
        vec![("nested::src::lib::other".into(), Confidence::Resolved)]
    );
}

#[test]
fn undeclared_cargo_dependency_feature_is_diagnosed_and_cannot_certify_local_cfg() {
    let f = Fixture::new();
    f.write(
        "Cargo.toml",
        "[package]\nname='demo'\nversion='0.1.0'\n[features]\nfast=['dep:backend']\nsafe=[]\n",
    );
    f.write(
        "src/lib.rs",
        "#[cfg(feature=\"safe\")] fn maybe(){} fn run(){maybe();}\n",
    );
    let mut cfg = config();
    cfg.resolution.rust.features = Some(vec!["fast".into()]);
    let report = f.scan(cfg);
    assert_eq!(
        targets(&report, "src::lib::run")[0].1,
        Confidence::Ambiguous
    );
    assert!(
        report
            .resolution_diagnostics
            .iter()
            .any(|message| message.starts_with("Cargo.toml:") && message.contains("dep:backend")),
        "{:?}",
        report.resolution_diagnostics
    );
}

#[test]
fn cargo_dependency_features_preserve_local_flags_and_implicit_optional_semantics() {
    let f = Fixture::new();
    f.write("Cargo.toml", "[package]\nname='demo'\nversion='0.1.0'\n[dependencies]\nserde={version='1',optional=true}\nrgb={version='1',optional=true}\nbackend={version='1',optional=true}\nplain='1'\n[features]\nfast=['safe','dep:backend','serde/derive','rgb?/serde','plain/extra']\nsafe=[]\n");
    f.write("src/lib.rs", "#[cfg(feature=\"fast\")] fn fast(){} #[cfg(feature=\"safe\")] fn safe(){} #[cfg(feature=\"serde\")] fn serde(){} #[cfg(feature=\"rgb\")] fn rgb(){} #[cfg(feature=\"backend\")] fn backend(){} fn run(){fast();safe();serde();rgb();backend();}\n");
    let mut cfg = config();
    cfg.resolution.rust.features = Some(vec!["fast".into()]);
    let report = f.scan(cfg);
    let calls = targets(&report, "src::lib::run");
    for flag in ["fast", "safe", "serde"] {
        assert!(
            calls.contains(&(format!("src::lib::{flag}"), Confidence::Resolved)),
            "{calls:?}"
        );
    }
    for flag in ["rgb", "backend"] {
        assert!(
            report
                .graph
                .nodes
                .iter()
                .all(|node| node.label != format!("src::lib::{flag}")
                    || node.kind == NodeKind::Unknown),
            "{calls:?}"
        );
    }
    assert!(
        report
            .resolution_diagnostics
            .iter()
            .any(|message| message.contains("dependency feature propagation is not linked")),
        "{:?}",
        report.resolution_diagnostics
    );
    cfg = config();
    cfg.resolution.rust.features = Some(vec!["serde".into()]);
    let report = f.scan(cfg);
    assert!(targets(&report, "src::lib::run")
        .contains(&("src::lib::serde".into(), Confidence::Resolved)));
}

#[test]
fn cargo_binary_required_features_gate_custom_and_inferred_roots() {
    let f = Fixture::new();
    f.write("Cargo.toml", "[package]\nname='demo'\nversion='0.1.0'\n[features]\nfast=['safe']\nsafe=[]\n[[bin]]\nname='tools'\npath='tools/entry.rs'\nrequired-features=['safe']\n[[bin]]\nname='demo'\nrequired-features=['safe']\n");
    f.write("tools/entry.rs", "fn leaf(){} fn main(){leaf();}\n");
    f.write("src/main.rs", "fn leaf(){} fn main(){leaf();}\n");
    for (selected, confidence) in [
        (Some(vec!["fast".into()]), Some(Confidence::Resolved)),
        (Some(vec![]), None),
        (None, Some(Confidence::Ambiguous)),
    ] {
        let mut cfg = config();
        cfg.resolution.rust.features = selected;
        let report = f.scan(cfg);
        for module in ["tools::entry", "src::main"] {
            if let Some(confidence) = &confidence {
                assert_eq!(
                    targets(&report, &format!("{module}::main")),
                    vec![(format!("{module}::leaf"), confidence.clone())]
                );
            } else {
                assert!(
                    report
                        .graph
                        .nodes
                        .iter()
                        .all(|node| node.label != format!("{module}::main")),
                    "{:?}",
                    report.graph.nodes
                );
            }
        }
        assert!(
            report.files.iter().all(|file| !file.functions.is_empty()),
            "Source IR remains available for metrics/tree"
        );
        if confidence == Some(Confidence::Ambiguous) {
            assert!(report
                .resolution_diagnostics
                .iter()
                .any(|message| message.contains("binary required-features")));
        }
    }
}

#[test]
fn cargo_feature_metadata_changes_rebuild_cfg_selection_with_cached_source_ir() {
    let f = Fixture::new();
    f.write("src/lib.rs","#[cfg(feature=\"safe\")] fn selected(){enabled();}\n#[cfg(not(feature=\"safe\"))] fn selected(){disabled();}\nfn enabled(){} fn disabled(){} fn run(){selected();}\n");
    f.write(
        "Cargo.toml",
        "[package]\nname='demo'\nversion='0.1.0'\n[features]\nfast=['safe']\nsafe=[]\n",
    );
    let mut cfg = config();
    cfg.cache.enabled = true;
    cfg.resolution.rust.features = Some(vec!["fast".into()]);
    assert_eq!(
        targets(&f.scan(cfg.clone()), "src::lib::selected"),
        vec![("src::lib::enabled".into(), Confidence::Resolved)]
    );
    f.write(
        "Cargo.toml",
        "[package]\nname='demo'\nversion='0.1.0'\n[features]\nfast=[]\nsafe=[]\n",
    );
    let report = f.scan(cfg);
    assert_eq!(
        targets(&report, "src::lib::selected"),
        vec![("src::lib::disabled".into(), Confidence::Resolved)]
    );
    assert_eq!(report.summary.cache_hits, report.files.len());
}

#[test]
fn pkg_selector_matches_nearest_package_json_name() {
    let f = Fixture::new();
    f.write(
        "package.json",
        r#"{"name":"acme","private":true,"workspaces":["packages/*"]}"#,
    );
    f.write("packages/api/package.json", r#"{"name":"@acme/api"}"#);
    f.write(
        "packages/api/src/index.ts",
        "import { load } from '../../db/src/store';\nexport function handle() { load(); }\n",
    );
    f.write("packages/db/package.json", r#"{"name":"@acme/db"}"#);
    f.write("packages/db/src/store.ts", "export function load() {}\n");
    f.write("tools/build.ts", "export function build() {}\n");
    let report = f.scan(config());
    let relations =
        report
            .graph
            .relations(&["pkg:@acme/api".into()], &["pkg:@acme/db".into()], None);
    let labels = |ids: &[String]| {
        ids.iter()
            .map(|id| report.graph.node(id).unwrap().label.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(labels(&relations.a), ["packages/api/src/index.handle"]);
    assert_eq!(labels(&relations.b), ["packages/db/src/store.load"]);
    assert_eq!(relations.a_to_b.len(), 1);
    // The workspace root package owns only files outside member packages.
    let root = report.graph.relations(&["pkg:acme".into()], &[], None);
    assert_eq!(labels(&root.a), ["tools/build.build"]);
}
