//! Deterministic architecture witnesses, never blocking diagnostics.
pub use crate::config::register_architecture_config as register_config;
pub(crate) mod spec;
pub use spec::*;
mod graph;
mod witnesses;
use witnesses::*;
mod grouping;
pub(crate) use grouping::{module_cycles, validate_module_assignments};
pub(crate) mod model;
mod rules;
pub use crate::config::architecture::*;
use crate::diagnostics::Violation;
use crate::graph::CallGraph;
use crate::ir::*;
pub use model::*;
use std::collections::{BTreeMap, BTreeSet};

/// Source witnesses shared by architecture checks and the interactive code map.
pub(crate) fn dependency_facts<'a>(
    files: &'a [FileModel],
    calls: &'a CallGraph,
) -> (Vec<Dependency<'a>>, usize) {
    let files = files.iter().collect::<Vec<_>>();
    let symbols = graph::Symbols::new(&files);
    graph::dependencies(&files, calls, &symbols)
}

/// Files the architecture graph covers: non-vendor and outside `architecture.exclude`.
fn selected_files<'a>(files: &'a [FileModel], config: &ArchitectureConfig) -> Vec<&'a FileModel> {
    let matchers = config
        .exclude
        .iter()
        .filter_map(|p| globset::Glob::new(p).ok().map(|g| g.compile_matcher()))
        .collect::<Vec<_>>();
    files
        .iter()
        .filter(|f| !f.vendor && !matchers.iter().any(|m| m.is_match(&f.path)))
        .collect()
}

