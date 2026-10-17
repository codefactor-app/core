use analyzer_core::{
    graph::{CallGraph, GraphEdge},
    ir::{AccessorKind, FileModel},
    Config, LanguageRegistry, LanguageStrategy,
};
use analyzer_typescript::TypeScript;
use std::path::Path;

fn analyze(source: &str) -> (FileModel, CallGraph) {
    let file = TypeScript.analyze(source, Path::new("property.ts"));
    assert!(file.parse_errors.is_empty(), "{:?}", file.parse_errors);
    let mut registry = LanguageRegistry::new();
    registry.register(TypeScript);
    let graph = CallGraph::build(&[file.clone()], &registry, &Config::default());
    (file, graph)
}

fn outgoing<'a>(graph: &'a CallGraph, caller: &str) -> Vec<&'a GraphEdge> {
    graph
        .edges
        .iter()
        .filter(|edge| edge.from.ends_with(&format!(".{caller}")))
        .collect()
}

#[test]
fn property_read_write_compound_update_and_literal_brackets_call_the_correct_accessor() {
    let (file, graph) = analyze(
        r#"
        function read() { return 1; } function write(n: number) {}
        class Store { plain = 0; get value() { return read(); } set value(n: number) { write(n); } }
        function use(store: Store) {
            const a = store.value;
            store.value = 1;
            store.value += 2;
            store.value++;
            ++store.value;
            store.value &&= 3;
            store.value ||= 4;
            store.value ??= 5;
            const b = store["value"];
            store['value'] = 6;
            const c = store.plain;
            store.plain = 7;
            delete store.value;
        }
    "#,
    );
    let caller = file
        .functions
        .iter()
        .find(|function| function.name == "use")
        .unwrap();
    assert!(
        caller.calls.is_empty(),
        "property operations are not explicit calls"
    );
    assert_eq!(
        outgoing(&graph, "use")
            .iter()
            .filter(|edge| edge.to.ends_with(".value#get"))
            .count(),
        8
    );
    assert_eq!(
        outgoing(&graph, "use")
            .iter()
            .filter(|edge| edge.to.ends_with(".value#set"))
            .count(),
        8
    );
    assert_eq!(
        outgoing(&graph, "use").len(),
        16,
        "ordinary fields and delete must not invent calls"
    );
    for access in caller.property_accesses.iter().filter(|access| {
        access.access == AccessorKind::Setter
            && access.site.range.start_line >= 10
            && access.site.range.start_line <= 12
    }) {
        assert!(access
            .site
            .guard
            .iter()
            .any(|guard| guard.kind == "ShortCircuit"));
    }
}

#[test]
fn optional_typed_getter_chains_preserve_loop_and_branch_conditions() {
    let (_, graph) = analyze(
        r#"
        class Child { get value(): number { return 1; } }
        class Store { get child(): Child { return new Child(); } }
        function use(store: Store, ready: boolean) {
            if (ready) { for (let n = 0; n < 2; n++) { const value = store?.child.value; } }
        }
    "#,
    );
    let edges = outgoing(&graph, "use");
    assert_eq!(
        edges.len(),
        2,
        "both getter operations in the chain must resolve: {edges:?}"
    );
    for edge in edges {
        assert_eq!(edge.in_loop.len(), 1);
        assert!(edge.guard.iter().any(|guard| guard.raw.contains("ready")));
        assert!(edge
            .guard
            .iter()
            .any(|guard| guard.normalized.contains("store != null")));
    }
}

#[test]
fn descriptor_overrides_block_missing_inherited_getters_setters_and_data_fields() {
    let (_, graph) = analyze(
        r#"
        class Parent { get value(): number { return 1; } set value(n: number) {} }
        class SetterOnly extends Parent { set value(n: number) {} }
        class GetterOnly extends Parent { get value(): number { return 2; } }
        class FieldOnly extends Parent { value: number = 3; }
        function use(a: SetterOnly, b: GetterOnly, c: FieldOnly, p: Parent) {
            const x = a.value; a.value = 1;
            const y = b.value; b.value = 2;
            const z = c.value; c.value = 3;
            const normal = p.value; p.value = 4;
        }
    "#,
    );
    let targets = outgoing(&graph, "use")
        .iter()
        .map(|edge| edge.to.as_ref())
        .collect::<Vec<_>>();
    assert_eq!(targets.len(), 4, "{targets:?}");
    assert!(targets
        .iter()
        .any(|target| target.ends_with(".SetterOnly.value#set")));
    assert!(targets
        .iter()
        .any(|target| target.ends_with(".GetterOnly.value#get")));
    assert_eq!(
        targets
            .iter()
            .filter(|target| target.contains(".Parent.value#"))
            .count(),
        2
    );
}

