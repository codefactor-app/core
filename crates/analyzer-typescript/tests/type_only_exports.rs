use analyzer_core::{graph::CallGraph, Config, LanguageRegistry, LanguageStrategy};
use analyzer_typescript::TypeScript;
use std::path::Path;

#[test]
fn typescript_5_type_only_star_exports_parse_in_ts_and_tsx() {
    for name in ["exports.ts", "exports.tsx"] {
        let source = "export /* comment */ type * from './types';\nexport type * as Types from './types';\nexport type { Item } from './types';\nexport type * from './other'\nexport type * as Other from './other'\n";
        let path = Path::new(name);
        let ast = TypeScript.parse(source, path);
        assert!(ast.errors.is_empty(), "{name}: {:?}", ast.errors);
        let file = TypeScript.normalize(source, path, &ast);
        assert!(
            file.parse_errors.is_empty(),
            "{name}: {:?}",
            file.parse_errors
        );
        assert_eq!(file.exports.len(), 5);
        assert!(file.exports.iter().all(|export| export.type_only));
        assert!(!file.functions.iter().any(|f| f.is_module_initializer()));
    }
}

#[test]
fn malformed_type_only_star_exports_still_report_parse_errors() {
    for name in ["broken.ts", "broken.tsx"] {
        for source in ["export type *;", "export type * as from './types';"] {
            let ast = TypeScript.parse(source, Path::new(name));
            assert!(!ast.errors.is_empty(), "{name}: {source}");
            assert!(!TypeScript
                .normalize(source, Path::new(name), &ast)
                .parse_errors
                .is_empty());
        }
    }
}

#[test]
fn untouched_date_fns_type_exports_parse_without_diagnostics() {
    let source = include_str!(
        "../../analyzer-cli/tests/call_resolution_holdout/projects/date-fns/src/types.ts"
    );
    for name in ["date-fns-types.ts", "date-fns-types.tsx"] {
        let path = Path::new(name);
        let ast = TypeScript.parse(source, path);
        assert!(ast.errors.is_empty(), "{name}: {:?}", ast.errors);
        assert!(TypeScript
            .normalize(source, path, &ast)
            .parse_errors
            .is_empty());
    }
}

#[test]
fn type_only_reexports_do_not_publish_runtime_calls_but_value_stars_and_specifiers_do() {
    for extension in ["ts", "tsx"] {
        let sources = [
            ("dep", "export function run() {} export function type() {}"),
            ("star", "export /* comment */ type * from './dep';"),
            ("namespace", "export type * as Types from './dep';"),
            ("named", "export type { run } from './dep';"),
            ("mixed", "export { type /* comment */ run as hidden, run as visible, type as visibleType } from './dep';"),
            ("values", "export * from './dep';"),
            ("app", "import {run as fromStar} from './star';\nimport {Types} from './namespace';\nimport {run as fromNamed} from './named';\nimport {hidden, visible, visibleType} from './mixed';\nimport {run as fromValues} from './values';\nexport function entry(){ fromStar(); Types.run(); fromNamed(); hidden(); visible(); fromValues(); visibleType(); }"),
        ];
        let files = sources
            .iter()
            .map(|(name, source)| {
                TypeScript.analyze(source, Path::new(&format!("{name}.{extension}")))
            })
            .collect::<Vec<_>>();
        for file in &files {
            assert!(
                file.parse_errors.is_empty(),
                "{}: {:?}",
                file.path,
                file.parse_errors
            );
        }
        for name in ["star", "namespace", "named"] {
            let file = files.iter().find(|f| f.module == name).unwrap();
            assert_eq!(file.exports.len(), 1);
            assert!(file.exports[0].type_only, "{name}: {:?}", file.exports);
        }
        let mixed = files.iter().find(|f| f.module == "mixed").unwrap();
        assert_eq!(mixed.exports.len(), 3, "{:?}", mixed.exports);
        assert!(mixed.exports[0].type_only);
        assert_eq!(mixed.exports[1].name, "visible");
        assert!(!mixed.exports[1].type_only);
        assert_eq!(mixed.exports[2].name, "visibleType");
        assert!(!mixed.exports[2].type_only);
        let mut registry = LanguageRegistry::new();
        registry.register(TypeScript);
        let graph = CallGraph::build(&files, &registry, &Config::default());
        let caller = files.iter().find(|f| f.module == "app").unwrap();
        let entry = caller.functions.iter().find(|f| f.name == "entry").unwrap();
        for call in &entry.calls {
            let target = if call.name == "visibleType" {
                "dep.type"
            } else {
                "dep.run"
            };
            let resolved = graph
                .edges
                .iter()
                .filter(|edge| edge.callsite == call.range)
                .filter(|edge| {
                    graph.nodes.iter().any(|node| {
                        node.id == edge.to.as_ref()
                            && matches!(node.kind, analyzer_core::graph::NodeKind::Function)
                            && node.label.ends_with(target)
                    })
                })
                .count();
            let value_call = ["visible", "fromValues", "visibleType"].contains(&call.name.as_str());
            assert_eq!(
                resolved,
                usize::from(value_call),
                "{extension}: {call:?}, edges: {:?}",
                graph.edges
            );
        }
        assert_eq!(entry.calls.len(), 7);
    }
}

