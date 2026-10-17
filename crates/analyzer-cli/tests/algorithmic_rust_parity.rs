use analyzer_core::{
    algorithmic::{self, ComplexityEstimate, EstimateStatus},
    graph::CallGraph,
    Config, FileModel, LanguageRegistry,
};
use std::path::Path;

fn analyze(source: &str) -> FileModel {
    let mut registry = LanguageRegistry::default();
    registry.register(analyzer_rust::Rust);
    let strategy = registry.by_extension("rs").unwrap();
    let mut files = vec![strategy.analyze(source, Path::new("parity.rs"))];
    assert!(
        files[0].parse_errors.is_empty(),
        "{:?}",
        files[0].parse_errors
    );
    let config = Config::default();
    let mut graph = CallGraph::build(&files, &registry, &config);
    algorithmic::analyze(&mut files, &mut graph, &config);
    files.remove(0)
}

fn estimate<'a>(file: &'a FileModel, name: &str) -> &'a ComplexityEstimate {
    file.functions
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("missing {name}"))
        .algorithmic_complexity
        .as_deref()
        .unwrap()
}

fn tight(file: &FileModel, name: &str, formula: &str) {
    let estimate = estimate(file, name);
    assert_eq!(
        estimate.status,
        EstimateStatus::Tight,
        "{name}: {estimate:?}"
    );
    assert_eq!(estimate.formula.as_deref(), Some(formula), "{name}");
}

fn unknown(file: &FileModel, name: &str) {
    let estimate = estimate(file, name);
    assert_eq!(
        estimate.status,
        EstimateStatus::Unknown,
        "{name}: {estimate:?}"
    );
    assert!(estimate.formula.is_none(), "{name}: {estimate:?}");
    assert!(!estimate.unknown_reasons.is_empty(), "{name}");
}

#[test]
fn borrowed_opaque_values_have_no_implicit_destructor_but_owned_values_do() {
    let file = analyze(
        r#"
struct Request;
fn unused(_request: &Request) -> bool { true }
fn borrowed(request: &Request) -> &Request { request }
fn unused_mut(_request: &mut Request) -> bool { true }
fn owned(_request: Request) -> bool { true }
fn called(request: &Request) { request.foreign(); }
fn overloaded(value: &Request) { let result = -value; }
fn copy_generic<const N: usize>(value: &[usize; N]) -> [usize; N] { *value }
"#,
    );
    for name in ["unused", "borrowed", "unused_mut"] {
        tight(&file, name, "Θ(1)");
    }
    for name in ["owned", "called", "overloaded", "copy_generic"] {
        unknown(&file, name);
    }
}

#[test]
fn declared_struct_projection_and_scalar_mutation_have_builtin_costs() {
    let file = analyze(
        r#"
struct Counter { value: usize, enabled: bool, title: String, items: Vec<usize> }
impl Counter {
    fn read(&self) -> usize { self.value }
    fn title(&self) -> &str { &self.title }
    fn length(&self) -> usize { self.items.len() }
    fn set(&mut self, enabled: bool) { self.enabled = enabled; }
    fn increment(&mut self) { self.value += 1; }
    fn receiver(&self) -> &Self { self }
    fn explicit_receiver(self: &Self) -> usize { self.value }
    fn run(&self, n: usize) { for _i in 0..n { self.read(); } }
    fn replace(&mut self, items: Vec<usize>) { self.items = items; }
    fn owned(self) -> bool { true }
    fn field_bound(&self) { for _i in 0..self.value {} }
}
fn read_argument(counter: &Counter) -> usize { counter.value }
"#,
    );
    for name in [
        "read",
        "title",
        "length",
        "set",
        "increment",
        "receiver",
        "explicit_receiver",
        "read_argument",
    ] {
        tight(&file, name, "Θ(1)");
    }
    tight(&file, "run", "Θ(n)");
    for name in ["replace", "owned", "field_bound"] {
        unknown(&file, name);
    }
}

#[test]
fn scalar_array_literals_and_borrowing_preserve_collection_sizes() {
    let file = analyze(
        r#"
fn literal() -> [usize; 3] { [1, 2, 3] }
fn fixed_array() { for value in [1, 2, 3] { let copy = -value; } }
fn repeated_array() { let items = [0; 8]; for value in &items { let copy = value; } }
fn borrowed_items(items: &[usize]) { let values = &items; for value in values.iter() {} }
fn borrowed_mut_items(items: &mut [usize]) { for value in &mut *items { let copy = value; } }
fn borrowed_opaque_items(items: &[Request]) { for value in items.iter() { let copy = value; } }
fn borrowed_mut(value: &mut usize) { let copy = &mut *value; }
fn custom_literal(value: &Request) { let values = [value]; }
"#,
    );
    for name in ["literal", "fixed_array", "repeated_array", "borrowed_mut"] {
        tight(&file, name, "Θ(1)");
    }
    tight(&file, "borrowed_items", "Θ(items.len())");
    tight(&file, "borrowed_mut_items", "Θ(items.len())");
    tight(&file, "borrowed_opaque_items", "Θ(items.len())");
    unknown(&file, "custom_literal");
}

#[test]
fn unknown_or_macro_generated_storage_never_receives_a_complete_formula() {
    let file = analyze(
        r#"
struct Plain { value: usize }
struct Wrapper;
struct Generic<T> { value: T }
#[foreign]
// An intervening comment does not detach a proc-macro attribute.
struct Generated { value: usize }
fn missing(value: &Plain) -> usize { value.missing }
fn deref(value: &Wrapper) -> usize { value.value }
fn generic(value: &Generic<usize>) -> usize { value.value }
fn attributed(value: &Generated) -> usize { value.value }
fn enclosing() {
    struct Plain { value: Foreign }
    fn nested(value: &Plain) { let result = -value.value; }
}
"#,
    );
    for name in ["missing", "deref", "generic", "attributed", "nested"] {
        unknown(&file, name);
    }
}
