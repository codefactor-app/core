use crate::config::{Config, Threshold};
use crate::ir::FileModel;
use crate::metrics::{AggregateGroup, MetricValue};
use std::collections::BTreeSet;

pub use crate::diagnostics::{Severity, Violation};

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
        &mut BaselineTracker::new(baseline),
        &Suppressions::from_files(files, sources),
    )
}

/// Baseline lookups that also record which entries were used, so a report can
/// show how much it hides and which entries no longer match anything.
pub struct BaselineTracker<'a> {
    entries: &'a BTreeSet<String>,
    matched: BTreeSet<String>,
    hits: usize,
}
impl<'a> BaselineTracker<'a> {
    pub fn new(entries: &'a BTreeSet<String>) -> Self {
        Self {
            entries,
            matched: BTreeSet::new(),
            hits: 0,
        }
    }
    /// True when the baseline hides this finding; the match is recorded.
    pub fn hides(&mut self, violation: &Violation) -> bool {
        let fingerprint = violation.fingerprint();
        if self.entries.contains(&fingerprint) {
            self.hits += 1;
            self.matched.insert(fingerprint);
            true
        } else {
            false
        }
    }
    pub fn suppressed(&self) -> usize {
        self.hits
    }
    pub fn stale(&self) -> usize {
        self.entries.len().saturating_sub(self.matched.len())
    }
}

pub(crate) fn evaluate_with_suppressions(
    metrics: &[MetricValue],
    files: &[FileModel],
    aggregates: &[AggregateGroup],
    config: &Config,
    baseline: &mut BaselineTracker<'_>,
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
    let entity_limits = config
        .limits
        .by_entity_glob
        .iter()
        .filter_map(|limit| {
            crate::config::entity_glob(&limit.glob)
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
        let Some(limit) = effective_limit(metric, file, config, paths, &entity_limits) else {
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
            if suppressions.contains(&v) {
                continue;
            }
            if baseline.hides(&v) {
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
                if let Some(l) = config
                    .limits
                    .aggregates
                    .get(&key)
                    .filter(|l| l.enabled != Some(false))
                {
                    if let Some(v) = check(
                        key,
                        "aggregate".into(),
                        "".into(),
                        0,
                        group.name.clone(),
                        value,
                        l,
                    ) {
                        if !baseline.hides(&v) {
                            out.push(v);
                        }
                    }
                }
            }
        }
    }
    for v in crate::algorithmic::violations(files, config) {
        if !suppressions.contains(&v) && !baseline.hides(&v) {
            out.push(v);
        }
    }
    out.sort_by(|a, b| {
        (&a.file, a.line, &a.rule, &a.entity).cmp(&(&b.file, b.line, &b.rule, &b.entity))
    });
    out
}

/// Apply numeric cycle thresholds and legacy file baselines first, then emit one SCC.
pub(crate) fn group_module_cycle_violations(
    violations: &mut Vec<Violation>,
    cycles: Vec<crate::architecture::ModuleCycleDiagnostic>,
    baseline: &mut BaselineTracker<'_>,
) {
    let mut candidates = Vec::new();
    violations.retain(|violation| {
        if violation.rule == "module_cycle" {
            candidates.push(violation.clone());
            false
        } else {
            true
        }
    });
    let mut candidates_by_file = std::collections::BTreeMap::<&str, Vec<&Violation>>::new();
    for candidate in &candidates {
        candidates_by_file
            .entry(candidate.file.as_str())
            .or_default()
            .push(candidate);
    }
    for cycle in cycles {
        let Some(mut representative) = cycle
            .files
            .iter()
            .filter_map(|file| candidates_by_file.get(file.as_str()))
            .flat_map(|candidates| candidates.iter().copied())
            .min_by(|a, b| {
                b.severity
                    .cmp(&a.severity)
                    .then_with(|| (&a.file, a.line, &a.entity).cmp(&(&b.file, b.line, &b.entity)))
            })
            .cloned()
        else {
            continue;
        };
        representative.module_cycle = Some(cycle);
        if !baseline.hides(&representative) {
            violations.push(representative);
        }
    }
    violations.sort_by(|a, b| {
        (&a.file, a.line, &a.rule, &a.entity).cmp(&(&b.file, b.line, &b.rule, &b.entity))
    });
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
        module_cycle: None,
    })
}

