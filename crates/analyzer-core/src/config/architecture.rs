use super::schema::KeySpec;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    #[default]
    Strict,
    Extended,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ArchitectureConfig {
    pub enabled: bool,
    pub profile: Profile,
    pub exclude: Vec<String>,
    pub disabled_rules: Vec<String>,
    pub rules: BTreeMap<String, RuleConfig>,
    pub layers: Vec<Layer>,
    /// Explicit file-level component groups; unmatched files retain native modules.
    pub modules: Vec<Layer>,
    /// Maximum namespace components for native modules, by canonical language id.
    pub module_depth: BTreeMap<String, usize>,
    pub forbidden_dependencies: Vec<Boundary>,
    pub composition_roots: Vec<String>,
    pub max_evidence: usize,
}
impl Default for ArchitectureConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            profile: Profile::Strict,
            exclude: [
                "**/test/**",
                "**/tests/**",
                "**/testing/**",
                "**/__tests__/**",
                "**/integration-tests/**",
                "**/fixtures/**",
                "**/__fixtures__/**",
                "**/generated/**",
                "**/migrations/**",
                "**/*.test.*",
                "**/*.spec.*",
            ]
            .map(str::to_owned)
            .to_vec(),
            disabled_rules: vec![],
            rules: BTreeMap::new(),
            layers: vec![],
            modules: vec![],
            module_depth: BTreeMap::new(),
            forbidden_dependencies: vec![],
            composition_roots: vec![],
            max_evidence: 50,
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuleConfig {
    pub min_tokens: Option<usize>,
    pub min_statements: Option<usize>,
    pub min_methods: Option<usize>,
    pub min_group_methods: Option<usize>,
    pub min_sites: Option<usize>,
    pub min_alternatives: Option<usize>,
    pub max_usage_ratio: Option<f64>,
    pub instability_delta: Option<f64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    pub name: String,
    pub selectors: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Boundary {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub principle: Option<String>,
}

impl ArchitectureConfig {
    pub fn rule(&self, id: &str) -> RuleConfig {
        self.rules.get(id).cloned().unwrap_or_default()
    }
    pub(crate) fn validate_modules(&self) -> Result<(), String> {
        for (language, depth) in &self.module_depth {
            if module_separator(language).is_none() {
                return Err(format!(
                    "architecture.module_depth: unsupported language {language}"
                ));
            }
            if *depth == 0 {
                return Err(format!(
                    "architecture.module_depth.{language}: depth must be a positive integer"
                ));
            }
        }
        let mut module_names = BTreeSet::new();
        for module in &self.modules {
            if module.name.trim().is_empty()
                || !module_names.insert(&module.name)
                || module.selectors.is_empty()
            {
                return Err("modules require unique nonempty names and selectors".into());
            }
            for selector in &module.selectors {
                validate_module_selector(selector)?;
            }
        }
        Ok(())
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.max_evidence == 0 || self.max_evidence > 500 {
            return Err("max_evidence must be between 1 and 500".into());
        }
        for pattern in &self.exclude {
            globset::Glob::new(pattern).map_err(|e| e.to_string())?;
        }
        for id in self.disabled_rules.iter().chain(self.rules.keys()) {
            if !crate::architecture::spec::RULES.iter().any(|r| r.id == id) {
                return Err(format!("unknown architecture rule {id}"));
            }
        }
        for (id, r) in &self.rules {
            for value in [
                r.min_tokens,
                r.min_statements,
                r.min_methods,
                r.min_group_methods,
                r.min_sites,
                r.min_alternatives,
            ]
            .into_iter()
            .flatten()
            {
                if value == 0 {
                    return Err(format!("{id}: thresholds must be positive"));
                }
            }
            for value in [r.max_usage_ratio, r.instability_delta]
                .into_iter()
                .flatten()
            {
                if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                    return Err(format!("{id}: ratio must be between 0 and 1"));
                }
            }
            if id == "arch_dry"
                && (r.min_tokens.is_some_and(|n| n < 30) || r.min_statements.is_some_and(|n| n < 2))
            {
                return Err("arch_dry requires min_tokens >= 30 and min_statements >= 2".into());
            }
            let allowed: &[&str] = match id.as_str() {
                "arch_dry" => &["min_tokens", "min_statements"],
                "arch_srp" => &["min_methods", "min_group_methods"],
                "arch_ocp" | "arch_factory" => &["min_sites", "min_alternatives"],
                "arch_isp" => &["min_methods", "max_usage_ratio"],
                "arch_stability" => &["instability_delta"],
                _ => &[],
            };
            let supplied = [
                ("min_tokens", r.min_tokens.is_some()),
                ("min_statements", r.min_statements.is_some()),
                ("min_methods", r.min_methods.is_some()),
                ("min_group_methods", r.min_group_methods.is_some()),
                ("min_sites", r.min_sites.is_some()),
                ("min_alternatives", r.min_alternatives.is_some()),
                ("max_usage_ratio", r.max_usage_ratio.is_some()),
                ("instability_delta", r.instability_delta.is_some()),
            ];
            if let Some((key, _)) = supplied
                .into_iter()
                .find(|(key, set)| *set && !allowed.contains(key))
            {
                return Err(format!("{id}: {key} is not applicable"));
            }
        }
        self.validate_modules()?;
        let mut names = BTreeSet::new();
        for l in &self.layers {
            if l.name.is_empty() || !names.insert(&l.name) || l.selectors.is_empty() {
                return Err("layers require unique nonempty names and selectors".into());
            }
            for s in &l.selectors {
                validate_selector(s)?;
            }
        }
        for s in &self.composition_roots {
            validate_selector(s)?;
        }
        for b in &self.forbidden_dependencies {
            if !names.contains(&b.from) || !names.contains(&b.to) || b.from == b.to {
                return Err("boundary must name two different configured layers".into());
            }
        }
        Ok(())
    }
}

