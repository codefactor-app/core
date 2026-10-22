//! Paths, metadata syntax and cfg expressions shared by resolution loaders.
use super::model::*;
use crate::{config::*, ir::TypeModel};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

pub(super) fn export_targets(value: &Value, require: bool, conditions: &[String]) -> Vec<String> {
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

pub(super) fn load_tsconfig(
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
        let value = match std::fs::read_to_string(&absolute) {
            Ok(text) => parse_jsonc(&text).unwrap_or_else(|error| {
                context.diagnostics.push(format!("{path}: {error}"));
                Value::Null
            }),
            Err(error) => {
                context.diagnostics.push(format!("{path}: {error}"));
                Value::Null
            }
        };
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
pub(super) fn resolve_tsconfig_parent(
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

pub(super) fn vendor_matchers(config: &Config) -> Vec<globset::GlobMatcher> {
    config
        .vendor
        .paths
        .iter()
        .filter_map(|pattern| {
            globset::Glob::new(pattern)
                .ok()
                .map(|glob| glob.compile_matcher())
        })
        .collect()
}

/// Enumerate package metadata, including ignored dependency roots, and project
/// compiler configurations. Vendor compiler configurations are loaded on demand.
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
    let vendor = vendor_matchers(config);
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
                if let Some(relative) = confined_relative(base, entry.path()) {
                    if matches!(
                        entry.file_name().to_str(),
                        Some("tsconfig.json" | "jsconfig.json")
                    ) && vendor.iter().any(|glob| glob.is_match(&relative))
                    {
                        continue;
                    }
                    files.insert(entry.path().to_path_buf());
                }
            }
        }
    }
    files
}
pub(super) fn confined_relative(base: &Path, path: &Path) -> Option<String> {
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
pub(super) fn parent(path: &str) -> String {
    normalize(Path::new(path).parent().unwrap_or_else(|| Path::new("")))
}
pub(super) fn join(base: &str, path: &str) -> String {
    normalize(&Path::new(base).join(path))
}

/// Strip comments and trailing commas without changing quoted strings.
pub(super) fn parse_jsonc(source: &str) -> Result<Value, serde_json::Error> {
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
    pub(super) fn without_test(expression: &str, depth: usize) -> Option<bool> {
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
pub(super) fn target_value<'a>(key: &str, target: &'a str) -> Option<&'a str> {
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
