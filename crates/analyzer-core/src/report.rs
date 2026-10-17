use crate::analysis::{AnalysisError, Analyzer};
use crate::cache::Cache;
use crate::discovery::{discover, DiscoveredFile};
use crate::graph::{CallGraph, Direction, GraphFilter, GraphQuery};
use crate::ir::*;
use crate::metrics::{aggregates, AggregateGroup, MetricValue};
use crate::rules::{evaluate, load_baseline, Violation};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Summary {
    pub files: usize,
    pub functions: usize,
    pub types: usize,
    pub warnings: usize,
    pub errors: usize,
    pub vendor_files: usize,
    pub parse_errors: usize,
    pub cache_hits: usize,
    #[serde(default)]
    pub architecture_warnings: usize,
    pub parsed_files: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnalysisReport {
    pub version: String,
    pub schema_version: String,
    pub root: String,
    pub config_path: Option<String>,
    #[serde(default)]
    pub strategy_capabilities: BTreeMap<String, Capabilities>,
    pub summary: Summary,
    pub aggregates: Vec<AggregateGroup>,
    pub files: Vec<FileModel>,
    pub metrics: Vec<MetricValue>,
    pub violations: Vec<Violation>,
    pub parse_errors: Vec<ParseError>,
    pub graph: CallGraph,
    pub deferred_vendor: Vec<DiscoveredVendor>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolution_diagnostics: Vec<String>,
    #[serde(default)]
    pub architecture: crate::architecture::ArchitectureSummary,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiscoveredVendor {
    pub path: String,
    pub package: String,
    pub language: String,
}

impl AnalysisReport {
    pub fn graph_query(
        &self,
        selector: &str,
        direction: Direction,
        depth: Option<usize>,
        filter: &GraphFilter,
        config: &crate::config::Config,
    ) -> GraphQuery {
        self.graph.query(selector, direction, depth, filter, config)
    }
    pub fn file(&self, path: &str) -> Option<&FileModel> {
        self.files.iter().find(|f| f.path == path)
    }
    pub fn symbol(&self, id: &str) -> Option<(&FileModel, &FunctionModel)> {
        self.files.iter().find_map(|file| {
            file.functions
                .iter()
                .find(|f| {
                    f.fqn == id || format!("fn:{}:{}:{}", file.language, file.path, f.fqn) == id
                })
                .map(|f| (file, f))
        })
    }
    pub fn search(&self, query: &str) -> Vec<&FunctionModel> {
        let mut out = self
            .files
            .iter()
            .flat_map(|f| &f.functions)
            .filter_map(|f| crate::search::fuzzy_score(&f.fqn, query).map(|score| (score, f)))
            .collect::<Vec<_>>();
        out.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.fqn.cmp(&b.1.fqn)));
        out.into_iter().map(|(_, function)| function).collect()
    }
}

pub fn scan(analyzer: &Analyzer, root: &Path) -> Result<AnalysisReport, AnalysisError> {
    scan_with_baseline(analyzer, root, true)
}

