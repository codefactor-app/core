//! Cargo manifest links are interpreted without invoking Cargo or build scripts.
use super::*;
pub(super) mod features;
pub(super) mod targets;

#[derive(Clone, Debug, Default)]
pub(super) struct Package {
    pub name: String,
    pub library_name: String,
    pub library_root: Option<String>,
    dependencies: BTreeMap<String, Vec<Dependency>>,
}
#[derive(Clone, Debug)]
struct Dependency {
    library_root: Option<String>,
    package_directory: Option<String>,
    features: Option<Vec<String>>,
    default_features: Option<bool>,
    optional: bool,
    feature_name: String,
    predicate: Option<String>,
    development: bool,
}

fn workspace_dependency<'a>(
    directory: &str,
    manifest: &toml::Value,
    name: &str,
    manifests: &'a BTreeMap<String, toml::Value>,
) -> Option<(String, &'a toml::Value)> {
    let explicit = manifest
        .get("package")
        .and_then(|package| package.get("workspace"))
        .and_then(toml::Value::as_str);
    let mut owner = explicit
        .map(|path| join(directory, path))
        .unwrap_or_else(|| directory.to_owned());
    loop {
        if let Some(value) = manifests
            .get(&join(&owner, "Cargo.toml"))
            .and_then(|manifest| manifest.get("workspace"))
        {
            return value
                .get("dependencies")
                .and_then(|dependencies| dependencies.get(name))
                .map(|dependency| (owner, dependency));
        }
        if explicit.is_some() || owner.is_empty() {
            return None;
        }
        owner = parent(&owner);
    }
}
fn dependency_sections(manifest: &toml::Value) -> Vec<(Option<String>, bool, &toml::value::Table)> {
    let mut result = Vec::new();
    for owner in std::iter::once((None, manifest)).chain(
        manifest
            .get("target")
            .and_then(toml::Value::as_table)
            .into_iter()
            .flat_map(|targets| {
                targets
                    .iter()
                    .map(|(predicate, value)| (Some(predicate.clone()), value))
            }),
    ) {
        for (section, development) in [("dependencies", false), ("dev-dependencies", true)] {
            if let Some(dependencies) = owner.1.get(section).and_then(toml::Value::as_table) {
                result.push((owner.0.clone(), development, dependencies));
            }
        }
    }
    result
}
fn effective_dependency(
    directory: &str,
    manifest: &toml::Value,
    name: &str,
    dependency: &toml::Value,
    manifests: &BTreeMap<String, toml::Value>,
) -> Option<(String, toml::Value)> {
    if dependency.get("workspace").and_then(toml::Value::as_bool) != Some(true) {
        return Some((directory.into(), dependency.clone()));
    }
    let (origin, inherited) = workspace_dependency(directory, manifest, name, manifests)?;
    let mut inherited = inherited.as_table().cloned().unwrap_or_else(|| {
        let mut table = toml::value::Table::new();
        table.insert("version".into(), inherited.clone());
        table
    });
    if let Some(optional) = dependency.get("optional") {
        inherited.insert("optional".into(), optional.clone());
    }
    if let Some(features) = dependency.get("features") {
        let features = features.as_array()?;
        inherited
            .entry("features")
            .or_insert_with(|| toml::Value::Array(Vec::new()))
            .as_array_mut()?
            .extend(features.iter().cloned());
    }
    // The behavior of this member override differs across Cargo versions/editions.
    if let Some(defaults) = dependency.get("default-features") {
        if inherited
            .get("default-features")
            .and_then(toml::Value::as_bool)
            .unwrap_or(true)
            != defaults.as_bool().unwrap_or(true)
        {
            inherited.insert(
                "default-features".into(),
                toml::Value::String("version-dependent workspace override".into()),
            );
        }
    }
    Some((origin, toml::Value::Table(inherited)))
}

