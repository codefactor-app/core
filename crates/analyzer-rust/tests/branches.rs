use analyzer_core::{graph::CallGraph, Confidence, Config, LanguageRegistry, LanguageStrategy};
use analyzer_rust::Rust;
use std::path::Path;

#[test]
fn rust_match_try_macro_and_closure() {
    let source = r#"
fn sink(x: i32) -> i32 { x }
fn fallible() -> Result<i32, ()> { Ok(1) }
fn flow(xs: Vec<i32>) -> Result<i32, ()> {
    let value = fallible()?;
    let transform = |x| sink(x);
    for x in xs {
        match x {
            n if n > 0 => { sink(n); }
            _ => { println!("none"); }
        }
    }
    Ok(sink(value))
}
"#;
    let strategy = Rust;
    let path = Path::new("branches.rs");
    let ast = strategy.parse(source, path);
    let file = strategy.normalize(source, path, &ast);
    assert!(ast.errors.is_empty(), "{:?}", ast.errors);
    let flow = file.functions.iter().find(|f| f.name == "flow").unwrap();
    assert!(flow.calls.iter().any(|c| c.name == "sink"
        && c.guard
            .iter()
            .any(|g| g.kind == "EarlyExit" || g.kind == "ShortCircuit")));
    assert!(flow.calls.iter().any(|c| c.name == "sink"
        && c.in_loop.iter().any(|l| l.header == "for x in xs")
        && c.guard
            .iter()
            .any(|g| g.kind == "Switch/Match" && g.raw == "n if n > 0")));
    assert!(flow
        .calls
        .iter()
        .any(|c| c.kind == analyzer_core::CallKind::Macro));
    assert!(file
        .functions
        .iter()
        .any(|f| f.name == "transform" && f.parent_function.as_deref() == Some("branches::flow")));
}

#[test]
fn scoped_rust_modules_do_not_leak_names() {
    let source = "mod inner { fn nested() {} } fn sibling() {}";
    let strategy = Rust;
    let path = Path::new("module.rs");
    let ast = strategy.parse(source, path);
    let file = strategy.normalize(source, path, &ast);
    assert!(ast.errors.is_empty());
    assert!(file
        .functions
        .iter()
        .any(|f| f.fqn == "module::inner::nested"));
    assert!(file.functions.iter().any(|f| f.fqn == "module::sibling"));
}

#[test]
fn generic_trait_bounds_resolve_all_known_implementations() {
    let source = r#"
trait Repository { fn query(&self); }
struct First;
struct Second;
impl Repository for First { fn query(&self) {} }
impl Repository for Second { fn query(&self) {} }
fn generic<T: Repository>(repo: &T) { repo.query(); }
fn constrained<T>(repo: &T) where T: Repository { repo.query(); }
fn dynamic(repo: &dyn Repository) { repo.query(); }
"#;
    let strategy = Rust;
    let path = Path::new("bounds.rs");
    let ast = strategy.parse(source, path);
    let file = strategy.normalize(source, path, &ast);
    assert!(ast.errors.is_empty(), "{:?}", ast.errors);
    for name in ["generic", "constrained", "dynamic"] {
        let function = file.functions.iter().find(|f| f.name == name).unwrap();
        assert_eq!(
            function.parameters[0].type_name.as_deref(),
            Some("Repository"),
            "{name}"
        );
    }
    let mut registry = LanguageRegistry::new();
    registry.register(Rust);
    let graph = CallGraph::build(&[file], &registry, &Config::default());
    for caller in ["generic", "constrained", "dynamic"] {
        let edges = graph
            .edges
            .iter()
            .filter(|e| e.from.ends_with(&format!("::{caller}")))
            .collect::<Vec<_>>();
        assert!(
            edges.iter().any(|e| e.to.ends_with("::First::query")),
            "{caller}: {edges:?}"
        );
        assert!(
            edges.iter().any(|e| e.to.ends_with("::Second::query")),
            "{caller}: {edges:?}"
        );
        assert!(
            edges.iter().all(|e| e.confidence == Confidence::Ambiguous),
            "{caller}: {edges:?}"
        );
    }
}

#[test]
fn conditional_question_mark_does_not_guard_later_unconditional_calls() {
    let source =
        "fn flow(enabled: bool) -> Result<(), ()> { if enabled { fallible()?; } after(); Ok(()) }";
    let strategy = analyzer_rust::Rust;
    let path = Path::new("conditional.rs");
    let ast = strategy.parse(source, path);
    assert!(ast.errors.is_empty(), "{:?}", ast.errors);
    let file = strategy.normalize(source, path, &ast);
    let flow = file.functions.iter().find(|f| f.name == "flow").unwrap();
    let after = flow.calls.iter().find(|c| c.name == "after").unwrap();
    assert!(after.guard.is_empty(), "{:?}", after.guard);
}