pub fn analyze(
    files: &[FileModel],
    calls: &CallGraph,
    config: &ArchitectureConfig,
) -> (ArchitectureSummary, Vec<Violation>) {
    let mut summary = ArchitectureSummary {
        enabled: config.enabled,
        profile: match config.profile {
            Profile::Strict => "strict",
            Profile::Extended => "extended",
        }
        .into(),
        ..Default::default()
    };
    let selected = selected_files(files, config);
    summary.analyzed_files = if config.enabled { selected.len() } else { 0 };
    summary.excluded_files = files.len() - summary.analyzed_files;
    let mut out = Vec::new();
    if !config.enabled {
        summary.rules = RULES
            .iter()
            .map(|r| coverage(r, "disabled", "Архитектурный анализ отключён", 0))
            .collect();
        return (summary, out);
    }
    if let Err(error) = config.validate() {
        summary.diagnostics.push(error);
        summary.rules = RULES
            .iter()
            .map(|r| coverage(r, "skipped", "Некорректная архитектурная конфигурация", 0))
            .collect();
        return (summary, out);
    }
    let mut started = std::time::Instant::now();
    let mut mark = |stage: &str| {
        if std::env::var_os("CODEFACTOR_PROFILE_TIME").is_some() {
            eprintln!(
                "time architecture={stage}: {:.3}s",
                started.elapsed().as_secs_f64()
            );
        }
        started = std::time::Instant::now();
    };
    let assignments = match grouping::assign_modules(&selected, calls, config) {
        Ok(assignments) => assignments,
        Err(error) => {
            summary.diagnostics.push(error);
            summary.rules = RULES
                .iter()
                .map(|r| coverage(r, "skipped", "Некорректное назначение модулей", 0))
                .collect();
            return (summary, out);
        }
    };
    let symbols = graph::Symbols::new(&selected);
    mark("symbols");
    let (dependencies, unresolved) = graph::dependencies(&selected, calls, &symbols);
    mark("dependencies");
    summary.dependencies = dependencies.len();
    summary.unresolved_dependencies = unresolved;
    let body_files = selected
        .iter()
        .filter(|f| f.architecture_facts.is_some())
        .count();
    let partial = unresolved > 0 || selected.iter().any(|f| !f.parse_errors.is_empty());
    for r in RULES {
        let (status, reason) = if config.disabled_rules.iter().any(|id| id == r.id) {
            ("disabled", "Правило отключено")
        } else if config.profile == Profile::Strict && !r.strict {
            (
                "skipped",
                "Доступно в extended: недостаточно корпусной валидации для strict",
            )
        } else if r.id == "arch_boundary" && config.forbidden_dependencies.is_empty() {
            ("skipped", "Границы и запрещённые зависимости не заданы")
        } else if r.requires_bodies && body_files == 0 {
            (
                "unsupported",
                "Стратегии не предоставили архитектурные факты тел",
            )
        } else if partial || r.requires_bodies && body_files < selected.len() {
            (
                "partial",
                "Проверены доступные факты; неизвестные зависимости и неполный разбор исключены",
            )
        } else {
            (
                "complete",
                "Проверены все доступные предпосылки внутри статической модели",
            )
        };
        let start = out.len();
        if matches!(status, "complete" | "partial") {
            match r.id {
                "arch_cycle" => {
                    rules::cycles(&dependencies, &symbols, &assignments, config, &mut out)
                }
                "arch_boundary" => rules::boundaries(
                    &dependencies,
                    calls,
                    &symbols,
                    config,
                    &mut summary.diagnostics,
                    &mut out,
                ),
                "arch_dry" => rules::duplicates(&selected, calls, &symbols, config, &mut out),
                "arch_srp" => rules::cohesion(&selected, calls, &symbols, config, &mut out),
                "arch_ocp" | "arch_factory" => {
                    rules::decisions(r.id, &selected, &symbols, config, &mut out)
                }
                "arch_lsp" => rules::substitution(&selected, &symbols, config, &mut out),
                "arch_isp" => rules::interfaces(&selected, &symbols, config, &mut out),
                "arch_demeter" => rules::demeter(&selected, &symbols, config, &mut out),
                "arch_stability" => rules::stability(&dependencies, &assignments, config, &mut out),
                _ => {}
            }
        }
        let mut c = coverage(r, status, reason, out.len() - start);
        if r.id == "arch_boundary" && !summary.diagnostics.is_empty() {
            c.status = "partial".into();
            c.reason = "Пересекающиеся назначения слоёв исключены; см. diagnostics".into();
        }
        summary.rules.push(c);
        mark(r.id);
    }
    // Repeated construction and type dispatch can describe the same decision sites.
    let mut redundant = BTreeSet::new();
    for factory in out.iter().filter(|v| v.rule == "arch_factory") {
        let f = factory.architecture.as_ref().unwrap();
        if out.iter().any(|v| {
            v.rule == "arch_ocp"
                && v.architecture
                    .as_ref()
                    .is_some_and(|d| d.participants == f.participants)
        }) {
            redundant.insert(factory.fingerprint());
        }
    }
    let extra = out
        .iter()
        .filter(|v| redundant.contains(&v.fingerprint()))
        .filter_map(|v| v.architecture.clone())
        .collect::<Vec<_>>();
    out.retain(|v| !redundant.contains(&v.fingerprint()));
    for d in extra {
        if let Some(issue) = out.iter_mut().find(|v| {
            v.rule == "arch_ocp"
                && v.architecture
                    .as_ref()
                    .is_some_and(|a| a.participants == d.participants)
        }) {
            issue
                .architecture
                .as_mut()
                .unwrap()
                .recommendations
                .extend(d.recommendations);
        }
    }
    for r in &mut summary.rules {
        r.findings = out.iter().filter(|v| v.rule == r.rule).count();
    }
    out.sort_by(|a, b| {
        (&a.file, &a.entity, &a.rule, a.fingerprint()).cmp(&(
            &b.file,
            &b.entity,
            &b.rule,
            b.fingerprint(),
        ))
    });
    summary.warnings = out.len();
    (summary, out)
}
fn coverage(r: &RuleSpec, status: &str, reason: &str, findings: usize) -> RuleCoverage {
    RuleCoverage {
        rule: r.id.into(),
        title: r.title.into(),
        principle: r.principle.into(),
        status: status.into(),
        reason: reason.into(),
        findings,
        calibration: if r.strict {
            "structural witnesses"
        } else {
            "experimental; corpus acceptance pending"
        }
        .into(),
    }
}