pub(super) fn load(
    base: &Path,
    mut manifests: BTreeMap<String, toml::Value>,
    context: &mut ResolutionContext,
) {
    // Follow explicitly declared local paths, including metadata ignored by discovery.
    loop {
        let mut additions = BTreeMap::new();
        for (path, manifest) in &manifests {
            let directory = parent(path);
            for (_, _, dependencies) in dependency_sections(manifest) {
                for (name, dependency) in dependencies {
                    let Some((origin, dependency)) =
                        effective_dependency(&directory, manifest, name, dependency, &manifests)
                    else {
                        continue;
                    };
                    let Some(local) = dependency.get("path").and_then(toml::Value::as_str) else {
                        continue;
                    };
                    let candidate = base.join(join(&origin, local)).join("Cargo.toml");
                    if Path::new(&normalize(&candidate)).starts_with(base) {
                        context.metadata_paths.insert(candidate.clone());
                    }
                    let Some(relative) = confined_relative(base, &candidate) else {
                        continue;
                    };
                    if manifests.contains_key(&relative) || additions.contains_key(&relative) {
                        continue;
                    }
                    match std::fs::read_to_string(&candidate)
                        .ok()
                        .and_then(|text| text.parse::<toml::Value>().ok())
                    {
                        Some(value) => {
                            additions.insert(relative, value);
                        }
                        None => {
                            context.diagnostics.push(format!("{path}: local Cargo dependency {name:?} has an unreadable manifest"));
                        }
                    }
                }
            }
        }
        if additions.is_empty() {
            break;
        }
        manifests.extend(additions);
    }
    for (path, manifest) in &manifests {
        let Some(name) = manifest
            .get("package")
            .and_then(|package| package.get("name"))
            .and_then(toml::Value::as_str)
        else {
            if manifest.get("package").is_some() {
                context.diagnostics.push(format!("{path}: Cargo package name is missing or invalid; package links remain unresolved"));
            }
            continue;
        };
        let directory = parent(path);
        let library_name = manifest
            .get("lib")
            .and_then(|lib| lib.get("name"))
            .and_then(toml::Value::as_str)
            .unwrap_or(name)
            .replace('-', "_");
        let root = manifest
            .get("lib")
            .and_then(|lib| lib.get("path"))
            .and_then(toml::Value::as_str)
            .map(|path| join(&directory, path))
            .or_else(|| {
                ((manifest.get("lib").is_some()
                    || manifest
                        .get("package")
                        .and_then(|package| package.get("autolib"))
                        .and_then(toml::Value::as_bool)
                        != Some(false))
                    && base.join(join(&directory, "src/lib.rs")).is_file())
                .then(|| join(&directory, "src/lib.rs"))
            });
        context.cargo_packages.insert(
            directory,
            Package {
                name: name.into(),
                library_name,
                library_root: root,
                ..Package::default()
            },
        );
    }
    let mut targets = targets::Builder::default();
    for (path, manifest) in &manifests {
        let directory = parent(path);
        let Some(package) = context.cargo_packages.get(&directory) else {
            continue;
        };
        let package_name = package.name.clone();
        if let Some(root) = &package.library_root {
            targets.add(targets::Target {
                root: root.clone(),
                package: directory.clone(),
                kind: "lib".into(),
                name: package.library_name.clone(),
                requirements: Some(Vec::new()),
            });
        }
        context
            .cargo_features
            .insert(directory.clone(), CargoFeatures::from_manifest(manifest));
        if let Some(bins) = manifest.get("bin").and_then(toml::Value::as_array) {
            for bin in bins {
                let name = bin
                    .get("name")
                    .and_then(toml::Value::as_str)
                    .unwrap_or(&package_name);
                let candidates = if let Some(path) = bin.get("path").and_then(toml::Value::as_str) {
                    vec![join(&directory, path)]
                } else {
                    let mut candidates = vec![
                        format!("src/bin/{name}.rs"),
                        format!("src/bin/{name}/main.rs"),
                    ];
                    if name == package_name {
                        candidates.insert(0, "src/main.rs".into());
                    }
                    candidates
                        .into_iter()
                        .map(|path| join(&directory, &path))
                        .filter(|path| base.join(path).is_file())
                        .collect()
                };
                for root in candidates {
                    targets.add(targets::Target {
                        requirements: context.binary_requirements(&root, bin),
                        root,
                        package: directory.clone(),
                        kind: "bin".into(),
                        name: name.into(),
                    });
                }
            }
        }
        let autobins = manifest
            .get("package")
            .and_then(|package| package.get("autobins"))
            .and_then(toml::Value::as_bool)
            .unwrap_or(true);
        if autobins {
            let main = join(&directory, "src/main.rs");
            if base.join(&main).is_file() {
                targets.add(targets::Target {
                    root: main,
                    package: directory.clone(),
                    kind: "bin".into(),
                    name: package_name,
                    requirements: Some(Vec::new()),
                });
            }
            if let Ok(entries) = std::fs::read_dir(base.join(join(&directory, "src/bin"))) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let (candidate, name) = if path.is_dir() {
                        (
                            path.join("main.rs"),
                            path.file_name()
                                .map(|name| name.to_string_lossy().into_owned()),
                        )
                    } else {
                        (
                            path.clone(),
                            path.file_stem()
                                .map(|name| name.to_string_lossy().into_owned()),
                        )
                    };
                    if candidate
                        .extension()
                        .is_some_and(|extension| extension == "rs")
                        && candidate.is_file()
                    {
                        if let Some((root, name)) = confined_relative(base, &candidate).zip(name) {
                            targets.add(targets::Target {
                                root,
                                package: directory.clone(),
                                kind: "bin".into(),
                                name,
                                requirements: Some(Vec::new()),
                            });
                        }
                    }
                }
            }
        }
    }
    targets.finish(context);
    for (path, manifest) in &manifests {
        let directory = parent(path);
        if !context.cargo_packages.contains_key(&directory) {
            continue;
        }
        let mut bindings = BTreeMap::<String, Vec<Dependency>>::new();
        for (predicate, development, dependencies) in dependency_sections(manifest) {
            for (name, raw) in dependencies {
                let Some((origin, dependency)) =
                    effective_dependency(&directory, manifest, name, raw, &manifests)
                else {
                    context.diagnostics.push(format!(
                        "{path}: workspace Cargo dependency {name:?} cannot be inherited"
                    ));
                    bindings
                        .entry(name.replace('-', "_"))
                        .or_default()
                        .push(Dependency {
                            library_root: None,
                            package_directory: None,
                            features: None,
                            default_features: None,
                            optional: false,
                            feature_name: name.clone(),
                            predicate: predicate.clone(),
                            development,
                        });
                    continue;
                };
                let optional = dependency
                    .get("optional")
                    .and_then(toml::Value::as_bool)
                    .unwrap_or(false);
                let mut alias = name.replace('-', "_");
                let mut package_directory = None;
                let library_root = dependency.get("path").and_then(toml::Value::as_str).and_then(|local| {
                    let candidate = base.join(join(&origin, local)).join("Cargo.toml");
                    let relative = confined_relative(base, &candidate);
                    let package = relative.as_ref().and_then(|relative| context.cargo_packages.get(&parent(relative)));
                    let requested = dependency.get("package").and_then(toml::Value::as_str).unwrap_or(name);
                    match package {
                        Some(package) if package.name == requested && package.library_root.is_some() => {
                            package_directory = relative.as_ref().map(|relative| parent(relative));
                            if dependency.get("package").is_none() { alias = package.library_name.clone(); }
                            package.library_root.clone()
                        }
                        Some(package) if package.name != requested => {
                            context.diagnostics.push(format!("{path}: local Cargo dependency {name:?} requests package {requested:?}, found {:?}", package.name));
                            None
                        }
                        _ => {
                            context.diagnostics.push(format!("{path}: local Cargo dependency {name:?} has no discovered library root (missing, unreadable or outside project)"));
                            None
                        }
                    }
                });
                bindings.entry(alias).or_default().push(Dependency {
                    library_root,
                    package_directory,
                    features: dependency
                        .get("features")
                        .map(|features| {
                            features
                                .as_array()?
                                .iter()
                                .map(|feature| feature.as_str().map(str::to_owned))
                                .collect()
                        })
                        .unwrap_or_else(|| Some(Vec::new())),
                    default_features: dependency
                        .get("default-features")
                        .map(toml::Value::as_bool)
                        .unwrap_or(Some(true)),
                    optional,
                    feature_name: name.clone(),
                    predicate: predicate.clone(),
                    development,
                });
            }
        }
        context
            .cargo_packages
            .get_mut(&directory)
            .unwrap()
            .dependencies = bindings;
    }
}

