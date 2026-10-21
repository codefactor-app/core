//! Physical Rust sources can belong to several Cargo compilations. Context copies
//! live only in graph assembly, so declarations and source metrics stay physical.
use super::*;

fn ownership(
    files: &[FileModel],
    resolution: &crate::resolution::ResolutionContext,
    config: &Config,
    all: bool,
) -> BTreeMap<String, BTreeSet<String>> {
    let exact = files
        .iter()
        .filter(|file| file.language == "rust")
        .map(|file| ("rust", file.module.as_str()))
        .collect();
    let mut index = ModuleIndex::new(exact);
    index.all_rust_roots = all;
    index.with_files(files, resolution, config).rust_owners
}
/// Rust files reachable from some crate root through `mod` declarations,
/// ignoring every cfg condition and disabled root.
pub(crate) fn owned_rust_files(
    files: &[FileModel],
    resolution: &crate::resolution::ResolutionContext,
    config: &Config,
) -> BTreeSet<String> {
    ownership(files, resolution, config, true)
        .into_keys()
        .collect()
}
fn remap(value: &mut String, old: &str, new: &str) {
    if value == old {
        *value = new.into();
    } else if let Some(suffix) = value.strip_prefix(&format!("{old}::")) {
        *value = format!("{new}::{suffix}");
    }
}
fn optional(value: &mut Option<String>, old: &str, new: &str) {
    if let Some(value) = value {
        remap(value, old, new);
    }
}
fn call(site: &mut CallSite, old: &str, new: &str) {
    remap(&mut site.name, old, new);
    optional(&mut site.receiver, old, new);
    optional(&mut site.receiver_type, old, new);
    optional(&mut site.receiver_root_type, old, new);
    optional(&mut site.rust_trait, old, new);
    for target in &mut site.targets {
        remap(target, old, new);
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
pub(super) fn models(
    files: &[FileModel],
    resolution: &crate::resolution::ResolutionContext,
    config: &Config,
) -> Option<Vec<FileModel>> {
    if resolution.cargo_roots.is_empty() {
        return None;
    }
    let all = ownership(files, resolution, config, true);
    let active = ownership(files, resolution, config, false);
    let mut models = Vec::new();
    for file in files {
        if file.language != "rust" || !all.contains_key(&file.path) {
            models.push(file.clone());
            continue;
        }
        for target in active.get(&file.path).into_iter().flatten() {
            let mut copy = file.clone();
            copy.cargo_target = Some(target.clone());
            if all.get(&file.path).is_some_and(|owners| owners.len() > 1) {
                copy.module = context_module(&file.module, target);
                copy.cargo_source_module = Some(file.module.clone());
                for function in &mut copy.functions {
                    remap(&mut function.fqn, &file.module, &copy.module);
                    optional(&mut function.parent_type, &file.module, &copy.module);
                    optional(&mut function.parent_function, &file.module, &copy.module);
                    optional(&mut function.rust_trait, &file.module, &copy.module);
                    optional(&mut function.return_type, &file.module, &copy.module);
                    for parameter in &mut function.parameters {
                        optional(&mut parameter.type_name, &file.module, &copy.module);
                    }
                    for site in &mut function.calls {
                        call(site, &file.module, &copy.module);
                    }
                }
                for typ in &mut copy.types {
                    remap(&mut typ.fqn, &file.module, &copy.module);
                    for parent in typ.implements.iter_mut().chain(&mut typ.extends) {
                        remap(parent, &file.module, &copy.module);
                    }
                    for method in &mut typ.methods {
                        remap(method, &file.module, &copy.module);
                    }
                    for field in &mut typ.fields {
                        optional(&mut field.type_name, &file.module, &copy.module);
                    }
                }
                for import in &mut copy.imports {
                    optional(&mut import.module, &file.module, &copy.module);
                }
            }
            models.push(copy);
        }
    }
    Some(models)
}

impl ModuleIndex<'_> {
    pub(super) fn rust_dependency_path(&self, file: &FileModel, imported: &str) -> Option<String> {
        let (alias, suffix) = imported.split_once("::").unwrap_or((imported, ""));
        if matches!(alias, "crate" | "self" | "super")
            || alias == file.module
            || imported.starts_with(&format!("{}::", file.module))
        {
            return None;
        }
        let resolution = self.resolution?;
        let mut found_binding = false;
        let mut symbols = BTreeSet::new();
        let roots = self.rust_roots.get(&file.module);
        let origins = roots
            .into_iter()
            .flatten()
            .filter_map(|root| self.rust_root_files.get(root))
            .collect::<BTreeSet<_>>();
        let origins = if origins.is_empty() {
            BTreeSet::from([&file.path])
        } else {
            origins
        };
        for origin in origins {
            let config = resolution.rust_config_for(origin, &self.rust_config);
            match resolution.rust_dependency_roots(origin, alias, &config) {
                crate::resolution::ResourceResolution::Unspecified => {}
                crate::resolution::ResourceResolution::Blocked => {
                    found_binding = true;
                }
                crate::resolution::ResourceResolution::Targets(paths) => {
                    found_binding = true;
                    for path in paths {
                        for module in self.rust_sources.get(&path).into_iter().flatten() {
                            let candidate = if suffix.is_empty() {
                                module.clone()
                            } else {
                                format!("{module}::{suffix}")
                            };
                            if let Some(symbol) = self.rust_canonical(&candidate) {
                                symbols.insert(symbol);
                            }
                        }
                    }
                }
            }
        }
        if !found_binding {
            None
        } else if symbols.len() == 1 {
            symbols.into_iter().next()
        } else {
            Some(format!("<unresolved-cargo>::{imported}"))
        }
    }
}
