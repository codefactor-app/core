use analyzer_core::{
    graph::{CallGraph, NodeKind},
    Confidence, Config, FileModel, LanguageRegistry, LanguageStrategy,
};
use std::{collections::BTreeSet, path::Path};

struct TempProject(std::path::PathBuf);
impl TempProject {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "codefactor-js-script-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn model(path: &str, source: &str) -> FileModel {
    let file = analyzer_javascript::JavaScript.analyze(source, Path::new(path));
    assert!(file.parse_errors.is_empty(), "{:?}", file.parse_errors);
    file
}
fn graph(files: &[FileModel]) -> CallGraph {
    let mut registry = LanguageRegistry::new();
    registry.register(analyzer_javascript::JavaScript);
    CallGraph::build(files, &registry, &Config::default())
}
fn resolved(graph: &CallGraph, from: &str) -> BTreeSet<String> {
    graph
        .edges
        .iter()
        .filter(|edge| edge.from.ends_with(from) && edge.confidence == Confidence::Resolved)
        .filter_map(|edge| {
            graph
                .nodes
                .iter()
                .find(|node| node.id == edge.to.as_ref() && node.kind == NodeKind::Function)
                .map(|node| node.label.clone())
        })
        .collect()
}

#[test]
fn object_methods_and_captured_const_functions_have_exact_targets() {
    let file = model(
        "objects.js",
        r#"
const object = { run() {}, forward() { object.run(); } };
function outer() {
    const inner = function() {};
    const arrow = () => inner();
    arrow();
    function nested() { inner(); object.run(); }
    nested();
}
"#,
    );
    let graph = graph(&[file]);
    assert_eq!(
        resolved(&graph, "objects.object.forward"),
        BTreeSet::from(["objects.object.run".into()])
    );
    assert!(resolved(&graph, "objects.outer.arrow").contains("objects.outer.inner"));
    assert!(resolved(&graph, "objects.outer.nested").contains("objects.outer.inner"));
    assert!(resolved(&graph, "objects.outer.nested").contains("objects.object.run"));
}

#[test]
fn browser_scripts_publish_only_proven_global_callables() {
    let provider = model(
        "provider.js",
        r#"
function helper() { return document.createElement('div'); }
const Box = { run() {}, forward() { Box.run(); } };
window.Box = Box;
window.Filter = { apply() { Box.run(); helper(); }, forward() { Filter.apply(); } };
"#,
    );
    let consumer = model("consumer.js", "function entry() { document.title; Box.run(); window.Box.run(); Filter.apply(); helper(); }");
    let graph = graph(&[provider, consumer]);
    assert_eq!(
        resolved(&graph, "consumer.entry"),
        BTreeSet::from([
            "provider.Box.run".into(),
            "provider.window.Filter.apply".into(),
            "provider.helper".into()
        ])
    );
    assert!(
        resolved(&graph, "provider.window.Filter.forward").contains("provider.window.Filter.apply")
    );
}

#[test]
fn explicit_publications_from_unconditional_blocks_preserve_private_bindings() {
    let provider = model("provider.js", "'use strict'; { const privateHelper = () => {}; const Box = { run() { privateHelper(); } }; window.Box = Box; window.Filter = { apply() { Box.run(); } }; }");
    let consumer = model(
        "consumer.js",
        "function entry() { document.title; Box.run(); Filter.apply(); privateHelper(); }",
    );
    let graph = graph(&[provider, consumer]);
    assert_eq!(
        resolved(&graph, "consumer.entry"),
        BTreeSet::from([
            "provider.Box.run".into(),
            "provider.window.Filter.apply".into()
        ])
    );
}

#[test]
fn local_parameters_catches_and_declarations_shadow_browser_globals() {
    let provider = model(
        "provider.js",
        "function helper() { document.title; } window.Box = { run() {} };",
    );
    let consumer = model(
        "consumer.js",
        r#"
function parameters(Box, helper) { document.title; Box.run(); helper(); }
function local() { const Box = runtime(); const helper = runtime(); Box.run(); helper(); }
function caught() { try { document.title; } catch (Box) { Box.run(); } }
function loop() { for (const Box of runtime()) { Box.run(); } }
function closure(Box) { return () => { document.title; Box.run(); }; }
"#,
    );
    let graph = graph(&[provider, consumer]);
    for name in [
        "consumer.parameters",
        "consumer.local",
        "consumer.caught",
        "consumer.loop",
        "consumer.closure.<lambda_1>",
    ] {
        assert!(resolved(&graph, name).is_empty(), "{name}: {graph:?}");
    }
    assert!(
        !graph
            .edges
            .iter()
            .any(|edge| edge.from.contains("consumer") && edge.to.contains("provider")),
        "{graph:?}"
    );
}

#[test]
fn module_commonjs_and_unproven_files_do_not_publish_script_globals() {
    let consumer = model(
        "consumer.js",
        "function entry() { document.title; helper(); Box.run(); }",
    );
    for (path, source) in [
        ("provider.js", "export function helper() { document.title; } window.Box = { run() {} };"),
        ("provider.js", "const api = require('api'); function helper() { document.title; } window.Box = { run() {} };"),
        ("provider.js", "function helper() { document.title; } window.Box = { run() {} }; module.exports = {};"),
        ("provider.mjs", "function helper() { document.title; } window.Box = { run() {} };"),
        ("provider.cjs", "function helper() { document.title; } window.Box = { run() {} };"),
        ("provider.js", "function helper() {} const Box = { run() {} };"),
    ] {
        let provider = model(path, source);
        assert!(!provider.browser_script, "{path}: {source}");
        assert!(resolved(&graph(&[provider, consumer.clone()]), "consumer.entry").is_empty(), "{path}: {source}");
    }
    let provider = model(
        "provider.js",
        "function helper() { document.title; } window.Box = { run() {} };",
    );
    let module_consumer = model(
        "consumer.js",
        "export function entry() { document.title; helper(); Box.run(); }",
    );
    assert!(resolved(&graph(&[provider, module_consumer]), "consumer.entry").is_empty());
}

