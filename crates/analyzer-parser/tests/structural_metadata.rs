use analyzer_parser::{normalize, Dialect};
use std::path::Path;

fn javascript(source: &str) -> analyzer_core::FileModel {
    let file = normalize(
        source,
        Path::new("score.js"),
        tree_sitter_javascript::LANGUAGE.into(),
        Dialect::JavaScript,
    );
    assert!(file.parse_errors.is_empty(), "{:?}", file.parse_errors);
    file
}

#[test]
fn cognitive_score_depends_on_control_structure_and_not_calls() {
    let minimal = javascript("function run(a, b) { if (a) { while (b) {} } else {} }");
    let with_calls = javascript("function run(a, b) { begin(); if (a) { before(); while (b) { work(); } after(); } else { fallback(); } end(); }");
    assert_eq!(minimal.functions[0].cognitive_complexity, Some(4));
    assert_eq!(with_calls.functions[0].cognitive_complexity, Some(4));
}

#[test]
fn cognitive_score_groups_boolean_chains_and_excludes_nested_functions() {
    let file = javascript("function same(a, b, c) { if (a && b && c) {} } function mixed(a, b, c) { if (a && (b || c)) {} } function outer() { function inner(a) { if (a) {} } inner(); }");
    for (name, expected) in [("same", 2), ("mixed", 3), ("outer", 0), ("inner", 1)] {
        let function = file.functions.iter().find(|f| f.name == name).unwrap();
        assert_eq!(function.cognitive_complexity, Some(expected), "{name}");
    }
}

#[test]
fn cognitive_else_if_and_switch_cases_do_not_add_nested_branch_penalties() {
    let file = javascript("function chain(a, b) { if (a) {} else if (b) {} else {} } function cases(n) { switch (n) { case 1: break; case 2: break; default: break; } }");
    assert_eq!(file.functions[0].cognitive_complexity, Some(3));
    assert_eq!(file.functions[1].cognitive_complexity, Some(1));
}

#[test]
fn rust_module_metadata_preserves_external_declarations_and_path_attributes() {
    let source = "#[path = \"alternate.rs\"]\n#[allow(dead_code)]\n// between the attribute and module\nmod alternate;\nmod standard;\nmod inline { mod nested; }\n#[path = r#\"raw.rs\"#]\nmod raw;";
    let file = normalize(
        source,
        Path::new("src/lib.rs"),
        tree_sitter_rust::LANGUAGE.into(),
        Dialect::Rust,
    );
    assert!(file.parse_errors.is_empty());
    for (name, external, path) in [
        ("alternate", true, Some("alternate.rs")),
        ("standard", true, None),
        ("inline", false, None),
        ("nested", true, None),
        ("raw", true, Some("raw.rs")),
    ] {
        let module = file.types.iter().find(|typ| typ.name == name).unwrap();
        assert_eq!(module.external_module, external, "{name}");
        assert_eq!(module.module_path.as_deref(), path, "{name}");
    }
}

#[test]
fn rust_method_visibility_distinguishes_plain_pub_from_restricted_and_private() {
    let source = "struct Store; impl Store { pub fn public(&self) {} fn private(&self) {} pub(crate) fn restricted(&self) {} pub(super) fn parent_only(&self) {} }";
    let file = normalize(
        source,
        Path::new("src/lib.rs"),
        tree_sitter_rust::LANGUAGE.into(),
        Dialect::Rust,
    );
    assert!(file.parse_errors.is_empty());
    for (name, private) in [
        ("public", false),
        ("private", true),
        ("restricted", true),
        ("parent_only", true),
    ] {
        let method = file.functions.iter().find(|f| f.name == name).unwrap();
        assert_eq!(
            method
                .modifiers
                .iter()
                .any(|modifier| modifier == "private"),
            private,
            "{name}"
        );
    }
}

