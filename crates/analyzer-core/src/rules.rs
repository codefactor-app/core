use crate::config::{Config, Threshold};
use crate::ir::FileModel;
use crate::metrics::{AggregateGroup, MetricValue};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Warn,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Violation {
    pub rule: String,
    pub severity: Severity,
    pub file: String,
    pub line: usize,
    pub entity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub algorithmic_complexity: Option<crate::algorithmic::ComplexityDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architecture: Option<crate::architecture::ArchitectureDiagnostic>,
}
impl Violation {
    pub fn value_text(&self) -> String {
        if self.architecture.is_some() {
            return "—".into();
        }
        self.algorithmic_complexity
            .as_ref()
            .map(|d| {
                d.estimate
                    .formula
                    .clone()
                    .unwrap_or_else(|| d.estimate.status.label().into())
            })
            .unwrap_or_else(|| self.value.map(|v| format!("{v:.2}")).unwrap_or_default())
    }
    pub fn limit_text(&self) -> String {
        if self.architecture.is_some() {
            return "—".into();
        }
        self.algorithmic_complexity
            .as_ref()
            .map(|d| {
                d.threshold
                    .clone()
                    .unwrap_or_else(|| "complete analysis required".into())
            })
            .unwrap_or_else(|| self.limit.map(|v| format!("{v:.2}")).unwrap_or_default())
    }
    pub fn message(&self) -> String {
        if let Some(d) = &self.architecture {
            return format!("{} {}", d.fact, d.hypothesis);
        }
        self.algorithmic_complexity
            .as_ref()
            .map(|d| d.message.clone())
            .unwrap_or_else(|| {
                format!(
                    "{} is {} (limit {})",
                    self.rule,
                    self.value.map(|v| v.to_string()).unwrap_or_default(),
                    self.limit.map(|v| v.to_string()).unwrap_or_default()
                )
            })
    }
    pub fn fingerprint(&self) -> String {
        if let Some(d) = &self.architecture {
            use sha2::{Digest, Sha256};
            let input = serde_json::to_vec(&(&self.rule, &d.participants))
                .expect("serializable participants");
            return format!("architecture:{}:{:x}", self.rule, Sha256::digest(input));
        }
        format!(
            "{}:{}:{}:{}",
            self.file,
            self.entity,
            self.rule,
            match self.severity {
                Severity::Warn => "warn",
                Severity::Error => "error",
            }
        )
    }
}

pub fn evaluate(
    metrics: &[MetricValue],
    files: &[FileModel],
    aggregates: &[AggregateGroup],
    config: &Config,
    baseline: &BTreeSet<String>,
    sources: &std::collections::BTreeMap<String, String>,
) -> Vec<Violation> {
    evaluate_with_suppressions(
        metrics,
        files,
        aggregates,
        config,
        baseline,
        &Suppressions::from_files(files, sources),
    )
}

