use analyzer_core::{graph::CallGraph, Config, LanguageRegistry, LanguageStrategy};
use analyzer_typescript::TypeScript;
use std::path::Path;

#[test]
fn typescript_optional_and_decorators() {
    let source = r#"
function traced(_target: unknown) {}
@traced
class Service {
  private client?: { get(): string };
  run(): string | undefined { return this.client?.get(); }
}
"#;
    let strategy = TypeScript;
    let path = Path::new("branches.ts");
    let ast = strategy.parse(source, path);
    let file = strategy.normalize(source, path, &ast);
    assert!(ast.errors.is_empty(), "{:?}", ast.errors);
    assert!(file
        .types
        .iter()
        .any(|t| t.name == "Service" && t.fields.len() == 1));
    assert!(file
        .types
        .iter()
        .find(|t| t.name == "Service")
        .unwrap()
        .decorators
        .iter()
        .any(|d| d == "@traced"));
    assert!(file
        .functions
        .iter()
        .find(|f| f.name == "run")
        .unwrap()
        .calls
        .iter()
        .any(|c| c.name == "get" && c.guard.iter().any(|g| g.kind == "ShortCircuit")));
}

#[test]
fn parameter_property_and_nested_type_signature_are_not_functions() {
    let source="class Service { constructor(private readonly repository: { query(): string }) {} run() { return this.repository.query(); } }";
    let strategy = TypeScript;
    let path = Path::new("fields.ts");
    let ast = strategy.parse(source, path);
    let file = strategy.normalize(source, path, &ast);
    assert!(ast.errors.is_empty());
    assert_eq!(
        file.functions
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        vec!["constructor", "run"]
    );
    assert_eq!(
        file.types[0]
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        vec!["repository"]
    );
    assert!(file.functions[1]
        .accessed_fields
        .contains(&"repository".to_string()));
}

#[test]
fn named_imports_link_factory_and_injected_property_across_files() {
    let strategy = TypeScript;
    let repository="export class Repo { query(): void {} } export function createRepo(): Repo { return new Repo(); }";
    let service="import { Repo as Store, createRepo } from './repository'; class Service { constructor(private repo: Store) {} run() { this.repo.query(); const another = createRepo(); another.query(); } }";
    let files = [("repository.ts", repository), ("service.ts", service)]
        .into_iter()
        .map(|(path, source)| {
            let path = Path::new(path);
            let ast = strategy.parse(source, path);
            assert!(ast.errors.is_empty(), "{path:?}: {:?}", ast.errors);
            strategy.normalize(source, path, &ast)
        })
        .collect::<Vec<_>>();
    let service_file = &files[1];
    assert!(
        service_file
            .imports
            .iter()
            .any(|i| i.alias.as_deref() == Some("Store") && i.path == "./repository.Repo"),
        "{:?}",
        service_file.imports
    );
    assert!(service_file
        .imports
        .iter()
        .any(|i| i.alias.as_deref() == Some("createRepo") && i.path == "./repository.createRepo"));
    let mut registry = LanguageRegistry::new();
    registry.register(TypeScript);
    let graph = CallGraph::build(&files, &registry, &Config::default());
    let edges = graph
        .edges
        .iter()
        .filter(|e| {
            e.from.ends_with("service::Service.run") || e.from.ends_with("service.Service.run")
        })
        .collect::<Vec<_>>();
    assert!(
        edges
            .iter()
            .any(|e| e.to.ends_with("repository.createRepo")),
        "{edges:?}"
    );
    assert_eq!(
        edges
            .iter()
            .filter(|e| e.to.ends_with("repository.Repo.query"))
            .count(),
        2,
        "{edges:?}"
    );
}
