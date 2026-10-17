use crate::analysis::LanguageRegistry;
use crate::config::{Config, VendorParse};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct DiscoveredFile {
    pub absolute: PathBuf,
    pub relative: String,
    pub language: String,
    pub vendor: bool,
    pub deferred: bool,
    pub vendor_package: Option<String>,
}

pub fn discover(root: &Path, config: &Config, registry: &LanguageRegistry) -> Vec<DiscoveredFile> {
    let mut out = Vec::new();
    let base = if root.is_file() {
        root.parent().unwrap_or(root)
    } else {
        root
    };
    let include = compile(&config.paths.include);
    let exclude = compile(&config.paths.exclude);
    let vendor = compile(&config.vendor.paths);
    let mut walk = ignore::WalkBuilder::new(root);
    walk.hidden(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .require_git(false)
        .filter_entry(|entry| entry.file_name() != ".git");
    for result in walk.build() {
        let Ok(entry) = result else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let Ok(relative) = path.strip_prefix(base) else {
            continue;
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        if !include.is_empty() && !include.iter().any(|g| g.is_match(&relative)) {
            continue;
        }
        if exclude.iter().any(|g| g.is_match(&relative)) {
            continue;
        }
        let Some(strategy) = registry.language_for_path(path, &config.languages.extensions) else {
            continue;
        };
        if !config.languages.enabled.is_empty()
            && !config
                .languages
                .enabled
                .iter()
                .any(|x| x == strategy.language_id())
        {
            continue;
        }
        let is_vendor = vendor.iter().any(|g| g.is_match(&relative));
        let package = if is_vendor {
            Some(vendor_package(&relative))
        } else {
            None
        };
        out.push(DiscoveredFile {
            absolute: path.to_path_buf(),
            relative,
            language: strategy.language_id().into(),
            vendor: is_vendor,
            deferred: is_vendor && config.vendor.parse != VendorParse::Full,
            vendor_package: package,
        });
    }
    // Git ignores usually hide dependency directories. Scan configured vendor roots
    // separately so lazy/full vendor mode can still reach them on demand.
    if root.is_dir() {
        let mut seen = out
            .iter()
            .map(|f| f.relative.clone())
            .collect::<std::collections::BTreeSet<_>>();
        for pattern in &config.vendor.paths {
            let prefix = pattern
                .split(['*', '?', '['])
                .next()
                .unwrap_or("")
                .trim_end_matches('/');
            if Path::new(prefix)
                .components()
                .any(|part| part.as_os_str() == ".git")
            {
                continue;
            }
            let start = if prefix.is_empty() {
                root.to_path_buf()
            } else {
                root.join(prefix)
            };
            if !start.exists() {
                continue;
            }
            for result in ignore::WalkBuilder::new(&start)
                .hidden(false)
                .git_ignore(false)
                .git_exclude(false)
                .git_global(false)
                .filter_entry(|entry| entry.file_name() != ".git")
                .build()
            {
                let Ok(entry) = result else { continue };
                if !entry.file_type().is_some_and(|t| t.is_file()) {
                    continue;
                }
                let path = entry.path();
                let Ok(rel) = path.strip_prefix(root) else {
                    continue;
                };
                let relative = rel.to_string_lossy().replace('\\', "/");
                if seen.contains(&relative) {
                    continue;
                }
                if (!include.is_empty() && !include.iter().any(|g| g.is_match(&relative)))
                    || !vendor.iter().any(|g| g.is_match(&relative))
                    || exclude.iter().any(|g| g.is_match(&relative))
                {
                    continue;
                }
                let Some(strategy) = registry.language_for_path(path, &config.languages.extensions)
                else {
                    continue;
                };
                if !config.languages.enabled.is_empty()
                    && !config
                        .languages
                        .enabled
                        .iter()
                        .any(|x| x == strategy.language_id())
                {
                    continue;
                }
                seen.insert(relative.clone());
                out.push(DiscoveredFile {
                    absolute: path.to_path_buf(),
                    relative: relative.clone(),
                    language: strategy.language_id().into(),
                    vendor: true,
                    deferred: config.vendor.parse != VendorParse::Full,
                    vendor_package: Some(vendor_package(&relative)),
                });
            }
        }
    }
    out.sort_by(|a, b| a.relative.cmp(&b.relative));
    out
}
fn compile(patterns: &[String]) -> Vec<globset::GlobMatcher> {
    patterns
        .iter()
        .filter_map(|x| globset::Glob::new(x).ok().map(|g| g.compile_matcher()))
        .collect()
}
pub fn vendor_package(path: &str) -> String {
    let parts = path.split('/').collect::<Vec<_>>();
    // Dependency roots can be nested below a language directory. Group by the
    // package immediately below that root rather than by the first two path
    // components (which would collapse every python/vendor package together).
    let count = if let Some(index) = parts.iter().position(|part| *part == "node_modules") {
        index
            + if parts
                .get(index + 1)
                .is_some_and(|name| name.starts_with('@'))
            {
                3
            } else {
                2
            }
    } else if let Some(index) = parts.iter().position(|part| *part == "vendor") {
        index + 2
    } else if let Some(index) = parts.iter().position(|part| *part == "site-packages") {
        index + 2
    } else if let Some(index) = parts.iter().position(|part| *part == ".venv") {
        index + 2
    } else {
        2
    };
    let mut value = parts
        .iter()
        .take(count)
        .copied()
        .collect::<Vec<_>>()
        .join("/");
    if !value.contains('/') && parts.len() == 1 {
        value = parts[0].into()
    }
    value
}

#[cfg(test)]
mod tests {
    use super::vendor_package;
    #[test]
    fn groups_nested_dependency_roots_by_package() {
        assert_eq!(
            vendor_package("python/vendor/acme_http/library.py"),
            "python/vendor/acme_http"
        );
        assert_eq!(
            vendor_package("vendor/acme_http/library.py"),
            "vendor/acme_http"
        );
        assert_eq!(
            vendor_package("js/node_modules/lodash/index.js"),
            "js/node_modules/lodash"
        );
        assert_eq!(
            vendor_package("js/node_modules/@scope/tool/index.js"),
            "js/node_modules/@scope/tool"
        );
        assert_eq!(
            vendor_package("python/.venv/lib/site-packages/requests/api.py"),
            "python/.venv/lib/site-packages/requests"
        );
    }
}