pub fn scan_with_baseline(
    analyzer: &Analyzer,
    root: &Path,
    apply_baseline: bool,
) -> Result<AnalysisReport, AnalysisError> {
    let mut timings = ScanTimings::new();
    let canonical = std::fs::canonicalize(root)?;
    log::info!("scan started root={}", canonical.display());
    let discovered = discover(&canonical, &analyzer.config, &analyzer.registry);
    log::debug!("discovery found {} files", discovered.len());
    timings.mark("discovery");
    let scan_base = if canonical.is_file() {
        canonical.parent().unwrap_or(&canonical)
    } else {
        &canonical
    };
    let cache = if analyzer.config.cache.enabled {
        let path = PathBuf::from(&analyzer.config.cache.path);
        Some(Cache::new(if path.is_absolute() {
            path
        } else {
            scan_base.join(path)
        }))
    } else {
        None
    };
    let architecture_excludes = analyzer
        .config
        .architecture
        .exclude
        .iter()
        .filter_map(|p| globset::Glob::new(p).ok().map(|g| g.compile_matcher()))
        .collect::<Vec<_>>();
    let analyzed = discovered
        .par_iter()
        .filter(|f| !f.deferred)
        .map(|f| {
            let (file, source, cached) = parse_one_with_architecture(
                analyzer,
                f,
                cache.as_ref(),
                analyzer.collect_dependency_facts
                    || (analyzer.config.architecture.enabled
                        && !f.vendor
                        && !architecture_excludes
                            .iter()
                            .any(|m| m.is_match(&f.relative))),
            )?;
            let facts = crate::metrics::FileTextFacts::new(&file, &source);
            let suppressions = crate::rules::Suppressions::from_file(&file, &source);
            Ok::<_, AnalysisError>((file, facts, suppressions, cached))
        })
        .collect::<Vec<_>>();
    let mut files = Vec::new();
    let mut text_facts = BTreeMap::new();
    let mut suppressions = crate::rules::Suppressions::default();
    let mut cache_hits = 0;
    for item in analyzed {
        let (file, facts, file_suppressions, cached) = item?;
        if cached {
            cache_hits += 1;
        }
        text_facts.insert(file.path.clone(), facts);
        suppressions.extend(file_suppressions);
        files.push(file);
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let deferred_vendor: Vec<DiscoveredVendor> = discovered
        .iter()
        .filter(|f| f.deferred)
        .map(|f| DiscoveredVendor {
            path: f.relative.clone(),
            package: f.vendor_package.clone().unwrap_or_default(),
            language: f.language.clone(),
        })
        .collect();
    memory_trace("models");
    timings.mark("parse");
    let resolution = crate::resolution::ResolutionContext::load(scan_base, &analyzer.config);
    let mut graph =
        CallGraph::build_with_context(&files, &analyzer.registry, &analyzer.config, &resolution);
    if !deferred_vendor.is_empty() {
        graph.attach_deferred_vendor_with_files(
            &deferred_vendor
                .iter()
                .map(|d| (d.path.clone(), d.package.clone(), d.language.clone()))
                .collect::<Vec<_>>(),
            &files,
        );
    }
    memory_trace("graph");
    timings.mark("graph");
    crate::algorithmic::analyze(&mut files, &mut graph, &analyzer.config);
    memory_trace("algorithmic");
    timings.mark("algorithmic");
    let metric_registry = analyzer.metrics.clone();
    // Builtins and advanced metrics produce at most twelve values per function
    // (including duplicates), four per type and five per file. Reserving the
    // combined storage avoids reallocating it while the advanced vector is live.
    let metric_capacity = files
        .iter()
        .filter(|f| !f.vendor)
        .map(|f| 12 * f.functions.len() + 4 * f.types.len() + 5)
        .sum();
    let mut metrics = Vec::with_capacity(metric_capacity);
    for file in &files {
        metrics.extend(metric_registry.calculate(file, &analyzer.config));
    }
    let mut advanced =
        crate::metrics::advanced_metrics_consuming_text_facts(&files, &graph, text_facts);
    metric_registry.intern_values(&mut advanced);
    metrics.extend(advanced);
    drop(metric_registry);
    memory_trace("advanced_metrics");
    timings.mark("metrics");
    let aggs = aggregates(&files, &metrics, analyzer.config.metrics.histogram_bins);
    memory_trace("aggregates");
    timings.mark("aggregates");
    let baseline = if apply_baseline {
        load_baseline(&scan_base.join(".analyzer-baseline.json"))
    } else {
        Default::default()
    };
    let mut violations = crate::rules::evaluate_with_suppressions(
        &metrics,
        &files,
        &aggs,
        &analyzer.config,
        &baseline,
        &suppressions,
    );
    let (mut architecture, architecture_violations) =
        crate::architecture::analyze(&files, &graph, &analyzer.config.architecture);
    violations.extend(
        architecture_violations
            .into_iter()
            .filter(|v| !baseline.contains(&v.fingerprint()) && !suppressions.contains(v)),
    );
    architecture.warnings = violations
        .iter()
        .filter(|v| v.architecture.is_some())
        .count();
    for rule in &mut architecture.rules {
        rule.findings = violations
            .iter()
            .filter(|v| v.rule == rule.rule && v.architecture.is_some())
            .count();
    }
    memory_trace("architecture");
    timings.mark("architecture");
    memory_trace("rules");
    timings.mark("rules");
    let parse_errors = files
        .iter()
        .flat_map(|f| f.parse_errors.clone())
        .collect::<Vec<_>>();
    let summary = Summary {
        files: files.iter().filter(|f| !f.vendor).count(),
        functions: files
            .iter()
            .filter(|f| !f.vendor)
            .map(|f| {
                f.functions
                    .iter()
                    .filter(|function| !function.is_module_initializer())
                    .count()
            })
            .sum(),
        types: files
            .iter()
            .filter(|f| !f.vendor)
            .map(|f| f.types.len())
            .sum(),
        architecture_warnings: architecture.warnings,
        warnings: violations
            .iter()
            .filter(|v| v.severity == crate::rules::Severity::Warn)
            .count(),
        errors: violations
            .iter()
            .filter(|v| v.severity == crate::rules::Severity::Error)
            .count(),
        vendor_files: files.iter().filter(|f| f.vendor).count()
            + discovered.iter().filter(|f| f.deferred).count(),
        parse_errors: parse_errors.len(),
        cache_hits,
        parsed_files: files.len() - cache_hits,
    };
    log::info!(
        "scan completed files={} functions={} warnings={} errors={} cache_hits={}",
        summary.files,
        summary.functions,
        summary.warnings,
        summary.errors,
        cache_hits
    );
    Ok(AnalysisReport {
        version: env!("CARGO_PKG_VERSION").into(),
        schema_version: "3".into(),
        root: canonical.display().to_string(),
        resolution_diagnostics: {
            let mut diagnostics = resolution.report_diagnostics(&files, &analyzer.config);
            diagnostics.extend(graph.resolution_diagnostics.iter().cloned());
            diagnostics.sort();
            diagnostics.dedup();
            diagnostics
        },
        config_path: None,
        strategy_capabilities: analyzer
            .registry
            .capabilities()
            .into_iter()
            .filter(|(id, _)| {
                analyzer.config.languages.enabled.is_empty()
                    || analyzer.config.languages.enabled.contains(id)
            })
            .collect(),
        summary,
        aggregates: aggs,
        files,
        metrics,
        violations,
        parse_errors,
        graph,
        deferred_vendor,
        architecture,
    })
}

fn parse_one(
    analyzer: &Analyzer,
    f: &DiscoveredFile,
    cache: Option<&Cache>,
) -> Result<(FileModel, String, bool), AnalysisError> {
    parse_one_with_architecture(
        analyzer,
        f,
        cache,
        analyzer.collect_dependency_facts || (analyzer.config.architecture.enabled && !f.vendor),
    )
}

fn parse_one_with_architecture(
    analyzer: &Analyzer,
    f: &DiscoveredFile,
    cache: Option<&Cache>,
    architecture_enabled: bool,
) -> Result<(FileModel, String, bool), AnalysisError> {
    log::trace!(
        "analyzing file={} language={} architecture={architecture_enabled}",
        f.relative,
        f.language
    );
    let strategy = analyzer
        .registry
        .get(&f.language)
        .ok_or_else(|| AnalysisError::Config(format!("strategy missing: {}", f.language)))?;
    let range_path: std::sync::Arc<str> = f.relative.as_str().into();
    let source_range = SourceRange {
        file: range_path.clone(),
        start_line: 1,
        start_column: 1,
        end_line: 1,
        end_column: 1,
    };
    let bytes = match std::fs::read(&f.absolute) {
        Ok(bytes) => bytes,
        Err(error) => {
            return Ok((
                FileModel {
                    path: f.relative.clone(),
                    language: f.language.clone(),
                    vendor: f.vendor,
                    parse_errors: vec![ParseError {
                        message: format!("source file unreadable: {error}"),
                        range: source_range,
                    }],
                    ..FileModel::default()
                },
                String::new(),
                false,
            ));
        }
    };
    let (mut source, encoding_error) = match String::from_utf8(bytes) {
        Ok(source) => (source, None),
        Err(error) => {
            let diagnostic = ParseError {
                message: format!(
                    "source contains invalid UTF-8; decoded with replacement characters: {}",
                    error.utf8_error()
                ),
                range: source_range,
            };
            (
                String::from_utf8_lossy(error.as_bytes()).into_owned(),
                Some(diagnostic),
            )
        }
    };
    let bom_bytes = source.len() - source.trim_start_matches('\u{feff}').len();
    if bom_bytes != 0 {
        source.drain(..bom_bytes);
    }
    let hash = Cache::content_hash_parts(
        [
            f.relative.as_str(),
            "\0",
            encoding_error
                .as_ref()
                .map(|error| error.message.as_str())
                .unwrap_or(""),
            "\0",
            source.as_str(),
        ],
        &f.language,
        &format!(
            "core-ir-19:strategy-{}:architecture-v4-{}",
            strategy.version(),
            architecture_enabled
        ),
    );
    if let Some(cached) = cache.and_then(|c| c.get(&hash)) {
        let mut cached = cached;
        cached.path = f.relative.clone();
        cached.vendor = f.vendor;
        return Ok((cached, source, true));
    }
    let relative = Path::new(&f.relative);
    let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        strategy.analyze_with_architecture(&source, relative, architecture_enabled)
    }));
    let mut model = match parsed {
        Ok(ok) => ok,
        Err(_) => {
            let range = SourceRange {
                file: range_path.clone(),
                start_line: 1,
                start_column: 1,
                end_line: 1,
                end_column: 1,
            };
            let error = ParseError {
                message: "parser strategy failed".into(),
                range: range.clone(),
            };
            FileModel {
                parse_errors: vec![error],
                ..Default::default()
            }
        }
    };
    model.path = f.relative.clone();
    model.language = f.language.clone();
    model.loc = strategy.line_counts(&source, relative, &model);
    if let Some(error) = encoding_error {
        model.parse_errors.push(error);
    }
    model.vendor = f.vendor;
    model.source_hash = hash.clone();
    model.intern_range_paths();
    if let Some(cache) = cache {
        let _ = cache.put(&hash, &model);
    }
    Ok((model, source, false))
}