impl ResolutionContext {
    fn cargo_package(&self, file: &str) -> Option<(&str, &Package)> {
        if let Some(target) = self.cargo_targets.get(file) {
            return self
                .cargo_packages
                .get_key_value(&target.package)
                .map(|(directory, package)| (directory.as_str(), package));
        }
        if let Some(owners) = self.cargo_root_packages.get(file) {
            if owners.len() != 1 {
                return None;
            }
            let directory = owners.iter().next().unwrap();
            return self
                .cargo_packages
                .get_key_value(directory)
                .map(|(directory, package)| (directory.as_str(), package));
        }
        let mut directory = parent(file);
        loop {
            if let Some((directory, package)) = self.cargo_packages.get_key_value(&directory) {
                return Some((directory, package));
            }
            if directory.is_empty() {
                return None;
            }
            directory = parent(&directory);
        }
    }
    /// Names that select the owning Cargo package of a source file: the package
    /// name, its `-`/`_` spelling and the library crate name.
    pub(crate) fn cargo_package_names(&self, file: &str) -> Vec<String> {
        let Some((_, package)) = self.cargo_package(file) else {
            return Vec::new();
        };
        let mut names = vec![
            package.name.clone(),
            package.name.replace('-', "_"),
            package.library_name.clone(),
        ];
        names.sort();
        names.dedup();
        names
    }
    /// Cargo crate bindings select an analyzed local library, never a package basename.
    pub(crate) fn rust_dependency_roots(
        &self,
        root: &str,
        alias: &str,
        config: &RustResolutionConfig,
    ) -> ResourceResolution {
        let Some((directory, package)) = self.cargo_package(root) else {
            return ResourceResolution::Unspecified;
        };
        let library_target = self.library_target(directory);
        if alias == package.library_name && library_target != Some(root) {
            return library_target
                .map(|id| ResourceResolution::Targets(vec![id.into()]))
                .unwrap_or(ResourceResolution::Blocked);
        }
        let Some(dependencies) = package.dependencies.get(alias) else {
            return ResourceResolution::Unspecified;
        };
        let active_dependencies = config
            .features
            .as_ref()
            .and_then(|selected| self.cargo_features.get(directory)?.closure(selected).ok());
        let mut roots = BTreeSet::new();
        let mut uncertain = false;
        for dependency in dependencies {
            match dependency.active(config, active_dependencies.as_ref()) {
                Some(true) => {}
                Some(false) => continue,
                None => {
                    uncertain = true;
                    continue;
                }
            }
            if let Some(target) = dependency
                .package_directory
                .as_deref()
                .and_then(|directory| self.library_target(directory))
            {
                roots.insert(target.to_owned());
            }
        }
        if uncertain || roots.is_empty() {
            ResourceResolution::Blocked
        } else {
            ResourceResolution::Targets(roots.into_iter().collect())
        }
    }
}

