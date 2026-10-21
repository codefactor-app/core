use analyzer_core::{graph::CallGraph, ir::AstNode, Config, LanguageRegistry, LanguageStrategy};
use analyzer_typescript::TypeScript;
use std::path::Path;

fn count_kind(node: &AstNode, kind: &str) -> usize {
    usize::from(node.raw_kind == kind)
        + node
            .children
            .iter()
            .map(|child| count_kind(child, kind))
            .sum::<usize>()
}

#[test]
fn semicolonless_generic_interface_call_signatures_parse_in_typescript_and_tsx() {
    let source = r#"
        interface Producer {
            <State>(recipe: (state: State) => State): (state?: State) => State
            <State, Args extends any[]>(recipe: (state: State, ...args: Args) => State,
                initialState: State): (state?: State, ...args: Args) => State
            <Base, Draft = Base>(base: Base, recipe: (draft: Draft) => Base): Base
        }
    "#;
    for name in ["signatures.ts", "signatures.tsx"] {
        let path = Path::new(name);
        let ast = TypeScript.parse(source, path);
        assert!(ast.errors.is_empty(), "{name}: {:?}", ast.errors);
        assert_eq!(count_kind(&ast.root, "call_signature"), 3);
        assert!(TypeScript
            .normalize(source, path, &ast)
            .parse_errors
            .is_empty());
    }
}

#[test]
fn newline_relational_operators_and_generic_arrows_keep_their_expression_structure() {
    for name in ["expressions.ts", "expressions.tsx"] {
        let source = "const lower = first\n < second;\nconst lowerEqual = first\n <= second;\nconst identity =\n <T,>(value: T) => value;";
        let ast = TypeScript.parse(source, Path::new(name));
        assert!(ast.errors.is_empty(), "{name}: {:?}", ast.errors);
        assert_eq!(count_kind(&ast.root, "lexical_declaration"), 3);
        assert_eq!(count_kind(&ast.root, "binary_expression"), 2);
        assert_eq!(count_kind(&ast.root, "arrow_function"), 1);
    }
}

#[test]
fn genuine_malformed_types_and_expressions_still_report_syntax_errors() {
    for name in ["broken.ts", "broken.tsx"] {
        for source in ["interface Broken { <T>(value: T):\n}", "const broken = ;"] {
            let path = Path::new(name);
            let ast = TypeScript.parse(source, path);
            assert!(!ast.errors.is_empty(), "{name}: {source}");
            assert!(!TypeScript
                .normalize(source, path, &ast)
                .parse_errors
                .is_empty());
        }
    }
}

#[test]
fn untouched_redux_and_immer_interface_sources_have_no_grammar_diagnostics() {
    for (name, source) in [
        ("redux-store.ts", include_str!("../../analyzer-cli/tests/call_resolution_corpus/projects/redux/src/types/store.ts")),
        ("immer-types.ts", include_str!("../../analyzer-cli/tests/call_resolution_corpus/projects/immer/src/types/types-external.ts")),
    ] {
        let path = Path::new(name);
        let ast = TypeScript.parse(source, path);
        assert!(ast.errors.is_empty(), "{name}: {:?}", ast.errors);
        assert!(TypeScript.normalize(source, path, &ast).parse_errors.is_empty());
    }
}

