use analyzer_core::{graph::CallGraph, Config, LanguageRegistry, LanguageStrategy};
use analyzer_php::Php;
use std::path::Path;

#[test]
fn php_catalog_names_and_control_flow_complexity() {
    let operations = Php.external_operations();
    assert_eq!(
        operations
            .iter()
            .find(|r| r.library == "Doctrine")
            .unwrap()
            .operation,
        "executeQuery"
    );
    assert_eq!(
        operations
            .iter()
            .find(|r| r.library == "Guzzle")
            .unwrap()
            .operation,
        "request"
    );
    let source = "<?php function flow($items) { foreach ($items as $item) { if ($item) return $item; } return 0; }";
    let path = Path::new("complexity.php");
    let ast = Php.parse(source, path);
    assert!(ast.errors.is_empty(), "{:?}", ast.errors);
    let file = Php.normalize(source, path, &ast);
    let flow = file.functions.iter().find(|f| f.name == "flow").unwrap();
    assert!(flow.complexity >= 3, "complexity {}", flow.complexity);
}

#[test]
fn php_match_and_doc_types() {
    let source = r#"<?php
/** @param Client $client @return string */
function flow($client) {
    $value = $client?->get() ?? fallback();
    return match ($value) { 'a' => send($value), default => send('other') };
}
"#;
    let strategy = Php;
    let path = Path::new("branches.php");
    let ast = strategy.parse(source, path);
    let file = strategy.normalize(source, path, &ast);
    assert!(ast.errors.is_empty(), "{:?}", ast.errors);
    let flow = file.functions.iter().find(|f| f.name == "flow").unwrap();
    assert_eq!(flow.parameters[0].type_name.as_deref(), Some("Client"));
    assert_eq!(flow.return_type.as_deref(), Some("string"));
    assert!(flow.calls.iter().any(|c| c.name == "send"
        && c.guard
            .iter()
            .any(|g| g.kind == "Switch/Match" && g.raw == "'a'" && g.branch == "case")));
    assert!(flow.calls.iter().any(|c| c.name == "send"
        && c.guard
            .iter()
            .any(|g| g.kind == "Switch/Match" && g.branch == "default")));
    assert!(flow.calls.iter().any(|c| c.name == "fallback"
        && c.guard
            .iter()
            .any(|g| g.kind == "ShortCircuit" && g.raw == "$client?->get()")));
}

#[test]
fn phpdoc_property_type_and_attributes() {
    let source="<?php #[\\SomeAttribute] class Box { /** @var Client */ private $client; /** @return Client */ public function get() { return $this->client; } }";
    let strategy = Php;
    let path = Path::new("doc.php");
    let ast = strategy.parse(source, path);
    let file = strategy.normalize(source, path, &ast);
    assert!(ast.errors.is_empty(), "{:?}", ast.errors);
    let typ = file.types.iter().find(|t| t.name == "Box").unwrap();
    assert!(typ.decorators.iter().any(|d| d.contains("SomeAttribute")));
    assert_eq!(typ.fields[0].type_name.as_deref(), Some("Client"));
    assert_eq!(
        file.functions
            .iter()
            .find(|f| f.name == "get")
            .unwrap()
            .return_type
            .as_deref(),
        Some("Client")
    );
}

#[test]
fn constructor_injection_links_imported_interface_implementations() {
    let strategy = Php;
    let repository="<?php namespace Example; interface RepositoryInterface { public function find(): void; } class Store implements RepositoryInterface { public function find(): void {} }";
    let service="<?php namespace Example; use Example\\RepositoryInterface as Repository; class Service { private Repository $repository; public function __construct(Repository $repository) { $this->repository = $repository; } public function load(): void { $this->repository->find(); } }";
    let files = [("Repository.php", repository), ("Service.php", service)]
        .into_iter()
        .map(|(path, source)| {
            let path = Path::new(path);
            let ast = strategy.parse(source, path);
            assert!(ast.errors.is_empty(), "{path:?}: {:?}", ast.errors);
            strategy.normalize(source, path, &ast)
        })
        .collect::<Vec<_>>();
    let mut registry = LanguageRegistry::new();
    registry.register(Php);
    let graph = CallGraph::build(&files, &registry, &Config::default());
    let edges = graph
        .edges
        .iter()
        .filter(|e| e.from.ends_with("Service::load"))
        .collect::<Vec<_>>();
    assert!(
        edges.iter().any(|e| e.to.ends_with("Store::find")),
        "{edges:?}"
    );
}
