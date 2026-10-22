//! Public report models; constructing scans belongs to the pipeline.
use crate::{
    graph::{CallGraph, Direction, GraphFilter, GraphQuery},
    ir::*,
    metrics::{AggregateGroup, MetricValue},
    rules::Violation,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Summary {
    pub files: usize,
    pub functions: usize,
    pub types: usize,
    pub warnings: usize,
    pub errors: usize,
    pub vendor_files: usize,
    pub parse_errors: usize,
    pub cache_hits: usize,
    #[serde(default)]
    pub architecture_warnings: usize,
    pub parsed_files: usize,
    /// Findings hidden by the baseline file during this scan.
    #[serde(default)]
    pub baseline_suppressed: usize,
    /// Baseline entries that matched nothing: stale after fixes or rule changes.
    #[serde(default)]
    pub baseline_stale: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnalysisReport {
    pub version: String,
    pub schema_version: String,
    pub root: String,
    pub config_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changes: Option<super::diff::ChangeSummary>,
    #[serde(default)]
    pub strategy_capabilities: BTreeMap<String, Capabilities>,
    pub summary: Summary,
    pub aggregates: Vec<AggregateGroup>,
    pub files: Vec<FileModel>,
    pub metrics: Vec<MetricValue>,
    pub violations: Vec<Violation>,
    pub parse_errors: Vec<ParseError>,
    pub graph: CallGraph,
    pub deferred_vendor: Vec<DiscoveredVendor>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolution_diagnostics: Vec<String>,
    #[serde(default)]
    pub architecture: crate::architecture::ArchitectureSummary,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiscoveredVendor {
    pub path: String,
    pub package: String,
    pub language: String,
}

impl AnalysisReport {
    pub fn graph_query(
        &self,
        selector: &str,
        direction: Direction,
        depth: Option<usize>,
        filter: &GraphFilter,
        config: &crate::config::Config,
    ) -> GraphQuery {
        self.graph.query(selector, direction, depth, filter, config)
    }
    pub fn file(&self, path: &str) -> Option<&FileModel> {
        self.files.iter().find(|f| f.path == path)
    }
    pub fn symbol(&self, id: &str) -> Option<(&FileModel, &FunctionModel)> {
        self.files.iter().find_map(|file| {
            file.functions
                .iter()
                .find(|f| {
                    f.fqn == id || format!("fn:{}:{}:{}", file.language, file.path, f.fqn) == id
                })
                .map(|f| (file, f))
        })
    }
    pub fn search(&self, query: &str) -> Vec<&FunctionModel> {
        let mut out = self
            .files
            .iter()
            .flat_map(|f| &f.functions)
            .filter_map(|f| crate::search::fuzzy_score(&f.fqn, query).map(|score| (score, f)))
            .collect::<Vec<_>>();
        out.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.fqn.cmp(&b.1.fqn)));
        out.into_iter().map(|(_, function)| function).collect()
    }
}
