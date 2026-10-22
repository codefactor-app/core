//! Project metadata used by the static resolver. Loading never invokes a build
//! tool or project code; only analyzed files can become graph targets.
use crate::config::{Config, RustResolutionConfig};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
mod cargo;
mod model;
use model::*;
pub use model::{ModulePath, ResolutionContext, ResourceResolution};
mod support;
use support::*;
pub use support::{
    cfg_requires_test, evaluate_cfg, metadata_files, module_path, normalize, split_arguments,
};
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
        Self::load_for_sources(root, config, std::iter::empty())
    }

    /// Load vendor compiler configurations only when they are needed by parsed
    /// sources. Explicit project `extends` are always followed independently.
    pub fn load_for_sources<'a>(
        root: &Path,
        config: &Config,
        sources: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let base = if root.is_file() {
            root.parent().unwrap_or(root)
        } else {
            root
        };
        let base = std::fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf());
        let mut context = Self::default();
        let mut metadata = metadata_files(&base, config);
        let vendor = vendor_matchers(config);
        for source in sources {
            if !vendor.iter().any(|glob| glob.is_match(source)) {
                continue;
            }
            let mut directory = Path::new(source).parent();
            while let Some(parent) = directory {
                let mut selected = false;
                for name in ["tsconfig.json", "jsconfig.json"] {
                    let path = base.join(parent).join(name);
                    if path.is_file() && confined_relative(&base, &path).is_some() {
                        metadata.insert(path);
                        selected = true;
                        break;
                    }
                }
                if selected {
                    break;
                }
                directory = parent.parent();
            }
        }
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
