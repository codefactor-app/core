use analyzer_core::graph::{
    CallGraph, Coverage, Direction, GraphEdge, GraphNode, GraphQuery, NodeKind,
};
use analyzer_core::ir::{
    Confidence, FileModel, FunctionModel, Guard, Loc, LoopContext, ParseError, SourceRange,
    TypeKind, TypeModel,
};
use analyzer_core::metrics::{AggregateGroup, Distribution, HistogramBin, MetricValue};
use analyzer_core::report::{AnalysisReport, Summary};
use analyzer_core::report_markdown::{graph_markdown, scan_markdown};
use analyzer_core::rules::{Severity, Violation};
use std::collections::BTreeMap;

fn fixture() -> (AnalysisReport, GraphQuery) {
    let range = SourceRange {
        file: "src/service.py".into(),
        start_line: 7,
        start_column: 1,
        end_line: 9,
        end_column: 20,
    };
    let guard = Guard {
        kind: "If".into(),
        raw: "not ready".into(),
        normalized: "!ready".into(),
        branch: "then".into(),
        range: range.clone(),
    };
    let loop_context = LoopContext {
        header: "for item in items".into(),
        line: 8,
        range: range.clone(),
    };
    let caller = GraphNode {
        id: "caller".into(),
        label: "src.service.Service.run".into(),
        kind: NodeKind::Function,
        file: Some("src/service.py".into()),
        line: Some(7),
        language: Some("python".into()),
        module: Some("src.service".into()),
        parent_type: Some("src.service.Service".into()),
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
    let callee = GraphNode {
        id: "callee".into(),
        label: "src.service.finish".into(),
        kind: NodeKind::Function,
        file: Some("src/service.py".into()),
        line: Some(12),
        language: Some("python".into()),
        module: Some("src.service".into()),
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
    let edge = GraphEdge {
        from: "caller".into(),
        to: "callee".into(),
        callsite: range.clone(),
        confidence: Confidence::Resolved,
        guard: vec![guard.clone()].into(),
        in_loop: vec![loop_context.clone()].into(),
        recursive: false,
        hidden_vendor: false,
        argument: None,
    };
    let mut graph = CallGraph::default();
    graph.nodes = vec![caller.clone(), callee.clone()];
    graph.edges = vec![edge.clone()];
    let query = GraphQuery {
        roots: vec!["caller".into()],
        nodes: graph.nodes.clone(),
        edges: graph.edges.clone(),
        cycles: vec![],
        hidden_vendor_edges: vec![],
        coverage: Coverage {
            functions: vec![caller.label.clone(), callee.label.clone()],
            classes: vec!["src.service.Service".into()],
            files: vec!["src/service.py".into()],
            modules: vec!["src.service".into()],
            entry_points: vec![caller.label.clone()],
            leaves: vec![callee.label.clone()],
            ..Default::default()
        },
        direction: Direction::Callees,
        depth: None,
    };
    let function = FunctionModel {
        name: "run".into(),
        fqn: caller.label.clone(),
        range: range.clone(),
        body_range: range.clone(),
        parent_type: Some("src.service.Service".into()),
        signature: "def run(self, items):".into(),
        complexity: 2,
        ..Default::default()
    };
    let file = FileModel {
        path: "src/service.py".into(),
        language: "python".into(),
        module: "src.service".into(),
        loc: Loc {
            total: 20,
            code: 15,
            comment: 3,
            blank: 2,
        },
        types: vec![TypeModel {
            name: "Service".into(),
            fqn: "src.service.Service".into(),
            kind: TypeKind::Class,
            range: range.clone(),
            ..Default::default()
        }],
        functions: vec![function],
        ..Default::default()
    };
    let metric = MetricValue {
        id: "function_lines".into(),
        scope: "function".into(),
        entity: caller.label.clone().into(),
        path: file.path.clone().into(),
        line: 7,
        value: 3.0,
    };
    let violation = Violation {
        rule: "function_lines".into(),
        severity: Severity::Warn,
        file: file.path.clone(),
        line: 7,
        entity: caller.label.clone(),
        value: Some(3.0),
        limit: Some(2.0),
        scope: "function".into(),
        architecture: None,
        module_cycle: None,
        algorithmic_complexity: None,
    };
    let distribution = Distribution {
        avg: 3.0,
        median: 3.0,
        max: 3.0,
        p90: 3.0,
        p95: 3.0,
        histogram: vec![HistogramBin {
            start: 3.0,
            end: 4.0,
            count: 1,
        }],
    };
    let report = AnalysisReport {
        version: "1".into(),
        schema_version: "1".into(),
        root: "/project".into(),
        config_path: Some("analyzer.yaml".into()),
        baseline_path: None,
        diagnostics: vec![],
        changes: None,
        strategy_capabilities: Default::default(),
        summary: Summary {
            files: 1,
            functions: 1,
            types: 1,
            warnings: 1,
            errors: 0,
            vendor_files: 0,
            parse_errors: 1,
            ..Default::default()
        },
        aggregates: vec![AggregateGroup {
            name: "project".into(),
            metrics: BTreeMap::from([("function_lines".into(), distribution)]),
        }],
        files: vec![file],
        metrics: vec![metric],
        violations: vec![violation],
        parse_errors: vec![ParseError {
            message: "syntax error".into(),
            range,
        }],
        graph,
        deferred_vendor: vec![],
        resolution_diagnostics: vec![],
        architecture: Default::default(),
    };
    (report, query)
}

#[test]
fn scan_markdown_contains_json_report_sections_and_detail() {
    let (report, _) = fixture();
    let json = serde_json::to_value(&report).unwrap();
    let md = scan_markdown(&report, false);
    for fragment in [
        "Version: 1",
        "Root: `/project`",
        "analyzer.yaml",
        "## Aggregates",
        "p90",
        "p95",
        "3.00–4.00: 1",
        "### Directory `src`",
        "#### File `src/service.py`",
        "##### Class `src.service.Service`",
        "Function `src.service.Service.run`",
        "function_lines",
        "## Violations",
        "syntax error",
        "## Call graph",
        "not ready",
        "!ready",
        "for item in items",
        "```mermaid",
    ] {
        assert!(md.contains(fragment), "missing {fragment:?} in Markdown");
    }
    assert_eq!(json["metrics"][0]["id"], "function_lines");
    assert_eq!(json["graph"]["edges"][0]["guard"][0]["raw"], "not ready");
    assert_eq!(json["parse_errors"][0]["message"], "syntax error");
}

#[test]
fn graph_markdown_contains_chain_conditions_coverage_and_mermaid() {
    let (_, query) = fixture();
    let json = serde_json::to_value(&query).unwrap();
    let md = graph_markdown(&query);
    for fragment in [
        "## Coverage",
        "Functions: 2",
        "src.service.Service.run",
        "src.service.finish",
        "## Call chains",
        "src/service.py",
        "Resolved",
        "not ready",
        "!ready",
        "for item in items",
        "## Diagram",
        "```mermaid",
    ] {
        assert!(md.contains(fragment), "missing {fragment:?} in Markdown");
    }
    assert_eq!(
        json["edges"][0]["in_loop"][0]["header"],
        "for item in items"
    );
}
