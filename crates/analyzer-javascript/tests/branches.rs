use analyzer_core::LanguageStrategy;
use analyzer_javascript::JavaScript;
use std::path::Path;

#[test]
fn complexity_counts_decisions_but_not_arithmetic() {
    let source = "function arithmetic(a,b){ return a+b; } function logical(a,b){ return a && b; } function coalesce(a,b){ return a ?? b; } function choice(a,b){ if(a && b){ return 1; } do { b++; } while(b < 3); switch(a){ case 1: return 2; default: return 3; } }";
    let strategy = JavaScript;
    let path = Path::new("complexity.js");
    let ast = strategy.parse(source, path);
    assert!(ast.errors.is_empty(), "{:?}", ast.errors);
    let file = strategy.normalize(source, path, &ast);
    let arithmetic = file
        .functions
        .iter()
        .find(|f| f.name == "arithmetic")
        .unwrap();
    let choice = file.functions.iter().find(|f| f.name == "choice").unwrap();
    assert_eq!(arithmetic.complexity, 1);
    assert_eq!(
        file.functions
            .iter()
            .find(|f| f.name == "logical")
            .unwrap()
            .complexity,
        2
    );
    assert_eq!(
        file.functions
            .iter()
            .find(|f| f.name == "coalesce")
            .unwrap()
            .complexity,
        2
    );
    assert!(choice.complexity >= 5, "complexity {}", choice.complexity);
}

#[test]
fn native_getters_and_setters_are_marked_accessors() {
    let source="class Box { get value() { return 1; } set value(v) { this.stored = v; } ordinary() { return 2; } }";
    let strategy = analyzer_javascript::JavaScript;
    let path = Path::new("accessors.js");
    let ast = strategy.parse(source, path);
    let file = strategy.normalize(source, path, &ast);
    assert!(ast.errors.is_empty(), "{:?}", ast.errors);
    assert_eq!(
        file.functions
            .iter()
            .filter(|f| f.modifiers.iter().any(|m| m == "accessor"))
            .count(),
        2
    );
    assert!(file
        .functions
        .iter()
        .find(|f| f.name == "ordinary")
        .unwrap()
        .modifiers
        .iter()
        .all(|m| m != "accessor"));
}

#[test]
fn javascript_branch_guards() {
    let source = r#"
function sink(x) { return x; }
function flow(value) {
  const one = value?.run();
  const two = value && sink(1);
  const three = value || sink(2);
  const four = value ? sink(3) : sink(4);
  switch (value) { case 1: sink(5); break; default: sink(6); }
  try { sink(7); } catch (e) { sink(8); } finally { sink(9); }
  return one ?? two;
}
"#;
    let strategy = JavaScript;
    let path = Path::new("branches.js");
    let ast = strategy.parse(source, path);
    let file = strategy.normalize(source, path, &ast);
    assert!(ast.errors.is_empty(), "{:?}", ast.errors);
    let flow = file.functions.iter().find(|f| f.name == "flow").unwrap();
    assert!(flow
        .calls
        .iter()
        .any(|c| c.name == "run" && c.guard.iter().any(|g| g.kind == "ShortCircuit")));
    assert!(flow
        .calls
        .iter()
        .any(|c| c.name == "sink" && c.guard.iter().any(|g| g.kind == "Ternary")));
    assert!(flow
        .calls
        .iter()
        .any(|c| c.name == "sink" && c.guard.iter().any(|g| g.kind == "Switch/Match")));
    assert!(flow
        .calls
        .iter()
        .any(|c| c.name == "sink" && c.guard.iter().any(|g| g.kind == "TryCatch")));
}

#[test]
fn commonjs_and_object_methods() {
    let source="const api = { run(value) { return helper(value); } }; function helper(v) { return v; } module.exports.start = function start() { return api.run(1); };";
    let strategy = JavaScript;
    let path = Path::new("commonjs.cjs");
    let ast = strategy.parse(source, path);
    let file = strategy.normalize(source, path, &ast);
    assert!(ast.errors.is_empty());
    assert!(file.functions.iter().any(|f| f.name == "run"));
    assert!(file.functions.iter().any(|f| f.name == "helper"));
    assert!(file.functions.iter().any(|f| f.name == "start"));
    assert!(file
        .functions
        .iter()
        .find(|f| f.name == "run")
        .unwrap()
        .calls
        .iter()
        .any(|c| c.name == "helper"));
}

#[test]
fn strict_equality_else_guard_preserves_coercion_semantics() {
    let source = "function flow(value) { if (value === 0) { zero(); } else { other(); } }";
    let strategy = JavaScript;
    let path = Path::new("strict.js");
    let ast = strategy.parse(source, path);
    assert!(ast.errors.is_empty(), "{:?}", ast.errors);
    let file = strategy.normalize(source, path, &ast);
    let flow = file.functions.iter().find(|f| f.name == "flow").unwrap();
    let other = flow.calls.iter().find(|c| c.name == "other").unwrap();
    assert_eq!(other.guard[0].normalized, "value !== 0");
}