fn effective_limit(
    m: &MetricValue,
    file: &FileModel,
    config: &Config,
    paths: &[&crate::config::PathLimits],
    entity_limits: &[(&crate::config::EntityGlobLimits, globset::GlobMatcher)],
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
    for (item, matcher) in entity_limits {
        if matcher.is_match(m.entity.as_ref()) {
            if let Some(limit_override) = item.metrics.get(m.id.as_ref()) {
                overlay(&mut limit, limit_override);
            }
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
    if limit.enabled == Some(false) || (limit.warn.is_none() && limit.error.is_none()) {
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
    if override_.enabled.is_some() {
        base.enabled = override_.enabled
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
                    module_cycle: None,
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
            module_cycle: None,
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

#[derive(Debug, thiserror::Error)]
pub enum BaselineError {
    #[error("baseline file not found: {path}")]
    NotFound { path: std::path::PathBuf },
    #[error("cannot read baseline {path}: {source}")]
    Unreadable {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("invalid baseline {path}: {source}")]
    Invalid {
        path: std::path::PathBuf,
        source: serde_json::Error,
    },
}

pub fn read_baseline(path: &std::path::Path) -> Result<BTreeSet<String>, BaselineError> {
    let bytes = std::fs::read(path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            BaselineError::NotFound { path: path.into() }
        } else {
            BaselineError::Unreadable {
                path: path.into(),
                source,
            }
        }
    })?;
    serde_json::from_slice::<BTreeSet<String>>(&bytes).map_err(|source| BaselineError::Invalid {
        path: path.into(),
        source,
    })
}

/// Compatibility helper for callers that do not need to distinguish read errors.
pub fn load_baseline(path: &std::path::Path) -> BTreeSet<String> {
    read_baseline(path).unwrap_or_default()
}
pub fn write_baseline(path: &std::path::Path, violations: &[Violation]) -> std::io::Result<()> {
    let entries = violations
        .iter()
        .map(Violation::fingerprint)
        .collect::<BTreeSet<_>>();
    let data = serde_json::to_vec_pretty(&entries).unwrap_or_default();
    std::fs::write(path, data)
}

#[cfg(test)]
mod baseline_tests {
    use super::*;

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "codefactor-read-baseline-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn baseline_reader_accepts_existing_arrays_and_deduplicates_fingerprints() {
        let fixture = Fixture::new();
        let path = fixture.0.join("baseline.json");
        std::fs::write(&path, r#"["second", "first", "second"]"#).unwrap();
        assert_eq!(
            read_baseline(&path).unwrap(),
            BTreeSet::from(["first".into(), "second".into()])
        );
        std::fs::write(&path, "[]").unwrap();
        assert!(read_baseline(&path).unwrap().is_empty());
    }

    #[test]
    fn baseline_reader_distinguishes_missing_unreadable_and_invalid_files() {
        let fixture = Fixture::new();
        let path = fixture.0.join("baseline.json");
        assert!(matches!(
            read_baseline(&path),
            Err(BaselineError::NotFound { path: missing }) if missing == path
        ));
        assert!(matches!(
            read_baseline(&fixture.0),
            Err(BaselineError::Unreadable { .. })
        ));
        for bytes in [
            b"not json".as_slice(),
            b"{}",
            b"null",
            b"[1]",
            b"[\"\xff\"]",
        ] {
            std::fs::write(&path, bytes).unwrap();
            assert!(matches!(
                read_baseline(&path),
                Err(BaselineError::Invalid { .. })
            ));
        }
    }
}

#[cfg(test)]
mod metric_pool_contract {
    use crate::{config::Config, ir::*, metrics::*};
    use std::sync::Arc;
    #[test]
    fn metric_pool_shares_rows_without_changing_json_aggregates_or_rules() {
        let mut file = FileModel {
            path: "long/module/source.ts".into(),
            language: "typescript".into(),
            module: "long.module.source".into(),
            ..Default::default()
        };
        for index in 0..100 {
            file.functions.push(FunctionModel {
                fqn: format!("long.module.source.function_{index}"),
                range: SourceRange {
                    file: file.path.clone().into(),
                    start_line: 1,
                    end_line: 5,
                    ..Default::default()
                },
                ..Default::default()
            });
        }
        let mut config = Config::default();
        config.limits.global.insert(
            "function_lines".into(),
            crate::config::Threshold {
                warn: Some(3.0),
                error: Some(8.0),
                enabled: None,
            },
        );
        let registry = MetricRegistry::new();
        let values = registry.calculate(&file, &config);
        assert!(values
            .iter()
            .all(|value| Arc::ptr_eq(&value.path, &values[0].path)));
        let function_values = values
            .iter()
            .filter(|value| value.entity.as_ref() == "long.module.source.function_0")
            .collect::<Vec<_>>();
        assert_eq!(function_values.len(), 6);
        assert!(function_values
            .iter()
            .all(|value| Arc::ptr_eq(&value.entity, &function_values[0].entity)));
        let serialized = serde_json::to_string(&values).unwrap();
        let mut decoded: Vec<MetricValue> = serde_json::from_str(&serialized).unwrap();
        registry.intern_values(&mut decoded);
        assert_eq!(serde_json::to_string(&decoded).unwrap(), serialized);
        assert!(decoded
            .iter()
            .all(|value| Arc::ptr_eq(&value.path, &decoded[0].path)));
        let files = [file];
        let aggregated = aggregates(&files, &decoded, 10);
        let project = aggregated
            .iter()
            .find(|group| group.name == "project")
            .unwrap();
        assert_eq!(project.metrics["function_lines"].median, 5.0);
        let violations = crate::rules::evaluate(
            &decoded,
            &files,
            &aggregated,
            &config,
            &Default::default(),
            &Default::default(),
        );
        assert_eq!(
            violations
                .iter()
                .filter(|value| value.rule == "function_lines")
                .count(),
            100
        );
    }
}
