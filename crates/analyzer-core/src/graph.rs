use crate::analysis::LanguageRegistry;
use crate::config::{CaptureArgs, Config, VendorMode};
use crate::ir::*;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;
mod cargo;
pub(crate) use cargo::owned_rust_files;
mod inheritance;
mod properties;

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
    pub algorithmic_complexity: Option<Arc<crate::algorithmic::ComplexityEstimate>>,
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
    outgoing: HashMap<Arc<str>, Vec<usize>>,
    #[serde(skip)]
    incoming: HashMap<Arc<str>, Vec<usize>>,
    #[serde(skip)]
    by_id: HashMap<String, usize>,
    /// Package names owning each project file, matched by `pkg:` selectors.
    #[serde(skip)]
    pub(crate) source_packages: BTreeMap<String, Vec<String>>,
}
impl<'de> Deserialize<'de> for CallGraph {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct StoredGraph {
            nodes: Vec<GraphNode>,
            edges: Vec<GraphEdge>,
            #[serde(default)]
            resolution_diagnostics: Vec<String>,
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
struct RelationBudget {
    work: usize,
    total: usize,
    stored_hops: usize,
    reasons: BTreeSet<String>,
    stopped: bool,
}
impl RelationBudget {
    fn step(&mut self, options: &RelationOptions) -> bool {
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
    fn store(&mut self, options: &RelationOptions, hops: usize) -> bool {
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

/// Exact declarations and all Rust `::` prefixes, borrowing file metadata.
struct ModuleIndex<'a> {
    exact: BTreeSet<(&'a str, &'a str)>,
    rust_prefixes: HashSet<&'a str>,
    rust_aliases: BTreeMap<String, BTreeSet<String>>,
    rust_contexts: BTreeMap<String, BTreeSet<String>>,
    rust_roots: BTreeMap<String, BTreeSet<String>>,
    rust_root_files: BTreeMap<String, String>,
    rust_sources: BTreeMap<String, BTreeSet<String>>,
    rust_owners: BTreeMap<String, BTreeSet<String>>,
    all_rust_roots: bool,
    esm_files: BTreeMap<String, Vec<String>>,
    resolution: Option<&'a crate::resolution::ResolutionContext>,
    rust_config: crate::config::RustResolutionConfig,
    node_conditions: Vec<String>,
    strategies: LanguageRegistry,
}
impl<'a> ModuleIndex<'a> {
    fn new(exact: BTreeSet<(&'a str, &'a str)>) -> Self {
        let mut rust_prefixes = HashSet::new();
        for (_, symbol) in exact.iter().filter(|(language, _)| *language == "rust") {
            rust_prefixes.insert(*symbol);
            for (offset, ch) in symbol.char_indices() {
                if ch == ':' && symbol[offset..].starts_with("::") {
                    rust_prefixes.insert(&symbol[..offset]);
                }
            }
        }
        Self {
            exact,
            rust_prefixes,
            rust_aliases: BTreeMap::new(),
            rust_contexts: BTreeMap::new(),
            rust_roots: BTreeMap::new(),
            rust_root_files: BTreeMap::new(),
            rust_sources: BTreeMap::new(),
            rust_owners: BTreeMap::new(),
            all_rust_roots: false,
            esm_files: BTreeMap::new(),
            resolution: None,
            rust_config: crate::config::RustResolutionConfig::default(),
            node_conditions: vec!["node".into()],
            strategies: LanguageRegistry::new(),
        }
    }
    fn with_registry(mut self, registry: &LanguageRegistry) -> Self {
        self.strategies = registry.clone();
        self
    }
    fn type_reference(&self, file: &FileModel, raw: &str) -> Option<String> {
        self.strategies.get(&file.language).map_or_else(
            || crate::types::normalize_reference(raw, &file.type_reference_syntax),
            |strategy| strategy.type_reference(raw),
        )
    }
    fn native_receiver(&self, language: &str, raw: &str) -> bool {
        self.strategies
            .get(language)
            .is_some_and(|strategy| strategy.is_native_receiver(raw))
    }
    fn namespace_separator(&self, language: &str) -> &str {
        self.strategies.get(language).map_or_else(
            || match language {
                "php" => "\\",
                "rust" => "::",
                _ => ".",
            },
            |strategy| strategy.namespace_separator(),
        )
    }
    fn method_separator(&self, language: &str) -> &str {
        self.strategies.get(language).map_or_else(
            || {
                if matches!(language, "php" | "rust") {
                    "::"
                } else {
                    "."
                }
            },
            |strategy| strategy.method_separator(),
        )
    }
    fn with_files(
        self,
        files: &[FileModel],
        resolution: &'a crate::resolution::ResolutionContext,
        config: &Config,
    ) -> Self {
        self.with_file_refs(&files.iter().collect::<Vec<_>>(), Some(resolution), config)
    }
    fn with_file_refs(
        mut self,
        files: &[&FileModel],
        resolution: Option<&'a crate::resolution::ResolutionContext>,
        config: &Config,
    ) -> Self {
        self.resolution = resolution;
        let default_resolution = crate::resolution::ResolutionContext::default();
        let resolution = resolution.unwrap_or(&default_resolution);
        self.rust_config = config.resolution.rust.clone();
        self.node_conditions = config.resolution.node_conditions.clone();
        for file in files
            .iter()
            .copied()
            .filter(|file| matches!(file.language.as_str(), "javascript" | "typescript"))
        {
            self.esm_files
                .entry(normalize_rust_file_path(std::path::Path::new(&file.path)))
                .or_default()
                .push(file.module.clone());
        }
        let rust = files
            .iter()
            .copied()
            .filter(|file| file.language == "rust")
            .collect::<Vec<_>>();
        let mut by_path = BTreeMap::<String, Vec<&FileModel>>::new();
        for file in &rust {
            by_path
                .entry(file.path.replace('\\', "/"))
                .or_default()
                .push(file);
            if let Some(target) = file.cargo_target.as_deref() {
                if resolution.rust_target_root(target) == Some(file.path.as_str()) {
                    self.rust_sources
                        .entry(target.into())
                        .or_default()
                        .insert(file.module.clone());
                }
            } else {
                self.rust_sources
                    .entry(file.path.clone())
                    .or_default()
                    .insert(file.module.clone());
            }
        }
        let mut pending = VecDeque::new();
        for file in &rust {
            let mut targets = if let Some(target) = file.cargo_target.as_deref() {
                if resolution.rust_target_root(target) == Some(file.path.as_str()) {
                    vec![target]
                } else {
                    Vec::new()
                }
            } else {
                resolution.rust_target_ids(&file.path)
            };
            if targets.is_empty()
                && resolution.cargo_roots.is_empty()
                && matches!(
                    std::path::Path::new(&file.path)
                        .file_stem()
                        .and_then(|stem| stem.to_str()),
                    Some("lib" | "main")
                )
            {
                targets.push(&file.path);
            }
            let distinct = targets.len() > 1;
            for target in targets {
                let rust_config = resolution.rust_config_for(target, &self.rust_config);
                if !self.all_rust_roots
                    && resolution.rust_root_enabled(target, &rust_config) == Some(false)
                {
                    continue;
                }
                let module = if distinct && file.cargo_target.is_none() {
                    cargo::context_module(&file.module, target)
                } else {
                    file.module.clone()
                };
                self.rust_root_files.insert(module.clone(), target.into());
                pending.push_back((*file, module.clone(), module, vec![file.path.as_str()]));
            }
        }
        let mut visited = BTreeSet::new();
        while let Some((file, context, root, ancestry)) = pending.pop_front() {
            if !visited.insert((file.path.as_str(), context.clone(), root.clone())) {
                continue;
            }
            if let Some(root_file) = self.rust_root_files.get(&root) {
                self.rust_owners
                    .entry(file.path.clone())
                    .or_default()
                    .insert(root_file.clone());
            }
            self.rust_contexts
                .entry(file.module.clone())
                .or_default()
                .insert(context.clone());
            self.rust_roots
                .entry(file.module.clone())
                .or_default()
                .insert(root.clone());
            let rust_config = resolution.rust_config_for(
                self.rust_root_files
                    .get(&root)
                    .map(String::as_str)
                    .unwrap_or(&file.path),
                &self.rust_config,
            );
            for namespace in file
                .types
                .iter()
                .filter(|ty| ty.kind == TypeKind::Namespace && ty.external_module)
            {
                use crate::resolution::ModulePath;
                let mut namespace_for_ownership;
                let namespace = if self.all_rust_roots {
                    namespace_for_ownership = namespace.clone();
                    namespace_for_ownership.rust_cfg.clear();
                    &namespace_for_ownership
                } else {
                    namespace
                };
                let module_path = crate::resolution::module_path(namespace, &rust_config);
                if matches!(module_path, ModulePath::Inactive | ModulePath::Unknown) {
                    continue;
                }
                let Some(relative) = namespace.fqn.strip_prefix(&format!("{}::", file.module))
                else {
                    continue;
                };
                let parts = relative.split("::").collect::<Vec<_>>();
                let source = std::path::Path::new(&file.path);
                let mut directory = source
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new(""))
                    .to_path_buf();
                let stem = source
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("");
                let mut implied_stem = (!matches!(stem, "lib" | "main" | "mod")
                    && !resolution.cargo_roots.contains(&file.path))
                .then_some(stem);
                let mut owner = file.module.clone();
                for part in &parts[..parts.len() - 1] {
                    owner.push_str("::");
                    owner.push_str(part);
                    let inline = file
                        .types
                        .iter()
                        .find(|ty| ty.kind == TypeKind::Namespace && ty.fqn == owner)
                        .map(|ty| crate::resolution::module_path(ty, &rust_config));
                    if let Some(ModulePath::Path(path)) = inline {
                        // An inline #[path] starts relative to the declaring
                        // file's directory, replacing its implied foo/ prefix.
                        implied_stem = None;
                        directory.push(path);
                    } else {
                        if let Some(stem) = implied_stem.take() {
                            directory.push(stem);
                        }
                        directory.push(part);
                    }
                }
                if matches!(module_path, ModulePath::Default) {
                    if let Some(stem) = implied_stem {
                        directory.push(stem);
                    }
                }
                let candidates = if let ModulePath::Path(path) = &module_path {
                    vec![directory.join(path)]
                } else {
                    vec![
                        directory.join(format!("{}.rs", namespace.name)),
                        directory.join(&namespace.name).join("mod.rs"),
                    ]
                };
                let matches = candidates
                    .iter()
                    .flat_map(|candidate| {
                        let normalized = normalize_rust_file_path(candidate);
                        by_path
                            .get(&normalized)
                            .into_iter()
                            .flatten()
                            .copied()
                            .filter(|target| {
                                target.cargo_target.as_ref().is_none_or(|target_owner| {
                                    self.rust_root_files.get(&root) == Some(target_owner)
                                })
                            })
                    })
                    .collect::<Vec<_>>();
                // Rust rejects foo.rs + foo/mod.rs ambiguity; do not invent a winner.
                let [target] = matches.as_slice() else {
                    continue;
                };
                let logical = format!("{context}::{relative}");
                self.rust_aliases
                    .entry(logical.clone())
                    .or_default()
                    .insert(target.module.clone());
                if !ancestry.contains(&target.path.as_str()) {
                    let mut ancestry = ancestry.clone();
                    ancestry.push(target.path.as_str());
                    pending.push_back((*target, logical, root.clone(), ancestry));
                }
            }
        }
        self
    }
    fn rust_canonical(&self, symbol: &str) -> Option<String> {
        if self.rust_aliases.is_empty() {
            return self
                .rust_prefixes
                .contains(symbol)
                .then(|| symbol.to_owned());
        }
        let mut current = symbol.to_owned();
        let mut visited = BTreeSet::new();
        while visited.insert(current.clone()) {
            let mut prefix = current.as_str();
            let alias = loop {
                if let Some(targets) = self.rust_aliases.get(prefix) {
                    break Some((prefix.len(), targets));
                }
                let Some((parent, _)) = prefix.rsplit_once("::") else {
                    break None;
                };
                prefix = parent;
            };
            let Some((prefix_length, targets)) = alias else {
                return self
                    .rust_prefixes
                    .contains(current.as_str())
                    .then_some(current);
            };
            if targets.len() != 1 {
                return None;
            }
            let target = targets.iter().next().unwrap();
            current = format!("{target}{}", &current[prefix_length..]);
        }
        None
    }
    fn esm_resource_symbol(
        &self,
        file: &FileModel,
        resource: &str,
        suffix: &str,
    ) -> Option<String> {
        self.esm_resource_symbol_kind(file, resource, suffix, false)
    }
    fn esm_resource_symbol_kind(
        &self,
        file: &FileModel,
        resource: &str,
        suffix: &str,
        require: bool,
    ) -> Option<String> {
        const EXTENSIONS: [&str; 8] = ["js", "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts"];
        let candidates_for = |base: &std::path::Path, directory_entry: bool| {
            let mut candidates = Vec::new();
            let explicit = base
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| EXTENSIONS.contains(&extension));
            if !directory_entry {
                if explicit {
                    candidates.push(base.to_path_buf());
                    // Source projects often spell emitted .js resources in TS
                    // imports. An existing literal file remains authoritative.
                    if !self.esm_files.contains_key(&normalize_rust_file_path(base)) {
                        match base.extension().and_then(|extension| extension.to_str()) {
                            Some("js") => {
                                candidates.push(base.with_extension("ts"));
                                candidates.push(base.with_extension("tsx"));
                            }
                            Some("jsx") => candidates.push(base.with_extension("tsx")),
                            Some("mjs") => candidates.push(base.with_extension("mts")),
                            Some("cjs") => candidates.push(base.with_extension("cts")),
                            _ => {}
                        }
                    }
                } else {
                    candidates.extend(EXTENSIONS.iter().map(|extension| {
                        std::path::PathBuf::from(format!("{}.{extension}", base.to_string_lossy()))
                    }));
                }
            }
            if !explicit {
                candidates.extend(
                    EXTENSIONS
                        .iter()
                        .map(|extension| base.join(format!("index.{extension}"))),
                );
            }
            candidates
        };
        let resolve = |candidates: Vec<std::path::PathBuf>| {
            let found = candidates
                .iter()
                .filter_map(|candidate| {
                    let path = normalize_rust_file_path(candidate);
                    self.esm_files.contains_key(&path).then_some(path)
                })
                .collect::<BTreeSet<_>>();
            if found.len() > 1 {
                Some(format!("<ambiguous-package>::{resource}{suffix}"))
            } else {
                found
                    .into_iter()
                    .next()
                    .map(|path| format!("{path}{suffix}"))
            }
        };
        if resource.starts_with(['.', '/']) {
            let base = std::path::Path::new(&file.path)
                .parent()
                .unwrap_or_else(|| std::path::Path::new(""))
                .join(resource);
            return resolve(candidates_for(&base, false));
        }
        if resource.contains(':') {
            return None;
        }
        let mut alias_applied = false;
        if let Some(context) = self.resolution {
            match context.alias_targets(&file.path, resource) {
                crate::resolution::ResourceResolution::Blocked => {
                    return Some(format!("<unresolved>::{resource}{suffix}"))
                }
                crate::resolution::ResourceResolution::Targets(targets) => {
                    alias_applied = true;
                    for target in targets {
                        if let Some(found) =
                            resolve(candidates_for(std::path::Path::new(&target), false))
                        {
                            return Some(found);
                        }
                    }
                }
                crate::resolution::ResourceResolution::Unspecified => {}
            }
        }
        let parts = resource.split('/').collect::<Vec<_>>();
        let package_parts = if resource.starts_with('@') { 2 } else { 1 };
        if parts.len() < package_parts || parts[..package_parts].iter().any(|part| part.is_empty())
        {
            return None;
        }
        let package = parts[..package_parts].join("/");
        let mut directory = std::path::Path::new(&file.path).parent();
        while let Some(parent) = directory {
            let package_dir = parent.join("node_modules").join(&package);
            let package_prefix = format!("{}/", normalize_rust_file_path(&package_dir));
            let present = self
                .esm_files
                .range(package_prefix.clone()..)
                .next()
                .is_some_and(|(path, _)| path.starts_with(&package_prefix));
            let package_path = normalize_rust_file_path(&package_dir);
            if present
                || self
                    .resolution
                    .is_some_and(|context| context.package_present(&package_path))
            {
                let subpath = parts[package_parts..].join("/");
                if let Some(context) = self.resolution {
                    match context.package_targets(
                        &package_path,
                        &subpath,
                        require,
                        &self.node_conditions,
                    ) {
                        crate::resolution::ResourceResolution::Blocked => {
                            return Some(format!("<unresolved>::{resource}{suffix}"))
                        }
                        crate::resolution::ResourceResolution::Targets(targets) => {
                            for target in targets {
                                if let Some(found) =
                                    resolve(candidates_for(std::path::Path::new(&target), false))
                                {
                                    return Some(found);
                                }
                            }
                            return Some(format!("<unresolved>::{resource}{suffix}"));
                        }
                        crate::resolution::ResourceResolution::Unspecified => {}
                    }
                }
                let base = package_dir.join(&subpath);
                // The original import resource is authoritative, including
                // literal dots and explicit file extensions. Never reinterpret
                // an imported symbol as another installed package or file.
                return resolve(candidates_for(&base, subpath.is_empty()));
            }
            directory = parent.parent();
        }
        alias_applied.then(|| format!("<unresolved>::{resource}{suffix}"))
    }
    fn contains(&self, symbol: &(&str, &str)) -> bool {
        self.exact.contains(symbol)
    }
    #[cfg(test)]
    fn rust_exists(&self, symbol: &str) -> bool {
        self.rust_canonical(symbol).is_some()
    }
}

fn normalize_rust_file_path(path: &std::path::Path) -> String {
    crate::resolution::normalize(path)
}

/// Shared declaration-level resolver for call and architecture analysis.
/// Import.module is lexical context, not the imported destination.
pub(crate) struct DeclarationResolver<'a> {
    modules: ModuleIndex<'a>,
    exports: BTreeMap<&'a str, BTreeMap<String, String>>,
    runtime_exports: BTreeMap<&'a str, BTreeMap<String, String>>,
    types: BTreeSet<(&'a str, &'a str)>,
}
impl<'a> DeclarationResolver<'a> {
    pub(crate) fn new(files: &[&'a FileModel]) -> Self {
        let modules = ModuleIndex::new(
            files
                .iter()
                .flat_map(|file| {
                    std::iter::once((file.language.as_str(), file.module.as_str()))
                        .chain(
                            file.types
                                .iter()
                                .map(|t| (file.language.as_str(), t.fqn.as_str())),
                        )
                        .chain(
                            file.functions
                                .iter()
                                .map(|f| (file.language.as_str(), f.fqn.as_str())),
                        )
                })
                .collect(),
        )
        .with_file_refs(files, None, &Config::default());
        let exports = export_bindings(files, &modules, None);
        let runtime_exports = export_bindings(files, &modules, Some(&exports));
        let types = files
            .iter()
            .flat_map(|file| {
                file.types
                    .iter()
                    .filter(|t| !matches!(t.kind, TypeKind::Namespace | TypeKind::Impl))
                    .map(|t| (file.language.as_str(), t.fqn.as_str()))
            })
            .collect();
        Self {
            modules,
            exports,
            runtime_exports,
            types,
        }
    }
    pub(crate) fn type_reference(
        &self,
        file: &FileModel,
        name: &str,
        owner: &str,
        range: &SourceRange,
    ) -> String {
        let target = resolve_type_reference(
            file,
            name,
            owner,
            range,
            &self.modules,
            &self.exports,
            &self.types,
        );
        let canonical = canonical_export(
            &file.language,
            &declaration_symbol(&self.modules, &target),
            &self.exports,
        );
        declaration_symbol(&self.modules, &canonical)
    }
    /// Preserve the physical ESM declaration before parser module names erase
    /// the distinction between e.g. model.js and model.ts.
    pub(crate) fn type_reference_source(
        &self,
        file: &FileModel,
        name: &str,
        owner: &str,
        range: &SourceRange,
    ) -> Option<&str> {
        if !matches!(file.language.as_str(), "javascript" | "typescript") {
            return None;
        }
        let target = resolve_type_reference(
            file,
            name,
            owner,
            range,
            &self.modules,
            &self.exports,
            &self.types,
        );
        let target = canonical_export(&file.language, &target, &self.exports);
        self.symbol_source(&target)
    }
    pub(crate) fn import_source(&self, file: &FileModel, import: &Import) -> Option<&str> {
        if !matches!(file.language.as_str(), "javascript" | "typescript") {
            return None;
        }
        self.symbol_source(&resolve_scoped_import(file, import, &self.modules))
    }
    pub(crate) fn module_source(&self, file: &FileModel, source: &str) -> Option<String> {
        if !matches!(file.language.as_str(), "javascript" | "typescript") {
            return None;
        }
        // Retain unresolved/ambiguous sentinels so callers cannot replace a
        // failed resource resolution with a guessed same-language module.
        self.modules.esm_resource_symbol(file, source, "")
    }
    fn symbol_source(&self, symbol: &str) -> Option<&str> {
        let mut prefix = symbol;
        loop {
            if let Some((source, _)) = self.modules.esm_files.get_key_value(prefix) {
                return Some(source);
            }
            prefix = prefix.rsplit_once('.')?.0;
        }
    }
    pub(crate) fn import_destination(&self, file: &FileModel, import: &Import) -> String {
        declaration_symbol(
            &self.modules,
            &resolve_scoped_import(file, import, &self.modules),
        )
    }
    pub(crate) fn module_destination(&self, file: &FileModel, source: &str) -> String {
        if matches!(file.language.as_str(), "javascript" | "typescript")
            && (source.starts_with("./") || source.starts_with("../"))
        {
            // Directory imports are resolved by the architecture module index
            // (e.g. folder/index). Keep their lexical path even when the shared
            // symbol index does not contain the directory itself.
            let parent = std::path::Path::new(&file.path)
                .parent()
                .unwrap_or(std::path::Path::new(""));
            let joined = parent.join(source);
            let mut parts = Vec::new();
            for component in joined.components() {
                match component {
                    std::path::Component::ParentDir => {
                        parts.pop();
                    }
                    std::path::Component::Normal(part) => {
                        parts.push(part.to_string_lossy().into_owned())
                    }
                    _ => {}
                }
            }
            return parts.join("/");
        }
        resolve_import_path(file, source, &self.modules)
    }
    /// Preserve a statically imported callable's identity even when its implementation
    /// is outside the project. Equal-priority conflicting aliases are not evidence.
    pub(crate) fn imported_callable(
        &self,
        file: &FileModel,
        symbol: &str,
        range: &SourceRange,
    ) -> Result<Option<String>, ()> {
        let applicable = file.imports.iter().filter(|i| {
            let alias = import_alias(i);
            import_applies(i, range)
                && (file.language != "python"
                    || (i.range.start_line, i.range.start_column)
                        <= (range.start_line, range.start_column))
                && (symbol == alias
                    || symbol.strip_prefix(alias).is_some_and(|rest| {
                        rest.starts_with('.') || rest.starts_with("::") || rest.starts_with('\\')
                    }))
        });
        let mut priority = None;
        let mut targets = BTreeSet::new();
        for import in applicable {
            let alias = import_alias(import);
            let candidate_priority = (
                alias.len(),
                import
                    .scope
                    .as_ref()
                    .map(|s| (s.start_line, s.start_column))
                    .unwrap_or_default(),
                if file.language == "python" {
                    // Python imports execute in source order. The normalizer may
                    // retain an entire function scope for a reassigned alias.
                    (import.range.start_line, import.range.start_column)
                } else {
                    (0, 0)
                },
            );
            if priority.is_none_or(|old| candidate_priority > old) {
                priority = Some(candidate_priority);
                targets.clear();
            }
            if priority == Some(candidate_priority) {
                let destination = if import.type_only {
                    format!("<type-only>::import:{}", import.path)
                } else {
                    resolve_scoped_import(file, import, &self.modules)
                };
                let destination = declaration_symbol(&self.modules, &destination);
                let canonical = canonical_export(
                    &file.language,
                    &format!("{destination}{}", &symbol[alias.len()..]),
                    &self.runtime_exports,
                );
                targets.insert(declaration_symbol(&self.modules, &canonical));
            }
        }
        match targets.len() {
            0 => Ok(None),
            1 => Ok(targets
                .into_iter()
                .next()
                .filter(|target| !target.starts_with("<type-only>::"))),
            _ => Err(()),
        }
    }
}

/// The call graph retains literal file identities, while architecture declaration
/// indexes use parser module names. Translate only a unique, known source file.
fn declaration_symbol(modules: &ModuleIndex<'_>, symbol: &str) -> String {
    let matching = modules
        .esm_files
        .iter()
        .filter_map(|(path, names)| {
            let suffix = symbol.strip_prefix(path)?;
            (suffix.is_empty() || suffix.starts_with('.')).then_some((path, names, suffix))
        })
        .max_by_key(|(path, _, _)| path.len());
    let Some((_, names, suffix)) = matching else {
        return symbol.to_owned();
    };
    let unique = names.iter().collect::<BTreeSet<_>>();
    if unique.len() == 1 {
        format!("{}{suffix}", unique.into_iter().next().unwrap())
    } else {
        format!("unresolved-source({symbol})")
    }
}

impl CallGraph {
    pub fn attach_deferred_vendor(&mut self, packages: &[(String, String, String)]) {
        self.attach_deferred_vendor_with_files(packages, &[]);
    }
    pub fn attach_deferred_vendor_with_files(
        &mut self,
        packages: &[(String, String, String)],
        files: &[FileModel],
    ) {
        self.reindex();
        if packages.is_empty() {
            return;
        }
        let mut existing = self
            .nodes
            .iter()
            .map(|node| node.id.clone())
            .collect::<BTreeSet<_>>();
        let mut by_name = BTreeMap::<(String, String), Vec<String>>::new();
        let mut package_tokens = BTreeMap::<String, Vec<String>>::new();
        for (path, package, language) in packages {
            let id = format!("vendor:{package}");
            if existing.insert(id.clone()) {
                self.nodes.push(GraphNode {
                    id: id.clone(),
                    label: package.clone(),
                    kind: NodeKind::VendorPackage,
                    file: None,
                    line: None,
                    language: Some(language.clone()),
                    module: None,
                    parent_type: None,
                    vendor: true,
                    package: Some(package.clone()),
                    category: None,
                    library: None,
                    operation: None,
                    argument: None,
                    algorithmic_complexity: None,
                    cargo_target: None,
                    cargo_target_id: None,
                    source_fqn: None,
                });
            }
            let mut tokens = package.split('/').map(str::to_string).collect::<Vec<_>>();
            if package == path {
                if let Some(last) = tokens.last_mut() {
                    if let Some(stem) = std::path::Path::new(last)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .map(str::to_string)
                    {
                        *last = stem;
                    }
                }
            }
            let marker = tokens
                .iter()
                .position(|part| {
                    matches!(
                        part.as_str(),
                        "vendor" | "node_modules" | "site-packages" | ".venv"
                    )
                })
                .unwrap_or(0);
            let relevant = tokens[marker..].to_vec();
            if let Some(name) = relevant.last() {
                by_name
                    .entry((language.clone(), name.clone()))
                    .or_default()
                    .push(id.clone());
            }
            package_tokens.insert(id, relevant);
        }
        let mut imports = BTreeMap::<String, &[Import]>::new();
        for file in files {
            for function in &file.functions {
                imports.insert(function_id(file, &function.fqn), &file.imports);
            }
        }
        let unknown_bindings = files
            .iter()
            .flat_map(|f| {
                f.functions.iter().flat_map(|fun| {
                    fun.calls.iter().filter(|c| c.binding_unknown).map(|c| {
                        (
                            c.range.file.as_ref(),
                            c.range.start_line,
                            c.range.start_column,
                        )
                    })
                })
            })
            .collect::<BTreeSet<_>>();
        let mut additions = Vec::new();
        for edge in &self.edges {
            if unknown_bindings.contains(&(
                edge.callsite.file.as_ref(),
                edge.callsite.start_line,
                edge.callsite.start_column,
            )) {
                continue;
            }
            let Some(node) = self.node(&edge.to).filter(|n| n.kind == NodeKind::Unknown) else {
                continue;
            };
            let label = node.label.as_str();
            let language = edge
                .to
                .strip_prefix("unknown:")
                .and_then(|s| s.split_once(':').map(|(language, _)| language))
                .unwrap_or("");
            let mut targets = BTreeSet::new();
            for segment in split_symbol(label) {
                if let Some(found) = by_name.get(&(language.into(), segment.into())) {
                    targets.extend(found.iter().cloned());
                }
            }
            for import in imports
                .get(edge.from.as_ref())
                .into_iter()
                .flat_map(|items| items.iter())
            {
                if !import_applies(import, &edge.callsite) {
                    continue;
                }
                let alias = import.alias.as_deref().unwrap_or_else(|| {
                    import
                        .path
                        .rsplit(['.', ':', '\\', '/'])
                        .find(|s| !s.is_empty())
                        .unwrap_or(&import.path)
                });
                if label != alias
                    && !label.starts_with(&format!("{alias}."))
                    && !label.starts_with(&format!("{alias}::"))
                    && label != import.path
                    && !label.starts_with(&format!("{}::", import.path))
                    && !label.starts_with(&format!("{}.", import.path))
                {
                    continue;
                }
                let imported = split_symbol(&import.path);
                for segment in &imported {
                    if let Some(found) = by_name.get(&(language.into(), (*segment).into())) {
                        for id in found {
                            let Some(tokens) = package_tokens.get(id) else {
                                continue;
                            };
                            let full = tokens.iter().map(String::as_str).collect::<Vec<_>>();
                            let package_name = full.last().copied().unwrap_or("");
                            let exact = imported.windows(full.len()).any(|window| window == full);
                            if exact || imported.contains(&package_name) {
                                targets.insert(id.clone());
                            }
                        }
                    }
                }
            }
            let confidence = if targets.len() > 1 {
                Confidence::Ambiguous
            } else {
                Confidence::Dynamic
            };
            for target in targets {
                let mut new = edge.clone();
                new.to = target.into();
                new.confidence = confidence.clone();
                additions.push(new);
            }
        }
        self.edges.extend(additions);
        self.reindex();
    }
    pub fn build(files: &[FileModel], registry: &LanguageRegistry, config: &Config) -> Self {
        Self::build_with_context(
            files,
            registry,
            config,
            &crate::resolution::ResolutionContext::default(),
        )
    }
    pub fn build_with_context(
        files: &[FileModel],
        registry: &LanguageRegistry,
        config: &Config,
        context: &crate::resolution::ResolutionContext,
    ) -> Self {
        let cargo_files = cargo::models(files, context, config);
        let files = cargo_files.as_deref().unwrap_or(files);
        let rust_configs = files
            .iter()
            .filter(|file| file.language == "rust")
            .map(|file| {
                (
                    file.module.clone(),
                    context.rust_config_for(
                        file.cargo_target.as_deref().unwrap_or(&file.path),
                        &config.resolution.rust,
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let inactive = |file: &FileModel, conditions: &[String]| {
            let rust_config = rust_configs
                .get(&file.module)
                .unwrap_or(&config.resolution.rust);
            context.rust_root_enabled(
                file.cargo_target.as_deref().unwrap_or(&file.path),
                rust_config,
            ) == Some(false)
                || conditions.iter().any(|condition| {
                    crate::resolution::evaluate_cfg(condition, rust_config) == Some(false)
                })
        };
        let active_files = files
            .iter()
            .any(|file| {
                file.language == "rust"
                    && (file
                        .functions
                        .iter()
                        .any(|function| inactive(file, &function.rust_cfg))
                        || file.types.iter().any(|ty| inactive(file, &ty.rust_cfg)))
            })
            .then(|| {
                files
                    .iter()
                    .map(|file| {
                        let mut file = file.clone();
                        if file.language == "rust" {
                            let file_context = file.clone();
                            file.functions
                                .retain(|function| !inactive(&file_context, &function.rust_cfg));
                            file.types
                                .retain(|ty| !inactive(&file_context, &ty.rust_cfg));
                        }
                        file
                    })
                    .collect::<Vec<_>>()
            });
        let files = active_files.as_deref().unwrap_or(files);
        let uncertain_functions = files
            .iter()
            .filter(|file| file.language == "rust")
            .flat_map(|file| {
                file.functions
                    .iter()
                    .filter(|function| {
                        let rust_config = rust_configs
                            .get(&file.module)
                            .unwrap_or(&config.resolution.rust);
                        context
                            .rust_root_enabled(
                                file.cargo_target.as_deref().unwrap_or(&file.path),
                                rust_config,
                            )
                            .is_none()
                            || function.rust_cfg.iter().any(|condition| {
                                crate::resolution::evaluate_cfg(condition, rust_config).is_none()
                            })
                    })
                    .map(|function| function_id(file, &function.fqn))
            })
            .collect::<HashSet<_>>();
        // Browser globals are explicit parser proofs. Ordinary JS modules never
        // participate, and explicit Node package type also overrides source heuristics.
        let browser_files = files
            .iter()
            .filter(|file| file.browser_script && !context.javascript_package_module(&file.path))
            .map(|file| file.path.as_str())
            .collect::<BTreeSet<_>>();
        let mut script_symbols = BTreeMap::<String, BTreeSet<String>>::new();
        let mut script_blocked = BTreeSet::<String>::new();
        for file in files
            .iter()
            .filter(|file| browser_files.contains(file.path.as_str()))
        {
            for binding in &file.script_globals {
                if let Some(target) = &binding.target {
                    script_symbols
                        .entry(binding.name.clone())
                        .or_default()
                        .insert(target.clone());
                } else {
                    script_blocked.insert(binding.name.clone());
                }
            }
        }
        let script_blocked_name = |name: &str| {
            if script_blocked.contains("*") {
                return true;
            }
            let mut prefix = Some(name);
            while let Some(scope) = prefix {
                if script_blocked.contains(scope) || script_blocked.contains(&format!("{scope}.*"))
                {
                    return true;
                }
                prefix = scope.rsplit_once('.').map(|(parent, _)| parent);
            }
            false
        };
        let blocked_script_targets = script_symbols
            .iter()
            .filter(|(name, _)| script_blocked_name(name))
            .flat_map(|(_, targets)| targets)
            .collect::<BTreeSet<_>>();
        let script_target = |name: &str| {
            if script_blocked_name(name) {
                return None;
            }
            let targets = script_symbols.get(name)?;
            (targets.len() == 1
                && !targets
                    .iter()
                    .any(|target| blocked_script_targets.contains(target)))
            .then(|| targets.iter().next().unwrap().clone())
        };
        let mut graph = Self {
            source_packages: files
                .iter()
                .filter(|file| !file.vendor)
                .map(|file| {
                    (
                        file.path.clone(),
                        context.source_package_names(&file.path, &file.language),
                    )
                })
                .filter(|(_, names)| !names.is_empty())
                .collect(),
            ..Self::default()
        };
        let mut edge_ids = BTreeMap::<String, Arc<str>>::new();
        let mut functions = BTreeMap::<&str, BTreeMap<String, Vec<(String, String)>>>::new();
        let project_modules = files
            .iter()
            .map(|f| (f.language.as_str(), f.module.as_str()))
            .collect::<BTreeSet<_>>();
        // JS/TS imports resolving here are project code, never a library package.
        let source_modules = files
            .iter()
            .filter(|f| !f.vendor && matches!(f.language.as_str(), "javascript" | "typescript"))
            .flat_map(|f| [f.module.as_str(), f.path.as_str()])
            .collect::<BTreeSet<_>>();
        let modules = files
            .iter()
            .flat_map(|file| {
                std::iter::once((file.language.as_str(), file.module.as_str()))
                    .chain(
                        matches!(file.language.as_str(), "javascript" | "typescript")
                            .then_some((file.language.as_str(), file.path.as_str())),
                    )
                    .chain(
                        file.types
                            .iter()
                            .map(|t| (file.language.as_str(), t.fqn.as_str())),
                    )
                    .chain(
                        file.functions
                            .iter()
                            .map(|f| (file.language.as_str(), f.fqn.as_str())),
                    )
            })
            .collect::<BTreeSet<_>>();
        let modules = ModuleIndex::new(modules)
            .with_registry(registry)
            .with_files(files, context, config);
        let file_refs = files.iter().collect::<Vec<_>>();
        let exports = export_bindings(&file_refs, &modules, None);
        let runtime_exports = export_bindings(&file_refs, &modules, Some(&exports));
        let esm_type_aliases = files
            .iter()
            .filter(|file| matches!(file.language.as_str(), "javascript" | "typescript"))
            .flat_map(|file| {
                file.types.iter().filter_map(|ty| {
                    if matches!(ty.kind, TypeKind::Namespace | TypeKind::Impl) {
                        return None;
                    }
                    ty.fqn
                        .strip_prefix(&file.module)
                        .map(|suffix| (file.language.as_str(), format!("{}{suffix}", file.path)))
                })
            })
            .collect::<Vec<_>>();
        let types = files
            .iter()
            .flat_map(|file| {
                file.types
                    .iter()
                    .filter(|t| !matches!(t.kind, TypeKind::Namespace | TypeKind::Impl))
                    .flat_map(|t| {
                        let compatible = match file.language.as_str() {
                            "javascript" => Some("typescript"),
                            "typescript" => Some("javascript"),
                            _ => None,
                        };
                        std::iter::once((file.language.as_str(), t.fqn.as_str()))
                            .chain(compatible.map(|language| (language, t.fqn.as_str())))
                    })
            })
            .chain(esm_type_aliases.iter().flat_map(|(language, alias)| {
                let other = if *language == "javascript" {
                    "typescript"
                } else {
                    "javascript"
                };
                [(*language, alias.as_str()), (other, alias.as_str())]
            }))
            .collect::<BTreeSet<_>>();
        let mut parents = BTreeMap::<&str, BTreeMap<&str, Vec<String>>>::new();
        let mut external_bases = ExternalBases::default();
        for file in files {
            for t in &file.types {
                for base in &t.extends {
                    let resolved = resolve_type_reference(
                        file, base, &t.fqn, &t.range, &modules, &exports, &types,
                    );
                    if types.contains(&(file.language.as_str(), resolved.as_str()))
                        || modules.native_receiver(&file.language, &resolved)
                        || file.language == "python"
                    {
                        parents
                            .entry(file.language.as_str())
                            .or_default()
                            .entry(t.fqn.as_str())
                            .or_default()
                            .push(resolved);
                    } else if matches!(file.language.as_str(), "javascript" | "typescript")
                        && resolved != *base
                        && (!resolved.starts_with(['<', '.', '/'])
                            || resolved.starts_with("<unresolved>::"))
                        && !resolved.starts_with("unresolved-import(")
                        && !source_modules.contains(resolved.as_str())
                        && !resolved
                            .char_indices()
                            .any(|(i, c)| c == '.' && source_modules.contains(&resolved[..i]))
                    {
                        external_bases
                            .bases
                            .entry((file.language.as_str(), t.fqn.as_str()))
                            .or_default()
                            .push(resolved);
                    }
                }
            }
        }
        if !external_bases.bases.is_empty() {
            for file in files {
                for t in &file.types {
                    if let Some(suffix) =
                        matches!(file.language.as_str(), "javascript" | "typescript")
                            .then(|| t.fqn.strip_prefix(&file.module))
                            .flatten()
                    {
                        external_bases.aliases.insert(
                            (file.language.as_str(), format!("{}{suffix}", file.path)),
                            t.fqn.as_str(),
                        );
                    }
                }
            }
        }
        let inheritance =
            inheritance::InheritanceIndex::new(files, &parents, registry, |file, ty, raw| {
                resolve_type_reference(file, raw, &ty.fqn, &ty.range, &modules, &exports, &types)
            });
        graph.resolution_diagnostics = inheritance.diagnostics.clone();
        // Field types belong to their declaration's scope, including imported aliases.
        let mut fields = BTreeMap::<&str, BTreeMap<&str, BTreeMap<&str, Option<String>>>>::new();
        for file in files {
            for ty in &file.types {
                for field in &ty.fields {
                    if field.is_static {
                        continue;
                    }
                    let identity = field
                        .type_name
                        .as_ref()
                        .filter(|raw| modules.type_reference(file, raw).is_some())
                        .map(|raw| {
                            resolve_type_reference(
                                file,
                                raw,
                                &ty.fqn,
                                &field.range,
                                &modules,
                                &exports,
                                &types,
                            )
                        });
                    // An untyped override still shadows a base field.
                    fields
                        .entry(file.language.as_str())
                        .or_default()
                        .entry(ty.fqn.as_str())
                        .or_default()
                        .insert(field.name.as_str(), identity);
                }
            }
        }
        // A native getter can provide the declared receiver type of a later
        // method/property operation. Setter-only descriptors still shadow bases.
        for file in files
            .iter()
            .filter(|file| matches!(file.language.as_str(), "javascript" | "typescript"))
        {
            for function in file.functions.iter().filter(|function| {
                function.parent_function.is_none()
                    && !function
                        .modifiers
                        .iter()
                        .any(|modifier| modifier == "static")
            }) {
                let Some(owner) = function.parent_type.as_deref() else {
                    continue;
                };
                let member = fields
                    .entry(file.language.as_str())
                    .or_default()
                    .entry(owner)
                    .or_default()
                    .entry(function.name.as_str())
                    .or_insert(None);
                if function
                    .modifiers
                    .iter()
                    .any(|modifier| modifier == "accessor")
                    && function.fqn.ends_with("#get")
                {
                    *member = function
                        .return_type
                        .as_ref()
                        .and_then(|raw| modules.type_reference(file, raw))
                        .map(|raw| {
                            resolve_type_reference(
                                file,
                                &raw,
                                &function.fqn,
                                &function.range,
                                &modules,
                                &exports,
                                &types,
                            )
                        });
                }
            }
        }
        // Exported module constants resolve their type in the declaring module.
        let constant_types = files
            .iter()
            .flat_map(|file| {
                file.constants.iter().filter_map(|constant| {
                    let symbol = format!("{}.{}", file.module, constant.name);
                    let raw = constant.type_name.as_deref()?;
                    let ty = resolve_type_reference(
                        file,
                        raw,
                        &symbol,
                        &constant.range,
                        &modules,
                        &exports,
                        &types,
                    );
                    // ESM imports name the declaring file (`src/db.ts.db`).
                    let alias = format!("{}.{}", file.path, constant.name);
                    let language = file.language.as_str();
                    Some([((language, symbol), ty.clone()), ((language, alias), ty)])
                })
            })
            .flatten()
            .collect::<BTreeMap<_, _>>();
        let mut returns_by_id = BTreeMap::<String, String>::new();
        let property_index = properties::PropertyIndex::new(files, &modules, &exports, &types);
        let mut implementations = BTreeMap::<&str, BTreeMap<String, Vec<String>>>::new();
        // Implementation contracts select a body independently of the receiver's type.
        let mut rust_methods = HashMap::<String, (String, Option<String>, bool)>::new();
        let mut rust_contracts = HashSet::<String>::new();
        let mut rust_defaults = HashSet::<String>::new();
        for file in files {
            for t in &file.types {
                for interface in &t.implements {
                    let identity = resolve_type_reference(
                        file, interface, &t.fqn, &t.range, &modules, &exports, &types,
                    );
                    implementations
                        .entry(file.language.as_str())
                        .or_default()
                        .entry(identity)
                        .or_default()
                        .push(if t.kind == TypeKind::Impl {
                            t.name
                                .rsplit_once(" for ")
                                .map(|(_, target)| {
                                    resolve_type_reference(
                                        file, target, &t.fqn, &t.range, &modules, &exports, &types,
                                    )
                                })
                                .unwrap_or_else(|| t.fqn.clone())
                        } else {
                            t.fqn.clone()
                        });
                }
            }

            for f in &file.functions {
                let id = function_id(file, &f.fqn);
                let owner = f.parent_type.as_ref().map(|parent| {
                    let target = if file.language == "rust"
                        && !types.contains(&(file.language.as_str(), parent.as_str()))
                    {
                        parent
                            .strip_prefix(&format!("{}::", file.module))
                            .unwrap_or(parent)
                    } else {
                        parent.as_str()
                    };
                    resolve_type_reference(
                        file, target, &f.fqn, &f.range, &modules, &exports, &types,
                    )
                });
                if file.language == "rust" && f.parent_function.is_none() {
                    if let Some(owner) = &owner {
                        let contract = f.rust_trait.as_ref().map(|contract| {
                            rust_contract_identity(
                                file, contract, &f.fqn, &f.range, &modules, &exports, &types,
                            )
                        });
                        let declaration = file
                            .types
                            .iter()
                            .any(|ty| ty.fqn == *owner && ty.kind == TypeKind::Trait);
                        if declaration {
                            rust_contracts.insert(owner.clone());
                            if f.body_range != f.range {
                                rust_defaults.insert(id.clone());
                            }
                        }
                        rust_methods.insert(id.clone(), (owner.clone(), contract, declaration));
                        functions
                            .entry("rust")
                            .or_default()
                            .entry(format!("{owner}::{}", f.name))
                            .or_default()
                            .push((id.clone(), file.path.clone()));
                    }
                }
                graph.nodes.push(GraphNode {
                    id: id.clone(),
                    label: f.fqn.clone(),
                    kind: NodeKind::Function,
                    file: Some(file.path.clone()),
                    line: Some(f.range.start_line),
                    language: Some(file.language.clone()),
                    module: Some(file.module.clone()),
                    parent_type: owner.clone(),
                    vendor: file.vendor,
                    package: vendor_package(&file.path, file.vendor),
                    category: None,
                    library: None,
                    operation: None,
                    argument: None,
                    algorithmic_complexity: None,
                    cargo_target: file.cargo_target.as_deref().map(|target| {
                        context
                            .rust_target_root(target)
                            .unwrap_or(target)
                            .to_owned()
                    }),
                    cargo_target_id: file.cargo_target.clone(),
                    source_fqn: file
                        .cargo_source_module
                        .as_ref()
                        .map(|source| f.fqn.replacen(&file.module, source, 1)),
                });
                functions
                    .entry(file.language.as_str())
                    .or_default()
                    .entry(f.fqn.clone())
                    .or_default()
                    .push((id.clone(), file.path.clone()));
                if matches!(file.language.as_str(), "javascript" | "typescript") {
                    if let Some(suffix) = f.fqn.strip_prefix(&file.module) {
                        let source_fqn = format!("{}{suffix}", file.path);
                        functions
                            .entry(file.language.as_str())
                            .or_default()
                            .entry(source_fqn.clone())
                            .or_default()
                            .push((id.clone(), file.path.clone()));
                        if f.accessor == Some(AccessorKind::Getter) {
                            if let Some(property) = source_fqn.strip_suffix("#get") {
                                functions
                                    .entry(file.language.as_str())
                                    .or_default()
                                    .entry(property.to_owned())
                                    .or_default()
                                    .push((id.clone(), file.path.clone()));
                            }
                        }
                    }
                }
                if f.accessor == Some(AccessorKind::Getter) {
                    if let Some(property) = f.fqn.strip_suffix("#get") {
                        functions
                            .entry(file.language.as_str())
                            .or_default()
                            .entry(property.to_owned())
                            .or_default()
                            .push((id.clone(), file.path.clone()));
                    }
                }
                if f.parent_function.is_none() {
                    if let Some(owner) = owner.filter(|owner| Some(owner) != f.parent_type.as_ref())
                    {
                        let separator = if matches!(file.language.as_str(), "php" | "rust") {
                            "::"
                        } else {
                            "."
                        };
                        functions
                            .entry(file.language.as_str())
                            .or_default()
                            .entry(format!("{owner}{separator}{}", f.name))
                            .or_default()
                            .push((id.clone(), file.path.clone()));
                    }
                }
                if let Some(ty) = f
                    .return_type
                    .as_ref()
                    .filter(|ty| modules.type_reference(file, ty).is_some())
                {
                    // A factory's return type belongs to its defining module,
                    // not the caller's scope (which may import it under an alias).
                    let resolved_type = resolve_type_reference(
                        file, ty, &f.fqn, &f.range, &modules, &exports, &types,
                    );
                    returns_by_id.insert(id, resolved_type);
                }
            }
        }
        let js = functions.get("javascript").cloned().unwrap_or_default();
        let ts = functions.get("typescript").cloned().unwrap_or_default();
        for (language, additions) in [("typescript", js), ("javascript", ts)] {
            for (symbol, targets) in additions {
                functions
                    .entry(language)
                    .or_default()
                    .entry(symbol)
                    .or_default()
                    .extend(targets);
            }
        }
        for language in implementations.values_mut() {
            for known in language.values_mut() {
                known.sort();
                known.dedup();
            }
        }
        let mut generated = BTreeMap::<String, GraphNode>::new();
        let mut catalogs =
            BTreeMap::<String, Vec<(ExternalOperationRule, Option<globset::GlobMatcher>)>>::new();
        for file in files {
            let mut normalized_symbols = HashMap::new();
            let identity_normalization =
                file.language == "php" && !exports.contains_key(file.language.as_str());
            let Some(strategy) = registry.get(&file.language) else {
                continue;
            };
            let rules = catalogs.entry(file.language.clone()).or_insert_with(|| {
                config
                    .external
                    .rules
                    .iter()
                    .cloned()
                    .chain(strategy.external_operations())
                    .map(|rule| {
                        let matcher = compile_pattern(&rule.pattern);
                        (rule, matcher)
                    })
                    .collect()
            });
            let imports_index =
                crate::scoped_imports::ScopedImports::new(file.imports.iter().map(|i| {
                    (
                        i,
                        canonical_export(
                            &file.language,
                            &if i.type_only {
                                format!("<type-only>::import:{}", i.path)
                            } else {
                                resolve_scoped_import(file, i, &modules)
                            },
                            &runtime_exports,
                        ),
                    )
                }));
            for f in &file.functions {
                // Rust bare paths are lexical: two inline modules can use the
                // same spelling for different declarations.
                if file.language == "rust" {
                    normalized_symbols.clear();
                }
                let from: Arc<str> = function_id(file, &f.fqn).into();
                let context = strategy.infer_types(file, f);
                let mut local_types = Cow::Borrowed(&context.variables);
                let mut calls = f
                    .calls
                    .iter()
                    .map(|call| (call, None))
                    .chain(
                        f.property_accesses
                            .iter()
                            .map(|access| (&access.site, Some(access))),
                    )
                    .collect::<Vec<_>>();
                calls.sort_by_key(|(call, _)| (call.range.start_line, call.range.start_column));
                for (call, access) in calls {
                    let script_call;
                    let call = if access.is_none()
                        && browser_files.contains(file.path.as_str())
                        && call.script_global.as_deref().is_some_and(|name| {
                            script_symbols.contains_key(name) || script_blocked_name(name)
                        }) {
                        let target = call.script_global.as_deref().and_then(&script_target);
                        script_call = CallSite {
                            confidence: if target.is_some() {
                                Confidence::Resolved
                            } else {
                                Confidence::Dynamic
                            },
                            binding_unknown: target.is_none(),
                            targets: target.into_iter().collect(),
                            local_binding: true,
                            ..call.clone()
                        };
                        &script_call
                    } else {
                        call
                    };
                    let erased_value_binding = !call.local_binding && {
                        let name = call.receiver.as_deref().unwrap_or(&call.name);
                        let (root, rest) = name.split_once('.').unwrap_or((name, ""));
                        let scope = imports_index.at(&call.range);
                        let imported = scope.get(root).map(|target| {
                            if rest.is_empty() {
                                target.clone()
                            } else {
                                format!("{target}.{rest}")
                            }
                        });
                        imported
                            .iter()
                            .map(String::as_str)
                            .chain(call.receiver_type.as_deref())
                            .chain(call.targets.iter().map(String::as_str))
                            .any(|target| {
                                target.starts_with("<type-only>::")
                                    || canonical_export(
                                        &file.language,
                                        &resolve_import_path(file, target, &modules),
                                        &runtime_exports,
                                    )
                                    .starts_with("<type-only>::")
                            })
                    };
                    let erased_call;
                    let call = if erased_value_binding {
                        erased_call = CallSite {
                            confidence: Confidence::Dynamic,
                            binding_unknown: true,
                            receiver_type: None,
                            receiver_root_type: None,
                            receiver_types: Vec::new(),
                            targets: Vec::new(),
                            ..call.clone()
                        };
                        &erased_call
                    } else {
                        call
                    };
                    let property_targets = access
                        .map(|access| {
                            property_index.resolve(
                                file,
                                f,
                                access,
                                &context,
                                &local_types,
                                &modules,
                                &exports,
                                &types,
                            )
                        })
                        .or_else(|| {
                            (matches!(file.language.as_str(), "javascript" | "typescript")
                                && call.receiver.is_some())
                            .then(|| {
                                property_index.callable_getter(
                                    file,
                                    f,
                                    call,
                                    &context,
                                    &local_types,
                                    &modules,
                                    &exports,
                                    &types,
                                )
                            })
                            .flatten()
                        })
                        .filter(|_| !erased_value_binding);
                    if let Some(targets) = property_targets {
                        if access.is_some()
                            || targets.iter().any(|target| {
                                !target.ends_with("#get") && !target.ends_with("#set")
                            })
                        {
                            if let Some(variable) = &call.assignment_to {
                                let inferred = call
                                    .receiver_types
                                    .is_empty()
                                    .then(|| {
                                        targets
                                            .iter()
                                            .map(|target| returns_by_id.get(target).cloned())
                                            .collect::<Option<BTreeSet<_>>>()
                                            .filter(|types| types.len() == 1)
                                            .and_then(|types| types.into_iter().next())
                                    })
                                    .flatten();
                                if let Some(ty) = inferred {
                                    local_types.to_mut().insert(variable.clone(), ty);
                                } else {
                                    local_types.to_mut().remove(variable);
                                }
                            }
                        }
                        let confidence = if targets.len() > 1 || !call.receiver_types.is_empty() {
                            Confidence::Ambiguous
                        } else {
                            Confidence::Resolved
                        };
                        for to in targets {
                            graph.edges.push(GraphEdge {
                                from: from.clone(),
                                recursive: from.as_ref() == to,
                                to: edge_ids
                                    .entry(to.clone())
                                    .or_insert_with(|| to.into())
                                    .clone(),
                                callsite: call.range.clone(),
                                confidence: confidence.clone(),
                                guard: call.guard.clone(),
                                in_loop: call.in_loop.clone(),
                                hidden_vendor: false,
                                argument: None,
                            });
                        }
                        continue;
                    }
                    let unresolved_union;
                    let call = if !call.receiver_types.is_empty() {
                        unresolved_union = CallSite {
                            binding_unknown: true,
                            receiver_type: None,
                            receiver_root_type: None,
                            targets: Vec::new(),
                            ..call.clone()
                        };
                        &unresolved_union
                    } else {
                        call
                    };
                    let blocked_property_descriptor =
                        matches!(file.language.as_str(), "javascript" | "typescript")
                            && property_index.blocks_inherited_call(
                                file,
                                f,
                                call,
                                &context,
                                &local_types,
                                &modules,
                                &exports,
                                &types,
                            );
                    let imports = imports_index.at(&call.range);
                    let mut candidates = if call.binding_unknown {
                        Vec::new()
                    } else {
                        strategy.resolve_project_call(call, &context, file, files)
                    };
                    if !call.binding_unknown {
                        candidates.extend(call.targets.clone());
                    }
                    let imported_function = !call.binding_unknown
                        && !call.local_binding
                        && call.receiver.is_none()
                        && matches!(file.language.as_str(), "javascript" | "typescript")
                        && imports.contains_key(&call.name);
                    if !call.binding_unknown
                        && !call.local_binding
                        && call.receiver.is_none()
                        && matches!(file.language.as_str(), "javascript" | "typescript")
                    {
                        if let Some(imported) = imports.get(&call.name) {
                            // Original resource metadata takes precedence over
                            // parser candidates that flattened away its extension.
                            candidates = vec![imported.clone()];
                        }
                    }
                    let mut qualified = candidates.clone();
                    let mut receiver_type = if call.binding_unknown {
                        None
                    } else {
                        call.receiver_type.clone()
                    };
                    let mut python_super_anchor = None;
                    if file.language == "python" && !call.binding_unknown {
                        if let Some(arguments) = call
                            .receiver
                            .as_deref()
                            .and_then(inheritance::super_arguments)
                        {
                            if arguments.is_empty() {
                                python_super_anchor = f.parent_type.clone();
                                receiver_type = f.parent_type.clone();
                            } else if arguments.len() == 2 {
                                let instance = &arguments[1];
                                receiver_type = f
                                    .parameters
                                    .iter()
                                    .any(|parameter| {
                                        parameter.implicit_receiver && parameter.name == *instance
                                    })
                                    .then(|| f.parent_type.clone())
                                    .flatten()
                                    .or_else(|| local_types.get(instance).cloned());
                                if receiver_type.is_some() {
                                    python_super_anchor = Some(resolve_type_reference(
                                        file,
                                        &arguments[0],
                                        &f.fqn,
                                        &call.range,
                                        &modules,
                                        &exports,
                                        &types,
                                    ));
                                }
                            }
                        }
                    }
                    if file.language == "rust" && !call.binding_unknown && !call.local_binding {
                        if let Some(receiver) = &call.receiver {
                            let target = rust_import_path(
                                file,
                                &format!("{receiver}::{}", call.name),
                                &f.fqn,
                                &modules,
                            );
                            if modules.contains(&("rust", target.as_str())) {
                                receiver_type =
                                    target.rsplit_once("::").map(|(owner, _)| owner.to_owned());
                                normalized_symbols.insert(target.clone(), target.clone());
                                qualified.push(target.clone());
                                candidates.push(target);
                            }
                        }
                    }
                    if !call.binding_unknown
                        && file.language != "rust"
                        && call
                            .receiver
                            .as_deref()
                            .is_some_and(|r| ["parent", "super", "super()"].contains(&r))
                    {
                        receiver_type = if file.language == "python" {
                            f.parent_type.clone()
                        } else {
                            f.parent_type
                                .as_ref()
                                .and_then(|t| {
                                    parents
                                        .get(file.language.as_str())
                                        .and_then(|types| types.get(t.as_str()))
                                })
                                .filter(|p| p.len() == 1)
                                .and_then(|p| p.first().cloned())
                        };
                    }
                    if !call.binding_unknown && receiver_type.is_none() {
                        if let Some(recv) = &call.receiver {
                            receiver_type = local_types
                                .get(recv)
                                .or_else(|| context.properties.get(recv))
                                .cloned();
                        }
                    }
                    // Set when the member-path walk or an imported constant already
                    // produced the qualified receiver identity.
                    let mut qualified_receiver = false;
                    // An imported module constant (`export const db = new Client()`)
                    // keeps the type its declaring module gives it.
                    let imported_constant = call
                        .receiver
                        .as_deref()
                        .filter(|_| !call.binding_unknown && !call.local_binding)
                        .and_then(|receiver| {
                            let root = receiver.split(['.', '?']).next()?;
                            let target = imports.get(root)?;
                            constant_types.get(&(
                                file.language.as_str(),
                                canonical_export(&file.language, target, &runtime_exports),
                            ))
                        });
                    if let Some(ty) = imported_constant.filter(|_| {
                        call.receiver
                            .as_deref()
                            .is_some_and(|receiver| imports.contains_key(receiver))
                    }) {
                        receiver_type = Some(ty.clone());
                        qualified_receiver = true;
                    }
                    if !call.binding_unknown
                        && (receiver_type.is_none() || imported_constant.is_some())
                    {
                        if let Some(receiver) = call.receiver.as_deref() {
                            let path = receiver
                                .replace("?->", ".")
                                .replace("->", ".")
                                .replace("?.", ".");
                            let parts = path.split('.').collect::<Vec<_>>();
                            if parts.len() > 1
                                && parts.iter().all(|part| {
                                    !part.is_empty()
                                        && part
                                            .chars()
                                            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '$'))
                                })
                            {
                                let root = parts[0].trim_start_matches('$');
                                let initial = imported_constant.cloned().or_else(|| {
                                    call.receiver_root_type
                                        .as_ref()
                                        .or_else(|| local_types.get(root))
                                        .map(|initial| {
                                            resolve_type_reference(
                                                file,
                                                initial,
                                                &f.fqn,
                                                &call.range,
                                                &modules,
                                                &exports,
                                                &types,
                                            )
                                        })
                                });
                                if let Some(initial) = initial {
                                    let mut ty = Some(initial);
                                    let mut external_member_path = false;
                                    // A type without project or native metadata (an
                                    // imported library class) has no field table.
                                    // Keep the member path under that type so chained
                                    // calls such as `client.user.findMany()` remain
                                    // attributable by external-operation patterns.
                                    let external_owner = |owner: &str| {
                                        let language = file.language.as_str();
                                        !owner.starts_with("<type-only>::")
                                            && !types.contains(&(language, owner))
                                            && !fields
                                                .get(language)
                                                .is_some_and(|known| known.contains_key(owner))
                                            && !parents
                                                .get(language)
                                                .is_some_and(|known| known.contains_key(owner))
                                            && !strategy.is_native_receiver(owner)
                                            && !project_modules.contains(&(language, owner))
                                            && !owner.char_indices().any(|(i, c)| {
                                                matches!(c, '.' | ':' | '\\')
                                                    && project_modules
                                                        .contains(&(language, &owner[..i]))
                                            })
                                    };
                                    for name in &parts[1..] {
                                        if let Some(owner) =
                                            ty.as_deref().filter(|owner| external_owner(owner))
                                        {
                                            ty = Some(format!("{owner}.{name}"));
                                            external_member_path = true;
                                            continue;
                                        }
                                        // A member inherited from a library base class
                                        // (`class Db extends PrismaClient`).
                                        if let Some(base) = ty.as_deref().and_then(|owner| {
                                            external_bases.member_owner(
                                                &file.language,
                                                owner,
                                                name,
                                                &fields,
                                                &parents,
                                            )
                                        }) {
                                            ty = Some(format!("{base}.{name}"));
                                            external_member_path = true;
                                            continue;
                                        }
                                        ty = ty.and_then(|owner| {
                                            if file.language == "python" {
                                                return inheritance.python_order(&owner).and_then(
                                                    |order| {
                                                        order
                                                            .iter()
                                                            .find_map(|base| {
                                                                fields
                                                                    .get("python")
                                                                    .and_then(|types| {
                                                                        types.get(base.as_str())
                                                                    })
                                                                    .and_then(|fields| {
                                                                        fields.get(*name)
                                                                    })
                                                            })
                                                            .cloned()
                                                            .flatten()
                                                    },
                                                );
                                            }
                                            declared_field_type(
                                                &file.language,
                                                &owner,
                                                name,
                                                &fields,
                                                &parents,
                                            )
                                        });
                                    }
                                    if ty.is_some() {
                                        qualified_receiver |=
                                            external_member_path || imported_constant.is_some();
                                        receiver_type = ty;
                                    } else if imported_constant.is_none() {
                                        receiver_type = None;
                                    }
                                }
                            }
                        }
                    }
                    if file.language == "php"
                        && call.kind == CallKind::Static
                        && !call.binding_unknown
                        && !call.local_binding
                        && receiver_type.is_none()
                    {
                        if let Some(receiver) = call.receiver.as_deref().filter(|receiver| {
                            !receiver.is_empty()
                                && !matches!(*receiver, "self" | "static" | "parent")
                                && receiver
                                    .chars()
                                    .all(|c| c.is_alphanumeric() || matches!(c, '_' | '\\'))
                        }) {
                            let source_receiver = if receiver.starts_with('\\') {
                                receiver.to_owned()
                            } else if !receiver.starts_with("namespace\\")
                                && file.imports.iter().any(|import| {
                                    type_import_priority(file, import, receiver, &call.range)
                                        .is_some()
                                })
                            {
                                receiver.to_owned()
                            } else {
                                format!("\\{}", php_namespace_reference(receiver, &f.fqn))
                            };
                            receiver_type = Some(resolve_type_reference(
                                file,
                                &source_receiver,
                                &f.fqn,
                                &call.range,
                                &modules,
                                &exports,
                                &types,
                            ));
                        }
                    }
                    if receiver_type.is_none()
                        && matches!(file.language.as_str(), "javascript" | "typescript")
                    {
                        receiver_type = property_index.receiver_type(
                            file,
                            f,
                            call,
                            &context,
                            &local_types,
                            &modules,
                            &exports,
                            &types,
                        );
                    }
                    let mut interface_dispatch = false;
                    if let Some(t) = receiver_type.as_deref() {
                        let expanded = imports
                            .get(t)
                            .or_else(|| {
                                (!call.local_binding)
                                    .then(|| {
                                        call.receiver
                                            .as_deref()
                                            .and_then(|receiver| imports.get(receiver))
                                    })
                                    .flatten()
                            })
                            .filter(|_| !qualified_receiver)
                            .cloned()
                            .unwrap_or_else(|| {
                                if qualified_receiver
                                    || types.contains(&(file.language.as_str(), t))
                                {
                                    return t.to_owned();
                                }
                                resolve_type_reference(
                                    file,
                                    t,
                                    &f.fqn,
                                    &call.range,
                                    &modules,
                                    &exports,
                                    &types,
                                )
                            });
                        let canonical = canonical_export(&file.language, &expanded, &exports);
                        let t = canonical.as_str();
                        receiver_type = Some(canonical.clone());
                        let known = implementations
                            .get(file.language.as_str())
                            .and_then(|types| types.get(t))
                            .map(Vec::as_slice)
                            .unwrap_or_default();
                        interface_dispatch = !known.is_empty();
                        for ty in std::iter::once(t).chain(
                            known
                                .iter()
                                .map(String::as_str)
                                .take(config.graph.max_implementations),
                        ) {
                            let a = format!("{ty}::{}", call.name);
                            let b = format!("{ty}.{}", call.name);
                            let native =
                                format!("{ty}{}{}", strategy.method_separator(), call.name);
                            if file.language == "rust" && modules.contains(&("rust", a.as_str())) {
                                normalized_symbols.insert(a.clone(), a.clone());
                            }
                            qualified.extend([a.clone(), b.clone(), native.clone()]);
                            candidates.extend([a, b, native]);
                        }
                    }
                    if !call.binding_unknown && !call.local_binding && !imported_function {
                        candidates.push(call.name.clone());
                    }
                    for (alias, path) in
                        imports.matching_prefixes(&call.name).filter(|(alias, _)| {
                            !call.binding_unknown
                                && !call.local_binding
                                && (call.receiver.is_none()
                                    || call.name.starts_with(&format!("{alias}."))
                                    || call.name.starts_with(&format!("{alias}::")))
                        })
                    {
                        let expanded = if call.name == alias {
                            Some(path.clone())
                        } else if let Some(rest) = call.name.strip_prefix(&format!("{alias}.")) {
                            Some(format!("{path}.{rest}"))
                        } else {
                            call.name
                                .strip_prefix(&format!("{alias}::"))
                                .map(|rest| format!("{path}::{rest}"))
                        };
                        if let Some(expanded) = expanded {
                            if file.language == "rust" {
                                normalized_symbols.insert(expanded.clone(), expanded.clone());
                            }
                            qualified.push(expanded.clone());
                            candidates.push(expanded)
                        }
                    }
                    for sep in [strategy.namespace_separator(), ".", "::", "\\"]
                        .into_iter()
                        .filter(|_| {
                            !call.binding_unknown
                                && !call.local_binding
                                && !imported_function
                                && file.language != "rust"
                        })
                    {
                        candidates.push(format!("{}{}{}", file.module, sep, call.name));
                    }
                    let mut normalize = |c: String| {
                        normalize_symbol(
                            &mut normalized_symbols,
                            c,
                            identity_normalization,
                            |symbol| {
                                canonical_export(
                                    &file.language,
                                    &if file.language == "rust" {
                                        rust_import_path(file, symbol, &f.fqn, &modules)
                                    } else {
                                        resolve_import_path(file, symbol, &modules)
                                    },
                                    &runtime_exports,
                                )
                            },
                        )
                    };
                    candidates = candidates.into_iter().map(&mut normalize).collect();
                    qualified = qualified.into_iter().map(&mut normalize).collect();
                    let constructor_type = (call.receiver.is_none()
                        && !call.binding_unknown
                        && (call.kind == CallKind::Constructor
                            || strategy.permits_constructor_inference()))
                    .then(|| {
                        candidates
                            .iter()
                            .find(|c| types.contains(&(file.language.as_str(), c.as_str())))
                            .cloned()
                    })
                    .flatten();
                    if let Some(ty) = &constructor_type {
                        candidates.push(format!(
                            "{ty}{}{}",
                            strategy.method_separator(),
                            strategy.constructor_name()
                        ));
                    }
                    candidates.sort();
                    candidates.dedup();
                    let mut external_candidates = if call.receiver.is_some() {
                        qualified
                            .iter()
                            .filter(|name| has_multiple_segments_for(name, strategy.as_ref()))
                            .cloned()
                            .collect::<Vec<_>>()
                    } else {
                        candidates.clone()
                    };
                    // `this.$connect()` in `class Db extends PrismaClient`.
                    if let Some(base) = receiver_type
                        .as_deref()
                        .filter(|_| !call.binding_unknown)
                        .and_then(|owner| {
                            external_bases.member_owner(
                                &file.language,
                                owner,
                                &call.name,
                                &fields,
                                &parents,
                            )
                        })
                    {
                        external_candidates.extend([
                            format!("{base}.{}", call.name),
                            format!("{base}::{}", call.name),
                        ]);
                    }
                    if let Some(receiver) = call
                        .receiver
                        .as_ref()
                        .filter(|_| !call.binding_unknown && !call.local_binding)
                    {
                        if !imports.contains_key(receiver) {
                            external_candidates.extend([
                                format!("{receiver}.{}", call.name),
                                format!("{receiver}::{}", call.name),
                                format!("{receiver}{}{}", strategy.method_separator(), call.name),
                            ]);
                        }
                        if let Some(imported) = imports.get(receiver) {
                            external_candidates.extend([
                                format!("{imported}.{}", call.name),
                                format!("{imported}::{}", call.name),
                                format!("{imported}{}{}", strategy.method_separator(), call.name),
                            ]);
                        }
                    }
                    // Library patterns name the package specifier: the default
                    // import is the package value (CJS/esModuleInterop), and an
                    // installed but unparsed package must not hide the library.
                    if matches!(file.language.as_str(), "javascript" | "typescript")
                        && !call.binding_unknown
                        && !call.local_binding
                    {
                        let symbol = call.receiver.as_deref().unwrap_or(&call.name);
                        let (root, rest) = symbol.split_once('.').unwrap_or((symbol, ""));
                        if let (Some(import), Some(resolved)) =
                            (imports.import(root), imports.get(root))
                        {
                            let owned = resolved.char_indices().any(|(i, c)| {
                                matches!(c, '.' | ':') && source_modules.contains(&resolved[..i])
                            }) || source_modules.contains(resolved.as_str());
                            if let Some(source) = import.source.as_deref().filter(|source| {
                                !owned
                                    && !source.starts_with(['.', '/', '#'])
                                    && !source.contains("://")
                            }) {
                                let package = [".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs"]
                                    .iter()
                                    .find_map(|extension| source.strip_suffix(extension))
                                    .unwrap_or(source);
                                let member = import.path.strip_prefix(package).unwrap_or("");
                                let mut base = format!(
                                    "{}{}",
                                    package.strip_prefix("node:").unwrap_or(package),
                                    if member == ".default" { "" } else { member }
                                );
                                if !rest.is_empty() {
                                    base = format!("{base}.{rest}");
                                }
                                external_candidates.push(if call.receiver.is_some() {
                                    format!("{base}.{}", call.name)
                                } else {
                                    base
                                });
                            }
                        }
                    }
                    let has_definition = !call.binding_unknown
                        && candidates.iter().any(|c| {
                            (call.receiver.is_none() || qualified.contains(c))
                                && functions
                                    .get(file.language.as_str())
                                    .is_some_and(|symbols| symbols.contains_key(c.as_str()))
                        });
                    let project_owns = |name: &str| {
                        project_modules.contains(&(file.language.as_str(), name))
                            || name.char_indices().any(|(i, c)| {
                                (matches!(c, '.' | ':' | '\\')
                                    || name[i..].starts_with(strategy.namespace_separator())
                                    || name[i..].starts_with(strategy.method_separator()))
                                    && project_modules
                                        .contains(&(file.language.as_str(), &name[..i]))
                            })
                    };
                    let constructor_reference = (call.receiver.is_none()
                        && !call.local_binding
                        && constructor_type.is_none()
                        && matches!(call.kind, CallKind::Constructor | CallKind::Function)
                        && (call.kind == CallKind::Constructor
                            || strategy.permits_constructor_inference()))
                    .then(|| {
                        imports
                            .get(&call.name)
                            .map(String::as_str)
                            .unwrap_or(&call.name)
                    })
                    .filter(|ty| {
                        (strategy.is_native_receiver(ty)
                            || (imports.contains_key(&call.name)
                                && strategy.is_native_function(ty)))
                            && !project_owns(ty)
                            && !types.contains(&(file.language.as_str(), *ty))
                    });
                    let standard_receiver =
                        receiver_type
                            .as_deref()
                            .filter(|ty| {
                                !types.contains(&(file.language.as_str(), *ty))
                                    && !project_owns(ty)
                                    && strategy.is_native_receiver(ty)
                            })
                            .or(constructor_reference)
                            .or_else(|| {
                                call.receiver.as_deref().filter(|r| {
                                    receiver_type.is_none()
                                        && !call.local_binding
                                        && !project_owns(r)
                                        && !file.types.iter().any(|t| {
                                            r == &t.name
                                                || [
                                                    strategy.namespace_separator(),
                                                    strategy.method_separator(),
                                                    ".",
                                                    "::",
                                                ]
                                                .iter()
                                                .any(|separator| {
                                                    r.starts_with(&format!("{}{separator}", t.name))
                                                })
                                        })
                                        && strategy.is_native_global_receiver(r)
                                })
                            });
                    let standard = !call.binding_unknown
                        && !has_definition
                        && (standard_receiver.is_some()
                            || (!call.local_binding
                                && !imports.contains_key(&call.name)
                                && call.receiver.is_none()
                                && constructor_type.is_none()
                                && ((call.kind == CallKind::Function
                                    && strategy.is_native_function(&call.name))
                                    || (call.kind == CallKind::Macro
                                        && strategy.is_native_macro(&call.name)))));
                    let external = rules
                        .iter()
                        .filter(|_| !call.binding_unknown && !has_definition)
                        .find(|(rule, matcher)| {
                            external_candidates.iter().any(|candidate| {
                                rule.pattern == *candidate
                                    || matcher
                                        .as_ref()
                                        .is_some_and(|matcher| matcher.is_match(candidate))
                            })
                        })
                        .map(|(rule, _)| rule);
                    if let Some(rule) = external.filter(|_| !standard) {
                        let id = format!(
                            "external:{}:{}:{}",
                            rule.category, rule.library, rule.operation
                        );
                        let arg = if config.external.capture_args == CaptureArgs::Literal {
                            literal_argument(call, config)
                        } else {
                            None
                        };
                        let node = generated.entry(id.clone()).or_insert_with(|| GraphNode {
                            id: id.clone(),
                            label: format!(
                                "{} · {} · {}",
                                rule.category, rule.library, rule.operation
                            ),
                            kind: NodeKind::External,
                            file: None,
                            line: None,
                            language: None,
                            module: None,
                            parent_type: None,
                            vendor: false,
                            package: None,
                            category: Some(rule.category.clone()),
                            library: Some(rule.library.clone()),
                            operation: Some(rule.operation.clone()),
                            argument: arg.clone(),
                            algorithmic_complexity: None,
                            cargo_target: None,
                            cargo_target_id: None,
                            source_fqn: None,
                        });
                        // A shared operation node may represent several resources.
                        // Do not advertise the first literal as their common value.
                        if node.argument != arg {
                            node.argument = None;
                        }
                        graph.edges.push(GraphEdge {
                            from: from.clone(),
                            to: edge_ids
                                .entry(id.clone())
                                .or_insert_with(|| id.into())
                                .clone(),
                            callsite: call.range.clone(),
                            confidence: Confidence::Resolved,
                            guard: call.guard.clone(),
                            in_loop: call.in_loop.clone(),
                            recursive: false,
                            hidden_vendor: false,
                            argument: arg,
                        });
                        continue;
                    }
                    let receiver_known = call.receiver.is_none()
                        || receiver_type.is_some()
                        || call.confidence == Confidence::Resolved
                        || call
                            .receiver
                            .as_ref()
                            .is_some_and(|receiver| imports.contains_key(receiver));
                    let usable_candidate = |candidate: &String| {
                        call.receiver.is_none()
                            || (receiver_known
                                && has_multiple_segments_for(candidate, strategy.as_ref())
                                && qualified.contains(candidate))
                    };
                    let mut targets = Vec::new();
                    // Lexical/file definitions take precedence over any project-wide
                    // name match. A suffix is not evidence of a call relationship:
                    // unrelated classes and nested callbacks often share the same name.
                    for candidate in &candidates {
                        if !usable_candidate(candidate) {
                            continue;
                        }
                        if let Some(found) = functions
                            .get(file.language.as_str())
                            .and_then(|symbols| symbols.get(candidate.as_str()))
                        {
                            targets.extend(
                                found
                                    .iter()
                                    .filter(|(_, path)| path == &file.path)
                                    .map(|(id, _)| id.clone()),
                            );
                        }
                    }
                    if targets.is_empty() {
                        for candidate in &candidates {
                            if !usable_candidate(candidate) {
                                continue;
                            }
                            // JS/TS/Python bare names live in a module scope. Fully
                            // qualified import/type targets can identify another file.
                            if !has_multiple_segments_for(candidate, strategy.as_ref())
                                && (call.receiver.is_some()
                                    || matches!(
                                        file.language.as_str(),
                                        "javascript" | "typescript" | "python"
                                    ))
                            {
                                continue;
                            }
                            if let Some(found) = functions
                                .get(file.language.as_str())
                                .and_then(|symbols| symbols.get(candidate.as_str()))
                            {
                                targets.extend(found.iter().map(|(id, _)| id.clone()));
                            }
                        }
                    }
                    if matches!(file.language.as_str(), "javascript" | "typescript")
                        && property_index.blocks_direct_call(
                            file,
                            f,
                            call,
                            &context,
                            &local_types,
                            &modules,
                            &exports,
                            &types,
                        )
                    {
                        targets.clear();
                    } else if blocked_property_descriptor {
                        // A data field can replace a same-name native accessor
                        // even when the ordinary symbol index contains it.
                        targets.retain(|id| !id.ends_with("#get") && !id.ends_with("#set"));
                    }
                    let mut inherited_standard_receiver = None;
                    let mut inheritance_uncertain = false;
                    let mut ordered_lookup = false;
                    if file.language == "php" && !call.binding_unknown {
                        if let Some(lookup) = inheritance.php_trait_lookup(
                            from.as_ref(),
                            f.parent_type.as_deref(),
                            call.receiver.as_deref(),
                            &call.name,
                        ) {
                            ordered_lookup = true;
                            targets = lookup.targets;
                            inheritance_uncertain = lookup.uncertain;
                            inherited_standard_receiver = lookup.runtime_owner;
                        }
                    }
                    if !call.binding_unknown && !ordered_lookup {
                        if let Some(owner) = receiver_type.as_ref().or(constructor_type.as_ref()) {
                            let method = if constructor_type.is_some() {
                                strategy.constructor_name()
                            } else {
                                &call.name
                            };
                            if matches!(file.language.as_str(), "php" | "python")
                                && types.contains(&(file.language.as_str(), owner.as_str()))
                                && !interface_dispatch
                                && (file.language == "python" || inheritance.php_type(owner))
                            {
                                ordered_lookup = true;
                                let lookup = if file.language == "php" {
                                    inheritance.php_lookup(owner, method, f.parent_type.as_deref())
                                } else {
                                    inheritance.python_lookup(
                                        owner,
                                        method,
                                        python_super_anchor.as_deref(),
                                        |base, name| {
                                            functions
                                                .get("python")
                                                .and_then(|symbols| {
                                                    symbols.get(&format!("{base}.{name}"))
                                                })
                                                .into_iter()
                                                .flatten()
                                                .map(|(id, _)| id.clone())
                                                .collect()
                                        },
                                    )
                                };
                                targets = lookup.targets;
                                inheritance_uncertain = lookup.uncertain;
                                inherited_standard_receiver = lookup.runtime_owner;
                            }
                        }
                    }
                    if targets.is_empty()
                        && !ordered_lookup
                        && !call.binding_unknown
                        && !blocked_property_descriptor
                    {
                        if let Some(ty) = receiver_type.as_ref().or(constructor_type.as_ref()) {
                            let method = if constructor_type.is_some() {
                                strategy.constructor_name()
                            } else {
                                &call.name
                            };
                            let mut pending = parents
                                .get(file.language.as_str())
                                .and_then(|types| types.get(ty.as_str()))
                                .into_iter()
                                .flatten()
                                .map(String::as_str)
                                .collect::<Vec<_>>();
                            let mut visited = BTreeSet::new();
                            while let Some(base) = pending.pop() {
                                if !visited.insert(base) {
                                    continue;
                                }
                                let mut found = Vec::new();
                                for sep in [strategy.method_separator(), ".", "::"] {
                                    if let Some(items) =
                                        functions.get(file.language.as_str()).and_then(|symbols| {
                                            symbols.get(&format!("{base}{sep}{method}"))
                                        })
                                    {
                                        found.extend(items.iter().map(|(id, _)| id.clone()));
                                    }
                                }
                                if found.is_empty() {
                                    if strategy.is_native_method(&base, method) {
                                        inherited_standard_receiver = Some(base.to_owned());
                                    }
                                    pending.extend(
                                        parents
                                            .get(file.language.as_str())
                                            .and_then(|types| types.get(base))
                                            .into_iter()
                                            .flatten()
                                            .map(String::as_str),
                                    );
                                } else {
                                    targets.extend(found);
                                }
                            }
                        }
                    }
                    if file.language == "rust" && call.receiver.is_some() {
                        let explicit_contract = call.rust_trait.as_ref().map(|contract| {
                            rust_contract_identity(
                                file,
                                contract,
                                &f.fqn,
                                &call.range,
                                &modules,
                                &exports,
                                &types,
                            )
                        });
                        let receiver_contract = receiver_type
                            .as_ref()
                            .filter(|receiver| rust_contracts.contains(*receiver));
                        let contract = explicit_contract.as_ref().or(receiver_contract);
                        if let Some(contract) = contract {
                            let argument_owner = receiver_contract
                                .filter(|_| call.kind == CallKind::Static)
                                .and_then(|_| call.arguments.first())
                                .and_then(|argument| {
                                    let variable = argument
                                        .trim()
                                        .trim_start_matches('&')
                                        .trim_start()
                                        .trim_start_matches("mut ")
                                        .trim();
                                    local_types
                                        .get(variable)
                                        .or_else(|| context.variables.get(variable))
                                })
                                .map(|owner| {
                                    resolve_type_reference(
                                        file,
                                        owner,
                                        &f.fqn,
                                        &call.range,
                                        &modules,
                                        &exports,
                                        &types,
                                    )
                                })
                                .filter(|owner| !rust_contracts.contains(owner));
                            let concrete = explicit_contract
                                .as_ref()
                                .and(receiver_type.as_ref())
                                .or(argument_owner.as_ref());
                            let implementation_matches = |target: &String| {
                                rust_methods.get(target).is_some_and(
                                    |(owner, implementation, _)| {
                                        implementation.as_ref().is_some_and(|implementation| {
                                            implementation == contract
                                                || (!contract.contains('<')
                                                    && implementation
                                                        .strip_prefix(contract)
                                                        .is_some_and(|suffix| {
                                                            suffix.starts_with('<')
                                                        }))
                                        }) && concrete.is_none_or(|concrete| concrete == owner)
                                    },
                                )
                            };
                            if targets.iter().any(implementation_matches) {
                                targets.retain(implementation_matches);
                                if concrete.is_some() {
                                    interface_dispatch = false;
                                }
                            } else {
                                targets.retain(|target| {
                                    rust_defaults.contains(target)
                                        && rust_methods
                                            .get(target)
                                            .is_some_and(|(owner, _, _)| owner == contract)
                                });
                                if concrete.is_some() {
                                    interface_dispatch = false;
                                }
                            }
                        } else if targets.iter().any(|target| {
                            rust_methods.get(target).is_some_and(
                                |(_, implementation, declaration)| {
                                    implementation.is_none() && !declaration
                                },
                            )
                        }) {
                            // Inherent methods take precedence over trait methods in ordinary receiver dispatch.
                            targets.retain(|target| {
                                rust_methods.get(target).is_none_or(
                                    |(_, implementation, declaration)| {
                                        implementation.is_none() && !declaration
                                    },
                                )
                            });
                        }
                    }
                    targets.sort();
                    targets.dedup();
                    if interface_dispatch && targets.len() > 1 {
                        let interface = receiver_type.as_deref().unwrap_or("");
                        targets.retain(|id| {
                            !id.ends_with(&format!("::{interface}::{}", call.name))
                                && !id.ends_with(&format!(":{interface}::{}", call.name))
                                && !id.ends_with(&format!(".{interface}.{}", call.name))
                        });
                    }
                    if let Some(var) = &call.assignment_to {
                        let inferred = if constructor_type.is_some() {
                            constructor_type.clone()
                        } else if call.kind == CallKind::Constructor {
                            call.receiver_type
                                .clone()
                                .or_else(|| Some(call.name.clone()))
                        } else if inheritance_uncertain {
                            None
                        } else {
                            let inferred = targets
                                .iter()
                                .map(|id| returns_by_id.get(id).cloned())
                                .collect::<Option<BTreeSet<_>>>();
                            inferred
                                .filter(|items| items.len() == 1)
                                .and_then(|items| items.into_iter().next())
                        };
                        if let Some(ty) = inferred {
                            local_types.to_mut().insert(var.clone(), ty);
                        } else if local_types.contains_key(var) {
                            local_types.to_mut().remove(var);
                        }
                    }
                    targets.sort();
                    targets.dedup();
                    if targets.is_empty() && (standard || inherited_standard_receiver.is_some())
                        || inheritance_uncertain && inherited_standard_receiver.is_some()
                    {
                        let standard_receiver =
                            standard_receiver.or(inherited_standard_receiver.as_deref());
                        let symbol = standard_receiver
                            .map(|r| {
                                if call.receiver.is_none() {
                                    if call.kind == CallKind::Constructor {
                                        format!("{r}::constructor")
                                    } else {
                                        r.to_owned()
                                    }
                                } else {
                                    format!("{r}::{}", call.name)
                                }
                            })
                            .unwrap_or_else(|| call.name.trim_start_matches('\\').to_owned());
                        let language = file.language.clone();
                        let id = format!("external:runtime:{language}:{symbol}");
                        generated.entry(id.clone()).or_insert_with(|| GraphNode {
                            id: id.clone(),
                            label: symbol.clone(),
                            kind: NodeKind::External,
                            file: None,
                            line: None,
                            language: Some(language.clone()),
                            module: None,
                            parent_type: standard_receiver.map(str::to_owned),
                            vendor: false,
                            package: None,
                            category: Some("runtime".into()),
                            library: Some(language),
                            operation: Some(symbol),
                            argument: None,
                            algorithmic_complexity: None,
                            cargo_target: None,
                            cargo_target_id: None,
                            source_fqn: None,
                        });
                        targets.push(id);
                    }
                    if targets.is_empty() {
                        // Unknown receivers must not merge unrelated methods with the same name.
                        let known_type = constructor_type
                            .as_deref()
                            .or(receiver_type.as_deref())
                            .map(|t| imports.get(t).map(String::as_str).unwrap_or(t))
                            .or_else(|| {
                                if call.binding_unknown || call.kind != CallKind::Constructor {
                                    return None;
                                }
                                imports.get(&call.name).map(String::as_str).or_else(|| {
                                    file.types
                                        .iter()
                                        .find(|t| t.name == call.name || t.fqn == call.name)
                                        .map(|t| t.fqn.as_str())
                                })
                            });
                        let method =
                            if call.kind == CallKind::Constructor || constructor_type.is_some() {
                                strategy.constructor_name()
                            } else {
                                call.name.as_str()
                            };
                        let label = if let Some(ty) = known_type {
                            format!("{ty}::{method}")
                        } else if let Some(receiver) = &call.receiver {
                            format!("{receiver}->{}", call.name)
                        } else {
                            call.name.clone()
                        };
                        let id = if (call.receiver.is_some() && known_type.is_none())
                            || call.binding_unknown
                        {
                            format!("unknown:{}:{}:{label}", file.language, from)
                        } else {
                            format!("unknown:{}:{label}", file.language)
                        };
                        generated.entry(id.clone()).or_insert_with(|| GraphNode {
                            id: id.clone(),
                            label,
                            kind: NodeKind::Unknown,
                            file: None,
                            line: None,
                            language: Some(file.language.clone()),
                            module: None,
                            parent_type: known_type.map(str::to_owned),
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
                        targets.push(id);
                    }
                    let confidence = if targets[0].starts_with("unknown:") {
                        if candidates
                            .iter()
                            .any(|candidate| candidate.starts_with("<ambiguous-package>::"))
                        {
                            Confidence::Ambiguous
                        } else {
                            Confidence::Dynamic
                        }
                    } else if targets.len() > 1
                        || inheritance_uncertain
                        || interface_dispatch
                        || uncertain_functions.contains(from.as_ref())
                        || targets
                            .iter()
                            .any(|target| uncertain_functions.contains(target))
                    {
                        Confidence::Ambiguous
                    } else {
                        Confidence::Resolved
                    };
                    for to in targets {
                        graph.edges.push(GraphEdge {
                            from: from.clone(),
                            recursive: from.as_ref() == to,
                            to: edge_ids
                                .entry(to.clone())
                                .or_insert_with(|| to.into())
                                .clone(),
                            callsite: call.range.clone(),
                            confidence: confidence.clone(),
                            guard: call.guard.clone(),
                            in_loop: call.in_loop.clone(),
                            hidden_vendor: false,
                            argument: None,
                        });
                    }
                }
            }
        }
        graph.nodes.extend(generated.into_values());
        // Resolution-only indexes are no longer needed while allocating query indexes.
        drop(functions);
        drop(returns_by_id);
        drop(implementations);
        drop(edge_ids);
        drop(catalogs);
        drop(modules);
        drop(types);
        drop(exports);
        drop(project_modules);
        graph.reindex();
        graph
    }
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
    pub fn select(&self, selector: &str) -> Vec<String> {
        if let Some(class) = selector.strip_prefix("class:") {
            return self
                .nodes
                .iter()
                .filter(|node| {
                    node.kind == NodeKind::Function
                        && (node.parent_type.as_deref() == Some(class)
                            || node.source_parent_type().as_deref() == Some(class))
                })
                .map(|node| node.id.clone())
                .collect();
        }
        let exact = self
            .nodes
            .iter()
            .filter(|n| {
                n.source_id() == selector
                    || n.id == selector
                    || (n.kind != NodeKind::Unknown
                        && (n.label == selector || n.source_fqn.as_deref() == Some(selector)))
                    || n.kind == NodeKind::External
                        && selector
                            .strip_prefix("category:")
                            .is_some_and(|c| n.category.as_deref() == Some(c))
                    || n.kind == NodeKind::Function
                        && n.file.as_deref().is_some_and(|f| {
                            selector.rsplit_once(':').is_some_and(|(p, line)| {
                                p == f && line.parse::<usize>().ok() == n.line
                            })
                        })
            })
            .map(|n| n.id.clone())
            .collect::<Vec<_>>();
        if !exact.is_empty() {
            return exact;
        }
        let matcher = if selector.contains(['*', '?', '[']) {
            globset::Glob::new(selector)
                .ok()
                .map(|g| g.compile_matcher())
        } else {
            None
        };
        let mut fuzzy = self
            .nodes
            .iter()
            .filter(|n| {
                n.kind == NodeKind::Function
                    && (matcher.as_ref().is_some_and(|glob| glob.is_match(&n.label))
                        || matcher.is_none() && n.label.contains(selector))
            })
            .map(|n| n.id.clone())
            .collect::<Vec<_>>();
        fuzzy.sort();
        fuzzy
    }
    pub fn query(
        &self,
        selector: &str,
        direction: Direction,
        depth: Option<usize>,
        filter: &GraphFilter,
        config: &Config,
    ) -> GraphQuery {
        let roots = self.select(selector);
        let mut ids = roots.iter().cloned().collect::<BTreeSet<_>>();
        let mut selected = BTreeSet::new();
        let directions = match direction {
            Direction::Both => vec![Direction::Callers, Direction::Callees],
            d => vec![d],
        };
        for dir in directions {
            let mut queue = roots
                .iter()
                .map(|id| (id.clone(), 0usize))
                .collect::<VecDeque<_>>();
            let mut visited = roots.iter().cloned().collect::<BTreeSet<_>>();
            while let Some((id, d)) = queue.pop_front() {
                if depth.is_some_and(|limit| d >= limit) {
                    continue;
                }
                let links = if dir == Direction::Callees {
                    self.outgoing.get(id.as_str())
                } else {
                    self.incoming.get(id.as_str())
                };
                for i in links.into_iter().flatten() {
                    let edge = &self.edges[*i];
                    let next = if dir == Direction::Callees {
                        edge.to.as_ref()
                    } else {
                        edge.from.as_ref()
                    };
                    let Some(node) = self.node(next) else {
                        continue;
                    };
                    if !filter.accepts(node, edge) {
                        continue;
                    }
                    selected.insert(*i);
                    ids.insert(next.to_string());
                    if visited.insert(next.to_string()) {
                        queue.push_back((next.to_string(), d + 1));
                    }
                }
            }
        }
        // Reachability uses BFS so depth remains the shortest distance. Cycle
        // classification must inspect all selected edges, not just BFS parents.
        let cycles = self.query_back_edges(&selected, &roots, direction == Direction::Callers);
        selected.retain(|index| !cycles.contains(index));
        let mut nodes = ids
            .iter()
            .filter_map(|id| self.node(id).cloned())
            .collect::<Vec<_>>();
        let mut edges = selected
            .into_iter()
            .map(|i| self.edges[i].clone())
            .collect::<Vec<_>>();
        let cycles = cycles
            .into_iter()
            .map(|i| self.edges[i].clone())
            .collect::<Vec<_>>();
        let evidence = edges.iter().chain(&cycles).cloned().collect::<Vec<_>>();
        let mut external_arguments = BTreeMap::<&str, BTreeSet<Option<&str>>>::new();
        for edge in &evidence {
            if self
                .node(edge.to.as_ref())
                .is_some_and(|node| node.kind == NodeKind::External)
            {
                external_arguments
                    .entry(edge.to.as_ref())
                    .or_default()
                    .insert(edge.argument.as_deref());
            }
        }
        for node in &mut nodes {
            if node.kind == NodeKind::External && node.category.as_deref() != Some("runtime") {
                if let Some(arguments) = external_arguments.get(node.id.as_str()) {
                    node.argument = if arguments.len() == 1 {
                        arguments
                            .iter()
                            .next()
                            .copied()
                            .flatten()
                            .map(str::to_owned)
                    } else {
                        None
                    };
                }
            }
        }
        let mut coverage = coverage(&nodes, &evidence, &roots, direction);
        let hidden_vendor_edges = apply_vendor_view(&mut nodes, &mut edges, config.vendor.mode);
        if !hidden_vendor_edges.is_empty() {
            coverage.vendor_boundary = true;
        }
        GraphQuery {
            roots,
            nodes,
            edges,
            cycles,
            hidden_vendor_edges,
            coverage,
            direction,
            depth,
        }
    }
    fn query_back_edges(
        &self,
        selected: &BTreeSet<usize>,
        roots: &[String],
        reverse: bool,
    ) -> BTreeSet<usize> {
        let mut adjacency = BTreeMap::<&str, Vec<(usize, &str)>>::new();
        for &index in selected {
            let edge = &self.edges[index];
            let (from, to) = if reverse {
                (&edge.to, &edge.from)
            } else {
                (&edge.from, &edge.to)
            };
            adjacency
                .entry(from.as_ref())
                .or_default()
                .push((index, to.as_ref()));
        }
        let mut colors = HashMap::<&str, u8>::new();
        let mut cycles = BTreeSet::new();
        for root in roots
            .iter()
            .map(String::as_str)
            .chain(adjacency.keys().copied())
        {
            if colors.contains_key(root) {
                continue;
            }
            colors.insert(root, 1);
            let mut stack = vec![(root, 0usize)];
            while let Some((node, next_index)) = stack.last_mut() {
                let next = adjacency
                    .get(node)
                    .and_then(|edges| edges.get(*next_index))
                    .copied();
                if let Some((index, target)) = next {
                    *next_index += 1;
                    match colors.get(target).copied() {
                        Some(1) => {
                            cycles.insert(index);
                        }
                        Some(_) => {}
                        None => {
                            colors.insert(target, 1);
                            stack.push((target, 0));
                        }
                    }
                } else {
                    colors.insert(*node, 2);
                    stack.pop();
                }
            }
        }
        cycles
    }
    pub fn paths(&self, from: &str, to: &str, shortest_only: bool, max_paths: usize) -> PathResult {
        let starts = self.select(from);
        let ends = self.select(to).into_iter().collect::<BTreeSet<_>>();
        if starts.is_empty() || ends.is_empty() {
            return PathResult {
                paths: Vec::new(),
                truncated: false,
            };
        }
        if shortest_only {
            return self.shortest_paths(starts, ends, max_paths);
        }
        // A reverse walk removes branches that cannot reach any target before
        // enumerating simple paths. Queue and expansion budgets bound dense DAGs.
        let mut reachable = ends.clone();
        let mut reverse = ends.iter().cloned().collect::<VecDeque<_>>();
        while let Some(id) = reverse.pop_front() {
            for index in self.incoming.get(id.as_str()).into_iter().flatten() {
                let previous = self.edges[*index].from.clone();
                if reachable.insert(previous.to_string()) {
                    reverse.push_back(previous.to_string());
                }
            }
        }
        const MAX_EXPANSIONS: usize = 1_000_000;
        const MAX_QUEUED_HOPS: usize = 1_000_000;
        let mut queue = starts
            .into_iter()
            .filter(|id| reachable.contains(id))
            .map(|id| vec![id])
            .collect::<Vec<_>>();
        let mut queued_hops = queue.len();
        let mut expansions = 0;
        let mut paths = Vec::new();
        let mut truncated = false;
        while let Some(path) = queue.pop() {
            queued_hops -= path.len();
            if expansions >= MAX_EXPANSIONS {
                truncated = true;
                break;
            }
            expansions += 1;
            let tail = path.last().unwrap();
            if ends.contains(tail) {
                if paths.len() == max_paths {
                    truncated = true;
                    break;
                }
                paths.push(path);
                if paths.len() == max_paths {
                    truncated = !queue.is_empty();
                    break;
                }
                continue;
            }
            let mut next_nodes = BTreeSet::new();
            for index in self.outgoing.get(tail.as_str()).into_iter().flatten() {
                let next = self.edges[*index].to.as_ref();
                if !next_nodes.insert(next) {
                    continue;
                }
                if !reachable.contains(next) || path.iter().any(|id| id == next) {
                    continue;
                }
                let mut next_path = path.clone();
                next_path.push(next.to_string());
                if queued_hops + next_path.len() > MAX_QUEUED_HOPS {
                    truncated = true;
                    break;
                }
                queued_hops += next_path.len();
                queue.push(next_path);
            }
            if truncated {
                break;
            }
        }
        PathResult { paths, truncated }
    }
    fn shortest_paths(
        &self,
        starts: Vec<String>,
        ends: BTreeSet<String>,
        max_paths: usize,
    ) -> PathResult {
        let mut distance = BTreeMap::<String, usize>::new();
        let mut predecessors = BTreeMap::<String, Vec<String>>::new();
        let mut queue = VecDeque::new();
        for start in starts {
            if distance.insert(start.clone(), 0).is_none() {
                queue.push_back(start);
            }
        }
        // A target in the source set is already a shortest (zero-hop) path.
        let mut shortest = ends
            .iter()
            .any(|end| distance.get(end) == Some(&0))
            .then_some(0);
        while let Some(id) = queue.pop_front() {
            let depth = distance[&id];
            if shortest.is_some_and(|limit| depth >= limit) {
                continue;
            }
            if ends.contains(&id) {
                shortest.get_or_insert(depth);
                continue;
            }
            for index in self.outgoing.get(id.as_str()).into_iter().flatten() {
                let next = self.edges[*index].to.as_ref();
                let next_depth = depth + 1;
                if shortest.is_some_and(|limit| next_depth > limit) {
                    continue;
                }
                match distance.get(next) {
                    None => {
                        distance.insert(next.to_string(), next_depth);
                        predecessors.insert(next.to_string(), vec![id.clone()]);
                        queue.push_back(next.to_string());
                    }
                    Some(existing) if *existing == next_depth => {
                        predecessors
                            .entry(next.to_string())
                            .or_default()
                            .push(id.clone());
                    }
                    _ => {}
                }
                if ends.contains(next) {
                    shortest =
                        Some(shortest.map_or(next_depth, |limit: usize| limit.min(next_depth)));
                }
            }
        }
        let Some(shortest) = shortest.or_else(|| {
            ends.iter()
                .any(|end| distance.get(end) == Some(&0))
                .then_some(0)
        }) else {
            return PathResult {
                paths: Vec::new(),
                truncated: false,
            };
        };
        for parents in predecessors.values_mut() {
            parents.sort();
            parents.dedup();
        }
        let mut queue = ends
            .iter()
            .filter(|end| distance.get(*end) == Some(&shortest))
            .map(|end| vec![end.clone()])
            .collect::<Vec<_>>();
        let mut paths = Vec::new();
        let mut truncated = false;
        const MAX_QUEUED_HOPS: usize = 1_000_000;
        let mut queued_hops = queue.len();
        while let Some(path) = queue.pop() {
            queued_hops -= path.len();
            let head = path.last().unwrap();
            if distance.get(head) == Some(&0) {
                if paths.len() == max_paths {
                    truncated = true;
                    break;
                }
                paths.push(path.into_iter().rev().collect());
                if paths.len() == max_paths {
                    truncated = !queue.is_empty();
                    break;
                }
                continue;
            }
            for predecessor in predecessors.get(head).into_iter().flatten() {
                let mut next_path = path.clone();
                next_path.push(predecessor.clone());
                if queued_hops + next_path.len() > MAX_QUEUED_HOPS {
                    truncated = true;
                    break;
                }
                queued_hops += next_path.len();
                queue.push(next_path);
            }
            if truncated {
                break;
            }
        }
        PathResult { paths, truncated }
    }
    pub fn external_list(&self, category: Option<&str>) -> Vec<&GraphNode> {
        self.nodes
            .iter()
            .filter(|n| {
                n.kind == NodeKind::External
                    && category.is_none_or(|c| n.category.as_deref() == Some(c))
            })
            .collect()
    }
    pub fn dead_functions(&self) -> Vec<&GraphNode> {
        self.nodes
            .iter()
            .filter(|n| {
                n.kind == NodeKind::Function
                    && self.incoming.get(n.id.as_str()).is_none_or(Vec::is_empty)
            })
            .collect()
    }
    pub fn roots(&self) -> Vec<&GraphNode> {
        self.dead_functions()
            .into_iter()
            .filter(|n| {
                self.outgoing
                    .get(n.id.as_str())
                    .is_some_and(|v| !v.is_empty())
            })
            .collect()
    }
    pub(crate) fn packages_of(&self, file: Option<&str>) -> &[String] {
        file.and_then(|file| self.source_packages.get(file))
            .map_or(&[], Vec::as_slice)
    }
    pub fn relations(
        &self,
        a: &[String],
        b: &[String],
        transitive_depth: Option<usize>,
    ) -> RelationResult {
        self.relations_with_config(
            &RelationOptions {
                a: a.to_vec(),
                b: b.to_vec(),
                transitive_depth,
                ..RelationOptions::default()
            },
            &Config::default(),
        )
    }

    /// Select two code sets, applying named sets, explicit exclusions and the
    /// default vendor boundary before computing their direct and indirect links.
    pub fn relations_with_config(
        &self,
        options: &RelationOptions,
        config: &Config,
    ) -> RelationResult {
        // Public callers can request smaller limits, never unbounded allocation.
        let mut bounded = options.clone();
        bounded.max_paths_per_pair = bounded.max_paths_per_pair.min(1_000);
        bounded.max_paths_total = bounded.max_paths_total.min(10_000);
        bounded.work_budget = bounded.work_budget.min(1_000_000);
        bounded.transitive_depth = bounded
            .transitive_depth
            .map(|depth| depth.min(self.nodes.len().max(1)));
        let options = &bounded;
        let mut warnings = Vec::new();
        let a = self.relation_set(&options.a, &options.exclude_a, config, &mut warnings);
        let b = self.relation_set(&options.b, &options.exclude_b, config, &mut warnings);
        let overlap = a.intersection(&b).cloned().collect();
        let a_to_b = self
            .edges
            .iter()
            .filter(|edge| a.contains(edge.from.as_ref()) && b.contains(edge.to.as_ref()))
            .cloned()
            .collect::<Vec<_>>();
        let b_to_a = self
            .edges
            .iter()
            .filter(|edge| b.contains(edge.from.as_ref()) && a.contains(edge.to.as_ref()))
            .cloned()
            .collect::<Vec<_>>();
        let depth = options.transitive_depth.unwrap_or(1);
        let mut budget = RelationBudget::default();
        let paths_a_to_b = self.relation_paths(&a, &b, &a_to_b, depth, options, &mut budget);
        let paths_b_to_a = self.relation_paths(&b, &a, &b_to_a, depth, options, &mut budget);
        let mut ids = a.union(&b).cloned().collect::<BTreeSet<_>>();
        for path in paths_a_to_b.iter().chain(&paths_b_to_a) {
            ids.extend(path.nodes.iter().cloned());
        }
        let counts = |members: &BTreeSet<String>| {
            let nodes = self
                .nodes
                .iter()
                .filter(|node| members.contains(&node.id))
                .collect::<Vec<_>>();
            RelationCounts {
                functions: nodes
                    .iter()
                    .filter(|node| node.kind == NodeKind::Function)
                    .count(),
                classes: nodes
                    .iter()
                    .filter_map(|node| node.parent_type.as_ref())
                    .collect::<BTreeSet<_>>()
                    .len(),
                files: nodes
                    .iter()
                    .filter_map(|node| node.file.as_ref())
                    .collect::<BTreeSet<_>>()
                    .len(),
                vendor: nodes.iter().filter(|node| node.vendor).count(),
            }
        };
        let mut result = RelationResult {
            counts_a: counts(&a),
            counts_b: counts(&b),
            a: a.into_iter().collect(),
            b: b.into_iter().collect(),
            a_to_b,
            b_to_a,
            matrix: BTreeMap::new(),
            overlap,
            nodes: self
                .nodes
                .iter()
                .filter(|node| ids.contains(&node.id))
                .cloned()
                .collect(),
            paths_a_to_b,
            paths_b_to_a,
            warnings,
            direct_call_matrix: BTreeMap::new(),
            direct_call_count: 0,
            route_count: 0,
            path_mode: options.path_mode,
            truncated: !budget.reasons.is_empty(),
            truncation_reasons: budget.reasons.into_iter().collect(),
            work_used: budget.work,
            options: options.clone(),
            routes_evaluated: true,
        };
        result.regroup_matrix("class");
        result
    }

    fn relation_set(
        &self,
        selectors: &[String],
        exclusions: &[String],
        config: &Config,
        warnings: &mut Vec<String>,
    ) -> BTreeSet<String> {
        let mut include = Vec::new();
        let mut exclude = Vec::new();
        expand_relation_selectors(
            selectors,
            config,
            &mut Vec::new(),
            false,
            &mut include,
            &mut exclude,
            warnings,
        );
        expand_relation_selectors(
            exclusions,
            config,
            &mut Vec::new(),
            true,
            &mut include,
            &mut exclude,
            warnings,
        );
        self.nodes
            .iter()
            .filter(|node| {
                let packages = self.packages_of(node.file.as_deref());
                include.iter().any(|selector| {
                    (!node.vendor || selector.starts_with("vendor:"))
                        && selector_matches(selector, node, packages)
                }) && !exclude
                    .iter()
                    .any(|selector| selector_matches(selector, node, packages))
            })
            .map(|node| node.id.clone())
            .collect()
    }

    fn relation_paths(
        &self,
        sources: &BTreeSet<String>,
        targets: &BTreeSet<String>,
        direct: &[GraphEdge],
        depth: usize,
        options: &RelationOptions,
        budget: &mut RelationBudget,
    ) -> Vec<RelationPath> {
        let mut paths = Vec::new();
        if depth == 0 || sources.is_empty() || targets.is_empty() {
            return paths;
        }
        let mut pair_counts = BTreeMap::<(String, String), usize>::new();
        // Direct call arrays remain complete even when returned route evidence is capped.
        for edge in direct {
            if !budget.step(options) {
                return paths;
            }
            let count = pair_counts
                .entry((edge.from.to_string(), edge.to.to_string()))
                .or_default();
            if *count >= options.max_paths_per_pair {
                budget.reasons.insert("max_paths_per_pair".into());
                continue;
            }
            if !budget.store(options, 1) {
                return paths;
            }
            *count += 1;
            paths.push(RelationPath {
                from: edge.from.to_string(),
                to: edge.to.to_string(),
                nodes: vec![edge.from.to_string(), edge.to.to_string()],
                edges: vec![edge.clone()],
            });
        }
        if depth < 2 || budget.stopped {
            return paths;
        }
        for start in sources {
            if !budget.step(options) {
                break;
            }
            let mut distance = BTreeMap::from([(start.as_str(), 0usize)]);
            let mut first_parent = BTreeMap::new();
            if options.path_mode != RelationPathMode::Simple {
                let mut queue = VecDeque::from([start.as_str()]);
                while let Some(at) = queue.pop_front() {
                    let hops = distance[at];
                    if hops >= depth {
                        continue;
                    }
                    for index in self.outgoing.get(at).into_iter().flatten() {
                        if !budget.step(options) {
                            return paths;
                        }
                        let next = self.edges[*index].to.as_ref();
                        if !distance.contains_key(next) {
                            distance.insert(next, hops + 1);
                            first_parent.insert(next, *index);
                            queue.push_back(next);
                        }
                    }
                }
            }
            if options.path_mode == RelationPathMode::OneShortest {
                for target in targets {
                    // Unreachable target pairs still cost work. Without charging
                    // this scan, two wide disconnected sets could exceed the
                    // enumeration budget by their Cartesian product.
                    if !budget.step(options) {
                        return paths;
                    }
                    let pair = (start.clone(), target.clone());
                    if pair_counts.contains_key(&pair)
                        || distance.get(target.as_str()).is_none_or(|hops| *hops < 2)
                    {
                        continue;
                    }
                    if options.max_paths_per_pair == 0 {
                        budget.reasons.insert("max_paths_per_pair".into());
                        continue;
                    }
                    let mut route = Vec::new();
                    let mut current = target.as_str();
                    while current != start {
                        if !budget.step(options) {
                            return paths;
                        }
                        let index = first_parent[current];
                        route.push(index);
                        current = self.edges[index].from.as_ref();
                    }
                    route.reverse();
                    if !budget.store(options, route.len()) {
                        return paths;
                    }
                    pair_counts.insert(pair, 1);
                    paths.push(RelationPath {
                        from: start.clone(),
                        to: target.clone(),
                        nodes: std::iter::once(start.clone())
                            .chain(route.iter().map(|i| self.edges[*i].to.to_string()))
                            .collect(),
                        edges: route.iter().map(|i| self.edges[*i].clone()).collect(),
                    });
                }
                continue;
            }
            // Iterative DFS retains one current route, rather than exponentially
            // many queued path clones. Distinct parallel callsites remain routes.
            let mut stack = vec![(start.as_str(), 0usize)];
            let mut route = Vec::<usize>::new();
            let mut visited = BTreeSet::from([start.as_str()]);
            while let Some((at, cursor)) = stack.last_mut() {
                if !budget.step(options) {
                    return paths;
                }
                let links = self.outgoing.get(*at).map(Vec::as_slice).unwrap_or(&[]);
                if *cursor >= links.len() || route.len() >= depth {
                    let (leaving, _) = stack.pop().unwrap();
                    visited.remove(leaving);
                    if !stack.is_empty() {
                        route.pop();
                    }
                    continue;
                }
                let index = links[*cursor];
                *cursor += 1;
                let edge = &self.edges[index];
                let next = edge.to.as_ref();
                if visited.contains(next) {
                    continue;
                }
                let hops = route.len() + 1;
                if options.path_mode != RelationPathMode::Simple
                    && distance.get(next) != Some(&hops)
                {
                    continue;
                }
                route.push(index);
                if hops > 1 && targets.contains(next) {
                    let count = pair_counts
                        .entry((start.clone(), next.to_owned()))
                        .or_default();
                    let default_suppressed =
                        options.path_mode == RelationPathMode::OneShortest && *count > 0;
                    if !default_suppressed {
                        if *count >= options.max_paths_per_pair {
                            budget.reasons.insert("max_paths_per_pair".into());
                        } else {
                            if !budget.store(options, hops) {
                                return paths;
                            }
                            *count += 1;
                            paths.push(RelationPath {
                                from: start.clone(),
                                to: next.to_owned(),
                                nodes: std::iter::once(start.clone())
                                    .chain(route.iter().map(|i| self.edges[*i].to.to_string()))
                                    .collect(),
                                edges: route.iter().map(|i| self.edges[*i].clone()).collect(),
                            });
                        }
                    }
                }
                visited.insert(next);
                stack.push((next, 0));
            }
        }
        paths
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
    fn accepts(&self, node: &GraphNode, edge: &GraphEdge) -> bool {
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
// Resolve imports using lexical module and actual declarations, never basename guesses.
fn import_alias(import: &Import) -> &str {
    crate::scoped_imports::alias(import)
}
fn import_applies(import: &Import, range: &SourceRange) -> bool {
    crate::scoped_imports::applies(import, range)
}
// Normalization depends only on this file and the immutable project indexes.
// Bound retained strings even for files with many distinct generated symbols.
fn normalize_symbol(
    cache: &mut HashMap<String, String>,
    symbol: String,
    identity: bool,
    normalize: impl FnOnce(&str) -> String,
) -> String {
    if identity {
        return symbol;
    }
    if let Some(result) = cache.get(&symbol) {
        return result.clone();
    }
    let result = normalize(&symbol);
    if cache.len() < 4096 {
        cache.insert(symbol, result.clone());
    }
    result
}

type ExportBindings<'a> = BTreeMap<&'a str, BTreeMap<String, String>>;

/// Type annotations and runtime bindings have separate export namespaces.
/// Erased value bindings retain an explicit marker, so direct declaration
/// lookup cannot bypass the absence of a runtime export.
fn export_bindings<'a>(
    files: &[&'a FileModel],
    modules: &ModuleIndex<'a>,
    type_exports: Option<&ExportBindings<'a>>,
) -> ExportBindings<'a> {
    let mut exports = BTreeMap::<&str, BTreeMap<String, String>>::new();
    for file in files {
        for e in &file.exports {
            if e.name == "*" {
                continue;
            }
            let target = e
                .source
                .as_ref()
                .map(|source| {
                    let suffix = if e.target.is_empty() {
                        String::new()
                    } else {
                        format!(".{}", e.target)
                    };
                    if matches!(file.language.as_str(), "javascript" | "typescript") {
                        if let Some(target) = modules.esm_resource_symbol(file, source, &suffix) {
                            return target;
                        }
                    }
                    resolve_import_path(file, &format!("{source}{suffix}"), &modules)
                })
                .unwrap_or_else(|| e.target.clone());
            let target = if e.type_only && type_exports.is_some() {
                format!("<type-only>::export:{target}")
            } else {
                target
            };
            exports
                .entry(file.language.as_str())
                .or_default()
                .insert(format!("{}.{}", file.module, e.name), target.clone());
            if matches!(file.language.as_str(), "javascript" | "typescript") {
                let target = target
                    .strip_prefix(&format!("{}.", file.module))
                    .map(|suffix| format!("{}.{suffix}", file.path))
                    .unwrap_or(target);
                exports
                    .entry(file.language.as_str())
                    .or_default()
                    .insert(format!("{}.{}", file.path, e.name), target);
            }
        }
    }
    for (from, to) in [("javascript", "typescript"), ("typescript", "javascript")] {
        let additions = exports.get(from).cloned().unwrap_or_default();
        for (symbol, target) in additions {
            exports
                .entry(to)
                .or_default()
                .entry(symbol)
                .or_insert(target);
        }
    }
    // Export-star contributes named exports only. A fixed point handles
    // barrels and cycles without recursion; explicit exports always win.
    let explicit_exports = exports
        .iter()
        .flat_map(|(language, map)| map.keys().map(move |key| (*language, key.clone())))
        .collect::<BTreeSet<_>>();
    let mut stars = Vec::new();
    for file in files {
        for export in file.exports.iter().filter(|export| export.name == "*") {
            if let Some(source) = export
                .source
                .as_ref()
                .and_then(|source| modules.esm_resource_symbol(file, source, ""))
            {
                stars.push((file, source, export.type_only));
            }
        }
    }
    let mut star_targets = BTreeMap::<(&str, String), BTreeSet<String>>::new();
    let mut work = 0usize;
    for _ in 0..=files.len() {
        let mut changed = false;
        for (file, source, type_only) in &stars {
            let prefix = format!("{source}.");
            let source_exports = if *type_only {
                type_exports.unwrap_or(&exports)
            } else {
                &exports
            };
            let bindings = source_exports
                .get(file.language.as_str())
                .into_iter()
                .flat_map(|map| {
                    map.range(prefix.clone()..)
                        .take_while(|(key, _)| key.starts_with(&prefix))
                })
                .filter_map(|(key, target)| {
                    let name = key.strip_prefix(&prefix)?;
                    (name != "default" && !name.contains('.')).then(|| {
                        (
                            name.to_owned(),
                            if *type_only && type_exports.is_some() {
                                format!("<type-only>::export:{target}")
                            } else {
                                canonical_export(&file.language, target, source_exports)
                            },
                        )
                    })
                })
                .collect::<Vec<_>>();
            for (name, target) in bindings {
                work += 1;
                if work > 100_000 {
                    break;
                }
                for language in ["javascript", "typescript"] {
                    for owner in [&file.module, &file.path] {
                        let key = format!("{owner}.{name}");
                        if explicit_exports.contains(&(language, key.clone())) {
                            continue;
                        }
                        let targets = star_targets.entry((language, key.clone())).or_default();
                        if !targets.insert(target.clone()) {
                            continue;
                        }
                        let target = if targets.len() == 1 {
                            targets.iter().next().unwrap().clone()
                        } else {
                            format!("<ambiguous-package>::export:{key}")
                        };
                        exports.entry(language).or_default().insert(key, target);
                        changed = true;
                    }
                }
            }
            if work > 100_000 {
                break;
            }
        }
        if !changed || work > 100_000 {
            break;
        }
    }
    if work > 100_000 {
        // Exhausting the propagation budget cannot certify the first
        // discovered provider while another provider remains unexamined.
        for (file, _, _) in &stars {
            for language in ["javascript", "typescript"] {
                for owner in [&file.module, &file.path] {
                    let map = exports.entry(language).or_default();
                    let prefix = format!("{owner}.");
                    let keys = map
                        .range(prefix.clone()..)
                        .take_while(|(key, _)| key.starts_with(&prefix))
                        .map(|(key, _)| key.clone())
                        .collect::<Vec<_>>();
                    for key in keys {
                        if !explicit_exports.contains(&(language, key.clone())) {
                            map.insert(
                                key.clone(),
                                format!("<ambiguous-package>::export-budget:{key}"),
                            );
                        }
                    }
                    map.insert(
                        owner.clone(),
                        format!("<ambiguous-package>::export-budget:{owner}"),
                    );
                }
            }
        }
    }
    exports
}

fn canonical_export(
    language: &str,
    symbol: &str,
    exports: &BTreeMap<&str, BTreeMap<String, String>>,
) -> String {
    let Some(exports) = exports.get(language) else {
        return symbol.to_owned();
    };
    let mut result = symbol.to_owned();
    let mut seen = BTreeSet::new();
    while seen.insert(result.clone()) {
        let found = std::iter::once(result.len())
            .chain(
                result
                    .char_indices()
                    .rev()
                    .filter_map(|(i, c)| (c == '.').then_some(i)),
            )
            .find_map(|i| {
                exports
                    .get(&result[..i])
                    .map(|target| format!("{target}{}", &result[i..]))
            });
        match found {
            Some(next) if next != result => result = next,
            _ => break,
        }
    }
    result
}
fn declared_field_type(
    language: &str,
    owner: &str,
    name: &str,
    fields: &BTreeMap<&str, BTreeMap<&str, BTreeMap<&str, Option<String>>>>,
    parents: &BTreeMap<&str, BTreeMap<&str, Vec<String>>>,
) -> Option<String> {
    let mut pending = vec![owner];
    let mut visited = BTreeSet::new();
    let mut matches = BTreeSet::new();
    let mut unknown = false;
    while let Some(owner) = pending.pop() {
        if !visited.insert(owner) {
            continue;
        }
        if let Some(ty) = fields
            .get(language)
            .and_then(|types| types.get(owner))
            .and_then(|fields| fields.get(name))
        {
            if let Some(ty) = ty {
                matches.insert(ty.as_str());
            } else {
                unknown = true;
            }
        } else {
            pending.extend(
                parents
                    .get(language)
                    .and_then(|types| types.get(owner))
                    .into_iter()
                    .flatten()
                    .map(String::as_str),
            );
        }
    }
    (!unknown && matches.len() == 1).then(|| matches.into_iter().next().unwrap().to_owned())
}

/// JS/TS library base classes imported from a package (`class Db extends
/// PrismaClient`). They have no metadata, so inherited members keep the
/// library identity for external-operation patterns.
#[derive(Default)]
struct ExternalBases<'a> {
    bases: BTreeMap<(&'a str, &'a str), Vec<String>>,
    /// ESM file-path spellings (`src/db.ts.Db`) of declared type FQNs.
    aliases: BTreeMap<(&'a str, String), &'a str>,
}
impl ExternalBases<'_> {
    /// The one library base whose member `name` a project type inherits,
    /// unless a project class in its hierarchy declares that member.
    fn member_owner(
        &self,
        language: &str,
        owner: &str,
        name: &str,
        fields: &BTreeMap<&str, BTreeMap<&str, BTreeMap<&str, Option<String>>>>,
        parents: &BTreeMap<&str, BTreeMap<&str, Vec<String>>>,
    ) -> Option<String> {
        if self.bases.is_empty() {
            return None;
        }
        let canonical = |owner: &str| {
            self.aliases
                .get(&(language, owner.to_owned()))
                .map_or_else(|| owner.to_owned(), |fqn| (*fqn).to_owned())
        };
        let mut pending = vec![canonical(owner)];
        let mut visited = BTreeSet::new();
        let mut bases = BTreeSet::new();
        while let Some(owner) = pending.pop() {
            if !visited.insert(owner.clone()) {
                continue;
            }
            // JS/TS instance methods and accessors are recorded as members too.
            if fields
                .get(language)
                .and_then(|types| types.get(owner.as_str()))
                .is_some_and(|fields| fields.contains_key(name))
            {
                return None;
            }
            bases.extend(
                self.bases
                    .get(&(language, owner.as_str()))
                    .into_iter()
                    .flatten()
                    .cloned(),
            );
            pending.extend(
                parents
                    .get(language)
                    .and_then(|types| types.get(owner.as_str()))
                    .into_iter()
                    .flatten()
                    .map(|base| canonical(base)),
            );
        }
        (bases.len() == 1).then(|| bases.into_iter().next().unwrap())
    }
}

/// Match imported roots only at the language's qualification separator. This
/// deliberately never falls back to a short name found elsewhere in the project.
pub(crate) fn type_import_suffix<'s>(
    language: &str,
    symbol: &'s str,
    alias: &str,
) -> Option<&'s str> {
    let suffix = symbol.strip_prefix(alias)?;
    let separator = match language {
        "php" => "\\",
        "rust" => "::",
        _ => ".",
    };
    (suffix.is_empty() || suffix.starts_with(separator)).then_some(suffix)
}
pub(crate) fn type_import_priority(
    file: &FileModel,
    import: &Import,
    identity: &str,
    range: &SourceRange,
) -> Option<(usize, (usize, usize), (usize, usize))> {
    let alias = import_alias(import);
    type_import_suffix(&file.language, identity, alias)?;
    if !import_applies(import, range)
        || file.language == "python"
            && (import.range.start_line, import.range.start_column)
                > (range.start_line, range.start_column)
    {
        return None;
    }
    Some((
        alias.len(),
        import
            .scope
            .as_ref()
            .map(|s| (s.start_line, s.start_column))
            .unwrap_or_default(),
        if file.language == "python" {
            (import.range.start_line, import.range.start_column)
        } else {
            (0, 0)
        },
    ))
}
fn resolve_type_reference(
    file: &FileModel,
    raw: &str,
    owner: &str,
    range: &SourceRange,
    modules: &ModuleIndex<'_>,
    exports: &BTreeMap<&str, BTreeMap<String, String>>,
    types: &BTreeSet<(&str, &str)>,
) -> String {
    let php_absolute =
        file.language == "php" && raw.trim().trim_start_matches('?').starts_with('\\');
    let identity = modules.type_reference(file, raw);
    let raw = identity.as_deref().unwrap_or(raw);
    if matches!(raw, "Self" | "self" | "static") {
        let separator = modules.method_separator(&file.language);
        let mut context = Some(owner);
        while let Some(candidate) = context {
            if types.contains(&(file.language.as_str(), candidate)) {
                return candidate.to_owned();
            }
            context = candidate.rsplit_once(separator).map(|(prefix, _)| prefix);
        }
    }
    if file.language == "rust" {
        if raw.starts_with("crate::") || raw.starts_with("super::") || raw.starts_with("self::") {
            return rust_import_path(file, raw, owner, modules);
        }
        if !types.contains(&(file.language.as_str(), raw)) {
            if let Some(target) = raw.strip_prefix(&format!("{}::", file.module)) {
                if modules.native_receiver("rust", target)
                    || target.starts_with("crate::")
                    || target.starts_with("super::")
                    || target.starts_with("self::")
                    || file
                        .imports
                        .iter()
                        .any(|i| import_alias(i) == target && import_applies(i, range))
                {
                    return resolve_type_reference(
                        file, target, owner, range, modules, exports, types,
                    );
                }
            }
        }
    }
    let php_namespace_relative = file.language == "php" && raw.starts_with("namespace\\");
    if !php_absolute && !php_namespace_relative {
        let mut priority = None;
        let mut targets = BTreeSet::new();
        for import in &file.imports {
            let imported_binding = matches!(file.language.as_str(), "javascript" | "typescript")
                && import.path == raw
                && import_applies(import, range)
                && import.source.as_deref().is_some_and(|source| {
                    let normalized = [".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs"]
                        .iter()
                        .find_map(|extension| source.strip_suffix(extension))
                        .unwrap_or(source);
                    normalized != import.path
                });
            let Some(candidate) = type_import_priority(file, import, raw, range).or_else(|| {
                imported_binding.then(|| {
                    (
                        raw.len(),
                        import
                            .scope
                            .as_ref()
                            .map(|s| (s.start_line, s.start_column))
                            .unwrap_or_default(),
                        (0, 0),
                    )
                })
            }) else {
                continue;
            };
            if priority.is_none_or(|old| candidate > old) {
                priority = Some(candidate);
                targets.clear();
            }
            if priority == Some(candidate) {
                let destination = resolve_scoped_import(file, import, modules);
                let suffix =
                    type_import_suffix(&file.language, raw, import_alias(import)).unwrap_or("");
                targets.insert(canonical_export(
                    &file.language,
                    &format!("{destination}{suffix}"),
                    exports,
                ));
            }
        }
        if targets.len() == 1 {
            return targets.into_iter().next().unwrap();
        }
        if !targets.is_empty() {
            // Conflicting equal-priority imports cannot identify one declaration.
            // Parentheses cannot occur in a declared type FQN, preventing fallback
            // from turning an ambiguous import into an unrelated project type.
            return format!("unresolved-import({raw})");
        }
    }
    if php_absolute || types.contains(&(file.language.as_str(), raw)) {
        return raw.to_owned();
    }
    if file.language == "php" {
        // Resolve a source annotation that was not already normalized/imported.
        return php_namespace_reference(raw, owner);
    }
    let sep = modules.namespace_separator(&file.language);
    let mut context = owner;
    while let Some((parent, _)) = context.rsplit_once(sep) {
        let candidate = format!("{parent}{sep}{raw}");
        if types.contains(&(file.language.as_str(), candidate.as_str())) {
            return candidate;
        }
        context = parent;
    }
    raw.to_owned()
}

fn php_namespace_reference(raw: &str, owner: &str) -> String {
    let namespace = owner
        .split("::")
        .next()
        .unwrap_or(owner)
        .rsplit_once('\\')
        .map(|(namespace, _)| namespace)
        .unwrap_or("");
    let raw = raw.strip_prefix("namespace\\").unwrap_or(raw);
    if namespace.is_empty() {
        raw.to_owned()
    } else {
        format!("{namespace}\\{raw}")
    }
}

fn resolve_scoped_import(file: &FileModel, import: &Import, modules: &ModuleIndex<'_>) -> String {
    if matches!(file.language.as_str(), "javascript" | "typescript") {
        if let Some(source) = &import.source {
            let normalized = [".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs"]
                .iter()
                .find_map(|extension| source.strip_suffix(extension))
                .unwrap_or(source);
            let suffix = import.path.strip_prefix(normalized).unwrap_or("");
            return modules
                .esm_resource_symbol_kind(file, source, suffix, import.require)
                .unwrap_or_else(|| import.path.clone());
        }
    }
    if let Some(binding) = &import.binding_path {
        return resolve_import_path(file, binding, modules);
    }
    if file.language == "rust" {
        return rust_import_path(
            file,
            &import.path,
            import.module.as_deref().unwrap_or(&file.module),
            modules,
        );
    }
    resolve_import_path(file, &import.path, modules)
}
fn rust_import_path(
    file: &FileModel,
    imported: &str,
    lexical: &str,
    modules: &ModuleIndex<'_>,
) -> String {
    let lexical = file
        .types
        .iter()
        .filter(|ty| ty.kind == TypeKind::Namespace)
        .map(|ty| ty.fqn.as_str())
        .filter(|namespace| {
            lexical == *namespace
                || lexical
                    .strip_prefix(namespace)
                    .is_some_and(|suffix| suffix.starts_with("::"))
        })
        .max_by_key(|namespace| namespace.len())
        .unwrap_or(&file.module);
    let contexts = modules
        .rust_contexts
        .get(&file.module)
        .cloned()
        .unwrap_or_else(|| BTreeSet::from([file.module.clone()]));
    let suffix = lexical.strip_prefix(&file.module).unwrap_or("");
    let mut candidates = BTreeSet::new();
    if let Some(rest) = imported.strip_prefix("crate::") {
        for root in modules.rust_roots.get(&file.module).into_iter().flatten() {
            if let Some(candidate) = modules.rust_canonical(&format!("{root}::{rest}")) {
                candidates.insert(candidate);
            }
        }
    } else {
        // Parser-supplied local FQNs are trustworthy; a different file's
        // physical filename alone does not establish a Rust module binding.
        if component_prefix(imported, &file.module) {
            if let Some(candidate) = modules.rust_canonical(imported) {
                return candidate;
            }
        }
        for context in contexts {
            let mut base = format!("{context}{suffix}");
            let mut rest = imported;
            if let Some(remainder) = rest.strip_prefix("self::") {
                rest = remainder;
            } else {
                while let Some(remainder) = rest.strip_prefix("super::") {
                    let root = modules.rust_roots.get(&file.module).and_then(|roots| {
                        roots
                            .iter()
                            .filter(|root| component_prefix(&base, root))
                            .max_by_key(|root| root.len())
                    });
                    if root.is_some_and(|root| *root == base) {
                        base.clear();
                        break;
                    }
                    base = base
                        .rsplit_once("::")
                        .map(|(parent, _)| parent)
                        .unwrap_or("")
                        .to_owned();
                    rest = remainder;
                }
            }
            if base.is_empty() {
                continue;
            }
            if let Some(candidate) = modules.rust_canonical(&format!("{base}::{rest}")) {
                candidates.insert(candidate);
            }
        }
    }
    if candidates.len() == 1 {
        candidates.into_iter().next().unwrap()
    } else if !candidates.is_empty() {
        format!("<unresolved>::{imported}")
    } else if let Some(resolved) = modules.rust_dependency_path(file, imported) {
        resolved
    } else if modules.rust_canonical(imported).is_some() {
        // Keep a discovered but unbound physical filename from becoming an
        // accidental project-wide target in later candidate lookup.
        format!("<unresolved>::{imported}")
    } else {
        imported.to_owned()
    }
}
fn resolve_import_path(file: &FileModel, imported: &str, modules: &ModuleIndex<'_>) -> String {
    let module_exists = |name: &str| {
        modules.contains(&(file.language.as_str(), name))
            || name.char_indices().any(|(index, ch)| {
                matches!(ch, '.' | ':' | '\\')
                    && modules.contains(&(file.language.as_str(), &name[..index]))
            })
    };
    if file.language == "rust" {
        return rust_import_path(file, imported, &file.module, modules);
    }
    let parent = std::path::Path::new(&file.path)
        .parent()
        .unwrap_or(std::path::Path::new(""));
    let relative = match file.language.as_str() {
        "javascript" | "typescript" => {
            if !imported.starts_with("./")
                && !imported.starts_with("../")
                && module_exists(imported)
            {
                return imported.to_owned();
            }
            if let Some(symbol) = modules.esm_resource_symbol(file, imported, "") {
                return symbol;
            }
            let path = parent.join(imported);
            let mut parts = Vec::new();
            for component in path.components() {
                match component {
                    std::path::Component::ParentDir => {
                        parts.pop();
                    }
                    std::path::Component::CurDir => {}
                    std::path::Component::Normal(part) => {
                        parts.push(part.to_string_lossy().into_owned())
                    }
                    _ => {}
                }
            }
            parts.join("/")
        }
        "python" => {
            let dots = imported.chars().take_while(|c| *c == '.').count();
            if dots == 0 && module_exists(imported) {
                return imported.to_owned();
            }
            let mut parts = parent
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            for _ in 1..dots {
                parts.pop();
            }
            let rest = imported.trim_start_matches('.');
            if dots == 0 {
                // Python 3 has no implicit relative imports, but a script run from this
                // directory still sees its siblings first. Bind there only when the imported
                // top-level name itself is a module (not merely the enclosing package), and
                // inside a regular package not when it names a standard-library module.
                let package = parts.join(".");
                if parts.is_empty()
                    || modules.contains(&("python", package.as_str()))
                        && modules.native_receiver("python", rest)
                {
                    return imported.to_owned();
                }
                let candidate = format!("{package}.{rest}");
                let head = package.len() + 1 + rest.find('.').unwrap_or(rest.len());
                let found = modules.contains(&("python", candidate.as_str()))
                    || candidate.match_indices('.').any(|(index, _)| {
                        index >= head && modules.contains(&("python", &candidate[..index]))
                    });
                return if found {
                    candidate
                } else {
                    imported.to_owned()
                };
            }
            if parts.is_empty() {
                rest.to_owned()
            } else {
                format!("{}.{rest}", parts.join("."))
            }
        }
        "php" => return imported.to_owned(),
        _ => {
            let sep = if file.module.contains("::") {
                "::"
            } else if file.module.contains('\\') {
                "\\"
            } else {
                "."
            };
            file.module
                .rsplit_once(sep)
                .map(|(parent, _)| format!("{parent}{sep}{imported}"))
                .unwrap_or_else(|| imported.to_owned())
        }
    };
    if module_exists(&relative) {
        relative
    } else {
        imported.to_owned()
    }
}

#[allow(clippy::too_many_arguments)]
fn rust_contract_identity(
    file: &FileModel,
    contract: &str,
    scope: &str,
    range: &SourceRange,
    modules: &ModuleIndex<'_>,
    exports: &BTreeMap<&str, BTreeMap<String, String>>,
    types: &BTreeSet<(&str, &str)>,
) -> String {
    let owner = resolve_type_reference(file, contract, scope, range, modules, exports, types);
    contract
        .find('<')
        .map(|start| {
            let specialization = contract[start..]
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect::<String>();
            format!("{owner}{specialization}")
        })
        .unwrap_or(owner)
}

fn function_id(file: &FileModel, fqn: &str) -> String {
    format!("fn:{}:{}:{}", file.language, file.path, fqn)
}
fn vendor_package(path: &str, vendor: bool) -> Option<String> {
    if vendor {
        Some(crate::discovery::vendor_package(path))
    } else {
        None
    }
}
fn compile_pattern(pattern: &str) -> Option<globset::GlobMatcher> {
    globset::GlobBuilder::new(pattern)
        .backslash_escape(false)
        .build()
        .ok()
        .map(|glob| glob.compile_matcher())
}
fn pattern_matches(pattern: &str, value: &str) -> bool {
    pattern == value || compile_pattern(pattern).is_some_and(|matcher| matcher.is_match(value))
}
fn has_multiple_segments(value: &str) -> bool {
    value
        .split(['.', ':', '\\', '/'])
        .filter(|part| !part.is_empty())
        .nth(1)
        .is_some()
}
fn has_multiple_segments_for(value: &str, strategy: &dyn crate::LanguageStrategy) -> bool {
    has_multiple_segments(value)
        || [strategy.namespace_separator(), strategy.method_separator()]
            .iter()
            .filter(|separator| !separator.is_empty())
            .any(|separator| {
                value
                    .split(separator)
                    .filter(|part| !part.is_empty())
                    .nth(1)
                    .is_some()
            })
}
fn split_symbol(value: &str) -> Vec<&str> {
    value
        .split(['.', ':', '\\', '/'])
        .filter(|part| !part.is_empty())
        .collect()
}

#[cfg(test)]
mod matcher_tests {
    use super::pattern_matches;
    #[test]
    fn architecture_declarations_translate_exact_esm_resources_and_reexports() {
        use crate::ir::{Export, FileModel, Import, SourceRange, TypeModel};
        let file = FileModel {
            path: "a.ts".into(),
            language: "typescript".into(),
            module: "a".into(),
            imports: vec![Import {
                path: "./barrel.Repo".into(),
                source: Some("./barrel.js".into()),
                alias: Some("Model".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let barrel = FileModel {
            path: "barrel.ts".into(),
            language: "typescript".into(),
            module: "barrel".into(),
            exports: vec![Export {
                name: "Repo".into(),
                target: "Repo".into(),
                source: Some("./b.js".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let target = FileModel {
            path: "b.ts".into(),
            language: "typescript".into(),
            module: "b".into(),
            types: vec![TypeModel {
                name: "Repo".into(),
                fqn: "b.Repo".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let resolver = super::DeclarationResolver::new(&[&file, &barrel, &target]);
        assert_eq!(
            resolver.type_reference(&file, "Model", "a.run", &SourceRange::default()),
            "b.Repo"
        );
        assert_eq!(
            resolver.import_destination(&file, &file.imports[0]),
            "barrel.Repo"
        );
        assert_eq!(
            resolver.imported_callable(&file, "Model.run", &SourceRange::default()),
            Ok(Some("b.Repo.run".into()))
        );
        assert_eq!(
            resolver.type_reference(&file, "Missing", "a.run", &SourceRange::default()),
            "Missing"
        );
    }

    #[test]
    fn architecture_declarations_retain_known_rust_module_roots() {
        use crate::ir::{FileModel, SourceRange, TypeKind, TypeModel};
        let root = FileModel {
            path: "src/lib.rs".into(),
            language: "rust".into(),
            module: "src::lib".into(),
            types: vec![TypeModel {
                name: "models".into(),
                fqn: "src::lib::models".into(),
                kind: TypeKind::Namespace,
                external_module: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let target = FileModel {
            path: "src/models.rs".into(),
            language: "rust".into(),
            module: "src::models".into(),
            types: vec![TypeModel {
                name: "Repo".into(),
                fqn: "src::models::Repo".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let resolver = super::DeclarationResolver::new(&[&root, &target]);
        assert_eq!(
            resolver.type_reference(
                &root,
                "crate::models::Repo",
                "src::lib::work",
                &SourceRange::default()
            ),
            "src::models::Repo"
        );
    }

    #[test]
    fn normalization_cache_reuses_results_and_bounds_retained_symbols() {
        let mut cache = std::collections::HashMap::new();
        let mut calls = 0;
        for symbol in ["module.тип", "module.тип", "other", "other"] {
            assert_eq!(
                super::normalize_symbol(&mut cache, symbol.into(), false, |name| {
                    calls += 1;
                    format!("canonical.{name}")
                }),
                format!("canonical.{symbol}")
            );
        }
        assert_eq!(calls, 2);
        for i in 0..5000 {
            let symbol = format!("symbol_{i}");
            assert_eq!(
                super::normalize_symbol(&mut cache, symbol.clone(), false, str::to_owned),
                symbol
            );
        }
        assert_eq!(cache.len(), 4096);
        assert_eq!(
            super::normalize_symbol(&mut cache, "module.тип".into(), false, |_| panic!(
                "cached"
            )),
            "canonical.module.тип"
        );
        let symbol = "unchanged_php_symbol".to_owned();
        let buffer = symbol.as_ptr();
        let result = super::normalize_symbol(&mut cache, symbol, true, |_| panic!("identity"));
        assert_eq!(result.as_ptr(), buffer);
        assert_eq!(cache.len(), 4096);
        let mut other_file = std::collections::HashMap::new();
        assert_eq!(
            super::normalize_symbol(&mut other_file, "module.тип".into(), false, |_| {
                "other.module".into()
            }),
            "other.module"
        );
    }

    #[test]
    fn rust_prefix_index_matches_range_lookup_including_overlapping_separators() {
        let names = [
            ("rust", "pkg::module::Type"),
            ("rust", "pkg-other::Type"),
            ("rust", "pkg.member"),
            ("rust", "тип::поле"),
            ("rust", "overlap:::name"),
            ("rust", "::root"),
            ("rust", "exact"),
            ("python", "foreign::Type"),
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
        let index = super::ModuleIndex::new(names.clone());
        for candidate in [
            "",
            "pkg",
            "pkg::module",
            "pkg::module::Type",
            "pkg-other",
            "pkg.member",
            "pkg::missing",
            "тип",
            "тип::поле",
            "overlap",
            "overlap:",
            "overlap::",
            "exact",
            "foreign",
            "unknown",
            "::root",
        ] {
            let prefix = format!("{candidate}::");
            let expected = names.contains(&("rust", candidate))
                || names
                    .range(("rust", prefix.as_str())..)
                    .next()
                    .is_some_and(|(lang, name)| *lang == "rust" && name.starts_with(&prefix));
            assert_eq!(index.rust_exists(candidate), expected, "{candidate}");
            assert_eq!(
                index.contains(&("rust", candidate)),
                names.contains(&("rust", candidate))
            );
        }
    }
    #[test]
    fn borrowed_field_index_preserves_overrides_ambiguity_cycles_and_languages() {
        use std::collections::{BTreeMap, BTreeSet};
        let old_fields: BTreeMap<(String, String, String), Option<String>> = [
            ("python", "A", "value", Some("First")),
            ("python", "B", "value", Some("Second")),
            ("python", "C", "value", None),
            ("rust", "A", "value", Some("Other")),
        ]
        .into_iter()
        .map(|(l, o, n, t)| ((l.into(), o.into(), n.into()), t.map(str::to_owned)))
        .collect();
        let old_parents: BTreeMap<(String, String), Vec<String>> = [
            ("python", "B", vec!["A"]),
            ("python", "C", vec!["A"]),
            ("python", "D", vec!["A", "B"]),
            ("python", "E", vec!["A", "A"]),
            ("python", "F", vec!["G"]),
            ("python", "G", vec!["F", "A"]),
            ("python", "H", vec!["A", "C"]),
            ("python", "I", vec!["missing"]),
        ]
        .into_iter()
        .map(|(l, o, b)| {
            (
                (l.into(), o.into()),
                b.into_iter().map(str::to_owned).collect(),
            )
        })
        .collect();
        let mut fields = BTreeMap::new();
        for ((l, o, n), t) in &old_fields {
            fields
                .entry(l.as_str())
                .or_insert_with(BTreeMap::new)
                .entry(o.as_str())
                .or_insert_with(BTreeMap::new)
                .insert(n.as_str(), t.clone());
        }
        let mut parents = BTreeMap::new();
        for ((l, o), b) in &old_parents {
            parents
                .entry(l.as_str())
                .or_insert_with(BTreeMap::new)
                .insert(o.as_str(), b.clone());
        }
        for language in ["python", "rust", "unknown"] {
            for owner in ["A", "B", "C", "D", "E", "F", "G", "H", "I", "missing"] {
                for name in ["value", "missing"] {
                    let mut pending = vec![owner.to_owned()];
                    let mut visited = BTreeSet::new();
                    let mut matches = BTreeSet::new();
                    let mut unknown = false;
                    while let Some(o) = pending.pop() {
                        if !visited.insert(o.clone()) {
                            continue;
                        }
                        if let Some(t) =
                            old_fields.get(&(language.to_owned(), o.clone(), name.to_owned()))
                        {
                            if let Some(t) = t {
                                matches.insert(t.clone());
                            } else {
                                unknown = true;
                            }
                        } else {
                            pending.extend(
                                old_parents
                                    .get(&(language.to_owned(), o))
                                    .into_iter()
                                    .flatten()
                                    .cloned(),
                            );
                        }
                    }
                    let expected = (!unknown && matches.len() == 1)
                        .then(|| matches.into_iter().next().unwrap());
                    assert_eq!(
                        super::declared_field_type(language, owner, name, &fields, &parents),
                        expected,
                        "{language}: {owner}.{name}"
                    );
                }
            }
        }
        for symbol in [
            "",
            ".",
            "a",
            "..a..",
            "a..b",
            "тип::поле",
            "a/b",
            "\\a\\b",
            "::::",
        ] {
            assert_eq!(
                super::has_multiple_segments(symbol),
                super::split_symbol(symbol).len() > 1,
                "{symbol}"
            );
        }
    }
    #[test]
    fn export_index_preserves_prefixes_cycles_and_language_isolation() {
        use std::collections::{BTreeMap, BTreeSet};
        let old: BTreeMap<(String, String), String> = [
            ("javascript", "pkg", "lib"),
            ("javascript", "pkg.Client", "dep.Client"),
            ("javascript", "dep.Client", "impl.Client"),
            ("javascript", "cycle.a", "cycle.b"),
            ("javascript", "cycle.b", "cycle.a"),
            ("javascript", "same", "same"),
            ("javascript", "пакет.Класс", "модуль.Класс"),
            ("typescript", "pkg.Client", "other.Client"),
        ]
        .into_iter()
        .map(|(lang, from, to)| ((lang.into(), from.into()), to.into()))
        .collect();
        let mut index = BTreeMap::<&str, BTreeMap<String, String>>::new();
        for ((language, alias), target) in &old {
            index
                .entry(language)
                .or_default()
                .insert(alias.clone(), target.clone());
        }
        for language in ["javascript", "typescript", "rust", "unknown"] {
            for symbol in [
                "pkg.Client.method",
                "pkg.other",
                "pkg.Client",
                "cycle.a.method",
                "same",
                "пакет.Класс.метод",
                "missing",
                "",
                "pkg.Client::method",
            ] {
                let mut expected = symbol.to_owned();
                let mut seen = BTreeSet::new();
                while seen.insert(expected.clone()) {
                    let found = std::iter::once(expected.len())
                        .chain(
                            expected
                                .char_indices()
                                .rev()
                                .filter_map(|(i, c)| (c == '.').then_some(i)),
                        )
                        .find_map(|i| {
                            old.get(&(language.to_owned(), expected[..i].to_owned()))
                                .map(|target| format!("{target}{}", &expected[i..]))
                        });
                    match found {
                        Some(next) if next != expected => expected = next,
                        _ => break,
                    }
                }
                assert_eq!(
                    super::canonical_export(language, symbol, &index),
                    expected,
                    "{language}: {symbol}"
                );
            }
        }
    }
    #[test]
    fn php_namespace_globs_keep_backslashes_as_separators() {
        assert!(pattern_matches(
            "Doctrine\\DBAL\\*::executeQuery",
            "Doctrine\\DBAL\\Connection::executeQuery"
        ));
        assert!(pattern_matches(
            "GuzzleHttp\\*::request",
            "GuzzleHttp\\Client::request"
        ));
        assert!(!pattern_matches(
            "GuzzleHttp\\*::request",
            "GuzzleHttp\\Client::send"
        ));
    }
}
fn literal_argument(call: &CallSite, config: &Config) -> Option<String> {
    let first = call.arguments.first()?.trim();
    if !((first.starts_with('"') && first.ends_with('"'))
        || (first.starts_with('\'') && first.ends_with('\'')))
    {
        return None;
    }
    let mut value = first[1..first.len() - 1]
        .chars()
        .take(config.external.max_arg_length)
        .collect::<String>();
    for pattern in &config.external.redact {
        if let Ok(re) = regex::Regex::new(pattern) {
            value = re.replace_all(&value, "[REDACTED]").to_string();
        }
    }
    Some(value)
}
fn apply_vendor_view(
    nodes: &mut Vec<GraphNode>,
    edges: &mut Vec<GraphEdge>,
    mode: VendorMode,
) -> Vec<GraphEdge> {
    if mode == VendorMode::Expanded {
        return Vec::new();
    }
    let packages = nodes
        .iter()
        .filter(|n| n.vendor)
        .filter_map(|n| n.package.clone())
        .collect::<BTreeSet<_>>();
    if mode == VendorMode::Hidden {
        let vendor = nodes
            .iter()
            .filter(|n| n.vendor)
            .map(|n| n.id.clone())
            .collect::<BTreeSet<_>>();
        let mut hidden = Vec::new();
        edges.retain(|e| {
            if vendor.contains(e.to.as_ref()) || vendor.contains(e.from.as_ref()) {
                let mut edge = e.clone();
                edge.hidden_vendor = true;
                hidden.push(edge);
                false
            } else {
                true
            }
        });
        nodes.retain(|n| !n.vendor);
        return hidden;
    }
    let mapping = nodes
        .iter()
        .filter(|n| n.vendor)
        .filter_map(|n| {
            n.package
                .as_ref()
                .map(|p| (n.id.clone(), format!("vendor:{p}")))
        })
        .collect::<BTreeMap<_, _>>();
    for e in edges.iter_mut() {
        if let Some(id) = mapping.get(e.from.as_ref()) {
            e.from = id.clone().into()
        }
        if let Some(id) = mapping.get(e.to.as_ref()) {
            e.to = id.clone().into()
        }
    }
    nodes.retain(|n| !n.vendor);
    for p in packages {
        nodes.push(GraphNode {
            id: format!("vendor:{p}"),
            label: p.clone(),
            kind: NodeKind::VendorPackage,
            file: None,
            line: None,
            language: None,
            module: None,
            parent_type: None,
            vendor: true,
            package: Some(p),
            category: None,
            library: None,
            operation: None,
            argument: None,
            algorithmic_complexity: None,
            cargo_target: None,
            cargo_target_id: None,
            source_fqn: None,
        });
    }
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    Vec::new()
}
fn coverage(
    nodes: &[GraphNode],
    edges: &[GraphEdge],
    roots: &[String],
    direction: Direction,
) -> Coverage {
    let mut c = Coverage::default();
    for n in nodes {
        match n.kind {
            NodeKind::Function => {
                if n.vendor {
                    c.vendor_functions.push(n.label.clone());
                    if let Some(t) = &n.parent_type {
                        c.vendor_classes.push(t.clone());
                    }
                } else {
                    c.functions.push(n.label.clone());
                    if let Some(t) = &n.parent_type {
                        c.classes.push(t.clone());
                    }
                    if let Some(f) = &n.file {
                        c.files.push(f.clone());
                    }
                    if let Some(m) = &n.module {
                        c.modules.push(m.clone());
                    }
                }
            }
            NodeKind::External => c.external.push(n.label.clone()),
            NodeKind::Unknown => c.unresolved.push(n.label.clone()),
            NodeKind::VendorPackage => c.vendor_boundary = true,
        }
    }
    for v in [
        &mut c.functions,
        &mut c.classes,
        &mut c.files,
        &mut c.modules,
        &mut c.vendor_functions,
        &mut c.vendor_classes,
        &mut c.external,
        &mut c.unresolved,
    ] {
        v.sort();
        v.dedup()
    }
    let incoming = edges
        .iter()
        .map(|e| e.to.to_string())
        .collect::<BTreeSet<_>>();
    let outgoing = edges
        .iter()
        .map(|e| e.from.to_string())
        .collect::<BTreeSet<_>>();
    for n in nodes {
        if n.kind == NodeKind::Function {
            if !incoming.contains(n.id.as_str()) {
                c.entry_points.push(n.label.clone())
            }
            if !outgoing.contains(n.id.as_str()) {
                c.leaves.push(n.label.clone())
            }
        }
    }
    if direction == Direction::Callers && c.entry_points.is_empty() {
        c.entry_points = roots.to_vec()
    }
    c
}
pub(crate) fn expand_relation_selectors(
    selectors: &[String],
    config: &Config,
    stack: &mut Vec<String>,
    excluding: bool,
    include: &mut Vec<String>,
    exclude: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    for selector in selectors {
        let (selector, excluding) = selector
            .strip_prefix('!')
            .map(|selector| (selector, true))
            .unwrap_or((selector.as_str(), excluding));
        let named = selector
            .strip_prefix("set:")
            .or_else(|| config.sets.contains_key(selector).then_some(selector));
        if let Some(name) = named {
            let warning = if stack.iter().any(|entry| entry == name) {
                Some(format!(
                    "cyclic named set: {} -> {name}",
                    stack.join(" -> ")
                ))
            } else if !config.sets.contains_key(name) {
                Some(format!("unknown named set: {name}"))
            } else {
                None
            };
            if let Some(warning) = warning {
                if !warnings.contains(&warning) {
                    warnings.push(warning);
                }
                continue;
            }
            stack.push(name.to_owned());
            expand_relation_selectors(
                &config.sets[name],
                config,
                stack,
                excluding,
                include,
                exclude,
                warnings,
            );
            stack.pop();
        } else if excluding {
            exclude.push(selector.to_owned());
        } else {
            include.push(selector.to_owned());
        }
    }
}

fn component_prefix(value: &str, prefix: &str) -> bool {
    value == prefix
        || value
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with("::") || suffix.starts_with(['.', '\\', '/']))
}

/// `packages` are the project package names owning the node's file.
pub(crate) fn selector_matches(selector: &str, node: &GraphNode, packages: &[String]) -> bool {
    let Some((kind, value)) = selector.split_once(':') else {
        return node.id == selector || node.label.contains(selector);
    };
    match kind {
        "glob" => pattern_matches(value, node.file.as_deref().unwrap_or("")),
        "dir" => {
            let value = value.trim_end_matches(['/', '\\']);
            node.file.as_deref().is_some_and(|file| {
                file == value
                    || file
                        .strip_prefix(value)
                        .is_some_and(|suffix| suffix.starts_with(['/', '\\']))
            })
        }
        "file" => node.file.as_deref() == Some(value),
        "class" => node.parent_type.as_deref() == Some(value),
        "ns" | "pkg" | "mod" => {
            node.module
                .as_deref()
                .is_some_and(|module| component_prefix(module, value))
                || component_prefix(&node.label, value)
                || kind == "pkg"
                    && (node
                        .package
                        .as_deref()
                        .is_some_and(|package| component_prefix(package, value))
                        || packages.iter().any(|package| package == value))
        }
        "vendor" => {
            node.vendor
                && (value == "*"
                    || value == "all"
                    || selector_matches(value, node, packages)
                    || node.package.as_deref().is_some_and(|package| {
                        component_prefix(package, value) || pattern_matches(value, package)
                    })
                    || node.file.as_deref().is_some_and(|file| {
                        component_prefix(file, value) || pattern_matches(value, file)
                    }))
        }
        _ => false,
    }
}
