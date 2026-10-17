use analyzer_core::{graph::CallGraph, Config, LanguageRegistry, LanguageStrategy};
use analyzer_python::Python;
use std::path::Path;

#[test]
fn python_branches_and_comprehension() {
    let source = r#"
def sink(x): return x

@decorator
def flow(xs):
    if not xs:
        return []
    values = [sink(x) for x in xs if x]
    match len(values):
        case 0: sink(0)
        case _: sink(1)
    try:
        sink(2)
    except ValueError:
        sink(3)
    finally:
        sink(4)
    return values
"#;
    let strategy = Python;
    let path = Path::new("branches.py");
    let ast = strategy.parse(source, path);
    let file = strategy.normalize(source, path, &ast);
    assert!(ast.errors.is_empty(), "{:?}", ast.errors);
    let flow = file.functions.iter().find(|f| f.name == "flow").unwrap();
    assert!(flow.modifiers.iter().any(|m| m == "@decorator"));
    let calls = flow
        .calls
        .iter()
        .filter(|c| c.name == "sink")
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 6);
    assert!(calls
        .iter()
        .any(|c| !c.in_loop.is_empty() && c.in_loop[0].header.contains("for x in xs")));
    assert!(calls
        .iter()
        .any(|c| c.guard.iter().any(|g| g.kind == "EarlyExit")));
    assert!(calls
        .iter()
        .any(|c| c.guard.iter().any(|g| g.kind == "Switch/Match")));
    assert!(calls
        .iter()
        .any(|c| c.guard.iter().any(|g| g.kind == "TryCatch")));
}

#[test]
fn annotated_injected_property_links_across_modules() {
    let strategy = Python;
    let repository = "class Repository:\n    def find(self):\n        pass\n";
    let service="from repository import Repository\nclass Service:\n    def __init__(self, repository: Repository):\n        self.repository = repository\n    def load(self):\n        return self.repository.find()\n";
    let files = [("repository.py", repository), ("service.py", service)]
        .into_iter()
        .map(|(path, source)| {
            let path = Path::new(path);
            let ast = strategy.parse(source, path);
            assert!(ast.errors.is_empty(), "{path:?}: {:?}", ast.errors);
            strategy.normalize(source, path, &ast)
        })
        .collect::<Vec<_>>();
    let mut registry = LanguageRegistry::new();
    registry.register(Python);
    let graph = CallGraph::build(&files, &registry, &Config::default());
    let edges = graph
        .edges
        .iter()
        .filter(|e| e.from.ends_with("service.Service.load"))
        .collect::<Vec<_>>();
    assert!(
        edges
            .iter()
            .any(|e| e.to.ends_with("repository.Repository.find")),
        "{edges:?}"
    );
}

#[test]
fn elif_branches_exclude_all_previous_conditions() {
    let source = "def flow(a, b, c):\n    if a:\n        first()\n    elif b:\n        second()\n    elif c:\n        third()\n    else:\n        last()\n    after()\n";
    let strategy = Python;
    let path = Path::new("branches.py");
    let ast = strategy.parse(source, path);
    assert!(ast.errors.is_empty(), "{:?}", ast.errors);
    let file = strategy.normalize(source, path, &ast);
    let flow = file.functions.iter().find(|f| f.name == "flow").unwrap();
    let conditions = |name: &str| {
        flow.calls
            .iter()
            .find(|c| c.name == name)
            .unwrap()
            .guard
            .iter()
            .map(|g| g.normalized.as_str())
            .collect::<Vec<_>>()
    };
    assert_eq!(conditions("second"), vec!["!a", "b"]);
    assert_eq!(conditions("third"), vec!["!a", "!b", "c"]);
    assert_eq!(conditions("last"), vec!["!a", "!b", "!c"]);
    assert!(conditions("after").is_empty());
}
