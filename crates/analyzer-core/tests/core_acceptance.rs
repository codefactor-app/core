use analyzer_core::analysis::{Analyzer, LanguageRegistry, LanguageStrategy};
use analyzer_core::config::{Config, ConfigLoader, KeySpec};
use analyzer_core::graph::{Direction, GraphFilter};
use analyzer_core::ir::*;
use std::path::Path;
use std::sync::Arc;

struct Mini;
impl LanguageStrategy for Mini {
    fn language_id(&self) -> &'static str {
        "mini"
    }
    fn file_extensions(&self) -> &'static [&'static str] {
        &["mini"]
    }
    fn parse(&self, _: &str, path: &Path) -> RawAst {
        RawAst {
            root: AstNode {
                kind: AstKind::File,
                raw_kind: "file".into(),
                range: range(path, 1),
                text: None,
                children: vec![],
            },
            errors: vec![],
        }
    }
    fn normalize(&self, source: &str, path: &Path, _: &RawAst) -> FileModel {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let calls = source
            .lines()
            .filter_map(|line| line.trim().strip_prefix("call "))
            .map(|target| CallSite {
                name: target.into(),
                range: range(path, 2),
                ..Default::default()
            })
            .collect();
        FileModel {
            path: path.display().to_string(),
            language: "mini".into(),
            module: "mini".into(),
            functions: vec![FunctionModel {
                name: name.clone(),
                fqn: name,
                range: range(path, 1),
                body_range: range(path, 1),
                calls,
                ..Default::default()
            }],
            ..Default::default()
        }
    }
}
fn range(path: &Path, line: usize) -> SourceRange {
    SourceRange {
        file: path.display().to_string().into(),
        start_line: line,
        start_column: 1,
        end_line: line,
        end_column: 1,
    }
}
fn temp() -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!(
        "analyzer-core-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn plugin_strategy_and_graph_query_with_cycle() {
    let root = temp();
    std::fs::write(root.join("a.mini"), "call b\n").unwrap();
    std::fs::write(root.join("b.mini"), "call a\n").unwrap();
    let mut registry = LanguageRegistry::new();
    registry.register(Mini);
    let report = Analyzer::new(registry, Config::default())
        .scan(&root)
        .unwrap();
    assert_eq!(report.summary.functions, 2);
    let query = report.graph.query(
        "a",
        Direction::Callees,
        None,
        &GraphFilter::all(),
        &Config::default(),
    );
    assert_eq!(query.coverage.functions.len(), 2);
    assert!(!query.cycles.is_empty());
    assert_eq!(report.graph.paths("a", "b", true, 10).paths.len(), 1);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn injected_metric_participates_in_scan_aggregates_and_rules() {
    use analyzer_core::metrics::{Metric, MetricRegistry, MetricValue};
    struct Custom;
    impl Metric for Custom {
        fn id(&self) -> &'static str {
            "custom_work"
        }
        fn scope(&self) -> &'static str {
            "file"
        }
        fn calculate(&self, file: &FileModel, _: &Config) -> Vec<MetricValue> {
            vec![MetricValue {
                id: self.id().into(),
                scope: self.scope().into(),
                entity: file.path.as_str().into(),
                path: file.path.as_str().into(),
                line: 1,
                value: 17.0,
            }]
        }
    }
    let root = temp();
    std::fs::write(root.join("a.mini"), "call a\n").unwrap();
    std::fs::write(
        root.join("analyzer.yaml"),
        "limits:\n  custom_work: {error: 16}\n",
    )
    .unwrap();
    let mut metrics = MetricRegistry::new();
    metrics.register(Custom);
    let mut loader = ConfigLoader::new();
    metrics.register_config_keys(&mut loader);
    let loaded = loader
        .load(&root.join("analyzer.yaml"), true, false)
        .unwrap();
    let mut registry = LanguageRegistry::new();
    registry.register(Mini);
    let analyzer = Analyzer::with_metrics(registry, loaded.config, metrics);
    let report = analyzer.scan(&root).unwrap();
    assert_eq!(
        report
            .metrics
            .iter()
            .find(|m| m.id.as_ref() == "custom_work")
            .unwrap()
            .value,
        17.0
    );
    let project = report
        .aggregates
        .iter()
        .find(|group| group.name == "project")
        .unwrap();
    assert_eq!(project.metrics["custom_work"].avg, 17.0);
    assert!(report.violations.iter().any(|v| v.rule == "custom_work"));
    assert!(analyzer
        .scan(&root)
        .unwrap()
        .violations
        .iter()
        .any(|v| v.rule == "custom_work"));
    assert!(analyzer
        .metrics
        .calculate(
            &FileModel {
                vendor: true,
                ..Default::default()
            },
            &analyzer.config
        )
        .is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn strategy_comment_metadata_keeps_the_existing_line_count_hook() {
    struct Comments;
    impl LanguageStrategy for Comments {
        fn language_id(&self) -> &'static str {
            "mini"
        }
        fn file_extensions(&self) -> &'static [&'static str] {
            &["mini"]
        }
        fn parse(&self, source: &str, path: &Path) -> RawAst {
            Mini.parse(source, path)
        }
        fn normalize(&self, source: &str, path: &Path, ast: &RawAst) -> FileModel {
            let mut model = Mini.normalize(source, path, ast);
            model.comment_ranges = Some(Vec::new());
            model
        }
        fn count_lines(&self, source: &str) -> Loc {
            assert_eq!(source, "call a\n");
            Loc {
                total: 1,
                code: 1,
                comment: 0,
                blank: 0,
            }
        }
    }
    let root = temp();
    std::fs::write(root.join("a.mini"), "call a\n").unwrap();
    let mut registry = LanguageRegistry::new();
    registry.register(Comments);
    let report = Analyzer::new(registry, Config::default())
        .scan(&root)
        .unwrap();
    assert_eq!(report.files[0].loc.total, 1);
    assert_eq!(report.files[0].loc.code, 1);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn nested_include_unknown_key_and_plugin_handler() {
    let root = temp();
    std::fs::create_dir_all(root.join("conf")).unwrap();
    std::fs::write(
        root.join("conf/limits.yaml"),
        "function_params: {warn: 2}\n",
    )
    .unwrap();
    std::fs::write(root.join("root.yaml"),"config_version: 1\nlanguages: [mini]\nlimits:\n  include: conf/limits.yaml\npaths:\n  include: [\"**/*.mini\"]\nplugin:\n  flag: false\nunknown: 123\n").unwrap();
    let mut loader = ConfigLoader::new();
    loader.register_key(KeySpec {
        name: "plugin".into(),
        description: "plugin namespace".into(),
        default: None,
        validator: Arc::new(|_| Ok(())),
        handler: None,
    });
    loader.register_key(KeySpec {
        name: "plugin.flag".into(),
        description: "test key".into(),
        default: None,
        validator: Arc::new(|v| {
            if v.is_bool() {
                Ok(())
            } else {
                Err("bool".into())
            }
        }),
        handler: Some(Arc::new(|config, value| {
            config.cache.enabled = value.as_bool().unwrap()
        })),
    });
    let result = loader.load(&root.join("root.yaml"), false, true).unwrap();
    assert_eq!(result.config.languages.enabled, vec!["mini"]);
    assert_eq!(result.config.paths.include, vec!["**/*.mini"]);
    assert_eq!(
        result.config.limits.global["function_params"].warn,
        Some(2.0)
    );
    assert!(result.skipped_keys.contains(&"unknown".into()));
    assert!(!result.config.cache.enabled);
    assert!(result.provenance.contains_key("limits.function_params"));
    assert!(loader.load(&root.join("root.yaml"), true, false).is_err());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn invalid_known_value_uses_default_and_include_cycle_warns() {
    let root = temp();
    std::fs::write(
        root.join("a.yaml"),
        "include: b.yaml\nmetrics:\n  histogram_bins: -1\n",
    )
    .unwrap();
    std::fs::write(root.join("b.yaml"), "include: a.yaml\n").unwrap();
    let load = ConfigLoader::new()
        .load(&root.join("a.yaml"), false, false)
        .unwrap();
    assert_eq!(load.config.metrics.histogram_bins, 10);
    assert!(load
        .diagnostics
        .iter()
        .any(|d| d.message.contains("cyclic")));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn rules_apply_overrides_suppression_baseline_and_vendor() {
    use analyzer_core::config::{PathLimits, Threshold};
    use analyzer_core::metrics::{aggregates, MetricRegistry};
    use analyzer_core::rules::{evaluate, Severity};
    use std::collections::{BTreeMap, BTreeSet};
    let root = temp();
    let path = "src/legacy/service.mini";
    let f = FunctionModel {
        name: "work".into(),
        fqn: "service.work".into(),
        range: SourceRange {
            file: path.into(),
            start_line: 2,
            end_line: 52,
            ..Default::default()
        },
        body_range: SourceRange {
            file: path.into(),
            start_line: 3,
            end_line: 52,
            ..Default::default()
        },
        parameters: (0..8)
            .map(|i| Parameter {
                name: format!("p{i}"),
                ..Default::default()
            })
            .collect(),
        complexity: 23,
        ..Default::default()
    };
    let file = FileModel {
        path: path.into(),
        language: "mini".into(),
        module: "service".into(),
        loc: Loc {
            total: 900,
            code: 800,
            comment: 50,
            blank: 50,
        },
        functions: vec![f],
        ..Default::default()
    };
    let mut config = Config::default();
    config.limits.by_language.insert(
        "mini".into(),
        BTreeMap::from([(
            "function_params".into(),
            Threshold {
                warn: Some(5.0),
                error: Some(9.0),
            },
        )]),
    );
    config.limits.by_path.push(PathLimits {
        glob: "src/legacy/**".into(),
        metrics: BTreeMap::from([(
            "function_params".into(),
            Threshold {
                warn: Some(6.0),
                error: None,
            },
        )]),
    });
    config.limits.by_entity.insert(
        "service.work".into(),
        BTreeMap::from([(
            "function_params".into(),
            Threshold {
                warn: Some(7.0),
                error: Some(8.0),
            },
        )]),
    );
    let values = MetricRegistry::new().calculate(&file, &config);
    let aggs = aggregates(&[file.clone()], &values, 10);
    assert_eq!(
        aggs.iter().find(|g| g.name == "project").unwrap().metrics["file_lines"].median,
        900.0
    );
    assert_eq!(
        aggs.iter().find(|g| g.name == "project").unwrap().metrics["file_lines"]
            .histogram
            .iter()
            .map(|b| b.count)
            .sum::<usize>(),
        1
    );
    let sources = BTreeMap::from([(path.into(), "line 1\nfn work\n".into())]);
    let violations = evaluate(
        &values,
        &[file.clone()],
        &aggs,
        &config,
        &BTreeSet::new(),
        &sources,
    );
    assert!(violations.iter().any(|v| v.rule == "function_params"
        && v.severity == Severity::Warn
        && v.limit == Some(7.0)));
    assert!(violations
        .iter()
        .any(|v| v.rule == "file_lines" && v.severity == Severity::Error));
    let baseline = violations
        .iter()
        .map(|v| v.fingerprint())
        .collect::<BTreeSet<_>>();
    assert!(evaluate(
        &values,
        &[file.clone()],
        &aggs,
        &config,
        &baseline,
        &sources
    )
    .is_empty());
    let ignored = BTreeMap::from([(
        path.into(),
        "// analyzer-ignore: function_params\nfn work\n".into(),
    )]);
    assert!(!evaluate(
        &values,
        &[file.clone()],
        &aggs,
        &config,
        &BTreeSet::new(),
        &ignored
    )
    .iter()
    .any(|v| v.rule == "function_params"));
    let mut vendor = file.clone();
    vendor.vendor = true;
    assert!(MetricRegistry::new().calculate(&vendor, &config).is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn config_includes_glob_and_list_with_source_provenance() {
    let root = temp();
    std::fs::create_dir_all(root.join("parts")).unwrap();
    std::fs::write(root.join("parts/a.yaml"), "function_lines: {warn: 7}\n").unwrap();
    std::fs::write(root.join("parts/b.yaml"), "function_lines: {error: 20}\n").unwrap();
    std::fs::write(
        root.join("parts/list.yaml"),
        "- glob: 'src/a/**'\n  function_params: {warn: 2}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("root.yaml"),
        "limits:\n  include: parts/[ab].yaml\n  by_path:\n    - include: parts/list.yaml\n",
    )
    .unwrap();
    let load = ConfigLoader::new()
        .load(&root.join("root.yaml"), false, true)
        .unwrap();
    assert_eq!(load.config.limits.global["function_lines"].warn, Some(7.0));
    assert_eq!(
        load.config.limits.global["function_lines"].error,
        Some(20.0)
    );
    assert_eq!(load.config.limits.by_path[0].glob, "src/a/**");
    assert_eq!(
        load.provenance["limits.function_lines"].file,
        root.join("parts/b.yaml").display().to_string()
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn config_provenance_distinguishes_repeated_list_keys() {
    let root = temp();
    let path = root.join("root.yaml");
    std::fs::write(&path,"limits:\n  by_path:\n    - glob: 'src/one/**'\n      function_params:\n        warn: 2\n    - glob: 'src/two/**'\n      function_params:\n        warn: 3\n").unwrap();
    let load = ConfigLoader::new().load(&path, false, true).unwrap();
    assert_eq!(
        load.provenance["limits.by_path.0.function_params.warn"].line,
        5
    );
    assert_eq!(
        load.provenance["limits.by_path.1.function_params.warn"].line,
        8
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn config_provenance_uses_inline_yaml_parser_positions() {
    let root = temp();
    let path = root.join("root.yaml");
    std::fs::write(
        &path,
        "limits: {function_params: {warn: 2, error: 4}}\nmetrics:\n  histogram_bins: 5\n",
    )
    .unwrap();
    let load = ConfigLoader::new().load(&path, false, true).unwrap();
    assert_eq!(load.provenance["limits.function_params.warn"].line, 1);
    assert_eq!(load.provenance["limits.function_params.error"].line, 1);
    assert_eq!(load.provenance["metrics.histogram_bins"].line, 3);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn config_rejects_unregistered_metric_names_in_all_limit_scopes() {
    let root = temp();
    let path = root.join("root.yaml");
    std::fs::write(&path,"limits:\n  functon_params: {warn: 2}\n  by_language:\n    mini:\n      functon_params: {warn: 2}\n  by_path:\n    - glob: 'src/**'\n      functon_params: {warn: 2}\n  by_entity:\n    service.work:\n      functon_params: {warn: 2}\n  aggregates:\n    functon_params.avg: {warn: 2}\n").unwrap();
    let loader = ConfigLoader::new();
    let load = loader.load(&path, false, true).unwrap();
    assert_eq!(load.skipped_keys.len(), 5);
    assert!(load
        .skipped_keys
        .contains(&"limits.by_entity.service.work.functon_params".into()));
    assert!(!load.config.limits.global.contains_key("functon_params"));
    assert!(!load
        .config
        .limits
        .by_language
        .get("mini")
        .is_some_and(|m| m.contains_key("functon_params")));
    assert!(loader.load(&path, true, false).is_err());
    let quiet = loader.load(&path, false, false).unwrap();
    assert!(!quiet
        .diagnostics
        .iter()
        .any(|d| d.message == "unknown key skipped"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn plugin_metric_registers_limit_and_config_handler() {
    use analyzer_core::metrics::{Metric, MetricRegistry, MetricValue};
    struct PluginMetric;
    impl Metric for PluginMetric {
        fn id(&self) -> &'static str {
            "custom_metric"
        }
        fn scope(&self) -> &'static str {
            "function"
        }
        fn calculate(&self, _: &FileModel, _: &Config) -> Vec<MetricValue> {
            vec![]
        }
        fn register_config(&self, loader: &mut ConfigLoader) {
            loader.register_key(KeySpec {
                name: "metrics.custom_weight".into(),
                description: "Custom metric weight".into(),
                default: None,
                validator: Arc::new(|v| {
                    if v.as_u64().is_some() {
                        Ok(())
                    } else {
                        Err("integer".into())
                    }
                }),
                handler: Some(Arc::new(|config, v| {
                    config.metrics.histogram_bins = v.as_u64().unwrap() as usize
                })),
            });
        }
    }
    let root = temp();
    let path = root.join("root.yaml");
    std::fs::write(
        &path,
        "limits:\n  custom_metric: {warn: 3}\nmetrics:\n  custom_weight: 17\n",
    )
    .unwrap();
    let mut metrics = MetricRegistry::new();
    metrics.register(PluginMetric);
    let mut loader = ConfigLoader::new();
    metrics.register_config_keys(&mut loader);
    let load = loader.load(&path, true, true).unwrap();
    assert_eq!(load.config.limits.global["custom_metric"].warn, Some(3.0));
    assert_eq!(load.config.metrics.histogram_bins, 17);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn list_include_provenance_shifts_with_inserted_items() {
    let root = temp();
    std::fs::write(root.join("items.yaml"),"- glob: 'src/a/**'\n  function_params: {warn: 2}\n- glob: 'src/b/**'\n  function_params: {warn: 3}\n").unwrap();
    std::fs::write(root.join("root.yaml"),"limits:\n  by_path:\n    - glob: 'src/first/**'\n    - include: items.yaml\n    - glob: 'src/last/**'\n").unwrap();
    let load = ConfigLoader::new()
        .load(&root.join("root.yaml"), false, true)
        .unwrap();
    assert_eq!(load.config.limits.by_path.len(), 4);
    assert_eq!(load.provenance["limits.by_path.0.glob"].line, 3);
    assert_eq!(load.provenance["limits.by_path.1.glob"].line, 1);
    assert_eq!(load.provenance["limits.by_path.2.glob"].line, 3);
    assert_eq!(load.provenance["limits.by_path.3.glob"].line, 5);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn discovery_honors_gitignore_and_single_file_scan() {
    let root = temp();
    std::fs::write(root.join(".gitignore"), "ignored.mini\n").unwrap();
    std::fs::write(root.join("kept.mini"), "ok\n").unwrap();
    std::fs::write(root.join("ignored.mini"), "ok\n").unwrap();
    let mut registry = LanguageRegistry::new();
    registry.register(Mini);
    let analyzer = Analyzer::new(registry, Config::default());
    let report = analyzer.scan(&root).unwrap();
    assert_eq!(report.summary.files, 1);
    assert_eq!(report.files[0].path, "kept.mini");
    let file_report = analyzer.scan(&root.join("kept.mini")).unwrap();
    assert_eq!(file_report.summary.files, 1);
    assert_eq!(file_report.files[0].path, "kept.mini");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn graph_resolves_interface_factory_external_and_relations() {
    use analyzer_core::config::VendorMode;
    use analyzer_core::graph::{CallGraph, NodeKind};
    let loc = |path: &str, line| SourceRange {
        file: path.into(),
        start_line: line,
        end_line: line,
        ..Default::default()
    };
    let make_call = |name: &str, line| CallSite {
        name: name.into(),
        range: loc("src/main.mini", line),
        ..Default::default()
    };
    let mut factory = make_call("make_repo", 2);
    factory.assignment_to = Some("repo".into());
    let mut method = make_call("save", 3);
    method.kind = CallKind::Method;
    method.receiver = Some("repo".into());
    method.guard = vec![Guard {
        kind: "if".into(),
        raw: "x > 0".into(),
        normalized: "x > 0".into(),
        branch: "then".into(),
        range: loc("src/main.mini", 3),
    }]
    .into();
    method.in_loop = vec![LoopContext {
        header: "for item in items".into(),
        line: 3,
        range: loc("src/main.mini", 3),
    }]
    .into();
    let mut external = make_call("HTTP.get", 4);
    external.arguments = vec!["'https://api.example.test/token=secret'".into()];
    let main = FileModel {
        path: "src/main.mini".into(),
        language: "mini".into(),
        module: "main".into(),
        functions: vec![FunctionModel {
            name: "run".into(),
            fqn: "run".into(),
            range: loc("src/main.mini", 1),
            body_range: loc("src/main.mini", 1),
            calls: vec![factory, method, external],
            ..Default::default()
        }],
        ..Default::default()
    };
    let api = FileModel {
        path: "src/api.mini".into(),
        language: "mini".into(),
        module: "api".into(),
        functions: vec![FunctionModel {
            name: "make_repo".into(),
            fqn: "make_repo".into(),
            return_type: Some("Repository".into()),
            range: loc("src/api.mini", 1),
            ..Default::default()
        }],
        ..Default::default()
    };
    let implementation = FileModel {
        path: "src/impl.mini".into(),
        language: "mini".into(),
        module: "impl".into(),
        types: vec![TypeModel {
            name: "PgRepository".into(),
            fqn: "PgRepository".into(),
            implements: vec!["Repository".into()],
            ..Default::default()
        }],
        functions: vec![FunctionModel {
            name: "save".into(),
            fqn: "PgRepository::save".into(),
            parent_type: Some("PgRepository".into()),
            range: loc("src/impl.mini", 1),
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut config = Config::default();
    config.external.rules.push(ExternalOperationRule {
        category: "http".into(),
        pattern: "HTTP.get".into(),
        library: "HTTP".into(),
        operation: "get".into(),
    });
    config.external.redact.push("secret".into());
    let mut registry = LanguageRegistry::new();
    registry.register(Mini);
    let graph = CallGraph::build(&[main, api, implementation], &registry, &config);
    let query = graph.query(
        "run",
        Direction::Callees,
        None,
        &GraphFilter::all(),
        &config,
    );
    assert_eq!(query.coverage.functions.len(), 3);
    assert_eq!(query.coverage.external.len(), 1);
    let method_edge = query
        .edges
        .iter()
        .find(|e| e.to.ends_with("PgRepository::save"))
        .unwrap();
    assert_eq!(method_edge.confidence, Confidence::Ambiguous);
    assert_eq!(method_edge.guard[0].branch, "then");
    assert_eq!(method_edge.in_loop[0].header, "for item in items");
    let external_node = query
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::External)
        .unwrap();
    assert_eq!(
        external_node.argument.as_deref(),
        Some("https://api.example.test/token=[REDACTED]")
    );
    assert!(graph
        .query(
            "category:http",
            Direction::Callers,
            None,
            &GraphFilter::all(),
            &config
        )
        .coverage
        .functions
        .contains(&"run".into()));
    assert_eq!(
        graph
            .query(
                "PgRepository::save",
                Direction::Callers,
                None,
                &GraphFilter::all(),
                &config
            )
            .coverage
            .functions
            .len(),
        2
    );
    assert_eq!(
        graph
            .paths("run", "PgRepository::save", true, 10)
            .paths
            .len(),
        1
    );
    let rel = graph.relations(
        &["file:src/main.mini".into()],
        &["file:src/impl.mini".into()],
        None,
    );
    assert_eq!(rel.a_to_b.len(), 1);
    config.vendor.mode = VendorMode::Hidden;
}

#[test]
fn lazy_vendor_expands_and_hidden_view_keeps_boundary() {
    use analyzer_core::config::{VendorMode, VendorParse};
    use analyzer_core::graph::NodeKind;
    let root = temp();
    std::fs::create_dir_all(root.join("vendor")).unwrap();
    std::fs::write(root.join(".gitignore"), "vendor/\n").unwrap();
    std::fs::write(root.join("main.mini"), "call lib\n").unwrap();
    std::fs::write(root.join("vendor/lib.mini"), "ok\n").unwrap();
    let mut registry = LanguageRegistry::new();
    registry.register(Mini);
    let mut config = Config::default();
    config.vendor.paths = vec!["vendor/**".into()];
    config.vendor.parse = VendorParse::Lazy;
    let analyzer = Analyzer::new(registry, config.clone());
    let mut report = analyzer.scan(&root).unwrap();
    assert_eq!(report.summary.files, 1);
    assert_eq!(report.deferred_vendor.len(), 1);
    let package = report.deferred_vendor[0].package.clone();
    let query = report.graph.query(
        "main",
        Direction::Callees,
        None,
        &GraphFilter::all(),
        &config,
    );
    assert!(query
        .nodes
        .iter()
        .any(|n| n.kind == NodeKind::VendorPackage));
    analyzer
        .expand_vendor(&root, &mut report, &package)
        .unwrap();
    assert_eq!(report.files.len(), 2);
    config.vendor.mode = VendorMode::Expanded;
    let expanded = report.graph.query(
        "main",
        Direction::Callees,
        None,
        &GraphFilter::all(),
        &config,
    );
    assert!(expanded
        .nodes
        .iter()
        .any(|n| n.vendor && n.kind == NodeKind::Function));
    config.vendor.mode = VendorMode::Hidden;
    let hidden = report.graph.query(
        "main",
        Direction::Callees,
        None,
        &GraphFilter::all(),
        &config,
    );
    assert!(!hidden.hidden_vendor_edges.is_empty());
    assert!(hidden.nodes.iter().all(|n| !n.vendor));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn cache_reuses_unchanged_model_and_invalidates_after_edit() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct CountMini(Arc<AtomicUsize>);
    impl LanguageStrategy for CountMini {
        fn language_id(&self) -> &'static str {
            "countmini"
        }
        fn file_extensions(&self) -> &'static [&'static str] {
            &["count"]
        }
        fn parse(&self, source: &str, path: &Path) -> RawAst {
            self.0.fetch_add(1, Ordering::SeqCst);
            Mini.parse(source, path)
        }
        fn normalize(&self, source: &str, path: &Path, ast: &RawAst) -> FileModel {
            Mini.normalize(source, path, ast)
        }
    }
    let root = temp();
    std::fs::write(root.join("one.count"), "call two\n").unwrap();
    let counter = Arc::new(AtomicUsize::new(0));
    let mut registry = LanguageRegistry::new();
    registry.register(CountMini(counter.clone()));
    let analyzer = Analyzer::new(registry, Config::default());
    let first = analyzer.scan(&root).unwrap();
    assert_eq!(
        (first.summary.cache_hits, first.summary.parsed_files),
        (0, 1)
    );
    let second = analyzer.scan(&root).unwrap();
    assert_eq!(
        (second.summary.cache_hits, second.summary.parsed_files),
        (1, 0)
    );
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    std::fs::write(root.join("one.count"), "call three\n").unwrap();
    let third = analyzer.scan(&root).unwrap();
    assert_eq!(
        (third.summary.cache_hits, third.summary.parsed_files),
        (0, 1)
    );
    assert_eq!(counter.load(Ordering::SeqCst), 2);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn baseline_creation_sees_existing_violations_and_new_ones() {
    use analyzer_core::config::Threshold;
    use analyzer_core::rules::write_baseline;
    let root = temp();
    std::fs::write(root.join("one.mini"), "one\n").unwrap();
    let mut registry = LanguageRegistry::new();
    registry.register(Mini);
    let mut config = Config::default();
    config
        .limits
        .global
        .insert("file_lines".into(), Threshold::new(0.0, 100.0));
    let analyzer = Analyzer::new(registry, config);
    let first = analyzer.scan_without_baseline(&root).unwrap();
    assert_eq!(first.violations.len(), 1);
    write_baseline(&root.join(".analyzer-baseline.json"), &first.violations).unwrap();
    assert!(analyzer.scan(&root).unwrap().violations.is_empty());
    assert_eq!(
        analyzer
            .scan_without_baseline(&root)
            .unwrap()
            .violations
            .len(),
        1
    );
    std::fs::write(root.join("two.mini"), "two\n").unwrap();
    let next = analyzer.scan(&root).unwrap();
    assert_eq!(next.violations.len(), 1);
    assert_eq!(next.violations[0].file, "two.mini");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn baseline_suppresses_aggregate_violations_too() {
    use analyzer_core::config::Threshold;
    use analyzer_core::rules::write_baseline;
    let root = temp();
    std::fs::write(root.join("one.mini"), "one\n").unwrap();
    let mut registry = LanguageRegistry::new();
    registry.register(Mini);
    let mut config = Config::default();
    config
        .limits
        .aggregates
        .insert("file_lines.avg".into(), Threshold::new(0.0, 100.0));
    let analyzer = Analyzer::new(registry, config);
    let first = analyzer.scan_without_baseline(&root).unwrap();
    assert!(first.violations.iter().any(|v| v.scope == "aggregate"));
    write_baseline(&root.join(".analyzer-baseline.json"), &first.violations).unwrap();
    assert!(analyzer.scan(&root).unwrap().violations.is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn methods_per_class_respects_native_accessor_modifier() {
    use analyzer_core::metrics::MetricRegistry;
    let f = FunctionModel {
        name: "value".into(),
        fqn: "Widget.value".into(),
        parent_type: Some("Widget".into()),
        modifiers: vec!["accessor".into()],
        ..Default::default()
    };
    let ordinary = FunctionModel {
        name: "run".into(),
        fqn: "Widget.run".into(),
        parent_type: Some("Widget".into()),
        ..Default::default()
    };
    let file = FileModel {
        path: "widget.ts".into(),
        language: "typescript".into(),
        types: vec![TypeModel {
            fqn: "Widget".into(),
            ..Default::default()
        }],
        functions: vec![f, ordinary],
        ..Default::default()
    };
    let mut config = Config::default();
    config.metrics.count_accessors = false;
    let values = MetricRegistry::new().calculate(&file, &config);
    assert_eq!(
        values
            .iter()
            .find(|m| m.id.as_ref() == "methods_per_class")
            .unwrap()
            .value,
        1.0
    );
}

#[test]
fn deferred_vendor_recognizes_qualified_module_calls() {
    use analyzer_core::graph::{CallGraph, GraphEdge, GraphNode, NodeKind};
    let mut graph = CallGraph::default();
    graph.nodes = vec![
        GraphNode {
            id: "fn:mini:main:run".into(),
            label: "run".into(),
            kind: NodeKind::Function,
            file: Some("main.mini".into()),
            line: Some(1),
            language: Some("mini".into()),
            module: Some("main".into()),
            parent_type: None,
            vendor: false,
            package: None,
            category: None,
            library: None,
            operation: None,
            argument: None,
            algorithmic_complexity: None,
            cargo_target: None,
            cargo_target_id: None,
            source_fqn: None,
        },
        GraphNode {
            id: "unknown:mini:lib.run".into(),
            label: "lib.run".into(),
            kind: NodeKind::Unknown,
            file: None,
            line: None,
            language: Some("mini".into()),
            module: None,
            parent_type: None,
            vendor: false,
            package: None,
            category: None,
            library: None,
            operation: None,
            argument: None,
            algorithmic_complexity: None,
            cargo_target: None,
            cargo_target_id: None,
            source_fqn: None,
        },
    ];
    graph.edges = vec![GraphEdge {
        from: "fn:mini:main:run".into(),
        to: "unknown:mini:lib.run".into(),
        callsite: SourceRange::default(),
        confidence: Confidence::Dynamic,
        guard: vec![].into(),
        in_loop: vec![].into(),
        recursive: false,
        hidden_vendor: false,
        argument: None,
    }];
    graph.attach_deferred_vendor(&[("vendor/lib.mini".into(), "vendor/lib".into(), "mini".into())]);
    assert!(graph
        .edges
        .iter()
        .any(|e| e.to.as_ref() == "vendor:vendor/lib"));
}

#[test]
fn syntax_error_is_reported_once_when_raw_and_normalized_models_share_it() {
    struct Broken;
    impl LanguageStrategy for Broken {
        fn language_id(&self) -> &'static str {
            "broken"
        }
        fn file_extensions(&self) -> &'static [&'static str] {
            &["broken"]
        }
        fn parse(&self, _: &str, path: &Path) -> RawAst {
            let error = ParseError {
                message: "bad syntax".into(),
                range: range(path, 1),
            };
            RawAst {
                root: AstNode {
                    kind: AstKind::File,
                    raw_kind: "file".into(),
                    range: range(path, 1),
                    text: None,
                    children: vec![],
                },
                errors: vec![error],
            }
        }
        fn normalize(&self, _: &str, path: &Path, _: &RawAst) -> FileModel {
            FileModel {
                parse_errors: vec![ParseError {
                    message: "bad syntax".into(),
                    range: range(path, 1),
                }],
                ..Default::default()
            }
        }
    }
    let root = temp();
    std::fs::write(root.join("bad.broken"), "invalid").unwrap();
    let mut registry = LanguageRegistry::new();
    registry.register(Broken);
    let report = Analyzer::new(registry, Config::default())
        .scan(&root)
        .unwrap();
    assert_eq!(report.summary.parse_errors, 1);
    assert_eq!(report.files[0].parse_errors.len(), 1);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn expanded_vendor_function_resolves_after_lazy_package_open() {
    use analyzer_core::config::VendorMode;
    use analyzer_core::graph::{CallGraph, NodeKind};
    let mut registry = LanguageRegistry::new();
    registry.register(Mini);
    let caller = FileModel {
        path: "python/main.mini".into(),
        language: "mini".into(),
        module: "python.main".into(),
        imports: vec![Import {
            path: "vendor.acme_http.library.library".into(),
            alias: Some("library".into()),
            ..Default::default()
        }],
        functions: vec![FunctionModel {
            name: "run".into(),
            fqn: "python.main.run".into(),
            calls: vec![CallSite {
                name: "library".into(),
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    let vendor = FileModel {
        path: "python/vendor/acme_http/library.mini".into(),
        language: "mini".into(),
        module: "python.vendor.acme_http.library".into(),
        vendor: true,
        functions: vec![FunctionModel {
            name: "library".into(),
            fqn: "python.vendor.acme_http.library.library".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut config = Config::default();
    let graph = CallGraph::build(&[caller, vendor], &registry, &config);
    let collapsed = graph.query(
        "python.main.run",
        Direction::Callees,
        None,
        &GraphFilter::all(),
        &config,
    );
    assert!(collapsed
        .nodes
        .iter()
        .any(|n| n.kind == NodeKind::VendorPackage
            && n.package.as_deref() == Some("python/vendor/acme_http")));
    config.vendor.mode = VendorMode::Expanded;
    let expanded = graph.query(
        "python.main.run",
        Direction::Callees,
        None,
        &GraphFilter::all(),
        &config,
    );
    let target = expanded
        .nodes
        .iter()
        .find(|n| n.vendor && n.kind == NodeKind::Function)
        .unwrap();
    assert_eq!(target.label, "python.vendor.acme_http.library.library");
    assert!(expanded
        .edges
        .iter()
        .any(|e| e.to.as_ref() == target.id && e.confidence == Confidence::Resolved));
}

#[test]
fn nested_gitignored_vendor_is_deferred_and_expandable() {
    use analyzer_core::config::VendorParse;
    let root = temp();
    std::fs::create_dir_all(root.join("python/vendor/acme_http")).unwrap();
    std::fs::write(root.join(".gitignore"), "python/vendor/\n").unwrap();
    std::fs::write(root.join("python/main.mini"), "call library\n").unwrap();
    std::fs::write(root.join("python/vendor/acme_http/library.mini"), "ok\n").unwrap();
    let mut registry = LanguageRegistry::new();
    registry.register(Mini);
    let mut config = Config::default();
    config.paths.include = vec!["python/**".into()];
    config.vendor.paths = vec!["**/vendor/**".into()];
    config.vendor.parse = VendorParse::Lazy;
    let analyzer = Analyzer::new(registry, config);
    let mut report = analyzer.scan(&root).unwrap();
    assert_eq!(report.summary.files, 1);
    assert_eq!(report.deferred_vendor.len(), 1);
    assert_eq!(report.deferred_vendor[0].package, "python/vendor/acme_http");
    analyzer
        .expand_vendor(&root, &mut report, "python/vendor/acme_http")
        .unwrap();
    assert!(report
        .files
        .iter()
        .any(|f| f.vendor && f.path == "python/vendor/acme_http/library.mini"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn path_queries_bound_dense_dag_enumeration() {
    use analyzer_core::graph::{CallGraph, GraphEdge, GraphNode, NodeKind};
    let node = |id: String| GraphNode {
        id: id.clone(),
        label: id,
        kind: NodeKind::Function,
        file: None,
        line: None,
        language: Some("mini".into()),
        module: None,
        parent_type: None,
        vendor: false,
        package: None,
        category: None,
        library: None,
        operation: None,
        argument: None,
        algorithmic_complexity: None,
        cargo_target: None,
        cargo_target_id: None,
        source_fqn: None,
    };
    let edge = |from: String, to: String| GraphEdge {
        from: from.into(),
        to: to.into(),
        callsite: SourceRange::default(),
        confidence: Confidence::Resolved,
        guard: vec![].into(),
        in_loop: vec![].into(),
        recursive: false,
        hidden_vendor: false,
        argument: None,
    };
    let mut graph = CallGraph::default();
    graph.nodes.push(node("start".into()));
    graph.nodes.push(node("end".into()));
    let mut prior = vec!["start".to_string()];
    for layer in 0..20 {
        let next = vec![format!("layer{layer}a"), format!("layer{layer}b")];
        for id in &next {
            graph.nodes.push(node(id.clone()));
        }
        for from in &prior {
            for to in &next {
                graph.edges.push(edge(from.clone(), to.clone()));
            }
        }
        prior = next;
    }
    for from in prior {
        graph.edges.push(edge(from, "end".into()));
    }
    graph.reindex();
    let shortest = graph.paths("start", "end", true, 10);
    assert_eq!(shortest.paths.len(), 10);
    assert!(shortest.truncated);
    let all = graph.paths("start", "end", false, 10);
    assert_eq!(all.paths.len(), 10);
    assert!(all.truncated);
    assert!(all
        .paths
        .iter()
        .all(|path| path.first().map(String::as_str) == Some("start")
            && path.last().map(String::as_str) == Some("end")));
}

#[test]
fn external_rule_does_not_capture_unrelated_method_with_same_name() {
    use analyzer_core::graph::{CallGraph, NodeKind};
    let mut registry = LanguageRegistry::new();
    registry.register(Mini);
    let file = FileModel {
        path: "main.mini".into(),
        language: "mini".into(),
        module: "main".into(),
        functions: vec![FunctionModel {
            name: "run".into(),
            fqn: "run".into(),
            calls: vec![
                CallSite {
                    name: "get".into(),
                    kind: CallKind::Method,
                    receiver: Some("repo".into()),
                    ..Default::default()
                },
                CallSite {
                    name: "get".into(),
                    kind: CallKind::Function,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut config = Config::default();
    config.external.rules.push(ExternalOperationRule {
        category: "io".into(),
        pattern: "get".into(),
        library: "global".into(),
        operation: "get".into(),
    });
    let graph = CallGraph::build(&[file], &registry, &config);
    assert_eq!(
        graph
            .edges
            .iter()
            .filter(|edge| graph
                .nodes
                .iter()
                .any(|node| node.id == edge.to.as_ref() && node.kind == NodeKind::External))
            .count(),
        1
    );
    assert_eq!(
        graph
            .edges
            .iter()
            .filter(|edge| graph
                .nodes
                .iter()
                .any(|node| node.id == edge.to.as_ref() && node.kind == NodeKind::Unknown))
            .count(),
        1
    );
}

#[test]
fn registered_language_aliases_normalize_and_unknown_ids_warn() {
    let root = temp();
    std::fs::write(
        root.join("list.yaml"),
        "languages: [js, ts, python, mystery]\n",
    )
    .unwrap();
    std::fs::write(
        root.join("map.yaml"),
        "languages:\n  enabled: [ts, js]\n  extensions:\n    mjs: js\n    foo: mystery\n",
    )
    .unwrap();
    let mut loader = ConfigLoader::new();
    for id in ["javascript", "typescript", "python"] {
        loader.register_language(id);
    }
    let list = loader.load(&root.join("list.yaml"), false, true).unwrap();
    assert_eq!(
        list.config.languages.enabled,
        vec!["javascript", "typescript", "python"]
    );
    assert!(list
        .diagnostics
        .iter()
        .any(|d| d.message.contains("mystery")));
    let map = loader.load(&root.join("map.yaml"), false, true).unwrap();
    assert_eq!(
        map.config.languages.enabled,
        vec!["typescript", "javascript"]
    );
    assert_eq!(
        map.config
            .languages
            .extensions
            .get("mjs")
            .map(String::as_str),
        Some("javascript")
    );
    assert!(!map.config.languages.extensions.contains_key("foo"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn deferred_vendor_links_explicit_import_without_arbitrary_file_stem_guess() {
    use analyzer_core::graph::CallGraph;
    let mut registry = LanguageRegistry::new();
    registry.register(Mini);
    let mut caller = FileModel {
        path: "python/main.mini".into(),
        language: "mini".into(),
        module: "python.main".into(),
        functions: vec![FunctionModel {
            name: "run".into(),
            fqn: "run".into(),
            calls: vec![CallSite {
                name: "library".into(),
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    let packages = vec![(
        "python/vendor/acme_http/library.mini".into(),
        "python/vendor/acme_http".into(),
        "mini".into(),
    )];
    let mut without_import = CallGraph::build(&[caller.clone()], &registry, &Config::default());
    without_import.attach_deferred_vendor_with_files(&packages, &[caller.clone()]);
    assert!(!without_import
        .edges
        .iter()
        .any(|edge| edge.to.as_ref() == "vendor:python/vendor/acme_http"));
    caller.imports.push(Import {
        path: "vendor.acme_http.library.library".into(),
        alias: Some("library".into()),
        ..Default::default()
    });
    let mut with_import = CallGraph::build(&[caller.clone()], &registry, &Config::default());
    with_import.attach_deferred_vendor_with_files(&packages, &[caller]);
    assert!(with_import
        .edges
        .iter()
        .any(|edge| edge.to.as_ref() == "vendor:python/vendor/acme_http"));
}

#[test]
#[ignore = "manual performance measurement on release profile"]
fn graph_query_100k_functions_depth_five() {
    use analyzer_core::graph::{CallGraph, GraphEdge, GraphNode, NodeKind};
    let mut graph = CallGraph::default();
    for i in 0..100_000 {
        graph.nodes.push(GraphNode {
            id: format!("fn:mini:f:{i}"),
            label: format!("f{i}"),
            kind: NodeKind::Function,
            file: Some("f.mini".into()),
            line: Some(i + 1),
            language: Some("mini".into()),
            module: Some("m".into()),
            parent_type: None,
            vendor: false,
            package: None,
            category: None,
            library: None,
            operation: None,
            argument: None,
            algorithmic_complexity: None,
            cargo_target: None,
            cargo_target_id: None,
            source_fqn: None,
        });
        if i > 0 {
            graph.edges.push(GraphEdge {
                from: format!("fn:mini:f:{}", i - 1).into(),
                to: format!("fn:mini:f:{i}").into(),
                callsite: SourceRange::default(),
                confidence: Confidence::Resolved,
                guard: vec![].into(),
                in_loop: vec![].into(),
                recursive: false,
                hidden_vendor: false,
                argument: None,
            });
        }
    }
    graph.reindex();
    let start = std::time::Instant::now();
    let query = graph.query(
        "f0",
        Direction::Callees,
        Some(5),
        &GraphFilter::all(),
        &Config::default(),
    );
    let elapsed = start.elapsed();
    assert_eq!(query.nodes.len(), 6);
    eprintln!("100k-function depth-five graph query: {:?}", elapsed);
    assert!(elapsed.as_millis() < 300);
}

#[test]
fn custom_external_rules_preserve_catalog_and_other_settings() {
    let root = temp();
    let path = root.join("config.yaml");
    std::fs::write(&path, "cache:\n  enabled: false\nexternal:\n  rules:\n    - category: http\n      pattern: client.send\n      library: custom\n      operation: send\n").unwrap();
    let load = ConfigLoader::new().load(&path, true, true).unwrap();
    assert!(!load.config.cache.enabled);
    assert_eq!(load.config.external.rules.len(), 1);
    assert_eq!(load.config.external.rules[0].pattern, "client.send");
    assert!(load.diagnostics.is_empty(), "{:?}", load.diagnostics);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn invalid_builtin_values_do_not_discard_valid_settings() {
    for invalid in [
        "paths: 42",
        "cache:\n  path: 42",
        "languages:\n  extensions:\n    py: 42",
        "limits:\n  by_path: [42]",
    ] {
        let root = temp();
        let path = root.join("config.yaml");
        std::fs::write(
            &path,
            format!("{invalid}\nmetrics:\n  histogram_bins: 17\n"),
        )
        .unwrap();
        let load = ConfigLoader::new().load(&path, false, true).unwrap();
        assert_eq!(
            load.config.metrics.histogram_bins, 17,
            "{invalid}: {:?}",
            load.diagnostics
        );
        assert!(!load.diagnostics.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn qualified_unknown_receivers_do_not_resolve_unrelated_methods() {
    let mut registry = LanguageRegistry::new();
    registry.register(Mini);
    let mut functions = vec![FunctionModel {
        name: "caller".into(),
        fqn: "pkg.caller".into(),
        calls: vec![CallSite {
            name: "get".into(),
            receiver: Some("unknown".into()),
            targets: vec!["unknown.get".into()],
            kind: CallKind::Method,
            ..Default::default()
        }],
        ..Default::default()
    }];
    // A module-level function sharing the method name is also unrelated.
    functions.push(FunctionModel {
        name: "get".into(),
        fqn: "pkg.get".into(),
        ..Default::default()
    });
    for i in 0..1000 {
        functions.push(FunctionModel {
            name: "get".into(),
            fqn: format!("pkg.Unrelated{i}.get"),
            parent_type: Some(format!("pkg.Unrelated{i}")),
            ..Default::default()
        });
    }
    let file = FileModel {
        path: "pkg.mini".into(),
        module: "pkg".into(),
        language: "mini".into(),
        functions,
        ..Default::default()
    };
    let graph = analyzer_core::graph::CallGraph::build(&[file], &registry, &Config::default());
    assert_eq!(
        graph.edges.len(),
        1,
        "unknown receiver must not target every unrelated get method"
    );
    assert_eq!(graph.edges[0].confidence, Confidence::Dynamic);
}

#[test]
fn vendor_discovery_never_enters_git_metadata() {
    let root = temp();
    std::fs::create_dir_all(root.join(".git/vendor/pkg")).unwrap();
    std::fs::create_dir_all(root.join("src/vendor/pkg")).unwrap();
    std::fs::write(root.join(".git/vendor/pkg/private.mini"), "call secret").unwrap();
    std::fs::write(root.join("src/vendor/pkg/library.mini"), "call other").unwrap();
    let mut registry = LanguageRegistry::new();
    registry.register(Mini);
    let mut config = Config::default();
    config.vendor.paths = vec!["**/vendor/**".into()];
    let discovered = analyzer_core::discovery::discover(&root, &config, &registry);
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].relative, "src/vendor/pkg/library.mini");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn invalid_utf8_source_does_not_abort_other_files() {
    let root = temp();
    std::fs::write(root.join("bad.mini"), b"call \xff\n").unwrap();
    std::fs::write(root.join("good.mini"), "call good\n").unwrap();
    let mut registry = LanguageRegistry::new();
    registry.register(Mini);
    let mut config = Config::default();
    config.cache.enabled = false;
    let report = Analyzer::new(registry, config).scan(&root).unwrap();
    assert_eq!(report.summary.files, 2);
    assert!(report
        .parse_errors
        .iter()
        .any(|error| error.range.file.as_ref() == "bad.mini"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn malformed_function_ranges_do_not_panic_advanced_metrics() {
    let file = FileModel {
        path: "a.mini".into(),
        functions: vec![FunctionModel {
            range: SourceRange {
                file: "a.mini".into(),
                start_line: 3,
                end_line: 1,
                ..Default::default()
            },
            ..Default::default()
        }],
        ..Default::default()
    };
    let sources = std::collections::BTreeMap::from([("a.mini".into(), "a\nb\nc\n".into())]);
    let metrics = analyzer_core::metrics::advanced_metrics(
        &[file],
        &Default::default(),
        &sources,
        &Default::default(),
    );
    assert!(!metrics.is_empty());
}

#[test]
fn lossy_decoding_does_not_reuse_cache_for_valid_replacement_character() {
    let root = temp();
    let path = root.join("bad.mini");
    std::fs::write(&path, b"call \xff\n").unwrap();
    let mut registry = LanguageRegistry::new();
    registry.register(Mini);
    let analyzer = Analyzer::new(registry, Config::default());
    assert_eq!(analyzer.scan(&root).unwrap().parse_errors.len(), 1);
    std::fs::write(path, "call \u{fffd}\n").unwrap();
    let report = analyzer.scan(&root).unwrap();
    assert!(report.parse_errors.is_empty());
    assert_eq!(report.summary.cache_hits, 0);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn repeated_boms_and_utf8_decoding_preserve_source_hashes_and_cache_hits() {
    use analyzer_core::cache::Cache;
    let root = temp();
    let mut registry = LanguageRegistry::new();
    registry.register(Mini);
    let analyzer = Analyzer::new(registry, Config::default());
    for bytes in [
        b"call target\r\n".as_slice(),
        "\u{feff}\u{feff}call target\r\n".as_bytes(),
        "\u{feff}\u{feff}".as_bytes(),
        b"\xef\xbb\xbfcall \xff\n".as_slice(),
    ] {
        std::fs::write(root.join("a.mini"), bytes).unwrap();
        let diagnostic = std::str::from_utf8(bytes)
            .err()
            .map(|error| {
                format!(
                    "source contains invalid UTF-8; decoded with replacement characters: {error}"
                )
            })
            .unwrap_or_default();
        let source = String::from_utf8_lossy(bytes);
        let source = source.trim_start_matches('\u{feff}');
        let expected = Cache::content_hash(
            &format!("a.mini\0{diagnostic}\0{source}"),
            "mini",
            "core-ir-19:strategy-1:architecture-v4-true",
        );
        let report = analyzer.scan(&root).unwrap();
        assert_eq!(report.files[0].source_hash, expected);
        assert_eq!(
            report.files[0].functions[0].calls.len(),
            usize::from(!source.is_empty())
        );
        let cached = analyzer.scan(&root).unwrap();
        assert_eq!(cached.summary.cache_hits, 1);
        assert_eq!(
            cached.parse_errors.len(),
            usize::from(!diagnostic.is_empty())
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn external_catalog_is_loaded_once_per_language_per_graph() {
    struct Catalog(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    impl LanguageStrategy for Catalog {
        fn language_id(&self) -> &'static str {
            "mini"
        }
        fn file_extensions(&self) -> &'static [&'static str] {
            &["mini"]
        }
        fn parse(&self, source: &str, path: &Path) -> RawAst {
            Mini.parse(source, path)
        }
        fn normalize(&self, source: &str, path: &Path, raw: &RawAst) -> FileModel {
            Mini.normalize(source, path, raw)
        }
        fn external_operations(&self) -> Vec<ExternalOperationRule> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            vec![ExternalOperationRule {
                category: "http".into(),
                pattern: "client.send".into(),
                library: "client".into(),
                operation: "send".into(),
            }]
        }
    }
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut registry = LanguageRegistry::new();
    registry.register(Catalog(calls.clone()));
    let files = (0..100)
        .map(|i| FileModel {
            path: format!("{i}.mini"),
            language: "mini".into(),
            functions: vec![FunctionModel {
                fqn: "caller".into(),
                calls: vec![CallSite {
                    name: "client.send".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        })
        .collect::<Vec<_>>();
    let graph = analyzer_core::graph::CallGraph::build(&files, &registry, &Config::default());
    assert_eq!(graph.edges.len(), 100);
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
}

#[test]
fn report_symbol_search_and_graph_helpers_preserve_entity_identity() {
    let root = temp();
    std::fs::write(root.join("alpha.mini"), "call beta\n").unwrap();
    std::fs::write(root.join("beta.mini"), "call alpha\n").unwrap();
    let mut registry = LanguageRegistry::new();
    registry.register(Mini);
    let analyzer = Analyzer::new(registry, Config::default());
    let report = analyzer.scan(&root).unwrap();
    assert!(report.file("missing.mini").is_none());
    assert_eq!(report.file("beta.mini").unwrap().functions[0].fqn, "beta");
    assert!(report.symbol("missing").is_none());
    let (file, symbol) = report.symbol("fn:mini:beta.mini:beta").unwrap();
    assert_eq!(file.path, "beta.mini");
    assert_eq!(symbol.fqn, "beta");
    assert_eq!(report.symbol("alpha").unwrap().0.path, "alpha.mini");
    assert_eq!(report.search("BETA")[0].fqn, "beta");
    assert_eq!(
        report
            .search("")
            .iter()
            .map(|symbol| symbol.fqn.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "beta"]
    );
    let query = report.graph_query(
        "alpha",
        Direction::Both,
        None,
        &GraphFilter::all(),
        &analyzer.config,
    );
    assert_eq!(query.nodes.len(), 2);
    assert!(!query.cycles.is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn parser_panic_isolated_to_its_file_and_reported_with_source_location() {
    struct Panicky;
    impl LanguageStrategy for Panicky {
        fn language_id(&self) -> &'static str {
            "mini"
        }
        fn file_extensions(&self) -> &'static [&'static str] {
            &["mini"]
        }
        fn parse(&self, source: &str, path: &Path) -> RawAst {
            assert!(!source.contains("panic-parser"), "simulated parser failure");
            Mini.parse(source, path)
        }
        fn normalize(&self, source: &str, path: &Path, raw: &RawAst) -> FileModel {
            Mini.normalize(source, path, raw)
        }
    }
    let root = temp();
    std::fs::write(root.join("broken.mini"), "panic-parser\n").unwrap();
    std::fs::write(root.join("good.mini"), "call good\n").unwrap();
    let mut registry = LanguageRegistry::new();
    registry.register(Panicky);
    let report = Analyzer::new(registry, Config::default())
        .scan(&root)
        .unwrap();
    assert_eq!(report.summary.files, 2);
    assert_eq!(report.summary.functions, 1);
    assert_eq!(report.parse_errors.len(), 1);
    assert_eq!(report.parse_errors[0].range.file.as_ref(), "broken.mini");
    assert_eq!(report.parse_errors[0].range.start_line, 1);
    assert_eq!(report.file("broken.mini").unwrap().language, "mini");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn graph_does_not_cross_product_shared_class_suffixes() {
    use analyzer_core::graph::CallGraph;
    let mut registry = LanguageRegistry::new();
    registry.register(Mini);
    let mut files = vec![FileModel {
        path: "caller.mini".into(),
        module: "caller".into(),
        language: "mini".into(),
        functions: vec![FunctionModel {
            fqn: "caller.start".into(),
            calls: vec![CallSite {
                name: "run".into(),
                receiver: Some("service".into()),
                receiver_type: Some("Service".into()),
                targets: vec!["Service.run".into()],
                kind: CallKind::Method,
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    }];
    for i in 0..1000 {
        files.push(FileModel {
            path: format!("module{i}.mini"),
            module: format!("module{i}"),
            language: "mini".into(),
            functions: vec![FunctionModel {
                fqn: format!("module{i}.Service.run"),
                parent_type: Some(format!("module{i}.Service")),
                ..Default::default()
            }],
            ..Default::default()
        });
    }
    let graph = CallGraph::build(&files, &registry, &Config::default());
    assert_eq!(graph.edges.len(), 1);
    assert_eq!(graph.edges[0].confidence, Confidence::Dynamic);
    assert!(graph.edges[0].to.starts_with("unknown:"));
}

#[test]
fn graph_local_definition_precedes_duplicate_global_names_and_qualified_import_resolves() {
    use analyzer_core::graph::CallGraph;
    let mut registry = LanguageRegistry::new();
    registry.register(Mini);
    let files = [
        FileModel {
            path: "local.mini".into(),
            module: "local".into(),
            language: "mini".into(),
            imports: vec![Import {
                alias: Some("imported".into()),
                path: "remote.exported".into(),
                ..Default::default()
            }],
            functions: vec![
                FunctionModel {
                    fqn: "start".into(),
                    calls: vec![
                        CallSite {
                            name: "shared".into(),
                            ..Default::default()
                        },
                        CallSite {
                            name: "imported".into(),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
                FunctionModel {
                    fqn: "shared".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        },
        FileModel {
            path: "remote.mini".into(),
            module: "remote".into(),
            language: "mini".into(),
            functions: vec![
                FunctionModel {
                    fqn: "shared".into(),
                    ..Default::default()
                },
                FunctionModel {
                    fqn: "remote.exported".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        },
    ];
    let graph = CallGraph::build(&files, &registry, &Config::default());
    assert_eq!(graph.edges.len(), 2);
    assert!(graph
        .edges
        .iter()
        .all(|edge| edge.confidence == Confidence::Resolved));
    assert!(graph
        .edges
        .iter()
        .any(|edge| edge.to.as_ref() == "fn:mini:local.mini:shared"));
    assert!(graph
        .edges
        .iter()
        .any(|edge| edge.to.as_ref() == "fn:mini:remote.mini:remote.exported"));
    assert!(!graph
        .edges
        .iter()
        .any(|edge| edge.to.as_ref() == "fn:mini:remote.mini:shared"));
}

#[test]
fn interface_dispatch_uses_qualified_identity_not_shared_short_name() {
    use analyzer_core::graph::CallGraph;
    let mut registry = LanguageRegistry::new();
    registry.register(Mini);
    let mut files = vec![FileModel {
        path: "caller.mini".into(),
        module: "caller".into(),
        language: "mini".into(),
        functions: vec![FunctionModel {
            fqn: "caller.run".into(),
            calls: vec![CallSite {
                name: "save".into(),
                receiver: Some("repo".into()),
                receiver_type: Some("first.Repository".into()),
                kind: CallKind::Method,
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    }];
    for namespace in ["first", "unrelated"] {
        files.push(FileModel {
            path: format!("{namespace}.mini"),
            module: namespace.into(),
            language: "mini".into(),
            types: vec![TypeModel {
                name: "Implementation".into(),
                fqn: format!("{namespace}.Implementation"),
                implements: vec![format!("{namespace}.Repository")],
                ..Default::default()
            }],
            functions: vec![FunctionModel {
                fqn: format!("{namespace}.Implementation.save"),
                ..Default::default()
            }],
            ..Default::default()
        });
    }
    let graph = CallGraph::build(&files, &registry, &Config::default());
    assert_eq!(graph.edges.len(), 1);
    assert!(graph.edges[0].to.ends_with("first.Implementation.save"));
    assert_eq!(graph.edges[0].confidence, Confidence::Ambiguous);
}

#[test]
fn graph_reindex_shares_edge_ids_after_json_roundtrip() {
    use analyzer_core::graph::{CallGraph, GraphEdge};
    let mut graph = CallGraph::default();
    for line in [1, 2] {
        graph.edges.push(GraphEdge {
            from: "same.source".into(),
            to: "same.target".into(),
            callsite: SourceRange {
                start_line: line,
                ..Default::default()
            },
            confidence: Confidence::Resolved,
            guard: vec![].into(),
            in_loop: vec![].into(),
            recursive: false,
            hidden_vendor: false,
            argument: None,
        });
    }
    let stored = serde_json::to_value(&graph).unwrap();
    assert_eq!(stored["edges"][0]["from"], "same.source");
    let restored: CallGraph = serde_json::from_value(stored.clone()).unwrap();
    assert!(Arc::ptr_eq(
        &restored.edges[0].from,
        &restored.edges[1].from
    ));
    assert!(Arc::ptr_eq(&restored.edges[0].to, &restored.edges[1].to));
    assert_eq!(serde_json::to_value(restored).unwrap(), stored);
}

#[test]
fn cache_restores_shared_source_paths_without_changing_ranges() {
    use analyzer_core::cache::Cache;
    let root = temp();
    let cache = Cache::new(root.clone());
    let model = FileModel {
        path: "shared.ts".into(),
        functions: vec![FunctionModel {
            range: SourceRange {
                file: "shared.ts".into(),
                start_line: 3,
                ..Default::default()
            },
            body_range: SourceRange {
                file: "shared.ts".into(),
                start_line: 4,
                ..Default::default()
            },
            calls: vec![CallSite {
                range: SourceRange {
                    file: "shared.ts".into(),
                    start_line: 5,
                    ..Default::default()
                },
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    let original = serde_json::to_value(&model).unwrap();
    cache.put("ranges", &model).unwrap();
    let restored = cache.get("ranges").unwrap();
    let function = &restored.functions[0];
    assert!(Arc::ptr_eq(&function.range.file, &function.body_range.file));
    assert!(Arc::ptr_eq(
        &function.range.file,
        &function.calls[0].range.file
    ));
    assert_eq!(serde_json::to_value(restored).unwrap(), original);
    std::fs::remove_dir_all(root).unwrap();
}
