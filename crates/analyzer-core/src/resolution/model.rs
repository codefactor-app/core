//! Resolution metadata and predicates, independent of metadata loading.
use super::cargo;
use crate::config::RustResolutionConfig;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

#[derive(Clone, Debug, Default)]
pub struct ResolutionContext {
    pub(super) packages: BTreeMap<String, Value>,
    pub(super) tsconfigs: BTreeMap<String, TsConfig>,
    pub(super) cargo_features: BTreeMap<String, CargoFeatures>,
    pub(super) cargo_targets: BTreeMap<String, Target>,
    pub(super) cargo_packages: BTreeMap<String, cargo::Package>,
    pub(super) cargo_linked_features: BTreeMap<String, Option<Vec<String>>>,
    pub(super) cargo_root_packages: BTreeMap<String, BTreeSet<String>>,
    pub cargo_roots: BTreeSet<String>,
    pub metadata_paths: BTreeSet<PathBuf>,
    pub diagnostics: Vec<String>,
}
#[derive(Clone, Debug, Default)]
pub(super) struct TsConfig {
    pub(super) base: Option<String>,
    pub(super) paths_base: String,
    pub(super) paths: BTreeMap<String, Vec<String>>,
    pub(super) valid: bool,
}
#[derive(Clone, Debug, Default)]
pub(super) struct CargoFeatures {
    pub(super) entries: BTreeMap<String, Option<Vec<String>>>,
    pub(super) dependencies: BTreeMap<String, bool>,
    pub(super) implicit: BTreeSet<String>,
}
pub(super) struct CargoFeatureClosure {
    pub(super) enabled: Vec<String>,
    pub(super) dependencies: BTreeSet<String>,
}
impl CargoFeatures {
    pub(super) fn from_manifest(manifest: &toml::Value) -> Self {
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
    pub(super) fn closure(&self, selected: &[String]) -> Result<CargoFeatureClosure, String> {
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
    pub(super) fn binary_requirements(
        &mut self,
        path: &str,
        bin: &toml::Value,
    ) -> Option<Vec<String>> {
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
}

#[derive(Clone, Debug)]
pub(super) struct Target {
    pub root: String,
    pub package: String,
    pub kind: String,
    pub name: String,
    pub requirements: Option<Vec<String>>,
}
impl Target {
    pub(super) fn enabled(&self, config: &RustResolutionConfig) -> Option<bool> {
        let required = self.requirements.as_ref()?;
        if required.is_empty() {
            return Some(true);
        }
        let features = config.features.as_ref()?;
        Some(required.iter().all(|feature| features.contains(feature)))
    }
}