#[test]
fn type_only_barrels_and_imports_preserve_typed_instance_calls() {
    for extension in ["ts", "tsx"] {
        let sources = [
            ("dep", "export class Service { run() {} static make() {} constructor() {} }"),
            ("star", "export type * from './dep';"),
            ("named", "export type {Service} from './dep';"),
            ("namespace", "export type * as Types from './dep';"),
            ("app", "import type {Service as StarService} from './star';\nimport {type Service as NamedService} from './named';\nimport type {Types} from './namespace';\nfunction a(instance: StarService){instance.run();}\nfunction b(instance: NamedService){instance.run();}\nfunction c(instance: Types.Service){instance.run();}\nfunction erased(){ new StarService(); NamedService.make(); const alias=NamedService; alias.make(); }"),
        ];
        let files = sources
            .iter()
            .map(|(name, source)| {
                TypeScript.analyze(source, Path::new(&format!("{name}.{extension}")))
            })
            .collect::<Vec<_>>();
        for file in &files {
            assert!(
                file.parse_errors.is_empty(),
                "{}: {:?}",
                file.path,
                file.parse_errors
            );
        }
        let app = files.iter().find(|f| f.module == "app").unwrap();
        assert_eq!(app.imports.len(), 3);
        assert!(app.imports.iter().all(|import| import.type_only));
        let mut registry = LanguageRegistry::new();
        registry.register(TypeScript);
        let graph = CallGraph::build(&files, &registry, &Config::default());
        for function in app.functions.iter().filter(|f| !f.is_module_initializer()) {
            for call in &function.calls {
                let targets = graph
                    .edges
                    .iter()
                    .filter(|edge| edge.callsite == call.range)
                    .flat_map(|edge| {
                        graph.nodes.iter().filter(move |node| {
                            node.id == edge.to.as_ref()
                                && node.kind == analyzer_core::graph::NodeKind::Function
                        })
                    })
                    .collect::<Vec<_>>();
                if function.name == "erased" {
                    assert!(targets.is_empty(), "{extension}: {call:?}: {targets:?}");
                } else {
                    assert_eq!(targets.len(), 1, "{extension}: {call:?}: {:?}", graph.edges);
                    assert!(targets[0].label.ends_with("dep.Service.run"), "{targets:?}");
                }
            }
        }
    }
}

