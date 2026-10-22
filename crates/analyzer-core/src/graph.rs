use crate::config::{CaptureArgs, Config};
use crate::ir::*;
use crate::language::LanguageRegistry;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;
mod model;
pub use model::*;
mod symbols;
use symbols::*;
pub(crate) use symbols::{
    expand_relation_selectors, selector_matches, type_import_priority, DeclarationResolver,
};
mod build;
mod cargo;
pub(crate) use cargo::owned_rust_files;
mod inheritance;
mod properties;
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
        build::assemble(files, registry, config, context)
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