#[test]
fn rust_trait_declarations_and_local_implementations_inherit_trait_visibility() {
    // The public impl intentionally precedes its trait declaration.
    let source = r#"
struct Store;
impl Public for Store { fn public_impl(&self) {} }
pub trait Public { fn public_impl(&self); fn public_default(&self) {} }
trait Private { fn private_impl(&self); fn private_default(&self) {} }
pub(crate) trait Restricted { fn restricted_impl(&self); }
impl Private for Store { fn private_impl(&self) {} }
impl Restricted for Store { fn restricted_impl(&self) {} }
mod api {
    pub trait Visible { fn visible(&self); }
    pub(super) trait Limited { fn limited(&self); }
}
mod consumer {
    struct Other;
    use super::api::Visible as Imported;
    impl Imported for Other { fn visible(&self) {} }
    impl super::api::Limited for Other { fn limited(&self) {} }
}
"#;
    let file = normalize(
        source,
        Path::new("src/lib.rs"),
        tree_sitter_rust::LANGUAGE.into(),
        Dialect::Rust,
    );
    assert!(file.parse_errors.is_empty());
    for (fqn, private) in [
        ("src::lib::Public::public_impl", false),
        ("src::lib::Public::public_default", false),
        ("src::lib::Store::public_impl", false),
        ("src::lib::Private::private_impl", true),
        ("src::lib::Private::private_default", true),
        ("src::lib::Store::private_impl", true),
        ("src::lib::Restricted::restricted_impl", true),
        ("src::lib::Store::restricted_impl", true),
        ("src::lib::api::Visible::visible", false),
        ("src::lib::api::Limited::limited", true),
        ("src::lib::consumer::Other::visible", false),
        ("src::lib::consumer::Other::limited", true),
    ] {
        let function = file
            .functions
            .iter()
            .find(|function| function.fqn == fqn)
            .unwrap();
        assert_eq!(
            function
                .modifiers
                .iter()
                .any(|modifier| modifier == "private"),
            private,
            "{fqn}"
        );
    }
    assert_eq!(
        file.types
            .iter()
            .find(|typ| typ.name == "Public")
            .unwrap()
            .visibility
            .as_deref(),
        Some("public")
    );
    assert_eq!(
        file.types
            .iter()
            .find(|typ| typ.name == "Private")
            .unwrap()
            .visibility
            .as_deref(),
        Some("private")
    );
}

#[test]
fn methods_of_external_traits_are_not_inferred_private_from_missing_pub() {
    let source = "struct Store; use external::Trait as Imported; impl Imported for Store { fn imported(&self) {} } impl external::Other for Store { fn qualified(&self) {} } impl Store { fn inherent(&self) {} }";
    let file = normalize(
        source,
        Path::new("src/lib.rs"),
        tree_sitter_rust::LANGUAGE.into(),
        Dialect::Rust,
    );
    assert!(file.parse_errors.is_empty());
    for name in ["imported", "qualified"] {
        let function = file
            .functions
            .iter()
            .find(|function| function.name == name)
            .unwrap();
        assert!(!function
            .modifiers
            .iter()
            .any(|modifier| modifier == "private"));
    }
    assert!(file
        .functions
        .iter()
        .find(|function| function.name == "inherent")
        .unwrap()
        .modifiers
        .iter()
        .any(|modifier| modifier == "private"));
}

#[test]
fn files_without_call_sites_retain_browser_exports_costs_and_architecture_facts() {
    let file = analyzer_parser::normalize_with_architecture(
        "document; function helper() { let value = 0; value += 1; return value; }",
        Path::new("provider.js"),
        tree_sitter_javascript::LANGUAGE.into(),
        Dialect::JavaScript,
        true,
    );
    assert!(file.parse_errors.is_empty());
    assert!(file
        .functions
        .iter()
        .all(|function| function.calls.is_empty()));
    let helper = file
        .functions
        .iter()
        .find(|function| function.name == "helper")
        .unwrap();
    assert!(helper.cost_ir.is_some());
    assert_eq!(helper.return_points, 1);
    assert_eq!(helper.cognitive_complexity, Some(0));
    assert!(file.architecture_facts.is_some());
    assert!(file.browser_script);
    assert!(file
        .script_globals
        .iter()
        .any(|global| global.name == "helper"
            && global.target.as_deref() == Some(helper.fqn.as_str())));
}

#[test]
fn nested_call_sites_and_destructured_property_reads_still_use_enclosing_types() {
    let file = normalize(
        "class Box { run() {} get value() { return 1; } } function outer(box: Box) { function inner() { box.run(); } const {value} = box; return inner; }",
        Path::new("score.ts"),
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Dialect::TypeScript,
    );
    assert!(file.parse_errors.is_empty(), "{:?}", file.parse_errors);
    let outer = file
        .functions
        .iter()
        .find(|function| function.name == "outer")
        .unwrap();
    assert!(outer.calls.is_empty());
    let read = outer
        .property_accesses
        .iter()
        .find(|access| access.site.name == "value")
        .unwrap();
    assert_eq!(read.site.receiver_type.as_deref(), Some("score.Box"));
    let inner = file
        .functions
        .iter()
        .find(|function| function.name == "inner")
        .unwrap();
    let call = inner.calls.iter().find(|call| call.name == "run").unwrap();
    assert_eq!(call.receiver_type.as_deref(), Some("score.Box"));
}
