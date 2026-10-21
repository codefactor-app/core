//! Project metadata used by the static resolver. Loading never invokes a build
//! tool or project code; only analyzed files can become graph targets.
use crate::config::{Config, RustResolutionConfig};
use crate::ir::TypeModel;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
mod cargo;

#[derive(Clone, Debug, Default)]
pub struct ResolutionContext {
    packages: BTreeMap<String, Value>,
    tsconfigs: BTreeMap<String, TsConfig>,
    cargo_features: BTreeMap<String, CargoFeatures>,
    cargo_targets: BTreeMap<String, cargo::targets::Target>,
    cargo_packages: BTreeMap<String, cargo::Package>,
    cargo_linked_features: BTreeMap<String, Option<Vec<String>>>,
    cargo_root_packages: BTreeMap<String, BTreeSet<String>>,
    pub cargo_roots: BTreeSet<String>,
    pub metadata_paths: BTreeSet<PathBuf>,
    pub diagnostics: Vec<String>,
}
#[derive(Clone, Debug, Default)]
struct TsConfig {
    base: Option<String>,
    paths_base: String,
    paths: BTreeMap<String, Vec<String>>,
    valid: bool,
}
#[derive(Clone, Debug, Default)]
struct CargoFeatures {
    entries: BTreeMap<String, Option<Vec<String>>>,
    dependencies: BTreeMap<String, bool>,
    implicit: BTreeSet<String>,
}
struct CargoFeatureClosure {
    enabled: Vec<String>,
    dependencies: BTreeSet<String>,
}
impl CargoFeatures {
    fn from_manifest(manifest: &toml::Value) -> Self {
        let entries = manifest
            .get("features")
            .and_then(toml::Value::as_table)
            .map(|features| {
                features
                    .iter()
                    .map(|(name, definition)| {
                        let definition = definition.as_array().and_then(|items| {
                            items
                                .iter()
                                .map(|item| item.as_str().map(str::to_owned))
                                .collect::<Option<Vec<_>>>()
                        });
                        (name.clone(), definition)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut features = Self {
            entries,
            ..Self::default()
        };
        let target_tables = manifest
            .get("target")
            .and_then(toml::Value::as_table)
            .into_iter()
            .flat_map(|targets| targets.values());
        for owner in std::iter::once(manifest).chain(target_tables) {
            for section in ["dependencies", "build-dependencies", "dev-dependencies"] {
                for (name, dependency) in owner
                    .get(section)
                    .and_then(toml::Value::as_table)
                    .into_iter()
                    .flatten()
                {
                    let optional = dependency
                        .get("optional")
                        .and_then(toml::Value::as_bool)
                        .unwrap_or(false);
                    *features.dependencies.entry(name.clone()).or_default() |= optional;
                }
            }
        }
        let suppressed = features
            .entries
            .values()
            .flatten()
            .flatten()
            .filter_map(|entry| entry.strip_prefix("dep:"))
            .collect::<BTreeSet<_>>();
        features.implicit = features
            .dependencies
            .iter()
            .filter(|(name, optional)| {
                **optional
                    && !suppressed.contains(name.as_str())
                    && !features.entries.contains_key(*name)
            })
            .map(|(name, _)| name.clone())
            .collect();
        for dependency in &features.implicit {
            features
                .entries
                .insert(dependency.clone(), Some(vec![format!("dep:{dependency}")]));
        }
        features
    }
    fn closure(&self, selected: &[String]) -> Result<CargoFeatureClosure, String> {
        let mut enabled = BTreeSet::new();
        let mut dependencies = BTreeSet::new();
        let mut pending = selected.to_vec();
        while let Some(feature) = pending.pop() {
            if let Some(dependency) = feature.strip_prefix("dep:") {
                if self.dependencies.get(dependency) != Some(&true) {
                    return Err(format!("{feature:?} does not name a declared optional dependency; local feature predicates remain unknown"));
                }
                dependencies.insert(feature);
                continue;
            }
            if let Some((dependency, forwarded)) = feature.split_once('/') {
                let weak = dependency.ends_with('?');
                let dependency = dependency.strip_suffix('?').unwrap_or(dependency);
                if forwarded.is_empty()
                    || forwarded.contains('/')
                    || !self.dependencies.contains_key(dependency)
                {
                    return Err(format!("dependency feature {feature:?} is invalid or undeclared; local feature predicates remain unknown"));
                }
                if !weak && self.implicit.contains(dependency) {
                    pending.push(dependency.to_owned());
                }
                dependencies.insert(feature);
                continue;
            }
            if !enabled.insert(feature.clone()) {
                continue;
            }
            let Some(definition) = self.entries.get(&feature) else {
                return Err(format!("feature {feature:?} is not a declared local or implicit optional dependency feature"));
            };
            let Some(definition) = definition else {
                return Err(format!("feature {feature:?} must be a list of strings"));
            };
            pending.extend(definition.iter().cloned());
        }
        Ok(CargoFeatureClosure {
            enabled: enabled.into_iter().collect(),
            dependencies,
        })
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum ResourceResolution {
    Unspecified,
    Targets(Vec<String>),
    Blocked,
}
#[derive(Debug, PartialEq, Eq)]
pub enum ModulePath {
    Default,
    Path(String),
    Inactive,
    Unknown,
}

impl ResolutionContext {
    fn feature_package(&self, file: &str) -> Option<(&str, &CargoFeatures)> {
        if let Some(target) = self.cargo_targets.get(file) {
            return self
                .cargo_features
                .get_key_value(&target.package)
                .map(|(directory, features)| (directory.as_str(), features));
        }
        if let Some(owners) = self.cargo_root_packages.get(file) {
            if owners.len() != 1 {
                return None;
            }
            let directory = owners.iter().next().unwrap();
            return self
                .cargo_features
                .get_key_value(directory)
                .map(|(directory, features)| (directory.as_str(), features));
        }
        let mut directory = parent(file);
        loop {
            if let Some((directory, features)) = self.cargo_features.get_key_value(&directory) {
                return Some((directory.as_str(), features));
            }
            if directory.is_empty() {
                return None;
            }
            directory = parent(&directory);
        }
    }
    /// Expand only the selected package's local feature declarations. The
    /// caller's None/empty settings retain their explicit meaning.
    pub fn rust_config_for(
        &self,
        file: &str,
        config: &RustResolutionConfig,
    ) -> RustResolutionConfig {
        let mut effective = config.clone();
        if let Some((directory, _)) = self.feature_package(file) {
            if let Some(features) = self.cargo_linked_features.get(directory) {
                effective.features = features.clone();
                return effective;
            }
        }
        if let Some(selected) = &config.features {
            if !selected.is_empty() {
                if let Some((_, package)) = self.feature_package(file) {
                    effective.features = package
                        .closure(selected)
                        .ok()
                        .map(|closure| closure.enabled);
                } else if !self.cargo_features.is_empty() {
                    // A shared/out-of-package file cannot inherit an arbitrary
                    // package's feature closure solely from its filename.
                    effective.features = None;
                }
            }
        }
        effective
    }
    pub fn rust_root_enabled(&self, file: &str, config: &RustResolutionConfig) -> Option<bool> {
        if let Some(target) = self.cargo_targets.get(file) {
            return target.enabled(config);
        }
        let targets = self
            .cargo_targets
            .values()
            .filter(|target| target.root == file)
            .map(|target| target.enabled(config))
            .collect::<Vec<_>>();
        if targets.is_empty() || targets.contains(&Some(true)) {
            Some(true)
        } else if targets.contains(&None) {
            None
        } else {
            Some(false)
        }
    }
    pub fn report_diagnostics(
        &self,
        files: &[crate::ir::FileModel],
        config: &Config,
    ) -> Vec<String> {
        let mut messages = self.diagnostics.iter().cloned().collect::<BTreeSet<_>>();
        let analyzed_sources = files.iter().map(|file| file.path.as_str()).collect();
        for file in files.iter().filter(|file| file.language == "rust") {
            self.report_cargo_dependencies(file, config, &analyzed_sources, &mut messages);
            let effective = self.rust_config_for(&file.path, &config.resolution.rust);
            for target in self.rust_target_ids(&file.path) {
                let target_config = self.rust_config_for(target, &config.resolution.rust);
                if self.rust_root_enabled(target, &target_config).is_none() {
                    messages.insert(format!("{target}: Cargo binary required-features cannot be determined from resolution.rust inputs"));
                }
            }
            if let Some(selected) = config
                .resolution
                .rust
                .features
                .as_ref()
                .filter(|selected| !selected.is_empty())
            {
                if let Some((directory, package)) = self.feature_package(&file.path) {
                    match package.closure(effective.features.as_deref().unwrap_or(selected)) {
                        Err(error) => {
                            messages.insert(format!("{}: {error}", join(directory, "Cargo.toml")));
                        }
                        Ok(closure)
                            if closure.dependencies.iter().any(|dependency| {
                                self.cargo_forwarding_unlinked(directory, dependency)
                            }) =>
                        {
                            messages.insert(format!("{}: local Cargo features are resolved; dependency feature propagation is not linked ({})", join(directory, "Cargo.toml"), closure.dependencies.into_iter().filter(|dependency| self.cargo_forwarding_unlinked(directory, dependency)).collect::<Vec<_>>().join(", ")));
                        }
                        Ok(_) => {}
                    }
                } else if !self.cargo_features.is_empty() {
                    messages.insert(format!(
                        "{}: enabled Cargo features cannot be associated with a package",
                        file.path
                    ));
                }
            }
            let conditions = file
                .functions
                .iter()
                .flat_map(|function| &function.rust_cfg)
                .chain(file.types.iter().flat_map(|ty| &ty.rust_cfg))
                .chain(file.types.iter().flat_map(|ty| {
                    ty.conditional_module_paths
                        .iter()
                        .map(|path| &path.condition)
                }));
            for condition in conditions {
                if evaluate_cfg(condition, &effective).is_none() {
                    messages.insert(format!(
                        "{}: cfg({condition}) cannot be determined from resolution.rust inputs",
                        file.path
                    ));
                }
            }
        }
        let count = messages.len();
        let mut messages = messages.into_iter().take(100).collect::<Vec<_>>();
        if count > messages.len() {
            messages.push(format!(
                "{} additional resolution diagnostics omitted",
                count - messages.len()
            ));
        }
        messages
    }
    pub fn load(root: &Path, config: &Config) -> Self {
        let base = if root.is_file() {
            root.parent().unwrap_or(root)
        } else {
            root
        };
        let base = std::fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf());
        let mut context = Self::default();
        let metadata = metadata_files(&base, config);
        let mut cargo = BTreeMap::new();
        let mut raw_ts = BTreeMap::new();
        for absolute in metadata {
            let Some(relative) = confined_relative(&base, &absolute) else {
                continue;
            };
            context.metadata_paths.insert(absolute.clone());
            let text = match std::fs::read_to_string(&absolute) {
                Ok(text) => text,
                Err(error) => {
                    context.diagnostics.push(format!("{relative}: {error}"));
                    continue;
                }
            };
            match absolute.file_name().and_then(|name| name.to_str()) {
                Some("package.json") => {
                    let value = serde_json::from_str(&text).unwrap_or_else(|error| {
                        context.diagnostics.push(format!("{relative}: {error}"));
                        Value::Null
                    });
                    context.packages.insert(parent(&relative), value);
                }
                Some("Cargo.toml") => match text.parse::<toml::Value>() {
                    Ok(value) => {
                        cargo.insert(relative, value);
                    }
                    Err(error) => context.diagnostics.push(format!("{relative}: {error}")),
                },
                Some("tsconfig.json" | "jsconfig.json") => {
                    let value = parse_jsonc(&text).unwrap_or_else(|error| {
                        context.diagnostics.push(format!("{relative}: {error}"));
                        Value::Null
                    });
                    raw_ts.insert(relative, value);
                }
                _ => {}
            }
        }
        let paths = raw_ts.keys().cloned().collect::<Vec<_>>();
        for path in paths {
            let mut stack = BTreeSet::new();
            let resolved = load_tsconfig(&base, &path, &mut raw_ts, &mut stack, &mut context);
            context.tsconfigs.insert(path, resolved);
        }
        cargo::load(&base, cargo, &mut context);
        cargo::features::link(config, &mut context);
        context
    }
    fn binary_requirements(&mut self, path: &str, bin: &toml::Value) -> Option<Vec<String>> {
        if let Some(required) = bin.get("required-features") {
            let requirements = required.as_array().and_then(|items| {
                items
                    .iter()
                    .map(|item| item.as_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>()
            });
            if requirements.is_none() {
                self.diagnostics.push(format!(
                    "{path}: Cargo binary required-features must be a list of strings"
                ));
            }
            requirements
        } else {
            Some(Vec::new())
        }
    }
    /// Explicit Node package context prevents inferring classic browser scope for .js.
    pub fn javascript_package_module(&self, path: &str) -> bool {
        let mut directory = Path::new(path).parent();
        while let Some(parent) = directory {
            let key = parent.to_string_lossy().replace('\\', "/");
            if let Some(package) = self.packages.get(key.as_str()) {
                return matches!(
                    package.get("type").and_then(Value::as_str),
                    Some("module" | "commonjs")
                );
            }
            directory = parent.parent();
        }
        false
    }

    /// Names a `pkg:` selector uses for a project file: its nearest Cargo package
    /// or the `name` of its nearest package.json.
    pub(crate) fn source_package_names(&self, path: &str, language: &str) -> Vec<String> {
        match language {
            "rust" => self.cargo_package_names(path),
            "javascript" | "typescript" => {
                let mut directory = Path::new(path).parent();
                while let Some(parent) = directory {
                    let key = parent.to_string_lossy().replace('\\', "/");
                    if let Some(package) = self.packages.get(key.as_str()) {
                        return package
                            .get("name")
                            .and_then(Value::as_str)
                            .map(|name| vec![name.to_owned()])
                            .unwrap_or_default();
                    }
                    directory = parent.parent();
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    pub fn package_present(&self, directory: &str) -> bool {
        self.packages.contains_key(directory)
    }
    pub fn package_targets(
        &self,
        directory: &str,
        subpath: &str,
        require: bool,
        conditions: &[String],
    ) -> ResourceResolution {
        let Some(package) = self.packages.get(directory) else {
            return ResourceResolution::Unspecified;
        };
        if !package.is_object() {
            return ResourceResolution::Blocked;
        }
        if let Some(exports) = package.get("exports") {
            let key = if subpath.is_empty() {
                ".".to_owned()
            } else {
                format!("./{subpath}")
            };
            let (value, wildcard) = if let Some(map) = exports
                .as_object()
                .filter(|map| map.keys().any(|key| key.starts_with('.')))
            {
                if let Some(value) = map.get(&key) {
                    (value, None)
                } else {
                    let best = map
                        .iter()
                        .filter_map(|(pattern, value)| {
                            let (prefix, suffix) = pattern.split_once('*')?;
                            if suffix.contains('*')
                                || !key.starts_with(prefix)
                                || !key.ends_with(suffix)
                                || key.len() < prefix.len() + suffix.len()
                            {
                                return None;
                            }
                            let matched = key[prefix.len()..key.len() - suffix.len()].to_owned();
                            Some((prefix.len(), suffix.len(), value, matched))
                        })
                        .max_by_key(|(prefix, suffix, _, _)| (*prefix, *suffix));
                    let Some((_, _, value, matched)) = best else {
                        return ResourceResolution::Blocked;
                    };
                    (value, Some(matched))
                }
            } else if subpath.is_empty() {
                (exports, None)
            } else {
                return ResourceResolution::Blocked;
            };
            let targets = export_targets(value, require, conditions);
            let targets = targets
                .into_iter()
                .filter_map(|target| {
                    let target = if let Some(wildcard) = &wildcard {
                        target.replace('*', wildcard)
                    } else {
                        target
                    };
                    // Exports cannot escape the package, even when a pattern's
                    // captured subpath contains '..' or 'node_modules'.
                    if !target.starts_with("./")
                        || target
                            .split('/')
                            .any(|part| part == ".." || part == "node_modules")
                    {
                        return None;
                    }
                    Some(join(directory, &target))
                })
                .collect::<Vec<_>>();
            if targets.is_empty() {
                ResourceResolution::Blocked
            } else {
                ResourceResolution::Targets(targets)
            }
        } else if subpath.is_empty() {
            package
                .get("main")
                .and_then(Value::as_str)
                .map(|main| ResourceResolution::Targets(vec![join(directory, main)]))
                .unwrap_or(ResourceResolution::Unspecified)
        } else {
            ResourceResolution::Unspecified
        }
    }
    pub fn alias_targets(&self, file: &str, resource: &str) -> ResourceResolution {
        let mut directory = Some(parent(file));
        let mut found = None;
        while let Some(current) = directory {
            for name in ["tsconfig.json", "jsconfig.json"] {
                if let Some(config) = self.tsconfigs.get(&join(&current, name)) {
                    found = Some(config);
                    break;
                }
            }
            if found.is_some() {
                break;
            }
            directory = if current.is_empty() {
                None
            } else {
                Some(parent(&current))
            };
        }
        let Some(config) = found else {
            return ResourceResolution::Unspecified;
        };
        if !config.valid {
            return ResourceResolution::Blocked;
        }
        if let Some(paths) = config.paths.get(resource) {
            return ResourceResolution::Targets(
                paths
                    .iter()
                    .map(|path| join(&config.paths_base, path))
                    .collect(),
            );
        }
        let best = config
            .paths
            .iter()
            .filter_map(|(pattern, paths)| {
                let (prefix, suffix) = pattern.split_once('*')?;
                if suffix.contains('*')
                    || !resource.starts_with(prefix)
                    || !resource.ends_with(suffix)
                    || resource.len() < prefix.len() + suffix.len()
                {
                    return None;
                }
                Some((
                    prefix.len(),
                    suffix.len(),
                    paths,
                    &resource[prefix.len()..resource.len() - suffix.len()],
                ))
            })
            .max_by_key(|(prefix, suffix, _, _)| (*prefix, *suffix));
        if let Some((_, _, paths, wildcard)) = best {
            return ResourceResolution::Targets(
                paths
                    .iter()
                    .map(|path| join(&config.paths_base, &path.replace('*', wildcard)))
                    .collect(),
            );
        }
        config
            .base
            .as_ref()
            .map(|base| ResourceResolution::Targets(vec![join(base, resource)]))
            .unwrap_or(ResourceResolution::Unspecified)
    }
}

fn export_targets(value: &Value, require: bool, conditions: &[String]) -> Vec<String> {
    match value {
        Value::String(target)
            if target.starts_with("./")
                && !target
                    .split('/')
                    .any(|part| part == ".." || part == "node_modules") =>
        {
            vec![target.clone()]
        }
        Value::Array(values) => values
            .iter()
            .find_map(|value| {
                let targets = export_targets(value, require, conditions);
                (!targets.is_empty()).then_some(targets)
            })
            .unwrap_or_default(),
        Value::Object(values) => {
            for (condition, value) in values {
                if condition == "default"
                    || condition == if require { "require" } else { "import" }
                    || conditions.contains(condition)
                {
                    return export_targets(value, require, conditions);
                }
            }
            Vec::new()
        }
        _ => Vec::new(),
    }
}

fn load_tsconfig(
    base: &Path,
    path: &str,
    raw: &mut BTreeMap<String, Value>,
    stack: &mut BTreeSet<String>,
    context: &mut ResolutionContext,
) -> TsConfig {
    if stack.len() >= 64 {
        context
            .diagnostics
            .push(format!("{path}: tsconfig extends depth exceeds 64"));
        return TsConfig::default();
    }
    if Path::new(path).is_absolute() || path == ".." || path.starts_with("../") {
        context
            .diagnostics
            .push(format!("{path}: tsconfig extends outside project"));
        return TsConfig::default();
    }
    if !stack.insert(path.into()) {
        context
            .diagnostics
            .push(format!("{path}: cyclic tsconfig extends"));
        return TsConfig::default();
    }
    let absolute = base.join(path);
    if absolute.exists() && confined_relative(base, &absolute).is_none() {
        context
            .diagnostics
            .push(format!("{path}: tsconfig symlink outside project"));
        stack.remove(path);
        return TsConfig::default();
    }
    context.metadata_paths.insert(absolute.clone());
    let value = if let Some(value) = raw.get(path) {
        value.clone()
    } else {
        let value = confined_relative(base, &absolute)
            .and_then(|_| std::fs::read_to_string(&absolute).ok())
            .and_then(|text| parse_jsonc(&text).ok())
            .unwrap_or(Value::Null);
        raw.insert(path.into(), value.clone());
        value
    };
    let mut result = TsConfig {
        paths_base: parent(path),
        valid: value.is_object(),
        ..TsConfig::default()
    };
    if let Some(extends) = value.get("extends") {
        let parents = extends
            .as_str()
            .map(|value| vec![value])
            .or_else(|| {
                extends
                    .as_array()
                    .map(|values| values.iter().filter_map(Value::as_str).collect())
            })
            .unwrap_or_default();
        for inherited in parents {
            let Some(inherited) = resolve_tsconfig_parent(base, path, inherited, context) else {
                result.valid = false;
                continue;
            };
            let inherited = load_tsconfig(base, &inherited, raw, stack, context);
            result.valid &= inherited.valid;
            if inherited.base.is_some() {
                result.base = inherited.base;
            }
            if !inherited.paths.is_empty() {
                result.paths = inherited.paths;
                result.paths_base = inherited.paths_base;
            }
        }
    }
    if let Some(options) = value.get("compilerOptions") {
        if let Some(base_url) = options.get("baseUrl").and_then(Value::as_str) {
            result.base = Some(join(&parent(path), base_url));
        }
        if let Some(paths) = options.get("paths").and_then(Value::as_object) {
            result.paths_base = result.base.clone().unwrap_or_else(|| parent(path));
            result.paths = paths
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        value
                            .as_array()
                            .map(|values| {
                                values
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .map(str::to_owned)
                                    .collect()
                            })
                            .unwrap_or_default(),
                    )
                })
                .collect();
        }
    }
    stack.remove(path);
    result
}

/// Resolve configuration packages without loading JavaScript or consulting a
/// package manager. Every selected file remains confined to the scan root.
fn resolve_tsconfig_parent(
    base: &Path,
    origin: &str,
    resource: &str,
    context: &mut ResolutionContext,
) -> Option<String> {
    if resource.starts_with('.') {
        let mut path = join(&parent(origin), resource);
        if !path.ends_with(".json") {
            path.push_str(".json");
        }
        return Some(path);
    }
    if Path::new(resource).is_absolute() || resource.contains('\\') {
        context
            .diagnostics
            .push(format!("{origin}: tsconfig extends outside project"));
        return None;
    }
    let pieces = resource.split('/').collect::<Vec<_>>();
    let package_parts = if resource.starts_with('@') { 2 } else { 1 };
    if pieces.len() < package_parts
        || pieces
            .iter()
            .any(|part| part.is_empty() || *part == "." || *part == "..")
    {
        context
            .diagnostics
            .push(format!("{origin}: invalid tsconfig package {resource:?}"));
        return None;
    }
    let package = pieces[..package_parts].join("/");
    let subpath = pieces[package_parts..].join("/");
    let mut directory = parent(origin);
    loop {
        let package_dir = join(&directory, &format!("node_modules/{package}"));
        let absolute_package = base.join(&package_dir);
        let manifest_path = join(&package_dir, "package.json");
        context.metadata_paths.insert(base.join(&manifest_path));
        if absolute_package.exists() {
            if confined_relative(base, &absolute_package).is_none() {
                context.diagnostics.push(format!(
                    "{origin}: tsconfig package symlink outside project"
                ));
                return None;
            }
            let manifest = context.packages.get(&package_dir).cloned().or_else(|| {
                std::fs::read_to_string(base.join(&manifest_path))
                    .ok()
                    .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            });
            let entry = if subpath.is_empty() {
                manifest
                    .as_ref()
                    .and_then(|m| m.get("tsconfig"))
                    .and_then(Value::as_str)
                    .unwrap_or("tsconfig.json")
            } else {
                subpath.as_str()
            };
            if Path::new(entry).is_absolute() || entry.contains('\\') {
                context.diagnostics.push(format!(
                    "{origin}: invalid tsconfig package entry {entry:?}"
                ));
                return None;
            }
            let target = join(&package_dir, entry);
            if target != package_dir && !target.starts_with(&format!("{package_dir}/")) {
                context.diagnostics.push(format!(
                    "{origin}: tsconfig package entry escapes its package"
                ));
                return None;
            }
            let mut candidates = vec![target.clone()];
            if !target.ends_with(".json") {
                candidates.push(format!("{target}.json"));
            }
            candidates.push(join(&target, "tsconfig.json"));
            for candidate in candidates {
                let absolute = base.join(&candidate);
                context.metadata_paths.insert(absolute.clone());
                if absolute.is_file() {
                    if confined_relative(base, &absolute).is_none() {
                        context.diagnostics.push(format!(
                            "{origin}: tsconfig extends symlink outside project"
                        ));
                        return None;
                    }
                    return Some(candidate);
                }
            }
            context.diagnostics.push(format!(
                "{origin}: tsconfig extends {resource:?} was not found in {package_dir}"
            ));
            return None;
        }
        if directory.is_empty() {
            break;
        }
        directory = parent(&directory);
    }
    context.diagnostics.push(format!(
        "{origin}: tsconfig package {resource:?} was not found"
    ));
    None
}

/// Enumerate metadata separately from source-language discovery, including
/// ignored dependency roots. Used by both the loader and the watch snapshot.
pub fn metadata_files(root: &Path, config: &Config) -> BTreeSet<PathBuf> {
    let base = if root.is_file() {
        root.parent().unwrap_or(root)
    } else {
        root
    };
    let mut starts = vec![base.to_path_buf()];
    for pattern in &config.vendor.paths {
        let prefix = pattern
            .split(['*', '?', '['])
            .next()
            .unwrap_or("")
            .trim_end_matches('/');
        starts.push(if prefix.is_empty() {
            base.to_path_buf()
        } else {
            base.join(prefix)
        });
    }
    let mut files = BTreeSet::new();
    for (index, start) in starts.into_iter().enumerate() {
        let mut walker = ignore::WalkBuilder::new(start);
        walker
            .hidden(false)
            .git_ignore(index == 0)
            .git_exclude(index == 0)
            .git_global(index == 0)
            .require_git(false)
            .filter_entry(|entry| {
                !matches!(
                    entry.file_name().to_str(),
                    Some(".git" | "target" | ".analyzer-cache")
                )
            });
        for entry in walker.build().flatten() {
            if entry.file_type().is_some_and(|kind| kind.is_file())
                && matches!(
                    entry.file_name().to_str(),
                    Some("package.json" | "tsconfig.json" | "jsconfig.json" | "Cargo.toml")
                )
            {
                if confined_relative(base, entry.path()).is_some() {
                    files.insert(entry.path().to_path_buf());
                }
            }
        }
    }
    files
}
fn confined_relative(base: &Path, path: &Path) -> Option<String> {
    let canonical = std::fs::canonicalize(path).ok()?;
    Some(normalize(canonical.strip_prefix(base).ok()?))
}
pub fn normalize(path: &Path) -> String {
    let mut normalized = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if normalized
                    .components()
                    .next_back()
                    .is_some_and(|part| matches!(part, Component::Normal(_)))
                {
                    normalized.pop();
                } else if !normalized.has_root() {
                    normalized.push("..");
                }
            }
            part => normalized.push(part.as_os_str()),
        }
    }
    normalized.to_string_lossy().replace('\\', "/")
}
fn parent(path: &str) -> String {
    normalize(Path::new(path).parent().unwrap_or_else(|| Path::new("")))
}
fn join(base: &str, path: &str) -> String {
    normalize(&Path::new(base).join(path))
}

/// Strip comments and trailing commas without changing quoted strings.
fn parse_jsonc(source: &str) -> Result<Value, serde_json::Error> {
    let bytes = source.trim_start_matches('\u{feff}').as_bytes();
    let mut out = bytes.to_vec();
    let mut index = 0;
    let mut quoted = false;
    while index < bytes.len() {
        if quoted {
            if bytes[index] == b'\\' {
                index += 2;
                continue;
            }
            if bytes[index] == b'"' {
                quoted = false;
            }
        } else if bytes[index] == b'"' {
            quoted = true;
        } else if bytes[index..].starts_with(b"//") {
            while index < bytes.len() && bytes[index] != b'\n' {
                out[index] = b' ';
                index += 1;
            }
            continue;
        } else if bytes[index..].starts_with(b"/*") {
            out[index] = b' ';
            index += 1;
            while index + 1 < bytes.len() && !bytes[index..].starts_with(b"*/") {
                out[index] = b' ';
                index += 1;
            }
            if index + 1 < bytes.len() {
                out[index] = b' ';
                out[index + 1] = b' ';
                index += 2;
                continue;
            }
        }
        index += 1;
    }
    index = 0;
    quoted = false;
    while index < out.len() {
        if quoted {
            if out[index] == b'\\' {
                index += 2;
                continue;
            }
            if out[index] == b'"' {
                quoted = false;
            }
        } else if out[index] == b'"' {
            quoted = true;
        } else if out[index] == b',' {
            let next = out[index + 1..]
                .iter()
                .find(|byte| !byte.is_ascii_whitespace());
            if next.is_some_and(|byte| matches!(*byte, b'}' | b']')) {
                out[index] = b' ';
            }
        }
        index += 1;
    }
    serde_json::from_slice(&out)
}

pub fn module_path(namespace: &TypeModel, config: &RustResolutionConfig) -> ModulePath {
    if !namespace.rust_cfg.is_empty() {
        let values = namespace
            .rust_cfg
            .iter()
            .map(|condition| evaluate_cfg(condition, config))
            .collect::<Vec<_>>();
        if values.contains(&Some(false)) {
            return ModulePath::Inactive;
        }
        if values.contains(&None) {
            return ModulePath::Unknown;
        }
    }
    let mut path = namespace.module_path.clone();
    for candidate in &namespace.conditional_module_paths {
        match evaluate_cfg(&candidate.condition, config) {
            Some(true) => {
                if path.is_some() {
                    return ModulePath::Unknown;
                }
                path = Some(candidate.path.clone());
            }
            Some(false) => {}
            None => return ModulePath::Unknown,
        }
    }
    path.map(ModulePath::Path).unwrap_or(ModulePath::Default)
}

/// Whether a `cfg` predicate is false whenever `test` is, regardless of every
/// other (unknown) flag, target or feature.
pub fn cfg_requires_test(expression: &str) -> bool {
    fn without_test(expression: &str, depth: usize) -> Option<bool> {
        let expression = expression.trim();
        if depth > 64 {
            return None;
        }
        for operator in ["all", "any", "not"] {
            if let Some(arguments) = expression
                .strip_prefix(operator)
                .map(str::trim_start)
                .and_then(|tail| tail.strip_prefix('('))
                .and_then(|tail| tail.strip_suffix(')'))
            {
                let values = split_arguments(arguments)
                    .into_iter()
                    .map(|argument| without_test(argument, depth + 1))
                    .collect::<Vec<_>>();
                return match operator {
                    "all" if values.contains(&Some(false)) => Some(false),
                    "all" if values.iter().all(|value| *value == Some(true)) => Some(true),
                    "any" if values.contains(&Some(true)) => Some(true),
                    "any" if values.iter().all(|value| *value == Some(false)) => Some(false),
                    "not" if values.len() == 1 => values[0].map(|value| !value),
                    _ => None,
                };
            }
        }
        (expression == "test").then_some(false)
    }
    without_test(expression, 0) == Some(false)
}

pub fn evaluate_cfg(expression: &str, config: &RustResolutionConfig) -> Option<bool> {
    let expression = expression.trim();
    if expression.chars().filter(|ch| *ch == '(').count() > 64 {
        return None;
    }
    for operator in ["all", "any", "not"] {
        if let Some(arguments) = expression
            .strip_prefix(operator)
            .map(str::trim_start)
            .and_then(|tail| tail.strip_prefix('('))
            .and_then(|tail| tail.strip_suffix(')'))
        {
            let values = split_arguments(arguments)
                .into_iter()
                .map(|argument| evaluate_cfg(argument, config))
                .collect::<Vec<_>>();
            return match operator {
                "all" if values.contains(&Some(false)) => Some(false),
                "all" if values.iter().all(|value| *value == Some(true)) => Some(true),
                "any" if values.contains(&Some(true)) => Some(true),
                "any" if values.iter().all(|value| *value == Some(false)) => Some(false),
                "not" if values.len() == 1 => values[0].map(|value| !value),
                _ => None,
            };
        }
    }
    if let Some((key, value)) = expression.split_once('=') {
        let key = key.trim();
        let value = value.trim().strip_prefix('"')?.strip_suffix('"')?;
        if value.contains('\\') {
            return None;
        }
        if key == "feature" {
            return config
                .features
                .as_ref()
                .map(|features| features.iter().any(|feature| feature == value));
        }
        if let Some(cfg) = &config.cfg {
            if cfg.iter().any(|flag| {
                flag.split_once('=').is_some_and(|(flag_key, flag_value)| {
                    flag_key.trim() == key && flag_value.trim().trim_matches('"') == value
                })
            }) {
                return Some(true);
            }
        }
        if key.starts_with("target_") {
            return target_value(key, config.target.as_deref()?).map(|actual| actual == value);
        }
        return config.cfg.as_ref().map(|_| false);
    }
    if expression.contains(['(', ')']) {
        return None;
    }
    if config
        .cfg
        .as_ref()
        .is_some_and(|flags| flags.iter().any(|flag| flag.trim() == expression))
    {
        return Some(true);
    }
    if expression == "unix" || expression == "windows" {
        let os = target_value("target_os", config.target.as_deref()?)?;
        return Some(if expression == "windows" {
            os == "windows"
        } else {
            matches!(
                os,
                "linux"
                    | "macos"
                    | "ios"
                    | "android"
                    | "freebsd"
                    | "openbsd"
                    | "netbsd"
                    | "dragonfly"
                    | "solaris"
                    | "illumos"
            )
        });
    }
    config
        .cfg
        .as_ref()
        .map(|flags| flags.iter().any(|flag| flag.trim() == expression))
}
fn target_value<'a>(key: &str, target: &'a str) -> Option<&'a str> {
    if target.contains(['/', '\\']) || target.ends_with(".json") {
        return None;
    }
    let parts = target.split('-').collect::<Vec<_>>();
    match key {
        "target_arch" => match *parts.first()? {
            "i386" | "i486" | "i586" | "i686" => Some("x86"),
            arch if arch.starts_with("arm") || arch.starts_with("thumb") => Some("arm"),
            arch if arch.starts_with("riscv32") => Some("riscv32"),
            arch if arch.starts_with("riscv64") => Some("riscv64"),
            "mipsel" => Some("mips"),
            "mips64el" => Some("mips64"),
            "powerpc64le" => Some("powerpc64"),
            arch if [
                "x86_64",
                "aarch64",
                "wasm32",
                "wasm64",
                "mips",
                "mips64",
                "powerpc",
                "powerpc64",
                "s390x",
                "sparc",
                "sparc64",
                "loongarch64",
                "bpf",
                "hexagon",
                "m68k",
                "xtensa",
                "avr",
                "msp430",
            ]
            .contains(&arch) =>
            {
                Some(arch)
            }
            _ => None,
        },
        "target_os" if parts.contains(&"darwin") => Some("macos"),
        "target_os" if parts.iter().any(|part| part.starts_with("android")) => Some("android"),
        "target_os" => parts.iter().copied().find(|part| {
            matches!(
                *part,
                "linux"
                    | "windows"
                    | "ios"
                    | "freebsd"
                    | "openbsd"
                    | "netbsd"
                    | "dragonfly"
                    | "solaris"
                    | "illumos"
                    | "none"
                    | "wasi"
                    | "emscripten"
            )
        }),
        "target_env" => parts
            .iter()
            .copied()
            .find(|part| matches!(*part, "gnu" | "msvc" | "musl" | "sgx" | "uclibc")),
        "target_vendor"
            if parts.len() >= 3
                && [
                    "unknown", "pc", "apple", "fortanix", "nvidia", "ibm", "wrs", "esp", "uwp",
                ]
                .contains(&parts[1]) =>
        {
            Some(parts[1])
        }
        "target_vendor" if parts.get(1).is_some_and(|part| *part == "linux") => Some("unknown"),
        _ => None,
    }
}
pub fn split_arguments(source: &str) -> Vec<&str> {
    let mut start = 0;
    let mut depth = 0;
    let mut quoted = false;
    let mut escaped = false;
    let mut result = Vec::new();
    for (index, ch) in source.char_indices() {
        if quoted {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = false;
            }
        } else {
            match ch {
                '"' => quoted = true,
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                ',' if depth == 0 => {
                    if !source[start..index].trim().is_empty() {
                        result.push(source[start..index].trim());
                    }
                    start = index + 1;
                }
                _ => {}
            }
        }
    }
    if !source[start..].trim().is_empty() {
        result.push(source[start..].trim());
    }
    result
}