impl Analyzer {
    pub fn expand_vendor(
        &self,
        root: &Path,
        report: &mut AnalysisReport,
        package: &str,
    ) -> Result<(), AnalysisError> {
        if self.config.vendor.parse == crate::config::VendorParse::None {
            return Err(AnalysisError::Config("vendor parsing is disabled".into()));
        }
        let root = std::fs::canonicalize(root)?;
        let discovered = discover(&root, &self.config, &self.registry);
        let selected = discovered
            .into_iter()
            .filter(|f| f.vendor && f.vendor_package.as_deref() == Some(package))
            .collect::<Vec<_>>();
        let cache = if self.config.cache.enabled {
            let path = PathBuf::from(&self.config.cache.path);
            Some(Cache::new(if path.is_absolute() {
                path
            } else {
                root.join(path)
            }))
        } else {
            None
        };
        let parsed = selected
            .par_iter()
            .map(|f| parse_one(self, f, cache.as_ref()))
            .collect::<Vec<_>>();
        for item in parsed {
            let (model, _, cached) = item?;
            if !report.files.iter().any(|f| f.path == model.path) {
                if cached {
                    report.summary.cache_hits += 1
                } else {
                    report.summary.parsed_files += 1
                }
                report.files.push(model);
            }
        }
        report.files.sort_by(|a, b| a.path.cmp(&b.path));
        report.deferred_vendor.retain(|f| f.package != package);
        let resolution = crate::resolution::ResolutionContext::load(&root, &self.config);
        report.graph =
            CallGraph::build_with_context(&report.files, &self.registry, &self.config, &resolution);
        report.resolution_diagnostics = resolution.report_diagnostics(&report.files, &self.config);
        report
            .resolution_diagnostics
            .extend(report.graph.resolution_diagnostics.iter().cloned());
        report.resolution_diagnostics.sort();
        report.resolution_diagnostics.dedup();
        if !report.deferred_vendor.is_empty() {
            report.graph.attach_deferred_vendor_with_files(
                &report
                    .deferred_vendor
                    .iter()
                    .map(|d| (d.path.clone(), d.package.clone(), d.language.clone()))
                    .collect::<Vec<_>>(),
                &report.files,
            );
        }
        crate::algorithmic::analyze(&mut report.files, &mut report.graph, &self.config);
        // Vendor expansion may resolve an unknown call in a non-vendor caller.
        // Re-evaluate diagnostics using fresh global cost summaries and source suppressions.
        let sources = report
            .files
            .iter()
            .map(|f| {
                let bytes = std::fs::read(root.join(&f.path)).unwrap_or_default();
                let source = String::from_utf8_lossy(&bytes)
                    .trim_start_matches('\u{feff}')
                    .to_owned();
                (f.path.clone(), source)
            })
            .collect::<BTreeMap<_, _>>();
        let metric_registry = self.metrics.clone();
        report.metrics = report
            .files
            .iter()
            .flat_map(|file| metric_registry.calculate(file, &self.config))
            .collect();
        let mut advanced = crate::metrics::advanced_metrics(&report.files, &report.graph, &sources);
        metric_registry.intern_values(&mut advanced);
        report.metrics.extend(advanced);
        report.aggregates = aggregates(
            &report.files,
            &report.metrics,
            self.config.metrics.histogram_bins,
        );
        report.parse_errors = report
            .files
            .iter()
            .flat_map(|file| file.parse_errors.clone())
            .collect();
        report.summary.parse_errors = report.parse_errors.len();
        let baseline = load_baseline(&root.join(".analyzer-baseline.json"));
        report.violations = evaluate(
            &report.metrics,
            &report.files,
            &report.aggregates,
            &self.config,
            &baseline,
            &sources,
        );
        let suppressions = crate::rules::Suppressions::from_files(&report.files, &sources);
        let (mut architecture, findings) =
            crate::architecture::analyze(&report.files, &report.graph, &self.config.architecture);
        report.violations.extend(
            findings
                .into_iter()
                .filter(|v| !baseline.contains(&v.fingerprint()) && !suppressions.contains(v)),
        );
        architecture.warnings = report
            .violations
            .iter()
            .filter(|v| v.architecture.is_some())
            .count();
        for rule in &mut architecture.rules {
            rule.findings = report
                .violations
                .iter()
                .filter(|v| v.rule == rule.rule && v.architecture.is_some())
                .count();
        }
        report.summary.architecture_warnings = architecture.warnings;
        report.architecture = architecture;
        report.summary.warnings = report
            .violations
            .iter()
            .filter(|v| v.severity == crate::rules::Severity::Warn)
            .count();
        report.summary.errors = report
            .violations
            .iter()
            .filter(|v| v.severity == crate::rules::Severity::Error)
            .count();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Unreadable;
    impl crate::analysis::LanguageStrategy for Unreadable {
        fn language_id(&self) -> &'static str {
            "unreadable"
        }
        fn file_extensions(&self) -> &'static [&'static str] {
            &["unreadable"]
        }
        fn parse(&self, _: &str, _: &Path) -> RawAst {
            panic!("unreadable sources must not be parsed")
        }
        fn normalize(&self, _: &str, _: &Path, _: &RawAst) -> FileModel {
            panic!("unreadable sources must not be normalized")
        }
    }
    #[test]
    fn file_disappearing_after_discovery_is_a_parse_error() {
        let mut registry = crate::analysis::LanguageRegistry::new();
        registry.register(Unreadable);
        let analyzer = Analyzer::new(registry, Default::default());
        let file = DiscoveredFile {
            absolute: PathBuf::from("definitely-not-present-after-discovery.unreadable"),
            relative: "missing.unreadable".into(),
            language: "unreadable".into(),
            vendor: false,
            deferred: false,
            vendor_package: None,
        };
        let (model, source, cached) = parse_one(&analyzer, &file, None).unwrap();
        assert!(!cached);
        assert!(source.is_empty());
        assert_eq!(model.path, "missing.unreadable");
        assert_eq!(model.parse_errors.len(), 1);
        assert_eq!(
            model.parse_errors[0].range.file.as_ref(),
            "missing.unreadable"
        );
        assert!(model.parse_errors[0].message.contains("unreadable"));
    }
}

fn memory_trace(stage: &str) {
    if std::env::var_os("CODEFACTOR_PROFILE_MEMORY").is_none() {
        return;
    }
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        let usage = status
            .lines()
            .filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:"))
            .collect::<Vec<_>>()
            .join("; ");
        eprintln!("memory stage={stage}: {usage}");
    }
}

struct ScanTimings(Option<std::time::Instant>);
impl ScanTimings {
    fn new() -> Self {
        Self(std::env::var_os("CODEFACTOR_PROFILE_TIME").map(|_| std::time::Instant::now()))
    }
    fn mark(&mut self, stage: &str) {
        if let Some(last) = &mut self.0 {
            let now = std::time::Instant::now();
            eprintln!(
                "time stage={stage}: {:.3}s",
                now.duration_since(*last).as_secs_f64()
            );
            *last = now;
        }
    }
}