#[test]
fn unknown_receivers_dynamic_keys_and_source_order_do_not_guess_accessors() {
    let (_, graph) = analyze(
        r#"
        class Store { get value(): number { return 1; } }
        function use(unknown: any, key: string) {
            const a = unknown.value;
            let store = unknown;
            const b = store.value;
            store = new Store();
            const c = store.value;
            const d = store[key];
            { const store = unknown; const e = store.value; }
            const f = store.value;
            store = unknown;
            const g = store.value;
        }
    "#,
    );
    let getters = outgoing(&graph, "use")
        .iter()
        .filter(|edge| edge.to.ends_with(".value#get"))
        .count();
    assert_eq!(getters, 2);
    assert!(
        !outgoing(&graph, "use")
            .iter()
            .any(|edge| edge.to.starts_with("unknown:") && edge.to.ends_with("value")),
        "property candidates must not create unknown calls"
    );
}

#[test]
fn object_destructuring_reads_aliases_nested_properties_and_writes_member_targets() {
    let (_, graph) = analyze(
        r#"
        class Child { get inner(): number { return 1; } }
        class Store { get value(): number { return 1; } set value(n: number) {} get child(): Child { return new Child(); } }
        function use(source: Store, target: Store) {
            const {value: alias, child: {inner}} = source;
            const {value = 3} = source;
            ({value: target.value} = source);
        }
    "#,
    );
    let edges = outgoing(&graph, "use");
    assert_eq!(
        edges
            .iter()
            .filter(|edge| edge.to.ends_with(".value#get"))
            .count(),
        3,
        "{edges:?}"
    );
    assert_eq!(
        edges
            .iter()
            .filter(|edge| edge.to.ends_with(".value#set"))
            .count(),
        1
    );
    assert_eq!(
        edges
            .iter()
            .filter(|edge| edge.to.ends_with(".child#get"))
            .count(),
        1
    );
    assert_eq!(
        edges
            .iter()
            .filter(|edge| edge.to.ends_with(".inner#get"))
            .count(),
        1
    );
}

#[test]
fn callable_getter_is_not_duplicated_and_factory_bindings_are_source_ordered() {
    let (_, graph) = analyze(
        r#"
        class Store { get value(): () => number { return () => 1; } set value(fn: () => number) {} }
        function make(): Store { return new Store(); }
        function use() { const store = make(); store.value(); const fn = store.value; }
    "#,
    );
    let edges = outgoing(&graph, "use");
    assert_eq!(
        edges
            .iter()
            .filter(|edge| edge.to.ends_with(".value#get"))
            .count(),
        2,
        "{edges:?}"
    );
    assert!(!edges.iter().any(|edge| edge.to.ends_with(".value#set")));
}

#[test]
fn static_and_instance_accessors_resolve_their_receiver_kinds() {
    let (_, graph) = analyze(
        r#"
        class Store {
            static get value(): number { return 1; }
            get value(): number { return 2; }
        }
        function use(store: Store) { const a = Store.value; const b = store.value; }
    "#,
    );
    assert_eq!(outgoing(&graph, "use").len(), 2);
    assert!(outgoing(&graph, "use")
        .iter()
        .any(|edge| edge.to.ends_with(".value#static#get")));
    assert!(outgoing(&graph, "use")
        .iter()
        .any(|edge| edge.to.ends_with(".value#get")));
}

#[test]
fn inherited_callable_getters_respect_setter_only_overrides_and_bracket_calls_do_not_duplicate_reads(
) {
    let (_, graph) = analyze(
        r#"
        class Parent { get value(): () => number { return () => 1; } set value(fn: () => number) {} }
        class SetterOnly extends Parent { set value(fn: () => number) {} }
        function use(parent: Parent, child: SetterOnly) { parent["value"](); child.value(); }
    "#,
    );
    let edges = outgoing(&graph, "use");
    assert_eq!(edges.len(), 1, "{edges:?}");
    assert!(edges[0].to.ends_with(".Parent.value#get"));
}

