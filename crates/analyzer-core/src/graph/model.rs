//! Graph transport types and adjacency indexes.
use crate::ir::*;
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Function,
    External,
    Unknown,
    VendorPackage,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub label: String,
    pub kind: NodeKind,
    pub file: Option<String>,
    pub line: Option<usize>,
    pub language: Option<String>,
    pub module: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cargo_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cargo_target_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_fqn: Option<String>,
    pub parent_type: Option<String>,
    pub vendor: bool,
    pub package: Option<String>,
    pub category: Option<String>,
    pub library: Option<String>,
    pub operation: Option<String>,
    pub argument: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub algorithmic_complexity: Option<Arc<crate::algorithmic::model::ComplexityEstimate>>,
}
impl GraphNode {
    /// Stable identity of the physical declaration behind a Cargo context node.
    pub fn source_id(&self) -> Cow<'_, str> {
        match self.source_fqn.as_deref() {
            Some(source) => Cow::Owned(format!(
                "fn:{}:{}:{source}",
                self.language.as_deref().unwrap_or("rust"),
                self.file.as_deref().unwrap_or("")
            )),
            None => Cow::Borrowed(&self.id),
        }
    }
    pub fn source_parent_type(&self) -> Option<Cow<'_, str>> {
        let parent = self.parent_type.as_deref()?;
        let Some((module, source)) = self.module.as_deref().zip(self.source_fqn.as_deref()) else {
            return Some(Cow::Borrowed(parent));
        };
        let physical_module = self
            .label
            .strip_prefix(module)
            .and_then(|suffix| source.strip_suffix(suffix));
        Some(match physical_module.zip(parent.strip_prefix(module)) {
            Some((module, suffix)) => Cow::Owned(format!("{module}{suffix}")),
            None => Cow::Borrowed(parent),
        })
    }
    pub fn display_label(&self) -> String {
        if let Some(e) = &self.algorithmic_complexity {
            if let Some(formula) = &e.formula {
                format!("{} · {} · {}", self.label, formula, e.status.label())
            } else {
                format!("{} · {}", self.label, e.status.label())
            }
        } else {
            self.label.clone()
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GraphEdge {
    pub from: Arc<str>,
    pub to: Arc<str>,
    pub callsite: SourceRange,
    pub confidence: Confidence,
    pub guard: Arc<[Guard]>,
    pub in_loop: Arc<[LoopContext]>,
    pub recursive: bool,
    pub hidden_vendor: bool,
    /// Redacted literal argument of this external operation at this source site.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argument: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct CallGraph {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolution_diagnostics: Vec<String>,
    #[serde(skip)]
    pub(super) outgoing: HashMap<Arc<str>, Vec<usize>>,
    #[serde(skip)]
    pub(super) incoming: HashMap<Arc<str>, Vec<usize>>,
    #[serde(skip)]
    pub(super) by_id: HashMap<String, usize>,
    /// Package names owning each project file, matched by `pkg:` selectors.
    #[serde(skip)]
    pub(crate) source_packages: BTreeMap<String, Vec<String>>,
}
impl<'de> Deserialize<'de> for CallGraph {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        pub(super) struct StoredGraph {
            pub(super) nodes: Vec<GraphNode>,
            pub(super) edges: Vec<GraphEdge>,
            #[serde(default)]
            pub(super) resolution_diagnostics: Vec<String>,
        }
        let stored = StoredGraph::deserialize(deserializer)?;
        let mut graph = Self {
            nodes: stored.nodes,
            edges: stored.edges,
            resolution_diagnostics: stored.resolution_diagnostics,
            ..Self::default()
        };
        graph.reindex();
        Ok(graph)
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Callers,
    Callees,
    Both,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GraphFilter {
    pub language: Option<String>,
    pub module: Option<String>,
    pub directory: Option<String>,
    pub exclude: Vec<String>,
    pub confidence: Vec<Confidence>,
    pub external_categories: Vec<String>,
    pub with_external: bool,
    #[serde(default)]
    pub with_runtime: bool,
}
impl Default for GraphFilter {
    fn default() -> Self {
        Self {
            language: None,
            module: None,
            directory: None,
            exclude: Vec::new(),
            confidence: Vec::new(),
            external_categories: Vec::new(),
            with_external: true,
            with_runtime: false,
        }
    }
}
impl GraphFilter {
    pub fn all() -> Self {
        Self::default()
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Coverage {
    pub functions: Vec<String>,
    pub classes: Vec<String>,
    pub files: Vec<String>,
    pub modules: Vec<String>,
    pub vendor_functions: Vec<String>,
    pub vendor_classes: Vec<String>,
    pub external: Vec<String>,
    pub unresolved: Vec<String>,
    pub entry_points: Vec<String>,
    pub leaves: Vec<String>,
    pub vendor_boundary: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GraphQuery {
    pub roots: Vec<String>,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub cycles: Vec<GraphEdge>,
    pub hidden_vendor_edges: Vec<GraphEdge>,
    pub coverage: Coverage,
    pub direction: Direction,
    pub depth: Option<usize>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PathResult {
    pub paths: Vec<Vec<String>>,
    pub truncated: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RelationResult {
    pub a: Vec<String>,
    pub b: Vec<String>,
    pub a_to_b: Vec<GraphEdge>,
    pub b_to_a: Vec<GraphEdge>,
    pub matrix: BTreeMap<String, BTreeMap<String, usize>>,
    pub overlap: Vec<String>,
    #[serde(default)]
    pub nodes: Vec<GraphNode>,
    #[serde(default)]
    pub paths_a_to_b: Vec<RelationPath>,
    #[serde(default)]
    pub paths_b_to_a: Vec<RelationPath>,
    #[serde(default)]
    pub counts_a: RelationCounts,
    #[serde(default)]
    pub counts_b: RelationCounts,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub direct_call_matrix: BTreeMap<String, BTreeMap<String, usize>>,
    #[serde(default)]
    pub direct_call_count: usize,
    #[serde(default)]
    pub route_count: usize,
    #[serde(default)]
    pub path_mode: RelationPathMode,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub truncation_reasons: Vec<String>,
    #[serde(default)]
    pub work_used: usize,
    #[serde(default)]
    pub options: RelationOptions,
    #[serde(default)]
    pub routes_evaluated: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationPathMode {
    #[default]
    OneShortest,
    AllShortest,
    #[serde(alias = "simple_routes", alias = "bounded_simple")]
    Simple,
}

/// Query arguments for relations; selectors can reference configured `set:name`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RelationOptions {
    pub a: Vec<String>,
    pub b: Vec<String>,
    pub exclude_a: Vec<String>,
    pub exclude_b: Vec<String>,
    pub transitive_depth: Option<usize>,
    pub path_mode: RelationPathMode,
    pub max_paths_per_pair: usize,
    pub max_paths_total: usize,
    pub work_budget: usize,
}
impl Default for RelationOptions {
    fn default() -> Self {
        Self {
            a: Vec::new(),
            b: Vec::new(),
            exclude_a: Vec::new(),
            exclude_b: Vec::new(),
            transitive_depth: None,
            path_mode: RelationPathMode::OneShortest,
            max_paths_per_pair: 100,
            max_paths_total: 10_000,
            work_budget: 1_000_000,
        }
    }
}

#[derive(Default)]
pub(super) struct RelationBudget {
    pub(super) work: usize,
    pub(super) total: usize,
    pub(super) stored_hops: usize,
    pub(super) reasons: BTreeSet<String>,
    pub(super) stopped: bool,
}
impl RelationBudget {
    pub(super) fn step(&mut self, options: &RelationOptions) -> bool {
        if self.stopped {
            return false;
        }
        if self.work >= options.work_budget {
            self.reasons.insert("work_budget".into());
            self.stopped = true;
            return false;
        }
        self.work += 1;
        true
    }
    pub(super) fn store(&mut self, options: &RelationOptions, hops: usize) -> bool {
        if self.total >= options.max_paths_total {
            self.reasons.insert("max_paths_total".into());
            self.stopped = true;
            return false;
        }
        // Even many long returned routes must fit a finite evidence budget.
        if self.stored_hops.saturating_add(hops) > options.work_budget {
            self.reasons.insert("evidence_budget".into());
            self.stopped = true;
            return false;
        }
        self.total += 1;
        self.stored_hops += hops;
        true
    }
}

/// An actual route through the call graph, retaining every callsite and guard.
/// Direct relation arrays contain only real direct calls, never synthetic edges.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RelationPath {
    pub from: String,
    pub to: String,
    pub nodes: Vec<String>,
    pub edges: Vec<GraphEdge>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RelationCounts {
    pub functions: usize,
    pub classes: usize,
    pub files: usize,
    pub vendor: usize,
}

impl RelationResult {
    /// Rebuild the matrix at file, class or function level. Each real callsite
    /// or indirect route counts once even when it occurs in both directions
    /// because the selected sets overlap. Distinct callsites remain distinct.
    pub fn regroup_matrix(&mut self, level: &str) {
        if level == "set" {
            self.regroup_matrix("function");
            self.matrix.clear();
            self.direct_call_matrix.clear();
            for (from, to, calls, paths) in [
                ("A", "B", &self.a_to_b, &self.paths_a_to_b),
                ("B", "A", &self.b_to_a, &self.paths_b_to_a),
            ] {
                let evidence = |edges: &[GraphEdge]| {
                    edges
                        .iter()
                        .map(|edge| (edge.from.clone(), edge.to.clone(), edge.callsite.clone()))
                        .collect::<Vec<_>>()
                };
                let direct = calls
                    .iter()
                    .map(|edge| evidence(std::slice::from_ref(edge)))
                    .collect::<BTreeSet<_>>()
                    .len();
                let routes = if !self.routes_evaluated
                    && self.paths_a_to_b.is_empty()
                    && self.paths_b_to_a.is_empty()
                {
                    direct
                } else {
                    paths
                        .iter()
                        .map(|path| evidence(&path.edges))
                        .collect::<BTreeSet<_>>()
                        .len()
                };
                if direct > 0 || routes > 0 {
                    self.matrix
                        .entry(from.into())
                        .or_default()
                        .insert(to.into(), routes);
                    self.direct_call_matrix
                        .entry(from.into())
                        .or_default()
                        .insert(to.into(), direct);
                }
            }
            return;
        }
        let nodes = self
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node))
            .collect::<HashMap<_, _>>();
        let group = |id: &str| {
            nodes
                .get(id)
                .and_then(|node| match level {
                    "file" => node.file.as_deref(),
                    "class" => node.parent_type.as_deref().or(node.file.as_deref()),
                    _ => Some(node.label.as_str()),
                })
                .unwrap_or(id)
                .to_owned()
        };
        let mut matrix = BTreeMap::<String, BTreeMap<String, usize>>::new();
        let mut seen = BTreeSet::new();
        let mut record =
            |from: &str, to: &str, evidence: Vec<(Arc<str>, Arc<str>, SourceRange)>| {
                if seen.insert((from.to_owned(), to.to_owned(), evidence)) {
                    *matrix
                        .entry(group(from))
                        .or_default()
                        .entry(group(to))
                        .or_default() += 1;
                }
            };
        if !self.routes_evaluated && self.paths_a_to_b.is_empty() && self.paths_b_to_a.is_empty() {
            // Older serialized results have only the original direct fields.
            for edge in self.a_to_b.iter().chain(&self.b_to_a) {
                record(
                    &edge.from,
                    &edge.to,
                    vec![(edge.from.clone(), edge.to.clone(), edge.callsite.clone())],
                );
            }
        } else {
            for path in self.paths_a_to_b.iter().chain(&self.paths_b_to_a) {
                record(
                    &path.from,
                    &path.to,
                    path.edges
                        .iter()
                        .map(|edge| (edge.from.clone(), edge.to.clone(), edge.callsite.clone()))
                        .collect(),
                );
            }
        }
        self.route_count = seen.len();
        self.matrix = matrix;
        let mut direct = BTreeMap::<String, BTreeMap<String, usize>>::new();
        let mut seen_direct = BTreeSet::new();
        for edge in self.a_to_b.iter().chain(&self.b_to_a) {
            if seen_direct.insert((edge.from.clone(), edge.to.clone(), edge.callsite.clone())) {
                *direct
                    .entry(group(&edge.from))
                    .or_default()
                    .entry(group(&edge.to))
                    .or_default() += 1;
            }
        }
        self.direct_call_count = seen_direct.len();
        for (from, targets) in &direct {
            for to in targets.keys() {
                self.matrix
                    .entry(from.clone())
                    .or_default()
                    .entry(to.clone())
                    .or_insert(0);
            }
        }
        self.direct_call_matrix = direct;
    }
}
impl CallGraph {
    pub fn reindex(&mut self) {
        // Deserialized reports have independent allocations for each string.
        // Rebuild sharing here as well as indexes, without a process-global pool.
        let mut ids = BTreeSet::<Arc<str>>::new();
        let mut paths = BTreeSet::<Arc<str>>::new();
        let mut guards = BTreeSet::<Arc<[Guard]>>::new();
        let mut loops = BTreeSet::<Arc<[LoopContext]>>::new();
        for edge in &mut self.edges {
            if let Some(shared) = paths.get(edge.callsite.file.as_ref()) {
                edge.callsite.file = shared.clone();
            } else {
                paths.insert(edge.callsite.file.clone());
            }
            if let Some(shared) = guards.get(edge.guard.as_ref()) {
                edge.guard = shared.clone();
            } else {
                guards.insert(edge.guard.clone());
            }
            if let Some(shared) = loops.get(edge.in_loop.as_ref()) {
                edge.in_loop = shared.clone();
            } else {
                loops.insert(edge.in_loop.clone());
            }
            for id in [&mut edge.from, &mut edge.to] {
                if let Some(shared) = ids.get(id.as_ref()) {
                    *id = shared.clone();
                } else {
                    ids.insert(id.clone());
                }
            }
        }
        self.nodes.sort_by(|a, b| a.id.cmp(&b.id));
        self.edges.sort_by(|a, b| {
            (&a.from, &a.to, &a.callsite.file, a.callsite.start_line).cmp(&(
                &b.from,
                &b.to,
                &b.callsite.file,
                b.callsite.start_line,
            ))
        });
        self.outgoing.clear();
        self.incoming.clear();
        self.by_id.clear();
        for (i, n) in self.nodes.iter().enumerate() {
            self.by_id.insert(n.id.clone(), i);
        }
        for (i, e) in self.edges.iter().enumerate() {
            self.outgoing.entry(e.from.clone()).or_default().push(i);
            self.incoming.entry(e.to.clone()).or_default().push(i);
        }
    }
    /// Outgoing edges in the canonical graph order. Rebuilt with `reindex`.
    pub fn outgoing_edges(&self, id: &str) -> impl Iterator<Item = &GraphEdge> {
        self.outgoing
            .get(id)
            .into_iter()
            .flatten()
            .map(|index| &self.edges[*index])
    }
    pub fn node(&self, id: &str) -> Option<&GraphNode> {
        self.by_id.get(id).and_then(|i| self.nodes.get(*i))
    }
}

impl GraphFilter {
    pub(super) fn accepts(&self, node: &GraphNode, edge: &GraphEdge) -> bool {
        if !self.with_runtime && node.category.as_deref() == Some("runtime") {
            return false;
        }
        if node.kind == NodeKind::External
            && (!self.with_external
                || (!self.external_categories.is_empty()
                    && !node
                        .category
                        .as_ref()
                        .is_some_and(|c| self.external_categories.contains(c))))
        {
            return false;
        }
        if self
            .language
            .as_ref()
            .is_some_and(|l| node.language.as_ref() != Some(l))
        {
            return false;
        }
        if self
            .module
            .as_ref()
            .is_some_and(|m| node.module.as_ref() != Some(m))
        {
            return false;
        }
        if self.directory.as_ref().is_some_and(|d| {
            !node.file.as_ref().is_some_and(|f| {
                let directory = d.trim_end_matches('/');
                directory.is_empty()
                    || f == directory
                    || f.strip_prefix(directory)
                        .is_some_and(|rest| rest.starts_with('/'))
            })
        }) {
            return false;
        }
        if self
            .exclude
            .iter()
            .any(|x| node.id.contains(x) || node.label.contains(x))
        {
            return false;
        }
        if !self.confidence.is_empty() && !self.confidence.contains(&edge.confidence) {
            return false;
        }
        true
    }
}