#[test]
fn explicit_package_module_context_overrides_browser_source_evidence() {
    for package_type in ["module", "commonjs"] {
        let temp = TempProject::new();
        std::fs::write(
            temp.path().join("package.json"),
            format!("{{\"type\":\"{package_type}\"}}"),
        )
        .unwrap();
        let config = Config::default();
        let context = analyzer_core::resolution::ResolutionContext::load(temp.path(), &config);
        let mut registry = LanguageRegistry::new();
        registry.register(analyzer_javascript::JavaScript);
        let files = [
            model(
                "provider.js",
                "function helper() { document.title; } window.Box = { run() {} };",
            ),
            model(
                "consumer.js",
                "function entry() { document.title; helper(); Box.run(); }",
            ),
        ];
        let graph = CallGraph::build_with_context(&files, &registry, &config, &context);
        assert!(
            resolved(&graph, "consumer.entry").is_empty(),
            "{package_type}: {graph:?}"
        );
    }
}

#[test]
fn object_mutations_obey_source_order_and_aliases() {
    let file = model(
        "mutation.js",
        r#"
function flow() {
    const object = { run() {} };
    const alias = object;
    object.run();
    alias.run = runtime;
    object.run();
}
function unrelated(key, runtime) { const object = { run() {} }; runtime[key] = runtime; object.run(); }
function computed(key, runtime) { const object = { run() {} }; const alias = object; alias[key] = runtime; object.run(); }
function descriptor(runtime) { const object = { run() {} }; Object.defineProperty(object, 'run', { value: runtime }); object.run(); }
"#,
    );
    let graph = graph(&[file]);
    assert_eq!(
        graph
            .edges
            .iter()
            .filter(|edge| edge.from.ends_with("mutation.flow")
                && edge.to.ends_with("mutation.flow.object.run")
                && edge.confidence == Confidence::Resolved)
            .count(),
        1,
        "{graph:?}"
    );
    assert!(
        resolved(&graph, "mutation.unrelated").contains("mutation.unrelated.object.run"),
        "{graph:?}"
    );
    assert!(
        resolved(&graph, "mutation.computed").is_empty(),
        "{graph:?}"
    );
    assert!(
        resolved(&graph, "mutation.descriptor").is_empty(),
        "{graph:?}"
    );
}

#[test]
fn duplicate_global_providers_and_dynamic_global_mutations_stay_unresolved() {
    let provider = model(
        "provider.js",
        "function helper() { document.title; } window.Box = { run() {} };",
    );
    let consumer = model(
        "consumer.js",
        "function entry() { document.title; Box.run(); helper(); }",
    );
    let duplicate = model(
        "other.js",
        "function helper() { document.title; } window.Box = { run() {} };",
    );
    assert!(resolved(
        &graph(&[provider.clone(), duplicate, consumer.clone()]),
        "consumer.entry"
    )
    .is_empty());
    for source in [
        "function mutate(key, value) { document.title; Box[key] = value; helper = value; }",
        "function mutate(value) { document.title; window.Box.run = value; window.helper = value; }",
        "function mutate(key, value) { document.title; Reflect.set(Box, key, value); helper = value; }",
        "function mutate(value) { document.title; Object.assign(Box, value); helper = value; }",
        "function mutate(value) { document.title; Object.assign(window, value); }",
        "function mutate(value) { document.title; Reflect.set(window, 'Box', value); Reflect.set(window, 'helper', value); }",
    ] {
        let writer = model("writer.js", source);
        assert!(resolved(&graph(&[provider.clone(), writer, consumer.clone()]), "consumer.entry").is_empty(), "{source}");
    }
}

#[test]
fn escaped_objects_and_eval_or_with_scopes_do_not_gain_literal_targets() {
    for source in [
        "function flow() { const object = { run() {} }; unknown(object); object.run(); }",
        "function flow() { const object = { run() {} }; let alias = object; alias.run = runtime; object.run(); }",
        "function flow() { const object = { run() {} }; const holder = { object }; unknown(holder); object.run(); }",
        "function flow() { const object = { run() {} }; eval(runtime); object.run(); }",
        "function flow(runtime) { const object = { run() {} }; with (runtime) { object.run(); } }",
    ] {
        let file = model("escape.js", source);
        assert!(resolved(&graph(&[file]), "escape.flow").is_empty(), "{source}");
    }
}

#[test]
fn runtime_global_replacements_and_conditional_publications_are_not_proofs() {
    let consumer = model(
        "consumer.js",
        "function entry() { document.title; Box.run(); }",
    );
    for source in [
        "window = runtime; window.Box = { run() {} };",
        "if (runtime) { window.Box = { run() {} }; }",
        "function publish() { window.Box = { run() {} }; }",
        "eval(runtime); window.Box = { run() {} };",
        "const window = runtime; window.Box = { run() {} };",
    ] {
        let file = model("provider.js", source);
        assert!(
            resolved(&graph(&[file, consumer.clone()]), "consumer.entry").is_empty(),
            "{source}"
        );
    }
}

#[test]
fn cross_file_global_writers_also_invalidate_provider_local_calls() {
    let provider = model("provider.js", "function helper() { document.title; } const Box = { run() {} }; window.Box = Box; window.Alias = Box; function entry() { Box.run(); helper(); }");
    let writer = model(
        "writer.js",
        "function mutate(key, value) { document.title; Alias[key] = value; helper = value; }",
    );
    assert!(resolved(&graph(&[provider, writer]), "provider.entry").is_empty());
}
