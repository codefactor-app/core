use analyzer_core::{
    graph::CallGraph, metrics::MetricRegistry, Confidence, Config, LanguageRegistry,
    LanguageStrategy,
};
use std::path::Path;

#[test]
fn missing_delimiters_remain_parse_diagnostics_in_every_language() {
    let cases: Vec<(Box<dyn LanguageStrategy>, &str, &str)> = vec![
        (
            Box::new(analyzer_python::Python),
            "broken.py",
            "def broken(:\n    pass\n",
        ),
        (
            Box::new(analyzer_php::Php),
            "broken.php",
            "<?php function broken() { return 1;",
        ),
        (
            Box::new(analyzer_javascript::JavaScript),
            "broken.js",
            "function broken() { return 1;",
        ),
        (
            Box::new(analyzer_typescript::TypeScript),
            "broken.ts",
            "function broken() { return 1;",
        ),
        (
            Box::new(analyzer_rust::Rust),
            "broken.rs",
            "fn broken() { let x = 1;",
        ),
    ];
    for (strategy, path, source) in cases {
        let ast = strategy.parse(source, Path::new(path));
        assert!(!ast.errors.is_empty(), "{path}: {ast:?}");
        let model = strategy.analyze(source, Path::new(path));
        assert!(!model.parse_errors.is_empty(), "{path}: {model:?}");
    }
}

#[test]
fn php_untyped_parameters_do_not_acquire_sigil_or_modifier_types() {
    let model = analyzer_php::Php.analyze("<?php class C { function __construct(public $promoted) {} function run($plain, &$reference, ...$values) {} }", Path::new("params.php"));
    assert!(model.parse_errors.is_empty());
    for function in model
        .functions
        .iter()
        .filter(|f| !f.is_module_initializer())
    {
        assert!(!function.parameters.is_empty());
        assert!(
            function.parameters.iter().all(|p| p.type_name.is_none()),
            "{:?}",
            function.parameters
        );
    }
}

#[test]
fn php_static_class_names_follow_namespace_imports_and_absolute_qualification() {
    let strategy = analyzer_php::Php;
    let sources = [
        ("global.php", "<?php class Platform { static function ping() {} }"),
        ("platform.php", "<?php namespace App; class Platform { static function ping() {} }"),
        ("nested.php", "<?php namespace App\\Nested; class Platform { static function ping() {} }"),
        ("main.php", "<?php namespace App; use App\\Nested\\Platform as Alias; class Runner { static function run($class) { Platform::ping(); namespace\\Platform::ping(); Alias::ping(); Nested\\Platform::ping(); \\Platform::ping(); $class::ping(); Missing::ping(); } }"),
    ];
    let files = sources
        .iter()
        .map(|(path, source)| strategy.analyze(source, Path::new(path)))
        .collect::<Vec<_>>();
    assert!(files.iter().all(|file| file.parse_errors.is_empty()));
    let mut registry = LanguageRegistry::new();
    registry.register(strategy);
    let graph = CallGraph::build(&files, &registry, &Config::default());
    let edges = graph
        .edges
        .iter()
        .filter(|edge| edge.from.ends_with("App\\Runner::run"))
        .collect::<Vec<_>>();
    assert_eq!(edges.len(), 7, "{edges:?}");
    let targets = [
        "App\\Platform::ping",
        "App\\Platform::ping",
        "App\\Nested\\Platform::ping",
        "App\\Nested\\Platform::ping",
        ":Platform::ping",
    ];
    for (index, expected) in targets.iter().enumerate() {
        let edge = edges
            .iter()
            .find(|edge| {
                edge.callsite.start_column
                    == files[3]
                        .functions
                        .iter()
                        .find(|f| f.name == "run")
                        .unwrap()
                        .calls[index]
                        .range
                        .start_column
            })
            .unwrap();
        assert!(
            edge.to.ends_with(expected),
            "index={index} expected={expected}; calls={:?}; edges={edges:?}",
            files[3]
                .functions
                .iter()
                .find(|f| f.name == "run")
                .unwrap()
                .calls
        );
        assert_eq!(edge.confidence, Confidence::Resolved);
    }
    assert_eq!(
        edges
            .iter()
            .filter(|edge| edge.confidence == Confidence::Dynamic)
            .count(),
        2
    );
}

#[test]
fn variadic_setting_counts_parsed_parameters_across_languages() {
    let cases: Vec<(Box<dyn LanguageStrategy>, &str, &str, usize)> = vec![
        (
            Box::new(analyzer_python::Python),
            "params.py",
            "class C:\n def run(self, value, *args, **kwargs): pass\n",
            2,
        ),
        (
            Box::new(analyzer_php::Php),
            "params.php",
            "<?php class C { function run($value, ...$args) {} }",
            1,
        ),
        (
            Box::new(analyzer_javascript::JavaScript),
            "params.js",
            "class C { run(value, ...args) {} }",
            1,
        ),
        (
            Box::new(analyzer_typescript::TypeScript),
            "params.ts",
            "class C { run(value: number, ...args: number[]) {} }",
            1,
        ),
    ];
    for (strategy, path, source, variadics) in cases {
        let model = strategy.analyze(source, Path::new(path));
        assert!(model.parse_errors.is_empty(), "{path}");
        let function = model.functions.iter().find(|f| f.name == "run").unwrap();
        assert_eq!(
            function.parameters.iter().filter(|p| p.variadic).count(),
            variadics,
            "{path}: {:?}",
            function.parameters
        );
        for count in [false, true] {
            let mut config = Config::default();
            config.metrics.count_variadic_parameters = count;
            config.metrics.count_implicit_receiver = false;
            let metrics = MetricRegistry::new().calculate(&model, &config);
            let value = metrics
                .iter()
                .find(|m| m.id.as_ref() == "function_params" && m.entity.as_ref() == function.fqn)
                .unwrap()
                .value;
            assert_eq!(
                value,
                (1 + if count { variadics } else { 0 }) as f64,
                "{path}"
            );
        }
    }
}

#[test]
fn php_import_prefix_cannot_rewrite_an_already_resolved_receiver_identity() {
    let strategy = analyzer_php::Php;
    let sources = [("platform.php", "<?php namespace Composer\\Util; class Platform { static function ping() {} }"), ("caller.php", "<?php namespace Composer\\Util; use Composer\\Composer; class Runner { static function run() { Platform::ping(); } }")];
    let files = sources
        .iter()
        .map(|(path, source)| strategy.analyze(source, Path::new(path)))
        .collect::<Vec<_>>();
    let mut registry = LanguageRegistry::new();
    registry.register(strategy);
    let graph = CallGraph::build(&files, &registry, &Config::default());
    let edge = graph
        .edges
        .iter()
        .find(|edge| edge.from.ends_with("Runner::run"))
        .unwrap();
    assert_eq!(
        edge.to.as_ref(),
        "fn:php:platform.php:Composer\\Util\\Platform::ping"
    );
    assert_eq!(edge.confidence, Confidence::Resolved);
}
