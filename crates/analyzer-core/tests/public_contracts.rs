//! A downstream strategy using the original public paths and default hooks.
use analyzer_core::analysis::{Analyzer, LanguageRegistry, LanguageStrategy};
use analyzer_core::config::{Config, ConfigLoader};
use analyzer_core::ir::{AstKind, AstNode, FileModel, ParseError, RawAst, SourceRange};
use std::path::Path;

struct DownstreamStrategy;
impl LanguageStrategy for DownstreamStrategy {
    fn language_id(&self) -> &'static str {
        "downstream"
    }
    fn file_extensions(&self) -> &'static [&'static str] {
        &["downstream"]
    }
    fn parse(&self, _: &str, path: &Path) -> RawAst {
        let range = SourceRange {
            file: path.to_string_lossy().as_ref().into(),
            ..Default::default()
        };
        RawAst {
            root: AstNode {
                kind: AstKind::File,
                raw_kind: "file".into(),
                range: range.clone(),
                text: None,
                children: vec![],
            },
            errors: vec![ParseError {
                message: "downstream parse diagnostic".into(),
                range,
            }],
        }
    }
    fn normalize(&self, _: &str, path: &Path, ast: &RawAst) -> FileModel {
        FileModel {
            path: path.to_string_lossy().into_owned(),
            language: self.language_id().into(),
            parse_errors: ast.errors.clone(),
            ..Default::default()
        }
    }
}

#[test]
fn downstream_defaults_keep_registry_and_parse_diagnostics_compatible() {
    let mut registry = LanguageRegistry::new();
    registry.register(DownstreamStrategy);
    let strategy = registry.by_extension(".downstream").unwrap();
    let model = strategy.analyze("code\n\n# comment\n", Path::new("input.downstream"));
    assert_eq!(model.parse_errors.len(), 1);
    assert_eq!(model.parse_errors[0].message, "downstream parse diagnostic");
    let counts: analyzer_core::ir::Loc = strategy.count_lines("code\n\n# comment\n");
    assert_eq!(
        (counts.total, counts.code, counts.blank, counts.comment),
        (3, 1, 1, 1)
    );
    let mut loader = ConfigLoader::new();
    registry.register_config_keys(&mut loader);
    analyzer_core::algorithmic::register_config(&mut loader);
    analyzer_core::architecture::register_config(&mut loader);
    let analyzer: analyzer_core::Analyzer = Analyzer::new(registry, Config::default());
    assert_eq!(analyzer.registry.languages(), vec!["downstream"]);
}

#[test]
fn graph_round_trip_rebuilds_the_public_adjacency_index() {
    let graph:analyzer_core::graph::CallGraph=serde_json::from_value(serde_json::json!({
        "nodes":[{"id":"a","label":"a","kind":"function","vendor":false},{"id":"b","label":"b","kind":"function","vendor":false}],
        "edges":[{"from":"a","to":"b","callsite":{"file":"x","start_line":1,"start_column":1,"end_line":1,"end_column":2},"confidence":"resolved","guard":[],"in_loop":[],"recursive":false,"hidden_vendor":false}]
    })).unwrap();
    assert_eq!(graph.node("b").unwrap().label, "b");
    assert_eq!(graph.outgoing_edges("a").count(), 1);
    let restored: analyzer_core::graph::CallGraph =
        serde_json::from_slice(&serde_json::to_vec(&graph).unwrap()).unwrap();
    assert_eq!(
        restored.outgoing_edges("a").next().unwrap().to.as_ref(),
        "b"
    );
}
