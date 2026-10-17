use analyzer_core::{
    graph::CallGraph, Confidence, Config, FileModel, LanguageRegistry, LanguageStrategy,
};
use analyzer_javascript::JavaScript;
use std::path::Path;

fn analyze(source: &str) -> (FileModel, CallGraph) {
    let file = JavaScript.analyze(source, Path::new("dynamics.js"));
    assert!(file.parse_errors.is_empty(), "{:?}", file.parse_errors);
    let mut registry = LanguageRegistry::new();
    registry.register(JavaScript);
    let graph = CallGraph::build(std::slice::from_ref(&file), &registry, &Config::default());
    (file, graph)
}
fn targets<'a>(graph: &'a CallGraph, caller: &str) -> Vec<&'a analyzer_core::graph::GraphEdge> {
    graph
        .edges
        .iter()
        .filter(|edge| edge.from.ends_with(&format!(".{caller}")))
        .collect()
}

#[test]
fn proven_symbol_fields_do_not_shadow_string_methods_or_accessors() {
    let (file, graph) = analyze(
        r#"
        const KEY = Symbol.for('state');
        class Parent { get value() { return 1; } }
        class Store extends Parent { [KEY] = 2; run() {} }
        function use() { const store = new Store(); store.run(); void store.value; }
        function local() {
            const KEY = Symbol('local');
            class Local extends Parent { [KEY] = 3; run() {} probe() { this.run(); void this.value; } }
        }
    "#,
    );
    assert_eq!(
        file.functions
            .iter()
            .flat_map(|function| &function.calls)
            .filter(|call| call.symbol_constant_assignment)
            .count(),
        1
    );
    assert!(file
        .types
        .iter()
        .flat_map(|ty| &ty.fields)
        .all(|field| field.unknown_key && field.symbol_key_reference.is_some()));
    for caller in ["use", "probe"] {
        let edges = targets(&graph, caller);
        for suffix in [".run", ".value#get"] {
            assert!(
                edges.iter().any(
                    |edge| edge.to.ends_with(suffix) && edge.confidence == Confidence::Resolved
                ),
                "{caller}: {edges:?}"
            );
        }
    }
}

