//! Deterministic architecture witnesses, never blocking diagnostics.
mod config;
mod graph;
mod grouping;
pub(crate) use grouping::{module_cycles, validate_module_assignments};
mod model;
mod rules;
use crate::graph::CallGraph;
use crate::ir::*;
use crate::rules::{Severity, Violation};
pub use config::*;
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

pub struct RuleSpec {
    pub id: &'static str,
    pub title: &'static str,
    pub principle: &'static str,
    pub strict: bool,
    pub requires_bodies: bool,
}
pub const RULES: &[RuleSpec] = &[
    RuleSpec {
        id: "arch_cycle",
        title: "Циклические зависимости",
        principle: "ADP",
        strict: true,
        requires_bodies: true,
    },
    RuleSpec {
        id: "arch_boundary",
        title: "Границы слоёв",
        principle: "Layers / DIP",
        strict: true,
        requires_bodies: false,
    },
    RuleSpec {
        id: "arch_dry",
        title: "Повторяющаяся реализация",
        principle: "DRY",
        strict: true,
        requires_bodies: true,
    },
    RuleSpec {
        id: "arch_srp",
        title: "Разделённые группы ответственности",
        principle: "SRP",
        strict: false,
        requires_bodies: true,
    },
    RuleSpec {
        id: "arch_ocp",
        title: "Повторяющийся выбор поведения",
        principle: "OCP",
        strict: false,
        requires_bodies: true,
    },
    RuleSpec {
        id: "arch_lsp",
        title: "Отказ от унаследованной операции",
        principle: "LSP",
        strict: false,
        requires_bodies: true,
    },
    RuleSpec {
        id: "arch_isp",
        title: "Раздельные группы потребителей интерфейса",
        principle: "ISP",
        strict: false,
        requires_bodies: true,
    },
    RuleSpec {
        id: "arch_factory",
        title: "Повторяющееся создание объектов",
        principle: "Construction",
        strict: false,
        requires_bodies: true,
    },
    RuleSpec {
        id: "arch_demeter",
        title: "Доступ через промежуточные объекты",
        principle: "LoD",
        strict: false,
        requires_bodies: true,
    },
    RuleSpec {
        id: "arch_stability",
        title: "Направление зависимости по устойчивости",
        principle: "SDP",
        strict: false,
        requires_bodies: false,
    },
];
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
pub(super) fn emit(
    out: &mut Vec<Violation>,
    id: &str,
    range: &SourceRange,
    entity: &str,
    fact: String,
    hypothesis: &str,
    participants: Vec<String>,
    evidence: Vec<Evidence>,
    measurements: BTreeMap<String, f64>,
    recommendations: Vec<Recommendation>,
    config: &ArchitectureConfig,
) {
    let spec = RULES.iter().find(|r| r.id == id).unwrap();
    let mut participants = participants;
    participants.sort();
    participants.dedup();
    let total = evidence.len();
    let mut evidence = evidence;
    evidence.sort_by(|a, b| (&a.range, &a.description).cmp(&(&b.range, &b.description)));
    evidence.dedup_by(|a, b| a.range == b.range && a.description == b.description);
    let mut affected_locations = evidence.iter().map(|e| e.range.clone()).collect::<Vec<_>>();
    affected_locations.dedup();
    evidence.truncate(config.max_evidence);
    out.push(Violation{rule:id.into(),severity:Severity::Warn,file:range.file.to_string(),line:range.start_line,entity:entity.into(),value:None,limit:None,scope:match id{"arch_cycle"|"arch_boundary"|"arch_stability"=>"module","arch_srp"|"arch_lsp"|"arch_isp"=>"class",_=>"function"}.into(),algorithmic_complexity:None,module_cycle:None,architecture:Some(ArchitectureDiagnostic{
        principle:spec.principle.into(),title:spec.title.into(),fact,hypothesis:hypothesis.into(),evidence_kind:if matches!(id,"arch_cycle"|"arch_boundary"|"arch_dry"){"structural_fact"}else{"structural_heuristic"}.into(),cycle:None,measurements,evidence,affected_locations,evidence_total:total,participants,recommendations,
        limitations:vec!["Свидетельства относятся к статической модели; динамические цели не подтверждаются.".into(),"Архитектурная трактовка и полезность изменения требуют проверки контекста проекта.".into()]
    })});
}
pub(super) fn recommendation(pattern: &str, proposal: &str, conditions: &[&str]) -> Recommendation {
    Recommendation {
        pattern: pattern.into(),
        proposal: proposal.into(),
        conditions: conditions.iter().map(|s| (*s).into()).collect(),
    }
}
pub(super) fn evidence(range: &SourceRange, description: impl Into<String>) -> Evidence {
    Evidence {
        range: range.clone(),
        description: description.into(),
        dependency: None,
    }
}
pub(super) fn type_id(f: &FileModel, t: &TypeModel) -> String {
    format!("{}:{}:{}", f.language, f.path, t.fqn)
}
pub(super) fn in_range(inner: &SourceRange, outer: &SourceRange) -> bool {
    inner.file == outer.file
        && (inner.start_line, inner.start_column) >= (outer.start_line, outer.start_column)
        && (inner.end_line, inner.end_column) <= (outer.end_line, outer.end_column)
}