pub(crate) fn evaluate_with_suppressions(
    metrics: &[MetricValue],
    files: &[FileModel],
    aggregates: &[AggregateGroup],
    config: &Config,
    baseline: &BTreeSet<String>,
    suppressions: &Suppressions,
) -> Vec<Violation> {
    let mut out = Vec::new();
    let path_limits = config
        .limits
        .by_path
        .iter()
        .filter_map(|limit| {
            globset::Glob::new(&limit.glob)
                .ok()
                .map(|glob| (limit, glob.compile_matcher()))
        })
        .collect::<Vec<_>>();
    let by_path = files
        .iter()
        .map(|f| {
            (
                f.path.as_str(),
                (
                    f,
                    path_limits
                        .iter()
                        .filter(|(_, matcher)| matcher.is_match(&f.path))
                        .map(|(limit, _)| *limit)
                        .collect::<Vec<_>>(),
                ),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    for metric in metrics {
        let Some((file, paths)) = by_path.get(metric.path.as_ref()) else {
            continue;
        };
        if file.vendor {
            continue;
        }
        let Some(limit) = effective_limit(metric, file, config, paths) else {
            continue;
        };
        if let Some(v) = check(
            metric.id.to_string(),
            metric.scope.to_string(),
            metric.path.to_string(),
            metric.line,
            metric.entity.to_string(),
            metric.value,
            &limit,
        ) {
            if baseline.contains(&v.fingerprint()) {
                continue;
            }
            if suppressions.contains(&v) {
                continue;
            }
            out.push(v);
        }
    }
    for group in aggregates {
        for (id, dist) in &group.metrics {
            for (suffix, value) in [
                ("avg", dist.avg),
                ("median", dist.median),
                ("max", dist.max),
                ("p90", dist.p90),
                ("p95", dist.p95),
            ] {
                let key = format!("{id}.{suffix}");
                if let Some(l) = config.limits.aggregates.get(&key) {
                    if let Some(v) = check(
                        key,
                        "aggregate".into(),
                        "".into(),
                        0,
                        group.name.clone(),
                        value,
                        l,
                    ) {
                        if !baseline.contains(&v.fingerprint()) {
                            out.push(v);
                        }
                    }
                }
            }
        }
    }
    for v in crate::algorithmic::violations(files, config) {
        if !baseline.contains(&v.fingerprint()) && !suppressions.contains(&v) {
            out.push(v);
        }
    }
    out.sort_by(|a, b| {
        (&a.file, a.line, &a.rule, &a.entity).cmp(&(&b.file, b.line, &b.rule, &b.entity))
    });
    out
}

fn check(
    rule: String,
    scope: String,
    file: String,
    line: usize,
    entity: String,
    value: f64,
    limit: &Threshold,
) -> Option<Violation> {
    let (severity, bound) = if let Some(e) = limit.error.filter(|e| value > *e) {
        (Severity::Error, e)
    } else if let Some(w) = limit.warn.filter(|w| value > *w) {
        (Severity::Warn, w)
    } else {
        return None;
    };
    Some(Violation {
        rule,
        severity,
        file,
        line,
        entity,
        value: Some(value),
        limit: Some(bound),
        scope,
        algorithmic_complexity: None,
        architecture: None,
    })
}

fn effective_limit(
    m: &MetricValue,
    file: &FileModel,
    config: &Config,
    paths: &[&crate::config::PathLimits],
) -> Option<Threshold> {
    let mut limit = config
        .limits
        .global
        .get(m.id.as_ref())
        .cloned()
        .unwrap_or_default();
    if let Some(lang) = config
        .limits
        .by_language
        .get(&file.language)
        .and_then(|x| x.get(m.id.as_ref()))
    {
        overlay(&mut limit, lang);
    }
    for item in paths {
        if let Some(path) = item.metrics.get(m.id.as_ref()) {
            overlay(&mut limit, path);
        }
    }
    if let Some(entity) = config
        .limits
        .by_entity
        .get(m.entity.as_ref())
        .and_then(|x| x.get(m.id.as_ref()))
    {
        overlay(&mut limit, entity);
    }
    if limit.warn.is_none() && limit.error.is_none() {
        None
    } else {
        Some(limit)
    }
}
fn overlay(base: &mut Threshold, override_: &Threshold) {
    if override_.warn.is_some() {
        base.warn = override_.warn
    }
    if override_.error.is_some() {
        base.error = override_.error
    }
}
/// Keep only actual directives, rather than rebuilding all source lines for each violation.
#[derive(Default)]
pub(crate) struct Suppressions(
    std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<usize, std::collections::BTreeSet<String>>,
    >,
);
impl Suppressions {
    pub(crate) fn from_files(
        files: &[FileModel],
        sources: &std::collections::BTreeMap<String, String>,
    ) -> Self {
        let mut out = Self::default();
        for file in files {
            if let Some(source) = sources.get(&file.path) {
                out.extend(Self::from_file(file, source));
            }
        }
        out
    }
    pub(crate) fn from_file(file: &FileModel, source: &str) -> Self {
        let Some(comments) = &file.comment_ranges else {
            return Self::from_source(&file.path, source);
        };
        if !source.contains("analyzer-ignore:") {
            return Self::default();
        }
        let lines = source.lines().collect::<Vec<_>>();
        let mut directives =
            std::collections::BTreeMap::<usize, std::collections::BTreeSet<String>>::new();
        for comment in comments {
            for line_number in comment.start_line..=comment.end_line.min(lines.len()) {
                let Some(line) = line_number
                    .checked_sub(1)
                    .and_then(|index| lines.get(index))
                else {
                    continue;
                };
                let start = if line_number == comment.start_line {
                    comment.start_column.saturating_sub(1)
                } else {
                    0
                };
                let end = if line_number == comment.end_line {
                    comment.end_column.saturating_sub(1).min(line.len())
                } else {
                    line.len()
                };
                if let Some(text) = line.get(start..end) {
                    if let Some((_, rules)) = text.split_once("analyzer-ignore:") {
                        directives
                            .entry(line_number)
                            .or_default()
                            .extend(directive_tokens(rules));
                    }
                }
            }
        }
        Self(std::collections::BTreeMap::from([(
            file.path.clone(),
            directives,
        )]))
    }
    #[cfg(test)]
    pub(crate) fn new(sources: &std::collections::BTreeMap<String, String>) -> Self {
        let mut out = Self::default();
        for (path, source) in sources {
            out.extend(Self::from_source(path, source));
        }
        out
    }
    pub(crate) fn from_source(path: &str, source: &str) -> Self {
        let mut out = Self::default();
        if source.contains("analyzer-ignore:") {
            let lines = source
                .lines()
                .enumerate()
                .filter_map(|(i, line)| {
                    let trimmed = line.trim_start();
                    // Strategies without syntax comments support full-line directives only.
                    if !["#", "//", "/*", "*"]
                        .iter()
                        .any(|marker| trimmed.starts_with(marker))
                    {
                        return None;
                    }
                    line.split_once("analyzer-ignore:")
                        .map(|(_, directives)| (i + 1, directive_tokens(directives).collect()))
                })
                .collect();
            out.0.insert(path.to_owned(), lines);
        }
        out
    }
    pub(crate) fn extend(&mut self, other: Self) {
        self.0.extend(other.0);
    }
    pub(crate) fn contains(&self, v: &Violation) -> bool {
        if v.line == 0 {
            return false;
        }
        self.0.get(v.file.as_str()).is_some_and(|lines| {
            lines
                .range(v.line.saturating_sub(1).max(1)..=v.line)
                .any(|(_, rules)| rules.contains(v.rule.as_str()) || rules.contains("all"))
        })
    }
}
fn directive_tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| c == ',' || c.is_whitespace() || c == '*' || c == '/')
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
}
#[cfg(test)]
fn suppressed(v: &Violation, source: &str) -> bool {
    let lines: Vec<&str> = source.lines().collect();
    let start = v.line.saturating_sub(2);
    let end = v.line.min(lines.len());
    lines.get(start..end).unwrap_or(&[]).iter().any(|line| {
        if !["#", "//", "/*", "*"]
            .iter()
            .any(|marker| line.trim_start().starts_with(marker))
        {
            return false;
        }
        line.split_once("analyzer-ignore:")
            .is_some_and(|(_, directives)| {
                directives
                    .split(|c: char| c == ',' || c.is_whitespace())
                    .any(|token| token == v.rule || token == "all")
            })
    })
}

#[cfg(test)]
mod suppression_tests {
    use super::*;
    #[test]
    fn indexed_suppressions_preserve_source_line_boundaries() {
        let source =
            "// analyzer-ignore: function_params\nfn a(){}\n// analyzer-ignore: all\nfn b(){}\n";
        let sources = std::collections::BTreeMap::from([("a.py".into(), source.into())]);
        let index = Suppressions::new(&sources);
        // The scan frees source buffers before evaluating numeric and symbolic rules.
        drop(sources);
        for line in 0..9 {
            for rule in ["function_params", "function_lines", "params"] {
                let violation = Violation {
                    rule: rule.into(),
                    severity: Severity::Warn,
                    file: "a.py".into(),
                    line,
                    entity: "a".into(),
                    value: Some(1.0),
                    limit: Some(0.0),
                    scope: "function".into(),
                    algorithmic_complexity: None,
                    architecture: None,
                };
                assert_eq!(index.contains(&violation), suppressed(&violation, source));
            }
        }
    }
    #[test]
    fn ignore_directive_matches_whole_rule_tokens() {
        let violation = Violation {
            rule: "function_params".into(),
            severity: Severity::Warn,
            file: "a.py".into(),
            line: 2,
            entity: "f".into(),
            value: Some(5.0),
            limit: Some(4.0),
            scope: "function".into(),
            algorithmic_complexity: None,
            architecture: None,
        };
        assert!(!suppressed(
            &violation,
            "# analyzer-ignore: function_params_extra\ndef f():\n"
        ));
        assert!(!suppressed(
            &violation,
            "# analyzer-ignore: other_allowed\ndef f():\n"
        ));
        assert!(suppressed(
            &violation,
            "# analyzer-ignore: other, function_params\ndef f():\n"
        ));
        assert!(suppressed(&violation, "# analyzer-ignore: all\ndef f():\n"));
    }
}

pub fn load_baseline(path: &std::path::Path) -> BTreeSet<String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default()
        .into_iter()
        .collect()
}
pub fn write_baseline(path: &std::path::Path, violations: &[Violation]) -> std::io::Result<()> {
    let entries = violations
        .iter()
        .map(Violation::fingerprint)
        .collect::<BTreeSet<_>>();
    let data = serde_json::to_vec_pretty(&entries).unwrap_or_default();
    std::fs::write(path, data)
}