#[test]
fn symbol_proofs_require_direct_const_initializers_and_native_intrinsics() {
    for (name, declarations) in [
        ("mutable", "let KEY = Symbol.for('state'); KEY = 'value';"),
        ("conditional", "const KEY = flag ? Symbol.for('state') : 'value';"),
        ("indirect", "const OTHER = Symbol.for('state'); const KEY = OTHER;"),
        ("shadowed", "const Symbol = {for(){return 'value';}}; const KEY = Symbol.for('state');"),
        ("replaced", "Symbol = fake; const KEY = Symbol.for('state');"),
        ("method_replaced", "Symbol.for = () => 'value'; const KEY = Symbol.for('state');"),
        ("object_descriptor", "Object.defineProperty(Symbol, 'for', {value: () => 'value'}); const KEY = Symbol.for('state');"),
        ("reflect_descriptor", "Reflect.defineProperty(Symbol, 'for', {value: () => 'value'}); const KEY = Symbol.for('state');"),
        ("global_member", "globalThis.Symbol.for = () => 'value'; const KEY = Symbol.for('state');"),
        ("global_bracket", "globalThis.Symbol['for'] = () => 'value'; const KEY = Symbol.for('state');"),
        ("global_symbol_bracket", "globalThis['Symbol']['for'] = () => 'value'; const KEY = Symbol.for('state');"),
        ("wrapped_object_descriptor", "Object.defineProperty((Symbol), 'for', {value: () => 'value'}); const KEY = Symbol.for('state');"),
        ("wrapped_reflect_descriptor", "Reflect.defineProperty((Symbol), 'for', {value: () => 'value'}); const KEY = Symbol.for('state');"),
        ("global_object_descriptor", "Object.defineProperty((globalThis.Symbol), 'for', {value: () => 'value'}); const KEY = Symbol.for('state');"),
        ("global_reflect_descriptor", "Reflect.defineProperty((globalThis.Symbol), 'for', {value: () => 'value'}); const KEY = Symbol.for('state');"),
    ] {
        let (file, graph) = analyze(&format!(r#"
            {declarations}
            class Parent {{ get value() {{ return 1; }} }}
            class Store extends Parent {{ [KEY] = 2; }}
            function use() {{ const store = new Store(); void store.value; }}
        "#));
        let field = file.types.iter().find(|ty| ty.name == "Store").unwrap().fields.first().unwrap();
        assert!(field.unknown_key && field.symbol_key_reference.is_none(), "{name}: {field:?}");
        assert!(!targets(&graph, "use").iter().any(|edge| edge.to.ends_with("#get")),
            "{name}: {:?}", graph.edges);
        if name != "indirect" {
            assert!(!file.functions.iter().flat_map(|function| &function.calls)
                .any(|call| call.symbol_constant_assignment), "{name}: {:?}", file.functions);
        }
    }
}

#[test]
fn imported_symbol_kind_survives_reexports_but_not_local_shadowing() {
    let files = [
        ("symbols.js", "export const KEY = Symbol.for('state');"),
        ("barrel.js", "export * from './symbols.js';"),
        (
            "reader.js",
            r#"
            import {KEY} from './barrel.js';
            class Parent { get value() { return 1; } }
            class Store extends Parent { [KEY] = 2; run() {} }
            function use() { const store = new Store(); store.run(); void store.value; }
            function shadow() {
                let KEY = 'value';
                class Local extends Parent { [KEY] = 2; probe() { void this.value; } }
                const store = new Local(); void store.value;
            }
            function otherBlock(flag) {
                if (flag) { const KEY = Symbol('local'); }
                class Local extends Parent { [KEY] = 2; probe() { void this.value; } }
                const store = new Local(); void store.value;
            }
            function constShadow() {
                const KEY = 'value';
                class Local extends Parent { [KEY] = 2; probe() { void this.value; } }
            }
        "#,
        ),
    ]
    .into_iter()
    .map(|(path, source)| JavaScript.analyze(source, Path::new(path)))
    .collect::<Vec<_>>();
    assert!(files.iter().all(|file| file.parse_errors.is_empty()));
    let mut registry = LanguageRegistry::new();
    registry.register(JavaScript);
    let graph = CallGraph::build(&files, &registry, &Config::default());
    let edges = targets(&graph, "use");
    for suffix in [".run", ".value#get"] {
        assert!(
            edges
                .iter()
                .any(|edge| edge.to.ends_with(suffix) && edge.confidence == Confidence::Resolved),
            "{edges:?}"
        );
    }
    assert!(
        !targets(&graph, "shadow.Local.probe")
            .iter()
            .any(|edge| edge.to.ends_with("#get")),
        "{:?}",
        graph.edges
    );
    assert!(
        !targets(&graph, "constShadow.Local.probe")
            .iter()
            .any(|edge| edge.to.ends_with("#get")),
        "{:?}",
        graph.edges
    );
    assert!(
        targets(&graph, "otherBlock.Local.probe")
            .iter()
            .any(|edge| edge.to.ends_with("#get") && edge.confidence == Confidence::Resolved),
        "{:?}",
        graph.edges
    );
}

#[test]
fn block_symbol_consts_cannot_prove_an_unrelated_string_export() {
    let files = [
        ("symbols.js", "export let KEY = 'value'; { const KEY = Symbol.for('state'); }"),
        ("reader.js", "import {KEY} from './symbols.js'; class Parent {get value(){return 1;}} class Store extends Parent {[KEY]=2;} function use(){const store=new Store();void store.value;}"),
    ].into_iter().map(|(path, source)| JavaScript.analyze(source, Path::new(path))).collect::<Vec<_>>();
    let mut registry = LanguageRegistry::new();
    registry.register(JavaScript);
    let graph = CallGraph::build(&files, &registry, &Config::default());
    assert!(
        !targets(&graph, "use")
            .iter()
            .any(|edge| edge.to.ends_with("#get")),
        "{:?}",
        graph.edges
    );
}

#[test]
fn unresolved_bare_symbol_imports_do_not_borrow_same_named_local_module_proofs() {
    let files = [
        ("symbols.js", "export const KEY = Symbol.for('state');"),
        ("reader.js", "import {KEY} from 'symbols'; class Parent {get value(){return 1;}} class Store extends Parent {[KEY]=2;} function use(){const store=new Store();void store.value;}"),
    ].into_iter().map(|(path, source)| JavaScript.analyze(source, Path::new(path))).collect::<Vec<_>>();
    let mut registry = LanguageRegistry::new();
    registry.register(JavaScript);
    let graph = CallGraph::build(&files, &registry, &Config::default());
    assert!(
        !targets(&graph, "use")
            .iter()
            .any(|edge| edge.to.ends_with("#get")),
        "{:?}",
        graph.edges
    );
}

#[test]
fn computed_string_proofs_follow_const_scopes_escapes_and_templates() {
    let (_, graph) = analyze(
        r#"
        const prefix = 'val'; const key = `${prefix}ue`;
        class Store { get [key]() { return 1; } plain = 2; }
        function use() {
            const store = new Store();
            void store[key]; void store['val\u0075e']; void store['val' + 'ue'];
            { const key = 'plain'; void store[key]; }
            void store[key];
        }
        function shadow(key) { const store = new Store(); void store[key]; }
        function tdz() { const store = new Store(); void store[key]; const key = 'value'; }
        function mutable() { const store = new Store(); let key = 'value'; void store[key]; }
    "#,
    );
    assert_eq!(
        targets(&graph, "use")
            .iter()
            .filter(|edge| edge.to.ends_with(".value#get"))
            .count(),
        4,
        "{:?}",
        graph.edges
    );
    for caller in ["shadow", "tdz", "mutable"] {
        assert!(
            !targets(&graph, caller)
                .iter()
                .any(|edge| edge.to.ends_with("#get")),
            "{caller}: {:?}",
            graph.edges
        );
    }
}

#[test]
fn source_ordered_known_instance_descriptors_use_real_distinct_function_ranges() {
    let (file, graph) = analyze(
        r#"
        class Store { get value() { return 0; } }
        function use() {
            const store = new Store(); const alias = store;
            void store.value;
            Object.defineProperty(alias, 'value', {get() { return 1; }, set(n) {}});
            void store.value; store.value = 2; store.value();
            Object.defineProperties(store, {first: {get() { return 3; }}, second: {get() { return 4; }}});
            void alias.first; void alias.second;
            Object.defineProperty(store, 'value', {value: 3});
            void store.value;
        }
    "#,
    );
    let edges = targets(&graph, "use");
    assert_eq!(
        edges
            .iter()
            .filter(|edge| edge.to.ends_with(".value#get"))
            .count(),
        1,
        "{edges:?}"
    );
    let descriptors = edges
        .iter()
        .filter(|edge| edge.to.contains("#descriptor_"))
        .collect::<Vec<_>>();
    assert_eq!(descriptors.len(), 5, "{edges:?}");
    assert!(descriptors
        .iter()
        .all(|edge| edge.confidence == Confidence::Resolved));
    let names = file
        .functions
        .iter()
        .filter(|function| function.name.starts_with("get#descriptor_"))
        .map(|function| &function.fqn)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(names.len(), 3, "{:?}", file.functions);
    for edge in descriptors {
        assert!(
            graph.nodes.iter().any(|node| node.id == edge.to.as_ref()
                && node.file.is_some()
                && node.line.is_some()),
            "{edge:?}"
        );
    }
}

#[test]
fn unknown_descriptors_conditions_and_rebindings_do_not_restore_old_getters() {
    let (_, graph) = analyze(
        r#"
        class Store { get value() { return 0; } }
        function use(descriptor, flag) {
            let store = new Store();
            Object.defineProperty(store, 'value', {get() { return 1; }});
            store = new Store();
            void store.value;
            Object.defineProperty(store, 'value', descriptor);
            void store.value;
            if (flag) Object.defineProperty(store, 'value', {get() { return 2; }});
            void store.value;
        }
    "#,
    );
    assert!(
        !targets(&graph, "use")
            .iter()
            .any(|edge| edge.to.ends_with("#get") || edge.to.contains("#descriptor_")),
        "{:?}",
        graph.edges
    );
}

#[test]
fn prototype_static_and_cross_function_mutations_invalidate_the_correct_space() {
    let (_, graph) = analyze(
        r#"
        class Store { get value() { return 0; } static get value() { return 1; } }
        Object.defineProperty(Store.prototype, 'value', {get() { return 2; }});
        function read(store) { const local = new Store(); void local.value; void Store.value; }
        function use() {
            const store = new Store();
            Object.defineProperty(Store, 'other', {get() { return 3; }});
            void Store.other;
            Object.defineProperty(Store.prototype, 'value', {get() { return 4; }});
            void store.value;
            Object.defineProperty(Store, 'other', {value: 5});
            void Store.other;
        }
    "#,
    );
    let read = targets(&graph, "read");
    assert!(
        !read.iter().any(|edge| edge.to.ends_with(".value#get")),
        "{read:?}"
    );
    assert_eq!(
        read.iter()
            .filter(|edge| edge.to.ends_with(".value#static#get"))
            .count(),
        1,
        "{read:?}"
    );
    assert_eq!(
        targets(&graph, "use")
            .iter()
            .filter(|edge| edge.to.contains("#descriptor_"))
            .count(),
        2,
        "{:?}",
        graph.edges
    );
}

#[test]
fn shadowed_object_api_and_unknown_computed_keys_do_not_invent_descriptors() {
    let (_, graph) = analyze(
        r#"
        class Store {}
        function use(Object, key) {
            const store = new Store();
            Object.defineProperty(store, 'value', {get() { return 1; }});
            void store.value;
            void store[key];
        }
    "#,
    );
    assert!(
        !targets(&graph, "use")
            .iter()
            .any(|edge| edge.to.contains("#descriptor_")),
        "{:?}",
        graph.edges
    );
}

#[test]
fn finite_receiver_alternatives_keep_exact_target_sets_and_ambiguous_confidence() {
    let (_, graph) = analyze(
        r#"
        class A { get value() { return 1; } run() {} }
        class B { get value() { return 2; } run() {} }
        class Data { value = 3; }
        function use(flag) {
            let store = new A(); if (flag) store = new B();
            void store.value; store.run();
            let second = flag ? new A() : new B(); void second.value;
            let third = new A(); third ||= new B(); void third.value;
            let mixed = flag ? new A() : new Data(); void mixed.value;
        }
        function unknown(flag, other) { let store = new A(); if (flag) store = other; void store.value; store.run(); }
    "#,
    );
    let edges = targets(&graph, "use")
        .into_iter()
        .filter(|edge| edge.to.contains(".A.") || edge.to.contains(".B."))
        .collect::<Vec<_>>();
    assert_eq!(edges.len(), 9, "{edges:?}");
    assert!(
        edges
            .iter()
            .all(|edge| edge.confidence == Confidence::Ambiguous),
        "{edges:?}"
    );
    assert_eq!(
        edges
            .iter()
            .filter(|edge| edge.to.ends_with(".A.run"))
            .count(),
        1
    );
    assert_eq!(
        edges
            .iter()
            .filter(|edge| edge.to.ends_with(".B.run"))
            .count(),
        1
    );
    assert!(
        !targets(&graph, "unknown")
            .iter()
            .any(|edge| edge.to.contains(".A.") || edge.to.contains(".B.")),
        "{:?}",
        graph.edges
    );
}

#[test]
fn loop_const_shadowing_and_replaced_object_api_remain_unknown() {
    let (file, graph) = analyze(
        r#"
        const key = 'x';
        class Box { get x() { return 1; } get y() { return 2; } }
        function loop() { const box = new Box(); for (const key of ['y']) { void box[key]; } }
        function changed(fake) {
            const box = new Box();
            Object = fake;
            Object.defineProperty(Box.prototype, 'x', {get() { return 3; }});
            void box.x;
        }
    "#,
    );
    assert!(
        !targets(&graph, "loop")
            .iter()
            .any(|edge| edge.to.ends_with("#get")),
        "{:?}",
        graph.edges
    );
    assert!(
        !targets(&graph, "changed")
            .iter()
            .any(|edge| edge.to.ends_with("#get") || edge.to.contains("#descriptor_")),
        "{:?}",
        graph.edges
    );
    assert!(
        file.functions
            .iter()
            .find(|function| function.name == "changed")
            .unwrap()
            .property_mutations[0]
            .unknown
    );
}

#[test]
fn own_get_only_descriptors_do_not_inherit_setters_and_prototype_changes_keep_instance_fields() {
    let (_, graph) = analyze(
        r#"
        class Parent { get value() { return 1; } set value(n) {} }
        class OwnField extends Parent { value = 2; }
        function use() {
            const store = new Parent();
            Object.defineProperty(store, 'value', {get() { return 3; }});
            store.value = 4;
            void store.value;
            const field = new OwnField();
            Object.defineProperty(OwnField.prototype, 'value', {get() { return 5; }});
            void field.value;
        }
    "#,
    );
    let edges = targets(&graph, "use");
    assert!(
        !edges.iter().any(|edge| edge.to.ends_with(".value#set")),
        "{edges:?}"
    );
    assert_eq!(
        edges
            .iter()
            .filter(|edge| edge.to.contains("#descriptor_"))
            .count(),
        1,
        "{edges:?}"
    );
}

#[test]
fn delete_and_reflect_descriptor_changes_invalidate_cross_function_getters() {
    for change in [
        "delete Store.prototype.value;",
        "Reflect.defineProperty(Store.prototype, 'value', descriptor);",
        "Reflect.deleteProperty(Store.prototype, 'value');",
    ] {
        let source = format!("class Store {{ get value() {{ return 1; }} }} {change} function read() {{ const store = new Store(); void store.value; }}");
        let (_, graph) = analyze(&source);
        assert!(
            !targets(&graph, "read")
                .iter()
                .any(|edge| edge.to.ends_with(".value#get")),
            "{change}: {:?}",
            graph.edges
        );
    }
}

#[test]
fn duplicate_descriptor_map_keys_and_unknown_partial_updates_never_restore_a_getter() {
    let (_, graph) = analyze(
        r#"
        class Store { run() {} get value() { return 1; } }
        function use(descriptor) {
            const store = new Store();
            store.run();
            Object.defineProperty(store, 'run', {value: 2});
            store.run();
            Object.defineProperties(store, {value: {get() { return 3; }}, value: {set(n) {}}});
            void store.value;
            Object.defineProperty(Store.prototype, 'value', descriptor);
            Object.defineProperty(Store.prototype, 'value', {set(n) {}});
            void store.value;
        }
    "#,
    );
    let edges = targets(&graph, "use");
    assert_eq!(
        edges
            .iter()
            .filter(|edge| edge.to.ends_with(".Store.run"))
            .count(),
        1,
        "{edges:?}"
    );
    assert!(
        !edges
            .iter()
            .any(|edge| edge.to.ends_with("#get") || edge.to.contains("get#descriptor_")),
        "{edges:?}"
    );
}

#[test]
fn unknown_local_mutations_do_not_poison_unrelated_project_functions() {
    let (_, graph) = analyze(
        r#"
        class Store { get value() { return 1; } run() {} }
        class Other { get value() { return 2; } run() {} }
        function noise(object, key, descriptor) {
            delete object[key]; Object.defineProperty(object, key, descriptor);
        }
        function change(descriptor) { Reflect.defineProperty(Store.prototype, 'value', descriptor); }
        function read() { const other = new Other(); void other.value; other.run(); }
        function local(object, key) {
            const other = new Other(); delete object[key]; void other.value; other.run();
        }
    "#,
    );
    let read = targets(&graph, "read");
    assert_eq!(
        read.iter()
            .filter(|edge| edge.to.ends_with(".Other.value#get"))
            .count(),
        1,
        "{read:?}"
    );
    assert_eq!(
        read.iter()
            .filter(|edge| edge.to.ends_with(".Other.run"))
            .count(),
        1,
        "{read:?}"
    );
    assert!(
        !targets(&graph, "local")
            .iter()
            .any(|edge| edge.to.ends_with(".Other.value#get") || edge.to.ends_with(".Other.run")),
        "{:?}",
        graph.edges
    );
}
