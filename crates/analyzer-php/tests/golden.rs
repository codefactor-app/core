use analyzer_core::LanguageStrategy;
use analyzer_php::Php;
use std::path::Path;

#[test]
fn php_architecture_golden() {
    let src = include_str!("fixtures/architecture.php");
    let p = Path::new("architecture.php");
    let s = Php;
    let ast = s.parse(src, p);
    let file = s.normalize(src, p, &ast);
    assert!(ast.errors.is_empty());
    assert_eq!(file.types.len(), 3);
    assert_eq!(file.functions.len(), 5);
    assert_eq!(
        file.types
            .iter()
            .find(|t| t.name == "Service")
            .unwrap()
            .fields
            .len(),
        1
    );
    assert!(file
        .functions
        .iter()
        .find(|f| f.name == "run")
        .unwrap()
        .accessed_fields
        .contains(&"store".to_string()));
    assert!(
        file.functions
            .iter()
            .find(|f| f.name == "run")
            .unwrap()
            .calls
            .iter()
            .any(|c| c.name == "query"
                && c.receiver_type.as_deref() == Some("\\App\\Database\\Store"))
    );
    assert!(file.functions.iter().any(|f| f.name == "run"
        && f.calls
            .iter()
            .any(|c| c.name == "query" && !c.in_loop.is_empty() && !c.guard.is_empty())));
}

#[test]
fn deeply_nested_concatenation_scan_and_ast_are_stack_safe() {
    // Mirrors PHP-CS-Fixer issue_8828_c, far beyond a Rayon worker's stack.
    let expression = std::iter::repeat_n("\" text \".$variable", 20_000)
        .collect::<Vec<_>>()
        .join(".");
    let source = format!("<?php if (true) {{ $foo = {expression}; }}");
    let worker = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || {
            let file = Php.analyze(&source, Path::new("deep.php"));
            assert!(file.parse_errors.is_empty());
            assert!(file.functions.is_empty());
            let ast = Php.parse(&source, Path::new("deep.php"));
            assert_eq!(ast.errors.len(), 1);
            assert!(ast.errors[0].message.contains("detailed AST incomplete"));
            assert!(ast.root.children.is_empty());
            let wrapped = format!("<?php function deep() {{ $foo = {expression}; }}");
            let file = Php.analyze(&wrapped, Path::new("deep-function.php"));
            assert!(file.parse_errors.is_empty(), "{:?}", file.parse_errors);
            let deep = file.functions.iter().find(|f| f.name == "deep").unwrap();
            assert_eq!(deep.complexity, 1);
            assert_eq!(deep.nesting_depth, 0);
            assert!(deep.calls.is_empty());
            assert!(matches!(deep.cost_ir.as_ref().unwrap().kind,
                analyzer_core::algorithmic::CostKind::Unknown(ref reason)
                    if reason.as_ref() == "cost extraction depth limit exceeded"));
            let encoded = serde_json::to_string(&file).unwrap();
            let mut decoded: analyzer_core::FileModel = serde_json::from_str(&encoded).unwrap();
            decoded.intern_range_paths();
            assert_eq!(serde_json::to_string(&decoded).unwrap(), encoded);
        })
        .unwrap();
    worker.join().unwrap();
}

#[test]
fn deep_php_calls_bindings_guards_and_initializers_survive_on_worker_stack() {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            let chain = (0..384)
                .map(|index| format!("->step{index}()"))
                .collect::<String>();
            let expression = std::iter::repeat_n("\" text \".$variable", 20_000)
                .collect::<Vec<_>>()
                .join(".")
                + ".$this->value";
            let source = format!(
                r#"<?php
class Deep {{
    private $value;
    function chain(Deep $service) {{ $service{chain}; }}
    function guarded(Deep $service, $stop, $items) {{
        if ($stop) {{ return; }}
        foreach ($items as $item) {{
            if ($item) {{ $text = {expression}; $service->inside(); }}
        }}
        $service->after();
    }}
}}
$service = new Deep();
$service{chain};
"#
            );
            for architecture in [false, true] {
                let mut file =
                    Php.analyze_with_architecture(&source, Path::new("deep.php"), architecture);
                assert!(file.parse_errors.is_empty(), "{:?}", file.parse_errors);
                let chain = file.functions.iter().find(|f| f.name == "chain").unwrap();
                assert_eq!(chain.calls.len(), 384);
                assert_eq!(chain.calls.first().unwrap().name, "step383");
                assert_eq!(chain.calls.last().unwrap().name, "step0");
                assert_eq!(chain.complexity, 1);
                assert_eq!(chain.nesting_depth, 0);
                let guarded = file.functions.iter().find(|f| f.name == "guarded").unwrap();
                assert_eq!(guarded.return_points, 1);
                assert_eq!(guarded.complexity, 4);
                assert_eq!(guarded.nesting_depth, 2);
                assert!(guarded.accessed_fields.iter().any(|name| name == "value"));
                let inside = guarded
                    .calls
                    .iter()
                    .find(|call| call.name == "inside")
                    .unwrap();
                assert_eq!(inside.receiver_type.as_deref(), Some("\\Deep"));
                assert_eq!(inside.in_loop.len(), 1);
                assert!(inside.guard.iter().any(|guard| guard.kind == "EarlyExit"));
                assert!(inside.guard.iter().any(|guard| guard.kind == "If"));
                let after = guarded
                    .calls
                    .iter()
                    .find(|call| call.name == "after")
                    .unwrap();
                assert!(after.in_loop.is_empty());
                assert_eq!(after.guard.len(), 1);
                assert_eq!(after.guard[0].kind, "EarlyExit");
                let initializer = file
                    .functions
                    .iter()
                    .find(|f| f.is_module_initializer())
                    .unwrap();
                assert_eq!(initializer.calls.len(), 385);
                file.intern_range_paths();
                let encoded = serde_json::to_string(&file).unwrap();
                let mut restored: analyzer_core::FileModel =
                    serde_json::from_str(&encoded).unwrap();
                restored.intern_range_paths();
                assert_eq!(serde_json::to_string(&restored).unwrap(), encoded);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
