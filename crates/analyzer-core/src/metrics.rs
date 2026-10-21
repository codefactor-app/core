use crate::architecture::ArchitectureConfig;
use crate::config::Config;
use crate::graph::CallGraph;
use crate::ir::*;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};

pub fn basic_line_counts(source: &str) -> Loc {
    let mut loc = Loc::default();
    for line in source.lines() {
        loc.total += 1;
        let t = line.trim();
        if t.is_empty() {
            loc.blank += 1;
        } else if t.starts_with("//")
            || t.starts_with('#')
            || t.starts_with("/*")
            || t.starts_with('*')
        {
            loc.comment += 1;
        } else {
            loc.code += 1;
        }
    }
    loc
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MetricValue {
    pub id: Arc<str>,
    pub scope: Arc<str>,
    pub entity: Arc<str>,
    pub path: Arc<str>,
    pub line: usize,
    pub value: f64,
}

/// A pool belongs to one registry/scan, never to process-global state.
#[derive(Default)]
struct MetricStrings(BTreeMap<Arc<str>, Arc<str>>);
impl MetricStrings {
    fn intern(&mut self, text: &str) -> Arc<str> {
        if let Some(shared) = self.0.get(text) {
            return shared.clone();
        }
        let shared: Arc<str> = text.into();
        self.0.insert(shared.clone(), shared.clone());
        shared
    }
}

pub trait Metric: Send + Sync {
    fn id(&self) -> &'static str;
    fn scope(&self) -> &'static str;
    fn calculate(&self, file: &FileModel, config: &Config) -> Vec<MetricValue>;
    fn register_config(&self, _loader: &mut crate::config::ConfigLoader) {}
}

pub fn builtin_metric_ids() -> &'static [&'static str] {
    &[
        "file_lines",
        "functions_per_file",
        "types_per_file",
        "methods_per_class",
        "fields_per_class",
        "function_lines",
        "function_body_lines",
        "function_params",
        "cyclomatic_complexity",
        "nesting_depth",
        "return_points",
        "comment_ratio",
        "inheritance_depth",
        "lcom4",
        "fan_in",
        "fan_out",
        "halstead_volume",
        "maintainability_index",
        "cognitive_complexity",
        "duplicate_functions",
        "module_cycle",
    ]
}

#[derive(Default)]
pub struct MetricRegistry {
    metrics: Vec<Arc<dyn Metric>>,
    strings: Mutex<MetricStrings>,
}
impl Clone for MetricRegistry {
    fn clone(&self) -> Self {
        Self {
            metrics: self.metrics.clone(),
            strings: Mutex::new(MetricStrings::default()),
        }
    }
}
impl MetricRegistry {
    pub fn new() -> Self {
        Self {
            metrics: vec![Arc::new(BuiltinMetric)],
            strings: Mutex::new(MetricStrings::default()),
        }
    }
    pub fn register<M: Metric + 'static>(&mut self, metric: M) {
        self.metrics.push(Arc::new(metric));
    }
    pub fn register_config_keys(&self, loader: &mut crate::config::ConfigLoader) {
        for metric in &self.metrics {
            if metric.id() != "builtin" {
                loader.register_metric(metric.id(), metric.scope());
                metric.register_config(loader);
            }
        }
    }
    pub fn intern_values(&self, values: &mut [MetricValue]) {
        let mut strings = self.strings.lock().unwrap();
        for value in values {
            value.id = strings.intern(&value.id);
            value.scope = strings.intern(&value.scope);
            value.entity = strings.intern(&value.entity);
            value.path = strings.intern(&value.path);
        }
    }
    pub fn calculate(&self, file: &FileModel, config: &Config) -> Vec<MetricValue> {
        if file.vendor {
            return Vec::new();
        }
        let mut result = self
            .metrics
            .iter()
            .flat_map(|m| m.calculate(file, config))
            .collect::<Vec<_>>();
        self.intern_values(&mut result);
        result.sort_by(|a, b| (&a.path, &a.entity, &a.id).cmp(&(&b.path, &b.entity, &b.id)));
        result
    }
}

