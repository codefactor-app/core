//! Declaration, import and native-symbol indexes shared by graph stages.
use super::model::*;
use crate::{config::*, ir::*, language::LanguageRegistry};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

/// Exact declarations and all Rust `::` prefixes, borrowing file metadata.
pub(super) struct ModuleIndex<'a> {
    pub(super) exact: BTreeSet<(&'a str, &'a str)>,
    pub(super) rust_prefixes: HashSet<&'a str>,
    pub(super) rust_aliases: BTreeMap<String, BTreeSet<String>>,
    pub(super) rust_contexts: BTreeMap<String, BTreeSet<String>>,
    pub(super) rust_roots: BTreeMap<String, BTreeSet<String>>,
    pub(super) rust_root_files: BTreeMap<String, String>,
    pub(super) rust_sources: BTreeMap<String, BTreeSet<String>>,
    pub(super) rust_owners: BTreeMap<String, BTreeSet<String>>,
    pub(super) all_rust_roots: bool,
    pub(super) esm_files: BTreeMap<String, Vec<String>>,
    pub(super) resolution: Option<&'a crate::resolution::ResolutionContext>,
    pub(super) rust_config: crate::config::RustResolutionConfig,
    pub(super) node_conditions: Vec<String>,
    pub(super) strategies: LanguageRegistry,
}
impl<'a> ModuleIndex<'a> {
    pub(super) fn new(exact: BTreeSet<(&'a str, &'a str)>) -> Self {
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
    pub(super) fn with_registry(mut self, registry: &LanguageRegistry) -> Self {
        self.strategies = registry.clone();
        self
    }
    pub(super) fn type_reference(&self, file: &FileModel, raw: &str) -> Option<String> {
        self.strategies.get(&file.language).map_or_else(
            || crate::types::normalize_reference(raw, &file.type_reference_syntax),
            |strategy| strategy.type_reference(raw),
        )
    }
    pub(super) fn native_receiver(&self, language: &str, raw: &str) -> bool {
        self.strategies
            .get(language)
            .is_some_and(|strategy| strategy.is_native_receiver(raw))
    }
    pub(super) fn namespace_separator(&self, language: &str) -> &str {
        self.strategies.get(language).map_or_else(
            || match language {
                "php" => "\\",
                "rust" => "::",
                _ => ".",
            },
            |strategy| strategy.namespace_separator(),
        )
    }
    pub(super) fn method_separator(&self, language: &str) -> &str {
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
    pub(super) fn with_files(
        self,
        files: &[FileModel],
        resolution: &'a crate::resolution::ResolutionContext,
        config: &Config,
    ) -> Self {
        self.with_file_refs(&files.iter().collect::<Vec<_>>(), Some(resolution), config)
    }
    pub(super) fn with_file_refs(
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
                    context_module(&file.module, target)
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
    pub(super) fn rust_canonical(&self, symbol: &str) -> Option<String> {
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
    pub(super) fn esm_resource_symbol(
        &self,
        file: &FileModel,
        resource: &str,
        suffix: &str,
    ) -> Option<String> {
        self.esm_resource_symbol_kind(file, resource, suffix, false)
    }
    pub(super) fn esm_resource_symbol_kind(
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
    pub(super) fn contains(&self, symbol: &(&str, &str)) -> bool {
        self.exact.contains(symbol)
    }
    #[cfg(test)]
    pub(super) fn rust_exists(&self, symbol: &str) -> bool {
        self.rust_canonical(symbol).is_some()
    }
}

pub(super) fn normalize_rust_file_path(path: &std::path::Path) -> String {
    crate::resolution::normalize(path)
}

/// Shared declaration-level resolver for call and architecture analysis.
/// Import.module is lexical context, not the imported destination.
pub(crate) struct DeclarationResolver<'a> {
    pub(super) modules: ModuleIndex<'a>,
    pub(super) exports: BTreeMap<&'a str, BTreeMap<String, String>>,
    pub(super) runtime_exports: BTreeMap<&'a str, BTreeMap<String, String>>,
    pub(super) types: KnownTypes,
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
    pub(super) fn symbol_source(&self, symbol: &str) -> Option<&str> {
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
pub(super) fn declaration_symbol(modules: &ModuleIndex<'_>, symbol: &str) -> String {
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
// Resolve imports using lexical module and actual declarations, never basename guesses.
pub(super) fn import_alias(import: &Import) -> &str {
    crate::scoped_imports::alias(import)
}
pub(super) fn import_applies(import: &Import, range: &SourceRange) -> bool {
    crate::scoped_imports::applies(import, range)
}
// Normalization depends only on this file and the immutable project indexes.
// Bound retained strings even for files with many distinct generated symbols.
pub(super) fn normalize_symbol(
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

/// Owned type keys allow project indexes to contain ESM aliases without self-references.
#[derive(Default)]
pub(super) struct KnownTypes(BTreeMap<String, BTreeSet<String>>);
impl KnownTypes {
    pub(super) fn contains(&self, key: &(&str, &str)) -> bool {
        self.0
            .get(key.0)
            .is_some_and(|values| values.contains(key.1))
    }
}
impl<'a> FromIterator<(&'a str, &'a str)> for KnownTypes {
    fn from_iter<I: IntoIterator<Item = (&'a str, &'a str)>>(iter: I) -> Self {
        let mut result = Self::default();
        for (language, name) in iter {
            result
                .0
                .entry(language.into())
                .or_default()
                .insert(name.into());
        }
        result
    }
}
pub(super) type ExportBindings<'a> = BTreeMap<&'a str, BTreeMap<String, String>>;

/// Type annotations and runtime bindings have separate export namespaces.
/// Erased value bindings retain an explicit marker, so direct declaration
/// lookup cannot bypass the absence of a runtime export.
pub(super) fn export_bindings<'a>(
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
                    resolve_import_path(file, &format!("{source}{suffix}"), modules)
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

pub(super) fn canonical_export(
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
pub(super) fn declared_field_type(
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
pub(super) struct ExternalBases<'a> {
    pub(super) bases: BTreeMap<(&'a str, &'a str), Vec<String>>,
    /// ESM file-path spellings (`src/db.ts.Db`) of declared type FQNs.
    pub(super) aliases: BTreeMap<(&'a str, String), &'a str>,
}
impl ExternalBases<'_> {
    /// The one library base whose member `name` a project type inherits,
    /// unless a project class in its hierarchy declares that member.
    pub(super) fn member_owner(
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
pub(super) fn resolve_type_reference(
    file: &FileModel,
    raw: &str,
    owner: &str,
    range: &SourceRange,
    modules: &ModuleIndex<'_>,
    exports: &BTreeMap<&str, BTreeMap<String, String>>,
    types: &KnownTypes,
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

pub(super) fn php_namespace_reference(raw: &str, owner: &str) -> String {
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

pub(super) fn resolve_scoped_import(
    file: &FileModel,
    import: &Import,
    modules: &ModuleIndex<'_>,
) -> String {
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
pub(super) fn rust_import_path(
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
pub(super) fn resolve_import_path(
    file: &FileModel,
    imported: &str,
    modules: &ModuleIndex<'_>,
) -> String {
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
pub(super) fn rust_contract_identity(
    file: &FileModel,
    contract: &str,
    scope: &str,
    range: &SourceRange,
    modules: &ModuleIndex<'_>,
    exports: &BTreeMap<&str, BTreeMap<String, String>>,
    types: &KnownTypes,
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

pub(super) fn function_id(file: &FileModel, fqn: &str) -> String {
    format!("fn:{}:{}:{}", file.language, file.path, fqn)
}
pub(super) fn vendor_package(path: &str, vendor: bool) -> Option<String> {
    if vendor {
        Some(crate::source::vendor_package(path))
    } else {
        None
    }
}
pub(super) fn compile_pattern(pattern: &str) -> Option<globset::GlobMatcher> {
    globset::GlobBuilder::new(pattern)
        .backslash_escape(false)
        .build()
        .ok()
        .map(|glob| glob.compile_matcher())
}
pub(super) fn pattern_matches(pattern: &str, value: &str) -> bool {
    pattern == value || compile_pattern(pattern).is_some_and(|matcher| matcher.is_match(value))
}
pub(super) fn has_multiple_segments(value: &str) -> bool {
    value
        .split(['.', ':', '\\', '/'])
        .filter(|part| !part.is_empty())
        .nth(1)
        .is_some()
}
pub(super) fn has_multiple_segments_for(
    value: &str,
    strategy: &dyn crate::LanguageStrategy,
) -> bool {
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
pub(super) fn split_symbol(value: &str) -> Vec<&str> {
    value
        .split(['.', ':', '\\', '/'])
        .filter(|part| !part.is_empty())
        .collect()
}
pub(super) fn literal_argument(call: &CallSite, config: &Config) -> Option<String> {
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
pub(super) fn apply_vendor_view(
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
pub(super) fn coverage(
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

pub(super) fn component_prefix(value: &str, prefix: &str) -> bool {
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
pub(super) fn context_module(module: &str, target: &str) -> String {
    let escaped = target
        .replace('%', "%25")
        .replace('[', "%5B")
        .replace(']', "%5D")
        .replace(':', "%3A");
    format!("{module}#cargo[{escaped}]")
}
