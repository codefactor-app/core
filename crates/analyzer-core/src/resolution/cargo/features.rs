//! Bounded feature requests across confined, statically selected local packages.
use super::*;
use std::collections::VecDeque;

fn and(left: Option<bool>, right: Option<bool>) -> Option<bool> {
    match (left, right) {
        (Some(false), _) | (_, Some(false)) => Some(false),
        (Some(true), Some(true)) => Some(true),
        _ => None,
    }
}
impl Dependency {
    pub(super) fn active(
        &self,
        config: &RustResolutionConfig,
        closure: Option<&CargoFeatureClosure>,
    ) -> Option<bool> {
        let development = if self.development {
            evaluate_cfg("test", config)
        } else {
            Some(true)
        };
        let target = self
            .predicate
            .as_ref()
            .map(|predicate| {
                predicate
                    .strip_prefix("cfg(")
                    .and_then(|predicate| predicate.strip_suffix(')'))
                    .map(|predicate| evaluate_cfg(predicate, config))
                    .unwrap_or_else(|| config.target.as_ref().map(|target| target == predicate))
            })
            .unwrap_or(Some(true));
        let optional = if !self.optional {
            Some(true)
        } else {
            closure.map(|closure| {
                closure
                    .dependencies
                    .contains(&format!("dep:{}", self.feature_name))
                    || closure
                        .dependencies
                        .iter()
                        .any(|entry| entry.starts_with(&format!("{}/", self.feature_name)))
            })
        };
        and(and(development, target), optional)
    }
}

pub(in crate::resolution) fn link(config: &Config, context: &mut ResolutionContext) {
    if context.cargo_packages.is_empty() {
        return;
    }
    let Some(selected) = config.resolution.rust.features.as_ref() else {
        context.cargo_linked_features = context
            .cargo_packages
            .keys()
            .map(|directory| (directory.clone(), None))
            .collect();
        return;
    };
    let mut requests = BTreeMap::<String, BTreeSet<String>>::new();
    let mut known = BTreeSet::new();
    let mut uncertain = BTreeSet::new();
    let mut queue = VecDeque::new();
    // A package's own valid selection is retained. A dependency reached from a
    // known caller gets requests in its own feature namespace instead of being
    // poisoned by a caller-only flag that it does not declare.
    for (directory, package) in &context.cargo_features {
        requests.insert(directory.clone(), BTreeSet::new());
        if package.closure(selected).is_ok() {
            requests
                .get_mut(directory)
                .unwrap()
                .extend(selected.iter().cloned());
            known.insert(directory.clone());
            let mut root_config = config.resolution.rust.clone();
            root_config.features = Some(package.closure(selected).unwrap().enabled);
            let active_roots = context
                .cargo_root_packages
                .iter()
                .filter(|(_, owners)| owners.contains(directory))
                .map(|(root, _)| context.rust_root_enabled(root, &root_config))
                .collect::<Vec<_>>();
            if active_roots.contains(&Some(true)) {
                queue.push_back(directory.clone());
            } else if active_roots.contains(&None) {
                uncertain.insert(directory.clone());
            }
        }
    }
    while let Some(directory) = queue.pop_front() {
        let package = &context.cargo_features[&directory];
        let selected = requests[&directory].iter().cloned().collect::<Vec<_>>();
        let closure = match package.closure(&selected) {
            Ok(closure) => closure,
            Err(reason) => {
                context.diagnostics.push(format!(
                    "{}: local dependency feature requests cannot be resolved: {reason}",
                    join(&directory, "Cargo.toml")
                ));
                uncertain.insert(directory);
                continue;
            }
        };
        let mut effective = config.resolution.rust.clone();
        effective.features = Some(closure.enabled.clone());
        for dependency in context.cargo_packages[&directory]
            .dependencies
            .values()
            .flatten()
        {
            let Some(target) = dependency.package_directory.as_ref() else {
                continue;
            };
            match dependency.active(&effective, Some(&closure)) {
                Some(false) => continue,
                None => {
                    uncertain.insert(target.clone());
                    continue;
                }
                Some(true) => {}
            }
            let Some(features) = dependency.features.as_ref() else {
                context.diagnostics.push(format!(
                    "{}: Cargo dependency {:?} features must be a list of strings",
                    join(&directory, "Cargo.toml"),
                    dependency.feature_name
                ));
                uncertain.insert(target.clone());
                continue;
            };
            let Some(defaults) = dependency.default_features else {
                context.diagnostics.push(format!("{}: Cargo dependency {:?} default-features cannot be determined (invalid or version-dependent workspace override)",join(&directory,"Cargo.toml"),dependency.feature_name));
                uncertain.insert(target.clone());
                continue;
            };
            let wanted = requests.entry(target.clone()).or_default();
            let before = wanted.len();
            wanted.extend(features.iter().cloned());
            if defaults
                && context.cargo_features[target]
                    .entries
                    .contains_key("default")
            {
                wanted.insert("default".into());
            }
            for entry in &closure.dependencies {
                if let Some((alias, feature)) = entry.split_once('/') {
                    if alias.trim_end_matches('?') == dependency.feature_name {
                        wanted.insert(feature.into());
                    }
                }
            }
            if known.insert(target.clone()) || wanted.len() != before {
                queue.push_back(target.clone());
            }
        }
    }
    uncertain.extend(
        context
            .cargo_packages
            .keys()
            .filter(|directory| !known.contains(*directory))
            .cloned(),
    );
    let mut queue = uncertain.iter().cloned().collect::<VecDeque<_>>();
    // Unknown requests can add flags later in the build. Do not certify any
    // destination's disabled cfg from a partial known union.
    while let Some(directory) = queue.pop_front() {
        let mut effective = config.resolution.rust.clone();
        effective.features = None;
        for dependency in context.cargo_packages[&directory]
            .dependencies
            .values()
            .flatten()
        {
            if dependency.active(&effective, None) == Some(false) {
                continue;
            }
            if let Some(target) = &dependency.package_directory {
                if uncertain.insert(target.clone()) {
                    queue.push_back(target.clone());
                }
            }
        }
    }
    for directory in context.cargo_packages.keys() {
        let enabled = if uncertain.contains(directory) {
            None
        } else {
            context.cargo_features[directory]
                .closure(&requests[directory].iter().cloned().collect::<Vec<_>>())
                .ok()
                .map(|closure| closure.enabled)
        };
        context
            .cargo_linked_features
            .insert(directory.clone(), enabled);
    }
}

impl ResolutionContext {
    pub(in crate::resolution) fn cargo_forwarding_unlinked(
        &self,
        directory: &str,
        entry: &str,
    ) -> bool {
        let Some((alias, _)) = entry.split_once('/') else {
            return false;
        };
        let alias = alias.trim_end_matches('?');
        self.cargo_packages.get(directory).is_none_or(|package| {
            package
                .dependencies
                .values()
                .flatten()
                .filter(|dependency| dependency.feature_name == alias)
                .all(|dependency| {
                    dependency.package_directory.as_ref().is_none_or(|target| {
                        self.cargo_linked_features
                            .get(target)
                            .is_none_or(Option::is_none)
                    })
                })
        })
    }
}
