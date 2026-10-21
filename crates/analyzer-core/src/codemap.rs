//! Revision-local dependency index and aggregation for the web code map.
use crate::{
    architecture,
    graph::{GraphNode, NodeKind},
    ir::*,
    AnalysisReport, Config, LanguageRegistry,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub const KINDS: [&str; 5] = ["call", "import", "inheritance", "type", "data"];

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct MapOptions {
    pub compact: bool,
    pub group: String,
    /// JSON encoded array, so paths containing commas remain unambiguous.
    pub expanded: String,
    pub kinds: String,
    pub external: bool,
}
impl MapOptions {
    pub fn validate(&self) -> Result<(), String> {
        if !["", "directory", "file", "module", "set"].contains(&self.group.as_str()) {
            return Err("Unknown grouping".into());
        }
        if !self.expanded.is_empty() {
            serde_json::from_str::<Vec<String>>(&self.expanded)
                .map_err(|_| "expanded must be a JSON array of block IDs")?;
        }
        if self
            .kinds
            .split(',')
            .filter(|s| !s.is_empty())
            .any(|k| k != "none" && !KINDS.contains(&k))
        {
            return Err("Unknown dependency kind".into());
        }
        Ok(())
    }
    fn expanded_ids(&self) -> BTreeSet<String> {
        serde_json::from_str::<Vec<String>>(&self.expanded)
            .unwrap_or_default()
            .into_iter()
            .collect()
    }
    fn accepts(&self, kind: &str) -> bool {
        self.kinds.is_empty() || self.kinds.split(',').any(|k| k == kind)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Entity {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub file: Option<String>,
    pub line: Option<usize>,
    pub language: String,
    pub module: String,
    pub parent_type: Option<String>,
    pub vendor: bool,
    pub package: Option<String>,
    pub status: String,
    #[serde(skip)]
    range: Option<SourceRange>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Witness {
    pub id: usize,
    pub from: String,
    pub to: String,
    pub kind: String,
    pub detail_kind: String,
    pub range: SourceRange,
    pub confidence: Confidence,
    pub guard: Vec<Guard>,
    pub in_loop: Vec<LoopContext>,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Counts {
    pub files: usize,
    pub types: usize,
    pub functions: usize,
    pub entities: usize,
}
#[derive(Clone, Debug, Serialize)]
pub struct Block {
    pub id: String,
    pub label: String,
    pub parent: Option<String>,
    pub kind: String,
    pub status: String,
    pub children: Vec<String>,
    pub counts: Counts,
    pub incoming: usize,
    pub outgoing: usize,
    pub internal: usize,
    pub shared_entities: usize,
}
#[derive(Clone, Debug, Serialize)]
pub struct Link {
    pub from: String,
    pub to: String,
    pub count: usize,
    pub uncertain: usize,
    pub kinds: BTreeMap<String, usize>,
    #[serde(skip)]
    pub witnesses: Vec<usize>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Coverage {
    pub files: usize,
    pub parse_errors: usize,
    pub files_without_body_facts: usize,
    pub deferred_vendor_files: usize,
    pub unresolved_dependencies: usize,
    pub complete: bool,
    pub diagnostics: Vec<String>,
}
#[derive(Serialize)]
pub struct MapView {
    pub blocks: Vec<Block>,
    pub frontier: Vec<String>,
    pub links: Vec<Link>,
    pub kinds: Vec<String>,
    pub coverage: Coverage,
    pub warnings: Vec<String>,
    #[serde(skip)]
    pub members: BTreeMap<String, BTreeSet<String>>,
}

pub struct DependencyIndex {
    pub entities: BTreeMap<String, Entity>,
    pub witnesses: Vec<Witness>,
    coverage: Coverage,
    sets: Config,
    /// Each declaration has a physical identity, independent of Cargo compilation context.
    file_entities: HashMap<String, Vec<String>>,
    /// Project package names per file for `pkg:` selectors.
    source_packages: BTreeMap<String, Vec<String>>,
    views: std::sync::Mutex<std::collections::VecDeque<(String, std::sync::Arc<MapView>)>>,
}
fn entity_id(kind: &str, file: &str, name: &str) -> String {
    format!("{kind}:{}:{file}:{name}", file.len())
}
fn category(kind: &str) -> &str {
    match kind {
        "call" => "call",
        "import" | "module_import" => "import",
        "inheritance" | "implementation" => "inheritance",
        "member_access" | "variable_access" => "data",
        _ => "type",
    }
}
fn contains(outer: &SourceRange, inner: &SourceRange) -> bool {
    outer.file == inner.file
        && (outer.start_line, outer.start_column) <= (inner.start_line, inner.start_column)
        && (outer.end_line, outer.end_column) >= (inner.end_line, inner.end_column)
}
fn declaration_module(file: &FileModel, name: &str, range: &SourceRange) -> String {
    // Inline Rust modules and TypeScript namespaces are explicit declarations.
    // FileModel.module describes the source file, not every declaration in it.
    if let Some(namespace) = file
        .types
        .iter()
        .filter(|ty| ty.kind == TypeKind::Namespace && contains(&ty.range, range))
        .min_by_key(|ty| {
            (
                ty.range.end_line - ty.range.start_line,
                ty.range.end_column.saturating_sub(ty.range.start_column),
            )
        })
    {
        return namespace.fqn.clone();
    }
    if file.language == "php" {
        // PHP can declare several namespace regions in one physical file. Local
        // functions/classes retain the namespace of their outer function.
        let owner = file
            .functions
            .iter()
            .filter(|fun| !fun.is_module_initializer() && contains(&fun.range, range))
            .max_by_key(|fun| {
                (
                    fun.range.end_line - fun.range.start_line,
                    fun.range.end_column.saturating_sub(fun.range.start_column),
                )
            })
            .map(|fun| fun.parent_type.as_deref().unwrap_or(&fun.fqn))
            .unwrap_or(name);
        let owner = owner.split("::").next().unwrap_or(owner);
        return owner
            .rsplit_once('\\')
            .map(|(namespace, _)| namespace.to_owned())
            .unwrap_or_default();
    }
    file.module.clone()
}
impl DependencyIndex {
    pub fn build(report: &AnalysisReport, config: &Config, registry: &LanguageRegistry) -> Self {
        let mut index = Self {
            entities: BTreeMap::new(),
            witnesses: Vec::new(),
            sets: config.clone(),
            file_entities: HashMap::new(),
            source_packages: report.graph.source_packages.clone(),
            views: std::sync::Mutex::new(std::collections::VecDeque::new()),
            coverage: Coverage {
                files: report.files.iter().filter(|f| !f.vendor).count(),
                parse_errors: report.parse_errors.len(),
                files_without_body_facts: report
                    .files
                    .iter()
                    .filter(|f| !f.vendor && f.architecture_facts.is_none())
                    .count(),
                deferred_vendor_files: report.deferred_vendor.len(),
                unresolved_dependencies: 0,
                complete: false,
                diagnostics: report.resolution_diagnostics.clone(),
            },
        };
        let packages = report
            .graph
            .nodes
            .iter()
            .filter_map(|n| n.file.as_ref().zip(n.package.as_ref()))
            .collect::<HashMap<_, _>>();
        for file in &report.files {
            let base = Entity {
                id: entity_id("file", &file.path, ""),
                label: file.path.clone(),
                kind: "file".into(),
                file: Some(file.path.clone()),
                line: Some(1),
                language: file.language.clone(),
                module: file.module.clone(),
                parent_type: None,
                vendor: file.vendor,
                package: packages.get(&file.path).map(|p| (*p).clone()),
                status: if file.vendor { "vendor" } else { "project" }.into(),
                range: None,
            };
            index.insert(base.clone());
            for ty in &file.types {
                if ty.kind == TypeKind::Impl {
                    continue;
                }
                index.insert(Entity {
                    id: entity_id("type", &file.path, &ty.fqn),
                    label: ty.fqn.clone(),
                    kind: "type".into(),
                    line: Some(ty.range.start_line),
                    module: declaration_module(file, &ty.fqn, &ty.range),
                    parent_type: Some(ty.fqn.clone()),
                    range: Some(ty.range.clone()),
                    ..base.clone()
                });
            }
            for fun in &file.functions {
                index.insert(Entity {
                    id: entity_id("function", &file.path, &fun.fqn),
                    label: fun.fqn.clone(),
                    kind: if fun.is_module_initializer() {
                        "initializer"
                    } else {
                        "function"
                    }
                    .into(),
                    line: Some(fun.range.start_line),
                    module: declaration_module(file, &fun.fqn, &fun.range),
                    parent_type: fun.parent_type.clone(),
                    range: Some(fun.range.clone()),
                    ..base.clone()
                });
            }
        }
        // Map every call graph context to its physical declaration or explicit boundary.
        let mut graph_entities = HashMap::new();
        for node in &report.graph.nodes {
            if node.category.as_deref() == Some("runtime") {
                continue;
            }
            let id = if node.kind == NodeKind::Function {
                node.file.as_deref().and_then(|file| {
                    let id = entity_id(
                        "function",
                        file,
                        node.source_fqn.as_deref().unwrap_or(&node.label),
                    );
                    index.entities.contains_key(&id).then_some(id)
                })
            } else {
                None
            };
            let id = id.unwrap_or_else(|| {
                let id = format!("boundary:{}", node.id);
                index.insert(Entity {
                    id: id.clone(),
                    label: node.label.clone(),
                    kind: "boundary".into(),
                    file: None,
                    line: None,
                    language: node.language.clone().unwrap_or_default(),
                    module: String::new(),
                    parent_type: node.parent_type.clone(),
                    vendor: node.vendor,
                    package: node.package.clone(),
                    status: match node.kind {
                        NodeKind::VendorPackage => "package",
                        NodeKind::Unknown => "unresolved",
                        _ => "external",
                    }
                    .into(),
                    range: None,
                });
                id
            });
            if let Some(package) = &node.package {
                if let Some(e) = index.entities.get_mut(&id) {
                    e.package = Some(package.clone());
                }
            }
            graph_entities.insert(node.id.as_str(), id);
        }
        let (dependencies, unresolved) =
            architecture::dependency_facts(&report.files, &report.graph);
        let files = report
            .files
            .iter()
            .map(|file| (file.path.as_str(), file))
            .collect::<HashMap<_, _>>();
        index.coverage.unresolved_dependencies = unresolved;
        for dep in dependencies.iter().filter(|d| d.kind != "call") {
            let (from, to) = if dep.kind == "import" {
                (
                    Some(index.import_source(files[dep.from], dep.range)),
                    Some(entity_id("file", dep.to, "")),
                )
            } else {
                (
                    index.endpoint(dep.from, dep.source_entity, Some(dep.range)),
                    index.endpoint(dep.to, dep.target_entity, None),
                )
            };
            if let (Some(from), Some(to)) = (from, to) {
                index.witnesses.push(Witness {
                    id: 0,
                    from,
                    to,
                    kind: category(dep.kind).into(),
                    detail_kind: dep.kind.into(),
                    range: dep.range.clone(),
                    confidence: Confidence::Resolved,
                    guard: vec![],
                    in_loop: vec![],
                });
            }
        }
        for edge in &report.graph.edges {
            let (Some(from), Some(to)) = (
                graph_entities.get(edge.from.as_ref()),
                graph_entities.get(edge.to.as_ref()),
            ) else {
                continue;
            };
            index.witnesses.push(Witness {
                id: 0,
                from: from.clone(),
                to: to.clone(),
                kind: "call".into(),
                detail_kind: "call".into(),
                range: edge.callsite.clone(),
                confidence: edge.confidence.clone(),
                guard: edge.guard.to_vec(),
                in_loop: edge.in_loop.to_vec(),
            });
        }
        // Package identities come from the resolver, not the import's spelling alone.
        let mut package_aliases = HashMap::<(String, String), (String, bool)>::new();
        for node in &report.graph.nodes {
            let Some(package) = &node.package else {
                continue;
            };
            let language = node.language.as_deref().unwrap_or("");
            let parsed = node.kind == NodeKind::Function && node.vendor;
            let mut aliases = vec![package.as_str()];
            if matches!(language, "javascript" | "typescript") {
                if let Some((_, specifier)) = package.rsplit_once("node_modules/") {
                    aliases.push(specifier);
                }
            }
            for alias in aliases {
                let entry = package_aliases
                    .entry((language.into(), alias.into()))
                    .or_insert((package.clone(), false));
                entry.1 |= parsed;
            }
        }
        // Keep unresolved imports as source-local witnesses, never guess a project target.
        let mut resolved_imports = HashMap::<&str, Vec<&SourceRange>>::new();
        for d in dependencies.iter().filter(|d| d.kind == "import") {
            resolved_imports.entry(d.from).or_default().push(d.range);
        }
        for file in &report.files {
            for import in &file.imports {
                if resolved_imports
                    .get(file.path.as_str())
                    .into_iter()
                    .flatten()
                    .any(|r| contains(r, &import.range) || contains(&import.range, r))
                {
                    continue;
                }
                let name = import.source.as_deref().unwrap_or(&import.path);
                if registry
                    .get(&file.language)
                    .is_some_and(|s| s.is_native_receiver(name) || s.is_native_function(name))
                {
                    continue;
                }
                let specifier = if matches!(file.language.as_str(), "javascript" | "typescript") {
                    let components = name.split('/').collect::<Vec<_>>();
                    if name.starts_with('@') {
                        components
                            .iter()
                            .take(2)
                            .copied()
                            .collect::<Vec<_>>()
                            .join("/")
                    } else {
                        components[0].into()
                    }
                } else {
                    name.into()
                };
                let existing = package_aliases
                    .get(&(file.language.clone(), name.into()))
                    .or_else(|| package_aliases.get(&(file.language.clone(), specifier)));
                let to = if let Some((package, parsed)) = existing {
                    let id = format!("package-import:{}:{package}", file.language);
                    index.insert(Entity {
                        id: id.clone(),
                        label: package.clone(),
                        kind: "boundary".into(),
                        file: None,
                        line: None,
                        language: file.language.clone(),
                        module: String::new(),
                        parent_type: None,
                        vendor: *parsed,
                        package: Some(package.clone()),
                        status: if *parsed { "vendor" } else { "package" }.into(),
                        range: None,
                    });
                    id
                } else {
                    // The same unresolved spelling can name different targets
                    // from different source files (notably ./relative imports).
                    let id = entity_id("import", &file.path, name);
                    index.insert(Entity {
                        id: id.clone(),
                        label: name.into(),
                        kind: "boundary".into(),
                        file: None,
                        line: None,
                        language: file.language.clone(),
                        module: String::new(),
                        parent_type: None,
                        vendor: false,
                        package: None,
                        status: "unresolved".into(),
                        range: None,
                    });
                    id
                };
                let from = index.import_source(file, &import.range);
                index.witnesses.push(Witness {
                    id: 0,
                    from,
                    to,
                    kind: "import".into(),
                    detail_kind: "import".into(),
                    range: import.range.clone(),
                    confidence: if existing.is_some() {
                        Confidence::Resolved
                    } else {
                        Confidence::Dynamic
                    },
                    guard: vec![],
                    in_loop: vec![],
                });
            }
        }
        index.witnesses.sort_by(|a, b| {
            (&a.from, &a.to, &a.detail_kind, &a.range).cmp(&(
                &b.from,
                &b.to,
                &b.detail_kind,
                &b.range,
            ))
        });
        index.witnesses.dedup_by(|a, b| {
            let duplicate = a.from == b.from
                && a.to == b.to
                && a.detail_kind == b.detail_kind
                && a.range == b.range;
            if duplicate {
                // A physical Rust callsite can occur in several Cargo contexts.
                // Keep one witness without promoting its least certain context.
                b.confidence = match (&a.confidence, &b.confidence) {
                    (Confidence::Dynamic, _) | (_, Confidence::Dynamic) => Confidence::Dynamic,
                    (Confidence::Ambiguous, _) | (_, Confidence::Ambiguous) => {
                        Confidence::Ambiguous
                    }
                    _ => Confidence::Resolved,
                };
            }
            duplicate
        });
        for (id, w) in index.witnesses.iter_mut().enumerate() {
            w.id = id;
        }
        index.coverage.complete = index.coverage.parse_errors == 0
            && index.coverage.files_without_body_facts == 0
            && index.coverage.unresolved_dependencies == 0
            && index.coverage.deferred_vendor_files == 0;
        index
    }
    fn insert(&mut self, e: Entity) {
        if self.entities.contains_key(&e.id) {
            return;
        }
        if let Some(file) = &e.file {
            self.file_entities
                .entry(file.clone())
                .or_default()
                .push(e.id.clone());
        }
        self.entities.insert(e.id.clone(), e);
    }
    fn import_source(&mut self, file: &FileModel, range: &SourceRange) -> String {
        let module = file
            .imports
            .iter()
            .find(|import| contains(range, &import.range) || contains(&import.range, range))
            .and_then(|import| import.module.as_deref())
            .or_else(|| {
                file.types
                    .iter()
                    .filter(|ty| ty.kind == TypeKind::Namespace && contains(&ty.range, range))
                    .min_by_key(|ty| {
                        (
                            ty.range.end_line - ty.range.start_line,
                            ty.range.end_column.saturating_sub(ty.range.start_column),
                        )
                    })
                    .map(|ty| ty.fqn.as_str())
            })
            .unwrap_or(&file.module);
        let base_id = entity_id("file", &file.path, "");
        if module == file.module {
            return base_id;
        }
        // Logical namespace regions share one physical file count, while their
        // imports must originate in the region that actually declares them.
        let id = entity_id("file-scope", &file.path, module);
        self.insert(Entity {
            id: id.clone(),
            module: module.into(),
            line: Some(range.start_line),
            ..self.entities[&base_id].clone()
        });
        id
    }
    fn named_entity(&self, file: &str, name: &str) -> Option<String> {
        self.file_entities
            .get(file)?
            .iter()
            .find(|id| self.entities[*id].kind != "file" && self.entities[*id].label == name)
            .cloned()
    }
    fn endpoint(&self, file: &str, name: &str, range: Option<&SourceRange>) -> Option<String> {
        self.named_entity(file, name)
            .or_else(|| {
                range.and_then(|range| {
                    self.file_entities
                        .get(file)?
                        .iter()
                        .filter(|id| {
                            self.entities[*id]
                                .range
                                .as_ref()
                                .is_some_and(|r| contains(r, range))
                        })
                        .min_by_key(|id| {
                            let r = self.entities[*id].range.as_ref().unwrap();
                            (
                                r.end_line - r.start_line,
                                r.end_column.saturating_sub(r.start_column),
                            )
                        })
                        .cloned()
                })
            })
            .or_else(|| {
                self.entities
                    .contains_key(&entity_id("file", file, ""))
                    .then(|| entity_id("file", file, ""))
            })
    }
    fn selector_matches(&self, selector: &str, e: &Entity) -> bool {
        let symbol = format!(
            "fn:{}:{}:{}",
            e.language,
            e.file.as_deref().unwrap_or(""),
            e.label
        );
        if selector == e.id || selector == e.label || selector == symbol {
            return true;
        }
        // File atoms are selected only by whole-file/module selectors, not a function's name.
        if e.kind == "file"
            && !["file:", "dir:", "glob:", "ns:", "mod:", "pkg:", "vendor:"]
                .iter()
                .any(|p| selector.starts_with(p))
        {
            return false;
        }
        let node = GraphNode {
            id: symbol,
            label: e.label.clone(),
            kind: NodeKind::Function,
            file: e.file.clone(),
            line: e.line,
            language: Some(e.language.clone()),
            module: Some(e.module.clone()),
            cargo_target: None,
            cargo_target_id: None,
            source_fqn: None,
            parent_type: e.parent_type.clone(),
            vendor: e.vendor || e.status == "package",
            package: e.package.clone(),
            category: None,
            library: None,
            operation: None,
            argument: None,
            algorithmic_complexity: None,
        };
        let packages = e
            .file
            .as_ref()
            .and_then(|file| self.source_packages.get(file))
            .map_or(&[][..], Vec::as_slice);
        crate::graph::selector_matches(selector, &node, packages)
    }
    pub fn cached_view(&self, options: &MapOptions) -> std::sync::Arc<MapView> {
        let key = format!(
            "{}|{}|{}|{}",
            options.group, options.expanded, options.kinds, options.external
        );
        let mut views = self.views.lock().unwrap();
        if let Some((_, view)) = views.iter().find(|(k, _)| k == &key) {
            return view.clone();
        }
        let view = std::sync::Arc::new(self.view(options));
        if views.len() >= 4 {
            views.pop_front();
        }
        views.push_back((key, view.clone()));
        view
    }
    pub fn view(&self, options: &MapOptions) -> MapView {
        let group = if options.group.is_empty() {
            "directory"
        } else {
            &options.group
        };
        let mut blocks = BTreeMap::<String, Block>::new();
        let mut members = BTreeMap::<String, BTreeSet<String>>::new();
        let mut entity_blocks = BTreeMap::<String, Vec<String>>::new();
        let mut warnings = Vec::new();
        let mut sets = Vec::new();
        if group == "set" {
            for name in self.sets.sets.keys() {
                let (mut include, mut exclude) = (vec![], vec![]);
                crate::graph::expand_relation_selectors(
                    &[format!("set:{name}")],
                    &self.sets,
                    &mut vec![],
                    false,
                    &mut include,
                    &mut exclude,
                    &mut warnings,
                );
                let id = format!("set:{name}");
                add_block(&mut blocks, &id, name, None, "set", "project");
                members.entry(id.clone()).or_default();
                sets.push((id, include, exclude));
            }
        }
        for e in self.entities.values() {
            if !options.external && (e.vendor || e.kind == "boundary") {
                continue;
            }
            let selected_sets = sets
                .iter()
                .filter(|(_, include, exclude)| {
                    include.iter().any(|selector| {
                        (!(e.vendor || e.status == "package") || selector.starts_with("vendor:"))
                            && self.selector_matches(selector, e)
                    }) && !exclude
                        .iter()
                        .any(|selector| self.selector_matches(selector, e))
                })
                .map(|(id, _, _)| id.clone())
                .collect::<Vec<_>>();
            let ids = if !selected_sets.is_empty() {
                selected_sets
            } else if e.vendor || e.kind == "boundary" {
                let label = e.package.as_deref().unwrap_or(&e.label);
                let id = if e.vendor || e.status == "package" {
                    format!("package:{}:{label}", e.language)
                } else {
                    e.id.clone()
                };
                add_block(&mut blocks, &id, label, None, "boundary", &e.status);
                vec![id]
            } else if group == "set" {
                add_block(
                    &mut blocks,
                    "unassigned",
                    "Вне наборов",
                    None,
                    "set",
                    "project",
                );
                vec!["unassigned".into()]
            } else if group == "file" {
                let path = e.file.as_deref().unwrap_or("");
                let id = format!("file:{path}");
                add_block(&mut blocks, &id, path, None, "file", "project");
                vec![id]
            } else {
                let (prefix, parts, separator) = if group == "module" {
                    let separator = match e.language.as_str() {
                        "php" => "\\",
                        "rust" => "::",
                        _ => ".",
                    };
                    (
                        format!("module:{}:", e.language),
                        e.module
                            .split(separator)
                            .filter(|p| !p.is_empty())
                            .map(str::to_owned)
                            .collect::<Vec<_>>(),
                        separator,
                    )
                } else {
                    let file = e.file.as_deref().unwrap_or("");
                    (
                        "dir:".to_string(),
                        file.rsplit_once('/')
                            .map(|(dir, _)| dir.split('/').map(str::to_owned).collect())
                            .unwrap_or_default(),
                        "/",
                    )
                };
                let mut parent = None;
                let mut ids = vec![];
                for depth in 1..=parts.len() {
                    let label = parts[..depth].join(separator);
                    let id = format!("{prefix}{label}");
                    add_block(
                        &mut blocks,
                        &id,
                        &label,
                        parent.as_deref(),
                        if group == "module" {
                            "module"
                        } else {
                            "directory"
                        },
                        "project",
                    );
                    parent = Some(id.clone());
                    ids.push(id);
                }
                let path = e.file.as_deref().unwrap_or("");
                let file_id = if group == "module" {
                    format!(
                        "module-file:{}:{}:{}:{path}",
                        e.language,
                        e.module.len(),
                        e.module
                    )
                } else {
                    format!("file:{path}")
                };
                add_block(
                    &mut blocks,
                    &file_id,
                    path,
                    parent.as_deref(),
                    "file",
                    "project",
                );
                ids.push(file_id);
                ids
            };
            for id in &ids {
                members.entry(id.clone()).or_default().insert(e.id.clone());
            }
            entity_blocks.insert(e.id.clone(), ids);
        }
        let expanded = options.expanded_ids();
        let visible = |ids: &[String]| -> Vec<String> {
            if group == "set" || group == "file" || ids.len() == 1 {
                return ids.to_vec();
            }
            vec![ids
                .iter()
                .find(|id| !expanded.contains(*id))
                .unwrap_or_else(|| ids.last().unwrap())
                .clone()]
        };
        let mut frontier = BTreeSet::new();
        let mut projection = HashMap::new();
        for (e, ids) in &entity_blocks {
            let ids = visible(ids);
            frontier.extend(ids.iter().cloned());
            projection.insert(e.as_str(), ids);
        }
        // Empty sets are useful even when they contain no declarations.
        if group == "set" {
            frontier.extend(sets.iter().map(|(id, _, _)| id.clone()));
        }
        let parents = blocks
            .values()
            .filter_map(|b| b.parent.as_ref().map(|p| (p.clone(), b.id.clone())))
            .collect::<Vec<_>>();
        for (p, c) in parents {
            blocks.get_mut(&p).unwrap().children.push(c);
        }
        for (id, entities) in &members {
            let b = blocks.get_mut(id).unwrap();
            let mut files = BTreeSet::new();
            for id in entities {
                let e = &self.entities[id];
                if let Some(file) = &e.file {
                    files.insert(file);
                }
                if e.kind == "type" {
                    b.counts.types += 1;
                }
                if e.kind == "function" {
                    b.counts.functions += 1;
                }
                if group == "set" && entity_blocks.get(id).is_some_and(|ids| ids.len() > 1) {
                    b.shared_entities += 1;
                }
            }
            b.counts.files = files.len();
            b.counts.entities = entities.len();
        }
        let mut links = BTreeMap::<(String, String), Link>::new();
        for w in self.witnesses.iter().filter(|w| options.accepts(&w.kind)) {
            let (Some(froms), Some(tos)) = (
                projection.get(w.from.as_str()),
                projection.get(w.to.as_str()),
            ) else {
                continue;
            };
            for from in froms {
                for to in tos {
                    // Aliasing the same physical entity into overlapping sets does not create a cross-module dependency.
                    if from != to && w.from == w.to {
                        continue;
                    }
                    let l = links
                        .entry((from.clone(), to.clone()))
                        .or_insert_with(|| Link {
                            from: from.clone(),
                            to: to.clone(),
                            count: 0,
                            uncertain: 0,
                            kinds: BTreeMap::new(),
                            witnesses: vec![],
                        });
                    l.count += 1;
                    l.uncertain += usize::from(w.confidence != Confidence::Resolved);
                    *l.kinds.entry(w.kind.clone()).or_default() += 1;
                    l.witnesses.push(w.id);
                }
            }
        }
        for l in links.values() {
            if l.from == l.to {
                blocks.get_mut(&l.from).unwrap().internal += l.count;
            } else {
                blocks.get_mut(&l.from).unwrap().outgoing += 1;
                blocks.get_mut(&l.to).unwrap().incoming += 1;
            }
        }
        MapView {
            blocks: blocks.into_values().collect(),
            frontier: frontier.into_iter().collect(),
            links: links.into_values().collect(),
            kinds: KINDS.iter().map(|s| s.to_string()).collect(),
            coverage: self.coverage.clone(),
            warnings,
            members,
        }
    }
}
fn add_block(
    blocks: &mut BTreeMap<String, Block>,
    id: &str,
    label: &str,
    parent: Option<&str>,
    kind: &str,
    status: &str,
) {
    blocks.entry(id.into()).or_insert_with(|| Block {
        id: id.into(),
        label: label.into(),
        parent: parent.map(str::to_owned),
        kind: kind.into(),
        status: status.into(),
        children: vec![],
        counts: Counts::default(),
        incoming: 0,
        outgoing: 0,
        internal: 0,
        shared_entities: 0,
    });
}
