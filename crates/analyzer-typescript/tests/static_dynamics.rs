use analyzer_core::{graph::CallGraph, Confidence, Config, LanguageRegistry, LanguageStrategy};
use analyzer_typescript::TypeScript;
use std::path::Path;

#[test]
fn wrapped_intrinsic_descriptor_mutations_cannot_prove_symbol_field_keys() {
    for mutation in [
        "Object.defineProperty((Symbol as any), 'for', {value: () => 'value'});",
        "Reflect.defineProperty(((globalThis.Symbol) as any), 'for', {value: () => 'value'});",
        "(globalThis.Symbol as any)['for'] = () => 'value';",
        "Object.defineProperty(<any>Symbol, 'for', {value: () => 'value'});",
        "Reflect.defineProperty(<any>(globalThis.Symbol), 'for', {value: () => 'value'});",
        "(<any>globalThis.Symbol)['for'] = () => 'value';",
        "Object.defineProperty((Symbol satisfies any), 'for', {value: () => 'value'});",
        "Reflect.defineProperty(Symbol!, 'for', {value: () => 'value'});",
        "Object.defineProperty(/* arg */ (/* value */ Symbol), 'for', {value: () => 'value'});",
    ] {
        let file = TypeScript.analyze(
            &format!(
                r#"
            {mutation}
            const KEY = Symbol.for('state');
            class Parent {{get value(): number {{ return 1; }}}}
            class Child extends Parent {{[KEY] = 2;}}
            function use(child: Child): void {{void child.value;}}
        "#
            ),
            Path::new("wrapped.ts"),
        );
        assert!(
            file.parse_errors.is_empty(),
            "{mutation}: {:?}",
            file.parse_errors
        );
        assert!(
            !file
                .functions
                .iter()
                .flat_map(|function| &function.calls)
                .any(|call| call.symbol_constant_assignment),
            "{mutation}: {:?}",
            file.functions
        );
        let field = &file
            .types
            .iter()
            .find(|ty| ty.name == "Child")
            .unwrap()
            .fields[0];
        assert!(
            field.unknown_key && field.symbol_key_reference.is_none(),
            "{mutation}: {field:?}"
        );
        let mut registry = LanguageRegistry::new();
        registry.register(TypeScript);
        let graph = CallGraph::build(&[file], &registry, &Config::default());
        assert!(
            !graph
                .edges
                .iter()
                .any(|edge| edge.from.ends_with(".use") && edge.to.ends_with("#get")),
            "{mutation}: {:?}",
            graph.edges
        );
    }
}

#[test]
fn imported_unique_symbol_field_does_not_shadow_string_accessors_or_methods() {
    let files = [
        (
            "symbols.ts",
            "export const KEY: unique symbol = Symbol.for('state');",
        ),
        ("barrel.ts", "export {KEY} from './symbols';"),
        (
            "reader.ts",
            r#"
            import {KEY} from './barrel';
            class Parent { get value(): number { return 1; } }
            class Store extends Parent { [KEY]: unknown; has(): boolean { return true; } }
            function use(store: Store): void { store.has(); void store.value; }
            function shadow(store: Store): void {
                let KEY = 'value';
                class Child extends Parent { [KEY] = 2; probe(): void { void this.value; } }
                const child = new Child(); void child.value;
            }
        "#,
        ),
    ]
    .into_iter()
    .map(|(path, source)| TypeScript.analyze(source, Path::new(path)))
    .collect::<Vec<_>>();
    assert!(
        files.iter().all(|file| file.parse_errors.is_empty()),
        "{files:?}"
    );
    let mut registry = LanguageRegistry::new();
    registry.register(TypeScript);
    let graph = CallGraph::build(&files, &registry, &Config::default());
    for suffix in [".has", ".value#get"] {
        assert!(
            graph.edges.iter().any(|edge| edge.from.ends_with(".use")
                && edge.to.ends_with(suffix)
                && edge.confidence == Confidence::Resolved),
            "{:?}",
            graph.edges
        );
    }
    assert!(
        !graph
            .edges
            .iter()
            .any(|edge| edge.from.ends_with(".shadow.Child.probe") && edge.to.ends_with("#get")),
        "{:?}",
        graph.edges
    );
}

#[test]
fn typed_conditional_receivers_and_const_descriptor_keys_survive_typescript_wrappers() {
    let source = r#"
        const suffix = 'ue' as const;
        const key = `val${suffix}`;
        class A { get [key](): number { return 1; } run(): void {} }
        class B { get value(): number { return 2; } run(): void {} }
        function use(flag: boolean) {
            let store: A | B = new A();
            if (flag) store = new B();
            void store[key]; store.run();
            const exact = new A();
            Object.defineProperty(exact, key, { get(): number { return 3; } });
            const alias = exact as A;
            void alias[key];
        }
    "#;
    let file = TypeScript.analyze(source, Path::new("dynamics.ts"));
    assert!(file.parse_errors.is_empty(), "{:?}", file.parse_errors);
    let mut registry = LanguageRegistry::new();
    registry.register(TypeScript);
    let graph = CallGraph::build(&[file], &registry, &Config::default());
    let edges = graph
        .edges
        .iter()
        .filter(|edge| edge.from.ends_with(".use"))
        .collect::<Vec<_>>();
    let alternatives = edges
        .iter()
        .filter(|edge| edge.to.ends_with(".value#get") || edge.to.ends_with(".run"))
        .collect::<Vec<_>>();
    assert_eq!(alternatives.len(), 4, "{edges:?}");
    assert!(
        alternatives
            .iter()
            .all(|edge| edge.confidence == Confidence::Ambiguous),
        "{edges:?}"
    );
    assert_eq!(
        edges
            .iter()
            .filter(
                |edge| edge.to.contains("#descriptor_") && edge.confidence == Confidence::Resolved
            )
            .count(),
        1,
        "{edges:?}"
    );
}
