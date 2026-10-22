//! Cost settings and deterministic override precedence.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UnknownPolicy {
    #[default]
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ComplexityLimits {
    pub warn: Option<String>,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AlgorithmicOverride {
    pub enabled: Option<bool>,
    pub heuristics: Option<bool>,
    pub unknown: Option<UnknownPolicy>,
    pub limits: ComplexityLimits,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AlgorithmicPathOverride {
    pub glob: String,
    pub enabled: Option<bool>,
    pub heuristics: Option<bool>,
    pub unknown: Option<UnknownPolicy>,
    pub limits: ComplexityLimits,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AlgorithmicConfig {
    pub enabled: bool,
    pub heuristics: bool,
    pub unknown: UnknownPolicy,
    pub limits: ComplexityLimits,
    pub by_language: BTreeMap<String, AlgorithmicOverride>,
    pub by_path: Vec<AlgorithmicPathOverride>,
    pub by_entity: BTreeMap<String, AlgorithmicOverride>,
}
impl Default for AlgorithmicConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            heuristics: true,
            unknown: UnknownPolicy::Info,
            limits: ComplexityLimits::default(),
            by_language: BTreeMap::new(),
            by_path: Vec::new(),
            by_entity: BTreeMap::new(),
        }
    }
}
#[derive(Clone)]
pub struct Settings {
    pub enabled: bool,
    pub heuristics: bool,
    pub unknown: UnknownPolicy,
    pub limits: ComplexityLimits,
}
impl AlgorithmicConfig {
    pub fn effective(&self, language: &str, path: &str, entity: &str) -> Settings {
        self.effective_with_paths(
            language,
            entity,
            self.by_path.iter().filter(|o| {
                globset::Glob::new(&o.glob)
                    .ok()
                    .is_some_and(|g| g.compile_matcher().is_match(path))
            }),
        )
    }
    pub(super) fn compiled_paths(&self) -> Vec<(&AlgorithmicPathOverride, globset::GlobMatcher)> {
        self.by_path
            .iter()
            .filter_map(|o| {
                globset::Glob::new(&o.glob)
                    .ok()
                    .map(|g| (o, g.compile_matcher()))
            })
            .collect()
    }
    pub(super) fn effective_with_paths<'a>(
        &self,
        language: &str,
        entity: &str,
        paths: impl Iterator<Item = &'a AlgorithmicPathOverride>,
    ) -> Settings {
        let mut s = Settings {
            enabled: self.enabled,
            heuristics: self.heuristics,
            unknown: self.unknown.clone(),
            limits: self.limits.clone(),
        };
        let overlay = |s: &mut Settings,
                       enabled: Option<bool>,
                       heuristics: Option<bool>,
                       unknown: &Option<UnknownPolicy>,
                       limits: &ComplexityLimits| {
            if let Some(v) = enabled {
                s.enabled = v;
            }
            if let Some(v) = heuristics {
                s.heuristics = v;
            }
            if let Some(v) = unknown {
                s.unknown = v.clone();
            }
            if limits.warn.is_some() {
                s.limits.warn = limits.warn.clone();
            }
            if limits.error.is_some() {
                s.limits.error = limits.error.clone();
            }
        };
        if let Some(o) = self.by_language.get(language) {
            overlay(&mut s, o.enabled, o.heuristics, &o.unknown, &o.limits);
        }
        for o in paths {
            overlay(&mut s, o.enabled, o.heuristics, &o.unknown, &o.limits);
        }
        if let Some(o) = self.by_entity.get(entity) {
            overlay(&mut s, o.enabled, o.heuristics, &o.unknown, &o.limits);
        }
        s
    }
}