pub(crate) fn module_separator(language: &str) -> Option<&'static str> {
    match language {
        "php" => Some("\\"),
        "python" => Some("."),
        "rust" => Some("::"),
        "javascript" | "typescript" => Some("/"),
        _ => None,
    }
}

fn validate_selector(s: &str) -> Result<(), String> {
    let (kind, value) = s.split_once(':').ok_or("expected kind:value selector")?;
    if value.is_empty() || !["dir", "glob", "ns", "pkg", "mod", "class", "file"].contains(&kind) {
        return Err(format!("unsupported selector {s}"));
    }
    if kind == "glob" {
        globset::Glob::new(value).map_err(|e| e.to_string())?;
    }
    Ok(())
}
fn validate_module_selector(selector: &str) -> Result<(), String> {
    validate_selector(selector)?;
    if selector.starts_with("class:") {
        return Err(
            "module groups require whole-file selectors (ns, mod, dir, glob, file, pkg)".into(),
        );
    }
    Ok(())
}
pub(super) fn keys() -> Vec<KeySpec> {
    let mut keys = Vec::new();
    let mapping: crate::config::Validator = Arc::new(|v| {
        if v.is_mapping() {
            Ok(())
        } else {
            Err("expected mapping".into())
        }
    });
    let list: crate::config::Validator = Arc::new(|v| {
        if v.is_sequence() {
            Ok(())
        } else {
            Err("expected list".into())
        }
    });
    let string: crate::config::Validator = Arc::new(|v| {
        if v.is_string() {
            Ok(())
        } else {
            Err("expected string".into())
        }
    });
    keys.push(KeySpec::new(
        "architecture",
        "Deterministic advisory architecture analysis",
        mapping.clone(),
    ));
    keys.push(KeySpec::new(
        "architecture.module_depth",
        "Maximum native module namespace depth by language",
        mapping.clone(),
    ));
    for language in ["php", "python", "rust", "javascript", "typescript"] {
        keys.push(KeySpec::new(
            format!("architecture.module_depth.{language}"),
            "Maximum native module namespace depth",
            Arc::new(|value| {
                if value
                    .as_u64()
                    .and_then(|depth| usize::try_from(depth).ok())
                    .is_some_and(|depth| depth > 0)
                {
                    Ok(())
                } else {
                    Err("depth must be a positive integer".into())
                }
            }),
        ));
    }
    keys.push(KeySpec::new(
        "architecture.enabled",
        "Enable architecture analysis",
        Arc::new(|v| {
            if v.is_bool() {
                Ok(())
            } else {
                Err("expected boolean".into())
            }
        }),
    ));
    keys.push(KeySpec::new(
        "architecture.profile",
        "Architecture analysis profile",
        Arc::new(|v| {
            if v.as_str()
                .is_some_and(|s| ["strict", "extended"].contains(&s))
            {
                Ok(())
            } else {
                Err("expected strict or extended".into())
            }
        }),
    ));
    keys.push(KeySpec::new(
        "architecture.max_evidence",
        "Maximum evidence per finding",
        Arc::new(|v| {
            if v.as_u64().is_some_and(|n| (1..=500).contains(&n)) {
                Ok(())
            } else {
                Err("max_evidence must be between 1 and 500".into())
            }
        }),
    ));
    keys.push(KeySpec::new(
        "architecture.rules",
        "Per-rule thresholds",
        mapping.clone(),
    ));
    for key in [
        "architecture.exclude",
        "architecture.disabled_rules",
        "architecture.layers",
        "architecture.modules",
        "architecture.forbidden_dependencies",
        "architecture.composition_roots",
    ] {
        keys.push(KeySpec::new(
            key,
            "Architecture configuration list",
            list.clone(),
        ));
    }
    keys.push(KeySpec::new(
        "architecture.exclude.*",
        "Excluded path glob",
        Arc::new(|v| {
            globset::Glob::new(v.as_str().ok_or("expected glob string")?)
                .map(|_| ())
                .map_err(|e| e.to_string())
        }),
    ));
    keys.push(KeySpec::new(
        "architecture.disabled_rules.*",
        "Disabled rule ID",
        Arc::new(|v| {
            let id = v.as_str().ok_or("expected rule ID string")?;
            if crate::architecture::spec::RULES.iter().any(|r| r.id == id) {
                Ok(())
            } else {
                Err(format!("unknown architecture rule {id}"))
            }
        }),
    ));
    for key in [
        "architecture.composition_roots.*",
        "architecture.layers.*.selectors.*",
    ] {
        keys.push(KeySpec::new(
            key,
            "Architecture selector",
            Arc::new(|v| validate_selector(v.as_str().ok_or("expected selector string")?)),
        ));
    }
    for key in [
        "architecture.layers.*",
        "architecture.modules.*",
        "architecture.forbidden_dependencies.*",
    ] {
        keys.push(KeySpec::new(
            key,
            "Architecture declaration",
            mapping.clone(),
        ));
    }
    keys.push(KeySpec::new(
        "architecture.modules.*.name",
        "Unique module group name",
        Arc::new(|v| {
            if v.as_str().is_some_and(|s| !s.trim().is_empty()) {
                Ok(())
            } else {
                Err("expected nonempty module name".into())
            }
        }),
    ));
    keys.push(KeySpec::new(
        "architecture.modules.*.selectors",
        "Module group selectors",
        list.clone(),
    ));
    keys.push(KeySpec::new(
        "architecture.modules.*.selectors.*",
        "Whole-file architecture selector",
        Arc::new(|v| validate_module_selector(v.as_str().ok_or("expected selector string")?)),
    ));
    keys.push(KeySpec::new(
        "architecture.layers.*.name",
        "Unique layer name",
        Arc::new(|v| {
            if v.as_str().is_some_and(|s| !s.is_empty()) {
                Ok(())
            } else {
                Err("expected nonempty layer name".into())
            }
        }),
    ));
    keys.push(KeySpec::new(
        "architecture.layers.*.selectors",
        "Layer selectors",
        list,
    ));
    for key in [
        "architecture.forbidden_dependencies.*.from",
        "architecture.forbidden_dependencies.*.to",
    ] {
        keys.push(KeySpec::new(key, "Layer name", string.clone()));
    }
    keys.push(KeySpec::new(
        "architecture.forbidden_dependencies.*.principle",
        "Optional principle",
        Arc::new(|v| {
            if v.is_string() || v.is_null() {
                Ok(())
            } else {
                Err("expected string".into())
            }
        }),
    ));
    for rule in crate::architecture::spec::RULES {
        let scope = format!("architecture.rules.{}", rule.id);
        keys.push(KeySpec::new(
            &scope,
            "Rule-specific thresholds",
            mapping.clone(),
        ));
        let allowed: &[&str] = match rule.id {
            "arch_dry" => &["min_tokens", "min_statements"],
            "arch_srp" => &["min_methods", "min_group_methods"],
            "arch_ocp" | "arch_factory" => &["min_sites", "min_alternatives"],
            "arch_isp" => &["min_methods", "max_usage_ratio"],
            "arch_stability" => &["instability_delta"],
            _ => &[],
        };
        for field in allowed {
            let minimum = match (rule.id, *field) {
                ("arch_dry", "min_tokens") => 30,
                ("arch_dry", "min_statements") => 2,
                _ => 1,
            };
            let ratio = matches!(*field, "max_usage_ratio" | "instability_delta");
            keys.push(KeySpec::new(
                format!("{scope}.{field}"),
                "Rule-specific threshold",
                Arc::new(move |v| {
                    if v.is_null() {
                        return Ok(());
                    }
                    if ratio {
                        if v.as_f64()
                            .is_some_and(|n| n.is_finite() && (0.0..=1.0).contains(&n))
                        {
                            Ok(())
                        } else {
                            Err("ratio must be between 0 and 1".into())
                        }
                    } else if v.as_u64().is_some_and(|n| n >= minimum) {
                        Ok(())
                    } else {
                        Err(format!("threshold must be an integer >= {minimum}"))
                    }
                }),
            ));
        }
    }
    keys
}