#[test]
fn erased_local_declarations_and_transitive_stars_cannot_bypass_runtime_export_blocking() {
    let sources = [
        ("dep", "function run(){} class C {constructor(){} static run(){} static get callable(){return run;}} export type {run, C};"),
        ("barrel", "export * from './dep';"),
        ("namespace", "export type * as Types from './dep';"),
        ("app", "import {run, C} from './dep'; import {run as viaBarrel} from './barrel'; import {Types} from './namespace'; function entry(){run(); new C(); C.run(); C.callable(); viaBarrel(); const alias=C; alias.run(); const instance=new C(); instance.run(); const nested=new Types.C(); nested.run();}"),
    ];
    let files = sources
        .iter()
        .map(|(name, source)| TypeScript.analyze(source, Path::new(&format!("{name}.ts"))))
        .collect::<Vec<_>>();
    for file in &files {
        assert!(
            file.parse_errors.is_empty(),
            "{}: {:?}",
            file.path,
            file.parse_errors
        );
    }
    let mut registry = LanguageRegistry::new();
    registry.register(TypeScript);
    let graph = CallGraph::build(&files, &registry, &Config::default());
    let app = files.iter().find(|f| f.module == "app").unwrap();
    let entry = app.functions.iter().find(|f| f.name == "entry").unwrap();
    assert_eq!(entry.calls.len(), 10);
    for call in &entry.calls {
        let targets = graph
            .edges
            .iter()
            .filter(|edge| edge.callsite == call.range)
            .flat_map(|edge| {
                graph.nodes.iter().filter(move |node| {
                    node.id == edge.to.as_ref()
                        && node.kind == analyzer_core::graph::NodeKind::Function
                })
            })
            .collect::<Vec<_>>();
        assert!(targets.is_empty(), "{call:?}: {targets:?}");
    }
}

#[test]
fn architecture_signatures_resolve_types_through_type_only_namespace_barrels() {
    let files = [
        TypeScript.analyze_with_architecture("export class Service {run(){}}", Path::new("dep.ts"), true),
        TypeScript.analyze_with_architecture("export type * as Types from './dep';", Path::new("barrel.ts"), true),
        TypeScript.analyze_with_architecture("import type {Types} from './barrel'; function entry(instance:Types.Service){instance.run();}", Path::new("app.ts"), true),
    ];
    let mut registry = LanguageRegistry::new();
    registry.register(TypeScript);
    let graph = CallGraph::build(&files, &registry, &Config::default());
    let config = Config::default();
    let (summary, _) = analyzer_core::architecture::analyze(&files, &graph, &config.architecture);
    assert!(summary.dependencies > 0, "{summary:?}");
    assert_eq!(summary.unresolved_dependencies, 0, "{summary:?} {graph:?}");
}

#[test]
fn explicitly_erased_external_constructors_cannot_produce_confident_operations() {
    let file = TypeScript.analyze("import type {Client} from 'pg'; function entry(){const client=new Client<number>(); client.query('select 1'); const alias=Client; const other=new alias(); other.query('select 2');} function typed(client:Client){client.query('select 3');}", Path::new("app.ts"));
    assert!(file.parse_errors.is_empty(), "{:?}", file.parse_errors);
    let mut registry = LanguageRegistry::new();
    registry.register(TypeScript);
    let graph = CallGraph::build(&[file.clone()], &registry, &Config::default());
    let entry = file.functions.iter().find(|f| f.name == "entry").unwrap();
    for call in &entry.calls {
        let edges = graph
            .edges
            .iter()
            .filter(|edge| edge.callsite == call.range)
            .collect::<Vec<_>>();
        assert!(!edges.is_empty(), "{call:?}");
        assert!(
            edges
                .iter()
                .all(|edge| edge.confidence != analyzer_core::Confidence::Resolved),
            "{call:?}: {edges:?}"
        );
    }
    let typed = file.functions.iter().find(|f| f.name == "typed").unwrap();
    assert!(
        graph
            .edges
            .iter()
            .any(|edge| edge.callsite == typed.calls[0].range
                && edge.confidence == analyzer_core::Confidence::Resolved),
        "{graph:?}"
    );
}