#[test]
fn imported_aliases_and_cross_file_getter_return_types_resolve_without_name_guessing() {
    let files = [
        TypeScript.analyze("export class Child { get value(): number { return 1; } work() {} } export class Store { get child(): Child { return new Child(); } }", Path::new("src/dep.ts")),
        TypeScript.analyze("import {Store as Box} from './dep'; export function use(box: Box) { box.child.work(); return box.child.value; }", Path::new("src/main.ts")),
    ];
    assert!(files.iter().all(|file| file.parse_errors.is_empty()));
    let mut registry = LanguageRegistry::new();
    registry.register(TypeScript);
    let graph = CallGraph::build(&files, &registry, &Config::default());
    let edges = outgoing(&graph, "use");
    assert_eq!(edges.len(), 4, "{edges:?}");
    assert!(edges
        .iter()
        .any(|edge| edge.to.ends_with(".Store.child#get")));
    assert!(edges
        .iter()
        .any(|edge| edge.to.ends_with(".Child.value#get")));
    assert!(edges.iter().any(|edge| edge.to.ends_with(".Child.work")));
}

#[test]
fn getter_return_types_also_bind_explicit_method_calls_after_a_property_chain() {
    let (_, graph) = analyze(
        r#"
        class Child { work() {} }
        class Store { get child(): Child { return new Child(); } }
        function use(store: Store) { store.child.work(); }
    "#,
    );
    let edges = outgoing(&graph, "use");
    assert_eq!(edges.len(), 2, "{edges:?}");
    assert!(edges
        .iter()
        .any(|edge| edge.to.ends_with(".Store.child#get")));
    assert!(edges.iter().any(|edge| edge.to.ends_with(".Child.work")));
}

#[test]
fn logical_assignment_guards_cover_setter_and_rhs_calls() {
    let (_, graph) = analyze(
        r#"
        function next(): number { return 1; }
        class Store { get value(): number { return 1; } set value(n: number) {} }
        function use(store: Store) { store.value &&= next(); store.value ||= next(); store.value ??= next(); }
    "#,
    );
    let edges = outgoing(&graph, "use");
    for edge in edges
        .iter()
        .filter(|edge| edge.to.ends_with(".value#set") || edge.to.ends_with(".next"))
    {
        assert!(
            edge.guard.iter().any(|guard| guard.kind == "ShortCircuit"),
            "{edge:?}"
        );
    }
    assert_eq!(edges.len(), 9);
}

#[test]
fn literal_accessor_names_are_normalized_and_typescript_type_queries_are_not_runtime_reads() {
    let (_, graph) = analyze(
        r#"
        class Store { get "value"(): number { return 1; } set ['value'](n: number) {} }
        function use(store: Store) { type Kind = typeof store.value; const runtime = typeof store.value; store['value'] = 3; }
    "#,
    );
    let edges = outgoing(&graph, "use");
    assert_eq!(edges.len(), 2, "{edges:?}");
    assert!(edges
        .iter()
        .any(|edge| edge.to.ends_with(".Store.value#get")));
    assert!(edges
        .iter()
        .any(|edge| edge.to.ends_with(".Store.value#set")));
}

#[test]
fn a_callable_data_field_override_keeps_an_explicit_unknown_call_and_does_not_invoke_a_base_getter()
{
    let (_, graph) = analyze(
        r#"
        class Parent { get value(): () => number { return () => 1; } }
        class Child extends Parent { value: () => number; }
        function use(child: Child) { child.value(); }
    "#,
    );
    let edges = outgoing(&graph, "use");
    assert_eq!(edges.len(), 1, "{edges:?}");
    assert!(edges[0].to.starts_with("unknown:"));
    assert!(!edges[0].to.ends_with(".Parent.value#get"));
}

#[test]
fn static_fields_shadow_static_descriptors_without_hiding_inherited_instance_accessors() {
    let (_, graph) = analyze(
        r#"
        class Parent { static get value(): number { return 1; } get item(): number { return 2; } }
        class Child extends Parent { static value = 0; static item = 0; }
        function use(child: Child) { const a = Child.value; const b = child.item; }
    "#,
    );
    let edges = outgoing(&graph, "use");
    assert_eq!(edges.len(), 1, "{edges:?}");
    assert!(edges[0].to.ends_with(".Parent.item#get"));
}

#[test]
fn for_of_targets_only_write_and_getter_results_bind_following_method_calls() {
    let (_, graph) = analyze(
        r#"
        class Child { work() {} }
        class Store { get child(): Child { return new Child(); } get value(): number { return 1; } set value(n: number) {} }
        function use(store: Store, values: number[]) { for (store.value of values) {} const child = store.child; child.work(); }
    "#,
    );
    let edges = outgoing(&graph, "use");
    assert_eq!(edges.len(), 3, "{edges:?}");
    assert!(edges.iter().any(|edge| edge.to.ends_with(".value#set")));
    assert!(!edges.iter().any(|edge| edge.to.ends_with(".value#get")));
    assert!(edges.iter().any(|edge| edge.to.ends_with(".Child.work")));
}