#[test]
fn module_initializer_collects_runtime_calls_with_real_ranges_and_keeps_callbacks_separate() {
    let source = "function work() {}\nfunction each(callback:()=>void) {callback();}\nlet ready=true;\nif (ready) { each(()=>{work();}); }\n";
    let file = TypeScript.analyze(source, Path::new("module.ts"));
    assert!(file.parse_errors.is_empty(), "{:?}", file.parse_errors);
    let initializer = file
        .functions
        .iter()
        .find(|f| f.is_module_initializer())
        .unwrap();
    assert_eq!(initializer.fqn, "module.<module_initializer>");
    assert_eq!(initializer.name, "<module_initializer>");
    assert!(initializer.parent_function.is_none() && initializer.parent_type.is_none());
    assert_eq!(initializer.calls.len(), 1);
    assert_eq!(initializer.calls[0].name, "each");
    assert_eq!(initializer.calls[0].range.start_line, 4);
    assert!(initializer.calls[0]
        .guard
        .iter()
        .any(|guard| guard.normalized.contains("ready")));
    let callback = file
        .functions
        .iter()
        .find(|f| f.name.starts_with("<lambda_"))
        .unwrap();
    assert!(
        callback.parent_function.is_none(),
        "existing callback identity must stay unchanged"
    );
    assert!(callback.fqn.starts_with("module.<lambda_"));
    assert_eq!(callback.calls[0].name, "work");
    let mut registry = LanguageRegistry::new();
    registry.register(TypeScript);
    let graph = CallGraph::build(&[file], &registry, &Config::default());
    let edge = graph
        .edges
        .iter()
        .find(|e| e.from.ends_with("module.<module_initializer>"))
        .unwrap();
    assert!(edge.to.ends_with("module.each"), "{edge:?}");
    assert_eq!(edge.callsite.start_line, 4);
    assert!(!graph
        .edges
        .iter()
        .any(|e| e.from.ends_with("module.<module_initializer>") && e.to.ends_with("module.work")));
}

#[test]
fn imports_and_type_declarations_do_not_create_an_initializer() {
    let source = "import type {Missing} from './missing';\ninterface Store { read():void; }\ntype Return = typeof work;\nfunction work() {}\nclass Example { instance = work(); method(){work();} }\n";
    let file = TypeScript.analyze(source, Path::new("declarations.ts"));
    assert!(file.parse_errors.is_empty(), "{:?}", file.parse_errors);
    assert!(!file.functions.iter().any(|f| f.is_module_initializer()));
}

#[test]
fn class_definition_evaluates_static_values_keys_and_blocks_but_not_instance_values_or_methods() {
    let source = "function staticValue(){} function instanceValue(){} function key(){return 'key';} function block(){} function method(){}\nclass Example {\n static value=staticValue();\n value=instanceValue();\n [key()]=instanceValue();\n static { block(); }\n method(){method();}\n}\n";
    let file = TypeScript.analyze(source, Path::new("class.ts"));
    assert!(file.parse_errors.is_empty(), "{:?}", file.parse_errors);
    let initializer = file
        .functions
        .iter()
        .find(|f| f.is_module_initializer())
        .unwrap();
    let calls = initializer
        .calls
        .iter()
        .map(|c| c.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(calls, ["staticValue", "key", "block"]);
    assert_eq!(
        initializer
            .calls
            .iter()
            .map(|c| c.range.start_line)
            .collect::<Vec<_>>(),
        [3, 5, 6]
    );
}

#[test]
fn module_import_binding_resolves_initializer_call_across_files() {
    let files = [
        TypeScript.analyze("export function each(cb:()=>void){}", Path::new("dep.ts")),
        TypeScript.analyze(
            "import {each} from './dep';\neach(()=>{});",
            Path::new("app.ts"),
        ),
    ];
    let mut registry = LanguageRegistry::new();
    registry.register(TypeScript);
    let graph = CallGraph::build(&files, &registry, &Config::default());
    let edges = graph
        .edges
        .iter()
        .filter(|e| e.from.ends_with("app.<module_initializer>"))
        .collect::<Vec<_>>();
    assert_eq!(edges.len(), 1, "{edges:?}");
    assert!(edges[0].to.ends_with("dep.each"));
    assert_eq!(edges[0].callsite.start_line, 2);
}

#[test]
fn enum_and_namespace_initializers_keep_runtime_calls() {
    let source = "enum E { X = initEnum() }\nnamespace N { initNamespace(); }\n";
    for extension in ["ts", "tsx"] {
        let name = format!("runtime.{extension}");
        let file = TypeScript.analyze(source, Path::new(&name));
        assert!(file.parse_errors.is_empty(), "{:?}", file.parse_errors);
        let initializer = file
            .functions
            .iter()
            .find(|f| f.is_module_initializer())
            .unwrap();
        assert_eq!(
            initializer
                .calls
                .iter()
                .map(|call| call.name.as_str())
                .collect::<Vec<_>>(),
            ["initEnum", "initNamespace"]
        );
        assert_eq!(
            initializer
                .calls
                .iter()
                .map(|call| call.range.start_line)
                .collect::<Vec<_>>(),
            [1, 2]
        );
    }
}