impl ResolutionContext {
    pub(super) fn report_cargo_dependencies(
        &self,
        file: &crate::ir::FileModel,
        config: &Config,
        analyzed_sources: &BTreeSet<&str>,
        messages: &mut BTreeSet<String>,
    ) {
        let Some((directory, package)) = self.cargo_package(&file.path) else {
            return;
        };
        let effective = self.rust_config_for(&file.path, &config.resolution.rust);
        let references = file
            .imports
            .iter()
            .map(|import| import.path.as_str())
            .chain(file.functions.iter().flat_map(|function| {
                function
                    .calls
                    .iter()
                    .filter_map(|call| call.receiver.as_deref())
            }))
            .chain(file.functions.iter().flat_map(|function| {
                function
                    .parameters
                    .iter()
                    .filter_map(|parameter| parameter.type_name.as_deref())
            }))
            .filter_map(|reference| reference.trim_start_matches('&').trim().split("::").next())
            .collect::<BTreeSet<_>>();
        for (alias, dependencies) in &package.dependencies {
            if !references.contains(alias.as_str()) {
                continue;
            }
            if dependencies.iter().any(|dependency| {
                dependency
                    .library_root
                    .as_deref()
                    .is_none_or(|root| !analyzed_sources.contains(root))
            }) {
                messages.insert(format!("{}: Cargo dependency {alias:?} is external or has no analyzed local library; calls remain unresolved", join(directory, "Cargo.toml")));
            }
            if dependencies.iter().any(|dependency| dependency.optional)
                && effective.features.is_none()
            {
                messages.insert(format!("{}: optional Cargo dependency {alias:?} needs explicit resolution.rust.features to establish its binding", join(directory, "Cargo.toml")));
            }
            if dependencies.iter().any(|dependency| {
                dependency.predicate.as_ref().is_some_and(|predicate| {
                    predicate
                        .strip_prefix("cfg(")
                        .and_then(|predicate| predicate.strip_suffix(')'))
                        .map(|predicate| evaluate_cfg(predicate, &effective).is_none())
                        .unwrap_or(effective.target.is_none())
                })
            }) {
                messages.insert(format!("{}: target-specific Cargo dependency {alias:?} has unknown target/cfg inputs; its binding is not certified", join(directory, "Cargo.toml")));
            }
        }
    }
}