#[test]
fn conditional_logical_rebinding_does_not_claim_a_definite_receiver_type_after_the_branch() {
    let (_, graph) = analyze(
        r#"
        class A { get value(): number { return 1; } }
        class B { get value(): number { return 2; } }
        function use(flag: boolean) { let store = new A(); flag &&= (store = new B()); const value = store.value; }
    "#,
    );
    let getters = outgoing(&graph, "use")
        .into_iter()
        .filter(|edge| edge.to.ends_with(".value#get"))
        .collect::<Vec<_>>();
    assert_eq!(getters.len(), 2, "{getters:?}");
    assert!(getters
        .iter()
        .all(|edge| edge.confidence == analyzer_core::Confidence::Ambiguous));
}

#[test]
fn computed_literal_data_fields_shadow_accessors_and_cast_getter_aliases_keep_their_type() {
    let (_, graph) = analyze(
        r#"
        class Parent { get value(): number { return 1; } }
        class Child extends Parent { ["value"] = 2; work() {} }
        class Store { get child(): Child { return new Child(); } }
        function use(store: Store, child: Child) { const value = child.value; const alias = store.child as Child; alias.work(); }
    "#,
    );
    let edges = outgoing(&graph, "use");
    assert_eq!(edges.len(), 2, "{edges:?}");
    assert!(edges.iter().any(|edge| edge.to.ends_with(".Child.work")));
    assert!(!edges
        .iter()
        .any(|edge| edge.to.ends_with(".Parent.value#get")));
}

#[test]
fn nested_functions_capture_hoisted_siblings_and_typed_parameters_but_only_arrows_inherit_this() {
    let (_, graph) = analyze(
        r#"
        class Store {
            work() {}
            method() { const arrow = () => this.work(); function regular() { this.work(); } arrow(); regular(); }
            field = () => this.work();
        }
        function outer(store: Store) {
            function first() { second(); store.work(); }
            function second() {}
            first();
        }
    "#,
    );
    assert!(outgoing(&graph, "first")
        .iter()
        .any(|edge| edge.to.ends_with(".second")));
    assert!(outgoing(&graph, "first")
        .iter()
        .any(|edge| edge.to.ends_with(".Store.work")));
    assert!(outgoing(&graph, "arrow")
        .iter()
        .any(|edge| edge.to.ends_with(".Store.work")));
    assert!(outgoing(&graph, "field")
        .iter()
        .any(|edge| edge.to.ends_with(".Store.work")));
    assert!(!outgoing(&graph, "regular")
        .iter()
        .any(|edge| edge.to.ends_with(".Store.work")));
}

#[test]
fn arrow_variable_names_do_not_come_from_a_bare_parameter_and_const_arrows_are_not_hoisted() {
    let (_, graph) = analyze(
        r#"
        function use() { before(); const before = () => 1; const after = value => value; after(1); }
    "#,
    );
    let edges = outgoing(&graph, "use");
    assert!(edges
        .iter()
        .any(|edge| edge.to.starts_with("unknown:") && edge.to.ends_with("before")));
    assert!(!edges.iter().any(|edge| edge.to.ends_with(".before")));
    assert!(edges.iter().any(|edge| edge.to.ends_with(".after")));
}

#[test]
fn setter_argument_metadata_only_records_exact_direct_or_logical_assignment_rhs() {
    let (file, _) = analyze(
        r#"
        class Store { get value(): number { return 1; } set value(n: number) {} }
        function use(store: Store, values: number[]) {
            store.value = 7;
            store.value += 2;
            store.value++;
            for (store.value of values) {}
            store.value &&= 3;
            store.value ||= 4;
            store.value ??= 5;
        }
    "#,
    );
    let caller = file
        .functions
        .iter()
        .find(|function| function.name == "use")
        .unwrap();
    let arguments = caller
        .property_accesses
        .iter()
        .filter(|access| access.access == AccessorKind::Setter)
        .map(|access| access.site.arguments.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        arguments,
        [
            vec!["7"],
            vec![],
            vec![],
            vec![],
            vec!["3"],
            vec!["4"],
            vec!["5"]
        ]
    );
}
