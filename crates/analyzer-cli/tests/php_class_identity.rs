use analyzer_core::{
    graph::{CallGraph, NodeKind},
    Confidence, Config, FileModel, LanguageRegistry, LanguageStrategy,
};
use std::{collections::BTreeSet, path::Path};
fn model(path: &str, source: &str) -> FileModel {
    let file = analyzer_php::Php.analyze(source, Path::new(path));
    assert!(
        file.parse_errors.is_empty(),
        "{path}: {:?}",
        file.parse_errors
    );
    file
}
fn graph(files: &[FileModel]) -> CallGraph {
    let mut registry = LanguageRegistry::new();
    registry.register(analyzer_php::Php);
    CallGraph::build(files, &registry, &Config::default())
}
fn targets(graph: &CallGraph, from: &str) -> BTreeSet<String> {
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
fn namespace_relative_and_absolute_constructors_and_parameter_types_are_distinct() {
    let file = model(
        "identity.php",
        r#"<?php
namespace { class Platform { function __construct() {} function run() {} } }
namespace App {
    class Platform { function __construct() {} function run() {} }
    function localConstructor() { $p = new Platform(); $p->run(); }
    function globalConstructor() { $p = new \Platform(); $p->run(); }
    function localTyped(Platform $p) { $p->run(); }
    function globalTyped(\Platform $p) { $p->run(); }
}
"#,
    );
    let graph = graph(&[file]);
    for (caller, expected) in [
        (
            "App\\localConstructor",
            BTreeSet::from([
                "App\\Platform::__construct".into(),
                "App\\Platform::run".into(),
            ]),
        ),
        (
            "App\\globalConstructor",
            BTreeSet::from(["Platform::__construct".into(), "Platform::run".into()]),
        ),
        (
            "App\\localTyped",
            BTreeSet::from(["App\\Platform::run".into()]),
        ),
        ("App\\globalTyped", BTreeSet::from(["Platform::run".into()])),
    ] {
        assert_eq!(targets(&graph, caller), expected, "{caller}: {graph:?}");
    }
}

#[test]
fn namespace_identity_is_preserved_across_files_and_missing_local_class_does_not_fall_back() {
    let files = [
        model("global.php", "<?php class Platform { function __construct() {} function run() {} }"),
        model("local.php", "<?php namespace App; class Platform { function __construct() {} function run() {} }"),
        model("caller.php", "<?php namespace App; function local() { $p = new Platform(); $p->run(); } function globalClass() { $p = new \\Platform(); $p->run(); } function localTyped(Platform $p) { $p->run(); } function globalTyped(\\Platform $p) { $p->run(); }"),
        model("missing.php", "<?php namespace App\\Nested; function missing() { $p = new Platform(); $p->run(); } function missingTyped(Platform $p) { $p->run(); }"),
    ];
    let graph = graph(&files);
    assert_eq!(
        targets(&graph, "App\\local"),
        BTreeSet::from([
            "App\\Platform::__construct".into(),
            "App\\Platform::run".into()
        ])
    );
    assert_eq!(
        targets(&graph, "App\\globalClass"),
        BTreeSet::from(["Platform::__construct".into(), "Platform::run".into()])
    );
    assert_eq!(
        targets(&graph, "App\\localTyped"),
        BTreeSet::from(["App\\Platform::run".into()])
    );
    assert_eq!(
        targets(&graph, "App\\globalTyped"),
        BTreeSet::from(["Platform::run".into()])
    );
    assert!(
        targets(&graph, "App\\Nested\\missing").is_empty(),
        "{graph:?}"
    );
    assert!(
        targets(&graph, "App\\Nested\\missingTyped").is_empty(),
        "{graph:?}"
    );
}

#[test]
fn imported_class_and_namespace_aliases_take_precedence_without_rewriting_absolute_names() {
    let files = [
        model("global.php", "<?php class Platform { function __construct() {} function run() {} }"),
        model("local.php", "<?php namespace App; class Platform { function __construct() {} function run() {} }"),
        model("vendor.php", "<?php namespace Vendor; class Platform { function __construct() {} function run() {} }"),
        model("caller.php", r#"<?php namespace App;
use Vendor\Platform;
use Vendor as Tools;
function imported() { $p = new Platform(); $p->run(); }
function importedTyped(Platform $p) { $p->run(); }
function alias() { $p = new Tools\Platform(); $p->run(); }
function aliasTyped(Tools\Platform $p) { $p->run(); }
function globalClass() { $p = new \Platform(); $p->run(); }
function relative() { $p = new namespace\Platform(); $p->run(); }
function relativeTyped(namespace\Platform $p) { $p->run(); }
"#),
    ];
    let graph = graph(&files);
    for caller in ["App\\imported", "App\\alias"] {
        assert_eq!(
            targets(&graph, caller),
            BTreeSet::from([
                "Vendor\\Platform::__construct".into(),
                "Vendor\\Platform::run".into()
            ]),
            "{caller}: {graph:?}"
        );
    }
    for caller in ["App\\importedTyped", "App\\aliasTyped"] {
        assert_eq!(
            targets(&graph, caller),
            BTreeSet::from(["Vendor\\Platform::run".into()]),
            "{caller}: {graph:?}"
        );
    }
    assert_eq!(
        targets(&graph, "App\\globalClass"),
        BTreeSet::from(["Platform::__construct".into(), "Platform::run".into()])
    );
    assert_eq!(
        targets(&graph, "App\\relative"),
        BTreeSet::from([
            "App\\Platform::__construct".into(),
            "App\\Platform::run".into()
        ])
    );
    assert_eq!(
        targets(&graph, "App\\relativeTyped"),
        BTreeSet::from(["App\\Platform::run".into()])
    );
}

#[test]
fn qualified_relative_class_names_do_not_flatten_into_matching_root_names() {
    let files = [
        model("root.php", "<?php namespace Tools; class Platform { function __construct() {} function run() {} }"),
        model("local.php", "<?php namespace App\\Tools; class Platform { function __construct() {} function run() {} }"),
        model("caller.php", "<?php namespace App; function local() { $p = new Tools\\Platform(); $p->run(); } function root() { $p = new \\Tools\\Platform(); $p->run(); } function localTyped(Tools\\Platform $p) { $p->run(); } function rootTyped(\\Tools\\Platform $p) { $p->run(); }"),
    ];
    let graph = graph(&files);
    assert_eq!(
        targets(&graph, "App\\local"),
        BTreeSet::from([
            "App\\Tools\\Platform::__construct".into(),
            "App\\Tools\\Platform::run".into()
        ])
    );
    assert_eq!(
        targets(&graph, "App\\root"),
        BTreeSet::from([
            "Tools\\Platform::__construct".into(),
            "Tools\\Platform::run".into()
        ])
    );
    assert_eq!(
        targets(&graph, "App\\localTyped"),
        BTreeSet::from(["App\\Tools\\Platform::run".into()])
    );
    assert_eq!(
        targets(&graph, "App\\rootTyped"),
        BTreeSet::from(["Tools\\Platform::run".into()])
    );
}

#[test]
fn bracketed_namespace_module_initializers_do_not_share_constructor_bindings() {
    let file = model(
        "initializer.php",
        r#"<?php
namespace { class Platform { function __construct() {} function run() {} } }
namespace App { class Platform { function __construct() {} function run() {} } $p = new Platform(); $p->run(); }
namespace Other { $p = new \Platform(); $p->run(); }
"#,
    );
    let graph = graph(&[file]);
    assert_eq!(
        targets(&graph, "App\\<module_initializer>"),
        BTreeSet::from([
            "App\\Platform::__construct".into(),
            "App\\Platform::run".into()
        ])
    );
    assert_eq!(
        targets(&graph, "Other\\<module_initializer>"),
        BTreeSet::from(["Platform::__construct".into(), "Platform::run".into()])
    );
}

#[test]
fn primitive_and_dynamic_class_inputs_do_not_create_class_targets() {
    let file = model("dynamic.php", "<?php namespace App; class Platform { function run() {} } function dynamic($class) { $p = new $class(); $p->run(); } function primitive(int $p) { $p->run(); }");
    let graph = graph(&[file]);
    assert!(targets(&graph, "App\\dynamic").is_empty());
    assert!(targets(&graph, "App\\primitive").is_empty());
}

#[test]
fn functions_sharing_class_names_keep_function_identity() {
    for name in ["Platform", "platform"] {
        let source = format!("<?php namespace App; class {name} {{ function __construct() {{}} }} function {name}() {{}} function entry() {{ {name}(); }} function create() {{ new {name}(); }}");
        let file = model("symbols.php", &source);
        let graph = graph(&[file]);
        assert_eq!(
            targets(&graph, "App\\entry"),
            BTreeSet::from([format!("App\\{name}")])
        );
        assert_eq!(
            targets(&graph, "App\\create"),
            BTreeSet::from([format!("App\\{name}::__construct")])
        );
    }
}

#[test]
fn catch_type_annotations_use_namespace_or_explicit_global_identity() {
    let file = model(
        "catch.php",
        r#"<?php
namespace { class Platform { function run() {} } }
namespace App {
    class Platform { function run() {} }
    function localCatch() { try { unknown(); } catch (Platform $p) { $p->run(); } }
    function globalCatch() { try { unknown(); } catch (\Platform $p) { $p->run(); } }
}
"#,
    );
    let graph = graph(&[file]);
    assert!(targets(&graph, "App\\localCatch").contains("App\\Platform::run"));
    assert!(!targets(&graph, "App\\localCatch").contains("Platform::run"));
    assert!(targets(&graph, "App\\globalCatch").contains("Platform::run"));
    assert!(!targets(&graph, "App\\globalCatch").contains("App\\Platform::run"));
}