struct BuiltinMetric;
impl Metric for BuiltinMetric {
    fn id(&self) -> &'static str {
        "builtin"
    }
    fn scope(&self) -> &'static str {
        "file,type,function"
    }
    fn calculate(&self, file: &FileModel, config: &Config) -> Vec<MetricValue> {
        if file.vendor {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(6 * file.functions.len() + 2 * file.types.len() + 3);
        let mut strings = MetricStrings::default();
        let mut add = |id: &str, scope: &str, entity: &str, line: usize, value: usize| {
            out.push(MetricValue {
                id: strings.intern(id),
                scope: strings.intern(scope),
                entity: strings.intern(entity),
                path: strings.intern(&file.path),
                line,
                value: value as f64,
            })
        };
        add("file_lines", "file", &file.path, 1, file.loc.total);
        let funcs = file
            .functions
            .iter()
            .filter(|f| !f.is_module_initializer())
            .filter(|f| config.metrics.count_nested_functions || f.parent_function.is_none())
            .count();
        add("functions_per_file", "file", &file.path, 1, funcs);
        let types = file
            .types
            .iter()
            .filter(|t| t.kind.is_type_declaration())
            .count();
        add("types_per_file", "file", &file.path, 1, types);
        let mut method_counts = BTreeMap::<&str, usize>::new();
        for f in &file.functions {
            if f.is_module_initializer() || f.parent_function.is_some() {
                continue;
            }
            if (config.metrics.count_private_methods || !f.modifiers.iter().any(|m| m == "private"))
                && (config.metrics.count_accessors
                    || (f.accessor.is_none() && !f.modifiers.iter().any(|m| m == "accessor")))
            {
                if let Some(owner) = f.parent_type.as_deref() {
                    *method_counts.entry(owner).or_default() += 1;
                }
            }
        }
        for t in &file.types {
            let n = method_counts.get(t.fqn.as_str()).copied().unwrap_or(0);
            add("methods_per_class", "class", &t.fqn, t.range.start_line, n);
            add(
                "fields_per_class",
                "class",
                &t.fqn,
                t.range.start_line,
                t.fields.len(),
            );
        }
        for f in &file.functions {
            if f.is_module_initializer() {
                continue;
            }
            let lines = f.range.end_line.saturating_sub(f.range.start_line) + 1;
            let body = f
                .body_range
                .end_line
                .saturating_sub(f.body_range.start_line)
                + 1;
            let params = f
                .parameters
                .iter()
                .filter(|p| config.metrics.count_implicit_receiver || !p.implicit_receiver)
                .filter(|p| config.metrics.count_variadic_parameters || !p.variadic)
                .count();
            add(
                "function_lines",
                "function",
                &f.fqn,
                f.range.start_line,
                lines,
            );
            add(
                "function_body_lines",
                "function",
                &f.fqn,
                f.range.start_line,
                body,
            );
            add(
                "function_params",
                "function",
                &f.fqn,
                f.range.start_line,
                params,
            );
            add(
                "cyclomatic_complexity",
                "function",
                &f.fqn,
                f.range.start_line,
                f.complexity.max(1),
            );
            add(
                "nesting_depth",
                "function",
                &f.fqn,
                f.range.start_line,
                f.nesting_depth,
            );
            add(
                "return_points",
                "function",
                &f.fqn,
                f.range.start_line,
                f.return_points,
            );
        }
        out
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Distribution {
    pub avg: f64,
    pub median: f64,
    pub max: f64,
    pub p90: f64,
    pub p95: f64,
    pub histogram: Vec<HistogramBin>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HistogramBin {
    pub start: f64,
    pub end: f64,
    pub count: usize,
}

pub fn distribution(values: &[f64], bins: usize) -> Distribution {
    let mut values = values.to_vec();
    distribution_in_place(&mut values, bins)
}

fn distribution_in_place(v: &mut [f64], bins: usize) -> Distribution {
    if v.is_empty() {
        return Distribution::default();
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let max = v[n - 1];
    let min = v[0];
    let width = if max > min {
        (max - min) / (bins.max(1) as f64)
    } else {
        1.0
    };
    let mut histogram = (0..bins.max(1))
        .map(|i| HistogramBin {
            start: min + width * i as f64,
            end: min + width * (i + 1) as f64,
            count: 0,
        })
        .collect::<Vec<_>>();
    let last_bin = histogram.len() - 1;
    for x in v.iter() {
        let i = if max > min {
            ((x - min) / width).floor() as usize
        } else {
            0
        };
        histogram[i.min(last_bin)].count += 1;
    }
    let percentile = |p: f64| {
        let pos = (n - 1) as f64 * p;
        let lo = pos.floor() as usize;
        let hi = pos.ceil() as usize;
        v[lo] + (v[hi] - v[lo]) * (pos - lo as f64)
    };
    Distribution {
        avg: v.iter().sum::<f64>() / n as f64,
        median: percentile(0.5),
        max,
        p90: percentile(0.9),
        p95: percentile(0.95),
        histogram,
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AggregateGroup {
    pub name: String,
    pub metrics: BTreeMap<String, Distribution>,
}
pub fn aggregates(files: &[FileModel], values: &[MetricValue], bins: usize) -> Vec<AggregateGroup> {
    // Group identities depend on the file, not the individual metric.
    let by_path = files
        .iter()
        .map(|f| {
            let dir = std::path::Path::new(&f.path)
                .parent()
                .map(|p| p.to_string_lossy())
                .unwrap_or_default();
            (
                f.path.as_str(),
                (
                    f.vendor,
                    format!("directory:{dir}"),
                    format!("module:{}", f.module),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut groups = BTreeMap::<&str, BTreeMap<&str, Vec<f64>>>::new();
    for m in values {
        let Some((vendor, directory, module)) = by_path.get(m.path.as_ref()) else {
            continue;
        };
        if *vendor {
            continue;
        }
        for group in ["project", directory.as_str(), module.as_str()] {
            groups
                .entry(group)
                .or_default()
                .entry(m.id.as_ref())
                .or_default()
                .push(m.value);
        }
    }
    groups
        .into_iter()
        .map(|(name, metrics)| AggregateGroup {
            name: name.to_owned(),
            metrics: metrics
                .into_iter()
                .map(|(id, mut values)| (id.to_owned(), distribution_in_place(&mut values, bins)))
                .collect(),
        })
        .collect()
}

/// Source-dependent facts, in the same order as the file's functions. Keeping
/// only volumes and fingerprints lets scan workers release source buffers before
/// the project-wide graph and metrics are constructed.
#[derive(Default)]
pub(crate) struct FileTextFacts {
    functions: Vec<FunctionTextFacts>,
}
struct FunctionTextFacts {
    volume: f64,
    duplicate_hash: Option<[u8; 32]>,
}
impl FileTextFacts {
    pub(crate) fn new(file: &FileModel, source: &str) -> Self {
        if file.vendor || file.functions.is_empty() {
            return Self::default();
        }
        let lines = source.lines().collect::<Vec<_>>();
        let functions = file
            .functions
            .iter()
            .map(|f| {
                if f.is_module_initializer() {
                    return FunctionTextFacts {
                        volume: 0.0,
                        duplicate_hash: None,
                    };
                }
                let body = function_source(source, &lines, &f.range);
                let tokens = tokenize(&body, &file.language);
                let volume = halstead_volume(&tokens);
                let duplicate_body = source_span(source, &lines, &f.body_range);
                let duplicate_tokens = duplicate_body.map(|body| tokenize(body, &file.language));
                let duplicate_tokens = duplicate_tokens.as_deref().unwrap_or(&tokens);
                let duplicate_hash = (duplicate_tokens.len() >= 30).then(|| {
                    use sha2::{Digest, Sha256};
                    let mut hasher = Sha256::new();
                    for (i, token) in duplicate_tokens.iter().enumerate() {
                        if i > 0 {
                            hasher.update(b" ");
                        }
                        hasher.update(token.as_bytes());
                    }
                    hasher.update(b"tokens1");
                    hasher.finalize().into()
                });
                FunctionTextFacts {
                    volume,
                    duplicate_hash,
                }
            })
            .collect();
        Self { functions }
    }
}
fn source_span<'a>(source: &'a str, lines: &[&'a str], range: &SourceRange) -> Option<&'a str> {
    let first = *lines.get(range.start_line.checked_sub(1)?)?;
    let start_column = range.start_column.checked_sub(1)?;
    let end_column = range.end_column.checked_sub(1)?;
    if start_column > first.len() {
        return None;
    }
    let start = first.as_ptr() as usize - source.as_ptr() as usize + start_column;
    let end = if range.end_line == lines.len() + 1 && end_column == 0 && source.ends_with('\n') {
        source.len()
    } else {
        let last = *lines.get(range.end_line.checked_sub(1)?)?;
        if end_column > last.len() {
            return None;
        }
        last.as_ptr() as usize - source.as_ptr() as usize + end_column
    };
    source.get(start..end)
}

pub fn advanced_metrics(
    files: &[FileModel],
    graph: &CallGraph,
    sources: &BTreeMap<String, String>,
    architecture: &ArchitectureConfig,
) -> Vec<MetricValue> {
    advanced_metrics_with_text_facts(files, graph, architecture, |file| {
        FileTextFacts::new(
            file,
            sources.get(&file.path).map(String::as_str).unwrap_or(""),
        )
    })
}

pub(crate) fn advanced_metrics_consuming_text_facts(
    files: &[FileModel],
    graph: &CallGraph,
    architecture: &ArchitectureConfig,
    mut facts: BTreeMap<String, FileTextFacts>,
) -> Vec<MetricValue> {
    advanced_metrics_with_text_facts(files, graph, architecture, |file| {
        facts
            .remove(&file.path)
            .expect("text facts for every parsed file")
    })
}

fn advanced_metrics_with_text_facts(
    files: &[FileModel],
    graph: &CallGraph,
    architecture: &ArchitectureConfig,
    mut facts_for: impl FnMut(&FileModel) -> FileTextFacts,
) -> Vec<MetricValue> {
    let capacity = files
        .iter()
        .filter(|f| !f.vendor)
        .map(|f| 6 * f.functions.len() + 2 * f.types.len() + 2)
        .sum();
    let mut out = Vec::with_capacity(capacity);
    let mut strings = MetricStrings::default();
    let mut incoming = BTreeMap::<&str, usize>::new();
    let mut outgoing = BTreeMap::<&str, usize>::new();
    // LCOM queries each method's graph nodes. Index both identities once instead
    // of scanning the whole project per method; Cargo can have several context
    // nodes for one physical declaration, and public graphs can contain duplicate IDs.
    let mut method_nodes = HashMap::<&str, Vec<_>>::new();
    let mut source_nodes = HashMap::<(&str, &str), Vec<_>>::new();
    for node in &graph.nodes {
        method_nodes.entry(&node.id).or_default().push(node);
        if let Some((file, source)) = node.file.as_deref().zip(node.source_fqn.as_deref()) {
            source_nodes.entry((file, source)).or_default().push(node);
        }
    }
    let canonical = graph
        .nodes
        .iter()
        .filter_map(|node| {
            node.source_fqn.as_ref().map(|source| {
                (
                    node.id.as_str(),
                    format!(
                        "fn:{}:{}:{source}",
                        node.language.as_deref().unwrap_or("rust"),
                        node.file.as_deref().unwrap_or("")
                    ),
                )
            })
        })
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    for edge in &graph.edges {
        let from = canonical
            .get(edge.from.as_ref())
            .map(String::as_str)
            .unwrap_or(edge.from.as_ref());
        let to = canonical
            .get(edge.to.as_ref())
            .map(String::as_str)
            .unwrap_or(edge.to.as_ref());
        if seen.insert((from, to, edge.callsite.clone())) {
            *incoming.entry(to).or_default() += 1;
            *outgoing.entry(from).or_default() += 1;
        }
    }
    let file_refs = files.iter().collect::<Vec<_>>();
    let resolver = crate::graph::DeclarationResolver::new(&file_refs);
    let mut parents = BTreeMap::<&str, BTreeMap<&str, Vec<String>>>::new();
    for file in files {
        for ty in &file.types {
            parents.entry(&file.language).or_default().insert(
                &ty.fqn,
                ty.extends
                    .iter()
                    .map(|base| resolver.type_reference(file, base, &ty.fqn, &ty.range))
                    .collect(),
            );
        }
    }
    drop(resolver);
    let mut inheritance = parents
        .iter()
        .map(|(language, types)| (*language, InheritanceDepths::new(types)))
        .collect::<BTreeMap<_, _>>();
    let mut duplicate = BTreeMap::<[u8; 32], Vec<(&str, &str, usize)>>::new();
    for file in files {
        if file.vendor {
            continue;
        }
        let path = &file.path;
        let ratio = if file.loc.total > 0 {
            file.loc.comment as f64 / file.loc.total as f64
        } else {
            0.0
        };
        out.push(MetricValue {
            id: strings.intern("comment_ratio"),
            scope: strings.intern("file"),
            entity: strings.intern(&path),
            path: strings.intern(&path),
            line: 1,
            value: ratio,
        });
        let mut methods_by_type = BTreeMap::<&str, Vec<&FunctionModel>>::new();
        for function in &file.functions {
            if let Some(owner) = function
                .parent_type
                .as_deref()
                .filter(|_| function.parent_function.is_none())
            {
                methods_by_type.entry(owner).or_default().push(function);
            }
        }
        for t in &file.types {
            let depth = inheritance
                .get_mut(file.language.as_str())
                .unwrap()
                .depth(&t.fqn);
            out.push(MetricValue {
                id: strings.intern("inheritance_depth"),
                scope: strings.intern("class"),
                entity: strings.intern(&t.fqn),
                path: strings.intern(&path),
                line: t.range.start_line,
                value: depth as f64,
            });
            let methods = methods_by_type
                .get(t.fqn.as_str())
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let lcom = lcom4(methods, |method| {
                let id = format!("fn:{}:{}:{}", file.language, file.path, method.fqn);
                method_nodes
                    .get(id.as_str())
                    .into_iter()
                    .flatten()
                    .copied()
                    .chain(
                        source_nodes
                            .get(&(file.path.as_str(), method.fqn.as_str()))
                            .into_iter()
                            .flatten()
                            .copied()
                            .filter(|node| node.id != id),
                    )
                    .flat_map(|node| graph.outgoing_edges(&node.id))
                    .filter(|edge| edge.confidence == Confidence::Resolved)
                    .filter_map(|edge| graph.node(edge.to.as_ref()))
                    .filter(|node| {
                        node.file.as_deref() == Some(file.path.as_str())
                            && node.language.as_deref() == Some(file.language.as_str())
                    })
                    .map(|node| node.source_fqn.as_deref().unwrap_or(&node.label))
                    .collect()
            });
            out.push(MetricValue {
                id: strings.intern("lcom4"),
                scope: strings.intern("class"),
                entity: strings.intern(&t.fqn),
                path: strings.intern(&path),
                line: t.range.start_line,
                value: lcom as f64,
            });
        }
        let facts = facts_for(file);
        debug_assert_eq!(facts.functions.len(), file.functions.len());
        for (f, text) in file.functions.iter().zip(facts.functions) {
            if f.is_module_initializer() {
                continue;
            }
            let id = format!("fn:{}:{}:{}", file.language, path, f.fqn);
            let volume = text.volume;
            let loc = f.range.end_line.saturating_sub(f.range.start_line) + 1;
            let complexity = f.complexity.max(1);
            let maintainability = (171.0
                - 5.2 * (volume.max(1.0)).ln()
                - 0.23 * complexity as f64
                - 16.2 * (loc.max(1) as f64).ln())
            .max(0.0)
                * 100.0
                / 171.0;
            let cognitive = f
                .cognitive_complexity
                .unwrap_or_else(|| complexity.saturating_sub(1)) as f64;
            for (name, value) in [
                ("fan_in", *incoming.get(id.as_str()).unwrap_or(&0) as f64),
                ("fan_out", *outgoing.get(id.as_str()).unwrap_or(&0) as f64),
                ("halstead_volume", volume),
                ("maintainability_index", maintainability),
                ("cognitive_complexity", cognitive),
            ] {
                out.push(MetricValue {
                    id: strings.intern(name),
                    scope: strings.intern("function"),
                    entity: strings.intern(&f.fqn),
                    path: strings.intern(&path),
                    line: f.range.start_line,
                    value,
                });
            }
            if let Some(hash) = text.duplicate_hash {
                duplicate
                    .entry(hash)
                    .or_default()
                    .push((path, &f.fqn, f.range.start_line));
            }
        }
    }
    for group in duplicate.into_values().filter(|g| g.len() > 1) {
        for (path, entity, line) in &group {
            out.push(MetricValue {
                id: strings.intern("duplicate_functions"),
                scope: strings.intern("function"),
                entity: strings.intern(&entity),
                path: strings.intern(&path),
                line: *line,
                value: (group.len() - 1) as f64,
            });
        }
    }
    // Same SCC and file scope as `arch_cycle`: resolved calls plus property,
    // constant and variable access. Files in `architecture.exclude` report 0.
    let cyclic = crate::architecture::cyclic_module_paths(files, graph, architecture);
    for file in files.iter().filter(|f| !f.vendor) {
        let cycle = cyclic.contains(file.path.as_str());
        out.push(MetricValue {
            id: strings.intern("module_cycle"),
            scope: strings.intern("module"),
            entity: strings.intern(&file.module),
            path: strings.intern(&file.path),
            line: 1,
            value: if cycle { 1.0 } else { 0.0 },
        });
    }
    out.sort_by(|a, b| (&a.path, &a.entity, &a.id).cmp(&(&b.path, &b.entity, &b.id)));
    out
}
/// Cache acyclic depths. Values reached through a cycle depend on the active path
/// and are deliberately not memoized, preserving the previous cycle semantics.
struct InheritanceDepths<'a> {
    types: &'a BTreeMap<&'a str, Vec<String>>,
    cache: BTreeMap<&'a str, usize>,
}
impl<'a> InheritanceDepths<'a> {
    fn new(types: &'a BTreeMap<&'a str, Vec<String>>) -> Self {
        Self {
            types,
            cache: BTreeMap::new(),
        }
    }
    fn depth(&mut self, name: &'a str) -> usize {
        struct Frame<'a> {
            name: &'a str,
            parents: &'a [String],
            next: usize,
            depth: usize,
            cyclic: bool,
        }
        if let Some(depth) = self.cache.get(name) {
            return *depth;
        }
        let Some(ty) = self.types.get(name) else {
            return 0;
        };
        if ty.is_empty() {
            self.cache.insert(name, 0);
            return 0;
        }
        let mut stack = vec![Frame {
            name,
            parents: ty,
            next: 0,
            depth: 0,
            cyclic: false,
        }];
        let mut active = std::collections::BTreeSet::from([name]);
        loop {
            let frame = stack.last_mut().unwrap();
            if let Some(parent) = frame.parents.get(frame.next) {
                frame.next += 1;
                if active.contains(parent.as_str()) {
                    frame.cyclic = true;
                    frame.depth = frame.depth.max(1);
                } else if let Some(depth) = self.cache.get(parent.as_str()) {
                    frame.depth = frame.depth.max(1 + depth);
                } else if let Some(ty) = self.types.get(parent.as_str()) {
                    active.insert(parent);
                    stack.push(Frame {
                        name: parent,
                        parents: ty,
                        next: 0,
                        depth: 0,
                        cyclic: false,
                    });
                } else {
                    frame.depth = frame.depth.max(1);
                }
            } else {
                let frame = stack.pop().unwrap();
                active.remove(frame.name);
                if !frame.cyclic {
                    self.cache.insert(frame.name, frame.depth);
                }
                if let Some(parent) = stack.last_mut() {
                    parent.depth = parent.depth.max(1 + frame.depth);
                    parent.cyclic |= frame.cyclic;
                } else {
                    return frame.depth;
                }
            }
        }
    }
}
fn function_source<'a>(source: &'a str, lines: &[&'a str], range: &SourceRange) -> Cow<'a, str> {
    if range.start_line == 0 || range.start_line > lines.len() || range.end_line < range.start_line
    {
        return Cow::Borrowed("");
    }
    let span = if range.start_column > 0 && range.end_column > 0 {
        source_span(source, lines, range).unwrap_or_default()
    } else {
        // Third-party models with no column metadata retain the line fallback.
        let selected = &lines[range.start_line - 1..range.end_line.min(lines.len())];
        let first = selected[0];
        let last = selected[selected.len() - 1];
        let start = first.as_ptr() as usize - source.as_ptr() as usize;
        let end = last.as_ptr() as usize - source.as_ptr() as usize + last.len();
        &source[start..end]
    };
    if span.contains("\r\n") {
        Cow::Owned(span.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(span)
    }
}
fn tokenize<'a>(source: &'a str, language: &str) -> Vec<&'a str> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let slash_comments = language != "python";
        let hash_comment = b == b'#'
            && !matches!(language, "rust" | "javascript" | "typescript")
            && !(language == "php" && bytes.get(i + 1) == Some(&b'['));
        if (slash_comments && b == b'/' && bytes.get(i + 1) == Some(&b'/')) || hash_comment {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1
            }
            continue;
        }
        if slash_comments && b == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i += 2;
            let mut depth = 1;
            while i + 1 < bytes.len() {
                if language == "rust" && bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            if depth > 0 {
                i = bytes.len();
            }
            continue;
        }
        if language == "rust" {
            if let Some(end) = rust_raw_string_end(bytes, i) {
                out.push(&source[i..end]);
                i = end;
                continue;
            }
            if b == b'\'' {
                let mut end = i + 1;
                while end < bytes.len()
                    && (bytes[end].is_ascii_alphanumeric()
                        || bytes[end] == b'_'
                        || bytes[end] >= 128)
                {
                    end += 1;
                }
                // A lifetime or loop label has no closing apostrophe. Treating
                // it as a string would consume the function's remaining code.
                if end > i + 1 && bytes.get(end) != Some(&b'\'') {
                    out.push(&source[i..end]);
                    i = end;
                    continue;
                }
            }
        }
        if b == b'"' || b == b'\'' || b == b'`' {
            let quote = b;
            let start = i;
            let width = if language == "python"
                && bytes.get(i..i + 3) == Some([quote, quote, quote].as_slice())
            {
                3
            } else {
                1
            };
            i += width;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i = (i + 2).min(bytes.len());
                    continue;
                }
                if bytes
                    .get(i..i + width)
                    .is_some_and(|end| end.iter().all(|b| *b == quote))
                {
                    i += width;
                    break;
                }
                i += 1
            }
            out.push(&source[start..i]);
            continue;
        }
        let start = i;
        if b.is_ascii_alphabetic() || b == b'_' || b >= 128 {
            i += 1;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] >= 128)
            {
                i += 1
            }
        } else if b.is_ascii_digit() {
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1
            }
        } else {
            i += 1;
            if i < bytes.len()
                && matches!(
                    (b, bytes[i]),
                    (b'=', b'=')
                        | (b'!', b'=')
                        | (b'<', b'=')
                        | (b'>', b'=')
                        | (b'&', b'&')
                        | (b'|', b'|')
                        | (b':', b':')
                        | (b'-', b'>')
                        | (b'=', b'>')
                        | (b'?', b'?')
                        | (b'?', b'.')
                        | (b'/', b'/')
                )
            {
                i += 1
            }
        }
        out.push(&source[start..i]);
    }
    out
}
fn rust_raw_string_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    if matches!(bytes.get(i), Some(b'b' | b'c')) {
        i += 1;
    }
    if bytes.get(i) != Some(&b'r') {
        return None;
    }
    i += 1;
    let hashes = i;
    while bytes.get(i) == Some(&b'#') {
        i += 1;
    }
    let hash_count = i - hashes;
    if bytes.get(i) != Some(&b'"') {
        return None;
    }
    i += 1;
    while i < bytes.len() {
        if bytes[i] == b'"'
            && bytes
                .get(i + 1..i + 1 + hash_count)
                .is_some_and(|tail| tail.iter().all(|b| *b == b'#'))
        {
            return Some(i + 1 + hash_count);
        }
        i += 1;
    }
    Some(bytes.len())
}
fn halstead_volume(tokens: &[&str]) -> f64 {
    if tokens.is_empty() {
        return 0.0;
    }
    let unique = tokens.iter().copied().collect::<HashSet<_>>().len();
    tokens.len() as f64 * (unique.max(1) as f64).log2()
}
fn lcom4<'a>(
    methods: &[&'a FunctionModel],
    mut targets: impl FnMut(&'a FunctionModel) -> Vec<&'a str>,
) -> usize {
    let mut parents: Vec<usize> = (0..methods.len()).collect();
    let mut ranks = vec![0u8; methods.len()];
    let mut components = methods.len();
    fn root(parents: &mut [usize], mut i: usize) -> usize {
        while parents[i] != i {
            parents[i] = parents[parents[i]];
            i = parents[i];
        }
        i
    }
    let mut union = |a, b| {
        let mut a = root(&mut parents, a);
        let mut b = root(&mut parents, b);
        if a == b {
            return;
        }
        if ranks[a] < ranks[b] {
            std::mem::swap(&mut a, &mut b);
        }
        parents[b] = a;
        if ranks[a] == ranks[b] {
            ranks[a] += 1;
        }
        components -= 1;
    };
    let mut fields = BTreeMap::<&str, usize>::new();
    let mut identities = BTreeMap::<&str, (Vec<usize>, bool)>::new();
    for (i, method) in methods.iter().enumerate() {
        identities.entry(&method.fqn).or_default().0.push(i);
        for field in &method.accessed_fields {
            if let Some(&other) = fields.get(field.as_str()) {
                union(i, other);
            } else {
                fields.insert(field, i);
            }
        }
    }
    // Connect each referenced overload group once; repeated calls do not scan it again.
    let mut connect = |caller, target: &str, table: &mut BTreeMap<&str, (Vec<usize>, bool)>| {
        if let Some((targets, connected)) = table.get_mut(target) {
            let first = targets[0];
            if !*connected {
                for &other in &targets[1..] {
                    union(first, other);
                }
                *connected = true;
            }
            union(caller, first);
        }
    };
    for (i, method) in methods.iter().enumerate() {
        for target in targets(method) {
            connect(i, target, &mut identities);
        }
    }
    components
}

#[cfg(test)]
fn lcom4_from_declared_targets(methods: &[&FunctionModel]) -> usize {
    lcom4(methods, |method| {
        method
            .calls
            .iter()
            .flat_map(|call| call.targets.iter().map(String::as_str))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prepared_text_facts_preserve_duplicates_ranges_and_file_order() {
        use sha2::{Digest, Sha256};
        let source = (0..40)
            .map(|i| format!("token_{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let source = format!("{source}\r\nshort\r\n{source}\r\n");
        let mut file = FileModel {
            path: "b.rs".into(),
            language: "rust".into(),
            module: "b".into(),
            ..Default::default()
        };
        for (start, end) in [(1, 1), (2, 2), (3, 3), (0, 2), (4, 9), (1, 3)] {
            file.functions.push(FunctionModel {
                fqn: "repeated_name".into(),
                complexity: start + 1,
                range: SourceRange {
                    start_line: start,
                    end_line: end,
                    ..Default::default()
                },
                ..Default::default()
            });
        }
        let facts = FileTextFacts::new(&file, &source);
        let lines: Vec<_> = source.lines().collect();
        for (f, text) in file.functions.iter().zip(&facts.functions) {
            let range = &f.range;
            let body = if range.start_line > 0
                && range.start_line <= lines.len()
                && range.end_line >= range.start_line
            {
                lines[range.start_line - 1..range.end_line.min(lines.len())].join("\n")
            } else {
                String::new()
            };
            let tokens = tokenize(&body, "rust");
            let unique = tokens
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len();
            let volume = tokens.len() as f64 * (unique.max(1) as f64).log2();
            assert_eq!(text.volume.to_bits(), volume.to_bits());
            let hash = (tokens.len() >= 30)
                .then(|| Sha256::digest(format!("{}tokens1", tokens.join(" ")).as_bytes()).into());
            assert_eq!(text.duplicate_hash, hash);
        }
        assert_eq!(
            facts.functions[0].duplicate_hash,
            facts.functions[2].duplicate_hash
        );
        assert!(facts.functions[1].duplicate_hash.is_none());
        let mut other = file.clone();
        other.path = "a.rs".into();
        let mut vendor = file.clone();
        vendor.path = "vendor.rs".into();
        vendor.vendor = true;
        let mut files = vec![file, vendor, other];
        let sources = files
            .iter()
            .map(|file| (file.path.clone(), source.clone()))
            .collect();
        let facts = files
            .iter()
            .map(|file| (file.path.clone(), FileTextFacts::new(file, &source)))
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let graph = CallGraph::default();
        let expected = advanced_metrics(&files, &graph, &sources, &Default::default());
        let actual =
            advanced_metrics_consuming_text_facts(&files, &graph, &Default::default(), facts);
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        assert_eq!(
            expected
                .iter()
                .filter(|v| v.id.as_ref() == "duplicate_functions")
                .count(),
            6
        );
    }

    #[test]
    fn indexed_lcom_matches_pairwise_connectivity_including_overloads() {
        for size in 0..64usize {
            let methods = (0..size)
                .map(|i| FunctionModel {
                    name: format!("method_{}", i % 7),
                    fqn: format!("Class::method_{i}"),
                    accessed_fields: if i % 3 == 0 {
                        vec![format!("field_{}", i % 5)]
                    } else {
                        Vec::new()
                    },
                    calls: if i % 4 == 0 {
                        vec![CallSite {
                            name: format!("method_{}", (i + 2) % 7),
                            targets: vec![format!("Class::method_{}", (i + 3) % size.max(1))],
                            ..CallSite::default()
                        }]
                    } else {
                        Vec::new()
                    },
                    ..FunctionModel::default()
                })
                .collect::<Vec<_>>();
            let methods = methods.iter().collect::<Vec<_>>();
            let invokes = |a: &FunctionModel, b: &FunctionModel| {
                a.calls.iter().any(|c| c.targets.contains(&b.fqn))
            };
            let mut seen = vec![false; size];
            let mut expected = 0;
            for i in 0..size {
                if seen[i] {
                    continue;
                }
                expected += 1;
                let mut pending = vec![i];
                seen[i] = true;
                while let Some(a) = pending.pop() {
                    for b in 0..size {
                        if !seen[b]
                            && (methods[a]
                                .accessed_fields
                                .iter()
                                .any(|f| methods[b].accessed_fields.contains(f))
                                || invokes(methods[a], methods[b])
                                || invokes(methods[b], methods[a]))
                        {
                            seen[b] = true;
                            pending.push(b);
                        }
                    }
                }
            }
            assert_eq!(lcom4_from_declared_targets(&methods), expected);
        }
        let overloaded = (0..10_000)
            .map(|_| FunctionModel {
                name: "overloaded".into(),
                fqn: "Class::overloaded".into(),
                calls: vec![CallSite {
                    name: "overloaded".into(),
                    targets: vec!["Class::overloaded".into()],
                    ..CallSite::default()
                }],
                ..FunctionModel::default()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            lcom4_from_declared_targets(&overloaded.iter().collect::<Vec<_>>()),
            1
        );
    }
    #[test]
    fn metric_pool_shares_rows_without_changing_json_aggregates_or_rules() {
        let mut file = FileModel {
            path: "long/module/source.ts".into(),
            language: "typescript".into(),
            module: "long.module.source".into(),
            ..Default::default()
        };
        for index in 0..100 {
            file.functions.push(FunctionModel {
                fqn: format!("long.module.source.function_{index}"),
                range: SourceRange {
                    file: file.path.clone().into(),
                    start_line: 1,
                    end_line: 5,
                    ..Default::default()
                },
                ..Default::default()
            });
        }
        let mut config = Config::default();
        config.limits.global.insert(
            "function_lines".into(),
            crate::config::Threshold {
                warn: Some(3.0),
                error: Some(8.0),
            },
        );
        let registry = MetricRegistry::new();
        let values = registry.calculate(&file, &config);
        assert!(values
            .iter()
            .all(|value| Arc::ptr_eq(&value.path, &values[0].path)));
        let function_values = values
            .iter()
            .filter(|value| value.entity.as_ref() == "long.module.source.function_0")
            .collect::<Vec<_>>();
        assert_eq!(function_values.len(), 6);
        assert!(function_values
            .iter()
            .all(|value| Arc::ptr_eq(&value.entity, &function_values[0].entity)));
        let serialized = serde_json::to_string(&values).unwrap();
        let mut decoded: Vec<MetricValue> = serde_json::from_str(&serialized).unwrap();
        registry.intern_values(&mut decoded);
        assert_eq!(serde_json::to_string(&decoded).unwrap(), serialized);
        assert!(decoded
            .iter()
            .all(|value| Arc::ptr_eq(&value.path, &decoded[0].path)));
        let files = [file];
        let aggregated = aggregates(&files, &decoded, 10);
        let project = aggregated
            .iter()
            .find(|group| group.name == "project")
            .unwrap();
        assert_eq!(project.metrics["function_lines"].median, 5.0);
        let violations = crate::rules::evaluate(
            &decoded,
            &files,
            &aggregated,
            &config,
            &Default::default(),
            &Default::default(),
        );
        assert_eq!(
            violations
                .iter()
                .filter(|value| value.rule == "function_lines")
                .count(),
            100
        );
    }

    #[test]
    fn lcom4_uses_field_access_and_method_calls() {
        let a = FunctionModel {
            name: "a".into(),
            fqn: "Class::a".into(),
            accessed_fields: vec!["item".into()],
            ..Default::default()
        };
        let b = FunctionModel {
            name: "b".into(),
            fqn: "Class::b".into(),
            accessed_fields: vec!["item".into()],
            ..Default::default()
        };
        let c = FunctionModel {
            name: "c".into(),
            fqn: "Class::c".into(),
            ..Default::default()
        };
        assert_eq!(lcom4_from_declared_targets(&[&a, &b, &c]), 2);
        let mut b = b;
        b.calls.push(CallSite {
            name: "c".into(),
            targets: vec!["Class::c".into()],
            ..Default::default()
        });
        assert_eq!(lcom4_from_declared_targets(&[&a, &b, &c]), 1);
    }

    #[test]
    fn lcom_graph_lookup_preserves_contexts_duplicates_and_target_filters() {
        use crate::graph::GraphEdge;
        for language in ["rust", "php", "python", "javascript", "typescript"] {
            let file = FileModel {
                path: "source".into(),
                language: language.into(),
                types: vec![TypeModel {
                    fqn: "Class".into(),
                    ..Default::default()
                }],
                functions: ["a", "b", "c", "d"]
                    .into_iter()
                    .map(|name| FunctionModel {
                        name: name.into(),
                        fqn: format!("Class::{name}"),
                        parent_type: Some("Class".into()),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            };
            let direct = format!("fn:{language}:source:Class::a");
            let node = |id: &str, name: &str, path: &str, lang: &str| {
                serde_json::from_value(serde_json::json!({
                    "id": id, "label": name, "kind": "function", "vendor": false,
                    "source_fqn": name, "file": path, "language": lang
                }))
                .unwrap()
            };
            let edge = |from: &str, to: &str, confidence: Confidence| GraphEdge {
                from: from.into(),
                to: to.into(),
                callsite: SourceRange::default(),
                confidence,
                guard: Default::default(),
                in_loop: Default::default(),
                recursive: false,
                hidden_vendor: false,
                argument: None,
            };
            let mut graph = CallGraph::default();
            graph.nodes = vec![
                node(&direct, "Class::a", "source", language),
                node(&direct, "duplicate", "other", language),
                node("b-lib", "Class::b", "source", language),
                node("b-bin", "Class::b", "source", language),
                node("c", "Class::c", "source", language),
                node("d", "Class::d", "source", language),
                node("b-other", "Class::b", "other", language),
                node("d-other", "Class::d", "other", language),
                node("d-language", "Class::d", "source", "other-language"),
            ];
            graph.edges = vec![
                edge(&direct, "b-lib", Confidence::Resolved),
                edge("b-bin", "c", Confidence::Resolved),
                edge("b-lib", "d", Confidence::Ambiguous),
                edge("b-lib", "d-other", Confidence::Resolved),
                edge("b-lib", "d-language", Confidence::Resolved),
                edge("b-other", "d", Confidence::Resolved),
            ];
            for reverse in [false, true] {
                if reverse {
                    graph.nodes.reverse();
                }
                graph.reindex();
                let methods = file.functions.iter().collect::<Vec<_>>();
                // Reference the original whole-graph selection, independently of the new index.
                let expected = lcom4(&methods, |method| {
                    let id = format!("fn:{language}:{}:{}", file.path, method.fqn);
                    graph
                        .nodes
                        .iter()
                        .filter(|node| {
                            node.id == id
                                || (node.source_fqn.as_deref() == Some(&method.fqn)
                                    && node.file.as_deref() == Some(&file.path))
                        })
                        .flat_map(|node| graph.outgoing_edges(&node.id))
                        .filter(|edge| edge.confidence == Confidence::Resolved)
                        .filter_map(|edge| graph.node(&edge.to))
                        .filter(|node| {
                            node.file.as_deref() == Some(&file.path)
                                && node.language.as_deref() == Some(language)
                        })
                        .map(|node| node.source_fqn.as_deref().unwrap_or(&node.label))
                        .collect()
                });
                assert_eq!(expected, 2);
                let metrics = advanced_metrics(
                    std::slice::from_ref(&file),
                    &graph,
                    &BTreeMap::new(),
                    &Default::default(),
                );
                let actual = metrics.iter().find(|m| m.id.as_ref() == "lcom4").unwrap();
                assert_eq!(
                    actual.value, expected as f64,
                    "{language}, reverse={reverse}"
                );
            }
        }
    }
    #[test]
    fn module_metrics_accept_missing_stale_and_duplicate_node_indexes() {
        use crate::graph::GraphEdge;
        let make_node = |id: &str, module: &str| {
            serde_json::from_value(serde_json::json!({"id":id,"label":id,"kind":"function","module":module,"vendor":false})).unwrap()
        };
        let edge = |from: &str, to: &str| GraphEdge {
            from: from.into(),
            to: to.into(),
            callsite: SourceRange::default(),
            confidence: Confidence::Resolved,
            guard: Default::default(),
            in_loop: Default::default(),
            recursive: false,
            hidden_vendor: false,
            argument: None,
        };
        let files: Vec<_> = ["A", "B", "C"]
            .into_iter()
            .map(|module| FileModel {
                path: module.into(),
                module: module.into(),
                ..Default::default()
            })
            .collect();
        let mut graph = CallGraph::default();
        graph.nodes = vec![make_node("a", "A"), make_node("b", "B")];
        graph.edges = vec![edge("a", "b"), edge("b", "a")];
        let compare = |graph: &CallGraph| {
            let actual = advanced_metrics(&files, graph, &BTreeMap::new(), &Default::default());
            let mut reindexed = graph.clone();
            reindexed.reindex();
            let expected =
                advanced_metrics(&files, &reindexed, &BTreeMap::new(), &Default::default());
            assert_eq!(
                serde_json::to_value(actual).unwrap(),
                serde_json::to_value(&expected).unwrap()
            );
        };
        compare(&graph);
        graph.reindex();
        compare(&graph);
        graph.nodes.push(make_node("a", "C"));
        compare(&graph);
        graph.reindex();
        compare(&graph);
        graph.nodes[0].id = "renamed".into();
        compare(&graph);
        graph.nodes.clear();
        compare(&graph);
    }
    #[test]
    fn borrowed_function_source_matches_line_join_for_all_line_boundaries() {
        for source in [
            "",
            "\n",
            "\n\n",
            "one",
            "one\n",
            "one\r\n",
            "\r\n\r\n",
            "a\nb\n",
            "a\r\nb\r\n",
            "a\r\nb\nc\r\nd",
            "a\rb\n",
            "\r\r\n",
            "тип\n\nполе\r\nконец\r",
            " \n\t\nlast",
        ] {
            let lines: Vec<_> = source.lines().collect();
            for start in 0..=lines.len() + 2 {
                for end in 0..=lines.len() + 2 {
                    let range = SourceRange {
                        start_line: start,
                        end_line: end,
                        ..Default::default()
                    };
                    let expected = if start > 0 && start <= lines.len() && end >= start {
                        lines[start - 1..end.min(lines.len())].join("\n")
                    } else {
                        String::new()
                    };
                    assert_eq!(
                        function_source(source, &lines, &range),
                        expected,
                        "{source:?}: {start}..{end}"
                    );
                }
            }
        }
        let source = "first\nsecond\n";
        let lines: Vec<_> = source.lines().collect();
        assert!(matches!(
            function_source(
                source,
                &lines,
                &SourceRange {
                    start_line: 1,
                    end_line: 2,
                    ..Default::default()
                }
            ),
            Cow::Borrowed(_)
        ));
    }
    #[test]
    fn unordered_halstead_vocabulary_preserves_exact_volume() {
        for source in [
            "",
            "x x x",
            "let x=1;let y=1;",
            "тип::поле += 12; /*ignored*/ тип::поле",
            "print(\"a b\"); print(\"a b\");",
            "// comment\nfn f(){return;}\n",
        ] {
            let tokens = tokenize(source, "rust");
            let unique = tokens
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len();
            let expected = if tokens.is_empty() {
                0.0
            } else {
                tokens.len() as f64 * (unique.max(1) as f64).log2()
            };
            assert_eq!(
                halstead_volume(&tokens).to_bits(),
                expected.to_bits(),
                "{source}"
            );
        }
    }
    #[test]
    fn halstead_tokens_exclude_comments_and_group_literals() {
        let code = "let x = \"a b\"; // ignored words\n/* also ignored */ return x;";
        let tokens = tokenize(code, "javascript");
        assert!(tokens.contains(&"\"a b\""));
        assert!(!tokens.contains(&"ignored"));
        assert_eq!(tokens.iter().filter(|t| **t == "\"a b\"").count(), 1);
    }

    #[test]
    fn tokens_follow_each_languages_comment_and_literal_syntax() {
        assert_eq!(
            tokenize("x // 2 + y # comment", "python"),
            ["x", "//", "2", "+", "y"]
        );
        assert_eq!(
            tokenize("this.#field + value // comment", "javascript"),
            ["this", ".", "#", "field", "+", "value"]
        );
        assert_eq!(
            tokenize("#[Flag] function f() {} # comment", "php"),
            ["#", "[", "Flag", "]", "function", "f", "(", ")", "{", "}"]
        );
        assert_eq!(
            tokenize("&'static str + x; 'label: loop {} 'x'", "rust"),
            ["&", "'static", "str", "+", "x", ";", "'label", ":", "loop", "{", "}", "'x'"]
        );
        assert_eq!(
            tokenize("x /* outer /* inner */ ignored */ + y", "rust"),
            ["x", "+", "y"]
        );
        assert_eq!(
            tokenize("r###\"text \" # // literal\"### + x", "rust"),
            ["r###\"text \" # // literal\"###", "+", "x"]
        );
        assert_eq!(
            tokenize("'''text ' # // literal''' + x", "python"),
            ["'''text ' # // literal'''", "+", "x"]
        );
        assert_eq!(
            tokenize("x # comment\n+ y // comment", "custom_language"),
            ["x", "+", "y"]
        );
    }
}

#[cfg(test)]
mod inheritance_tests {
    use super::*;
    fn original(name: &str, types: &BTreeMap<&str, &TypeModel>, active: &mut Vec<String>) -> usize {
        if active.iter().any(|n| n == name) {
            return 0;
        }
        let Some(ty) = types.get(name) else {
            return 0;
        };
        active.push(name.to_owned());
        let depth = ty
            .extends
            .iter()
            .map(|p| 1 + original(p, types, active))
            .max()
            .unwrap_or(0);
        active.pop();
        depth
    }

    #[test]
    fn memoized_depth_matches_path_dependent_cycles_and_missing_bases() {
        for seed in 0..80usize {
            let types = (0..6)
                .map(|i| TypeModel {
                    fqn: format!("T{i}"),
                    extends: (0..7)
                        .filter(|j| (seed.wrapping_mul(37) ^ (i * 17 + j * 11)).is_multiple_of(5))
                        .map(|j| format!("T{j}"))
                        .collect(),
                    ..Default::default()
                })
                .collect::<Vec<_>>();
            let index = types
                .iter()
                .map(|t| (t.fqn.as_str(), t))
                .collect::<BTreeMap<_, _>>();
            let parents = index
                .iter()
                .map(|(name, ty)| (*name, ty.extends.clone()))
                .collect();
            let mut depths = InheritanceDepths::new(&parents);
            for ty in types.iter().chain(types.iter().rev()) {
                assert_eq!(
                    depths.depth(&ty.fqn),
                    original(&ty.fqn, &index, &mut Vec::new()),
                    "seed={seed}, type={}",
                    ty.fqn
                );
            }
            assert_eq!(depths.depth("absent"), 0);
        }
    }

    #[test]
    fn deep_shared_inheritance_uses_bounded_stack_and_one_cache_entry_per_type() {
        let types = (0usize..10_000)
            .map(|i| TypeModel {
                fqn: format!("T{i}"),
                extends: [i.checked_sub(1), i.checked_sub(2)]
                    .into_iter()
                    .flatten()
                    .map(|j| format!("T{j}"))
                    .collect(),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        let index = types
            .iter()
            .map(|t| (t.fqn.as_str(), t))
            .collect::<BTreeMap<_, _>>();
        let parents = index
            .iter()
            .map(|(name, ty)| (*name, ty.extends.clone()))
            .collect();
        let mut depths = InheritanceDepths::new(&parents);
        assert_eq!(depths.depth(&types[9999].fqn), 9999);
        assert_eq!(depths.cache.len(), 10_000);
        for (i, ty) in types.iter().enumerate() {
            assert_eq!(depths.depth(&ty.fqn), i);
        }
    }
}

#[cfg(test)]
mod aggregate_tests {
    use super::*;
    fn original(files: &[FileModel], values: &[MetricValue], bins: usize) -> Vec<AggregateGroup> {
        let mut groups = BTreeMap::<String, BTreeMap<String, Vec<f64>>>::new();
        let by_path = files
            .iter()
            .map(|f| (f.path.as_str(), f))
            .collect::<BTreeMap<_, _>>();
        for m in values {
            let Some(file) = by_path.get(m.path.as_ref()).filter(|f| !f.vendor) else {
                continue;
            };
            let dir = std::path::Path::new(m.path.as_ref())
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            for name in [
                "project".to_owned(),
                format!("directory:{dir}"),
                format!("module:{}", file.module),
            ] {
                groups
                    .entry(name)
                    .or_default()
                    .entry(m.id.to_string())
                    .or_default()
                    .push(m.value);
            }
        }
        groups
            .into_iter()
            .map(|(name, metrics)| AggregateGroup {
                name,
                metrics: metrics
                    .into_iter()
                    .map(|(id, v)| (id, distribution(&v, bins)))
                    .collect(),
            })
            .collect()
    }
    #[test]
    fn borrowed_groups_and_in_place_statistics_preserve_order_and_float_results() {
        let files = ["a.rs", "src/b.rs", "src/c.rs", "vendor/d.rs"]
            .into_iter()
            .enumerate()
            .map(|(i, path)| FileModel {
                path: path.into(),
                module: if i < 2 { "shared" } else { "other" }.into(),
                vendor: i == 3,
                ..Default::default()
            })
            .collect::<Vec<_>>();
        let values = (0..150)
            .map(|i| MetricValue {
                id: if i % 2 == 0 { "first" } else { "second" }.into(),
                scope: "function".into(),
                entity: format!("f{i}").into(),
                path: ["a.rs", "src/b.rs", "src/c.rs", "vendor/d.rs", "absent.rs"][i % 5].into(),
                line: i + 1,
                value: if i % 3 == 0 {
                    -0.0
                } else {
                    (i as f64 - 70.0) * 0.13
                },
            })
            .collect::<Vec<_>>();
        for bins in [0, 1, 7, 20] {
            assert_eq!(
                serde_json::to_value(aggregates(&files, &values, bins)).unwrap(),
                serde_json::to_value(original(&files, &values, bins)).unwrap()
            );
        }
    }
}
