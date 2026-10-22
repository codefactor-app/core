//! Compare complete finding sets before limiting the visible report to changed files.
use super::AnalysisReport;
use crate::rules::{Severity, Violation};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Accept both complete and violations-only reports, including older versions.
/// Requiring `violations` prevents unrelated JSON from becoming an empty baseline.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreviousReport {
    #[serde(default)]
    pub schema_version: String,
    #[serde(default)]
    pub version: String,
    pub violations: Vec<Violation>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FindingCounts {
    pub total: usize,
    pub errors: usize,
    pub warnings: usize,
    pub architecture_warnings: usize,
}

impl FindingCounts {
    fn add(&mut self, violation: &Violation) {
        self.total += 1;
        match violation.severity {
            Severity::Error => self.errors += 1,
            Severity::Warn => self.warnings += 1,
        }
        self.architecture_warnings += usize::from(violation.architecture.is_some());
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ChangeSummary {
    pub new: Vec<Violation>,
    pub fixed: Vec<Violation>,
    pub unchanged: usize,
    pub changed_files: Vec<String>,
    pub filtered_out: FindingCounts,
    #[serde(default)]
    pub compared: bool,
}

/// Match occurrences in source order rather than collapsing duplicate fingerprints.
/// The mask keeps the same occurrence classification when a file filter is applied.
fn classify(previous: &[Violation], current: &[Violation]) -> (ChangeSummary, Vec<bool>) {
    let mut positions = BTreeMap::<String, VecDeque<usize>>::new();
    for (index, violation) in previous.iter().enumerate() {
        positions
            .entry(violation.fingerprint())
            .or_default()
            .push_back(index);
    }
    let mut matched_previous = vec![false; previous.len()];
    let mut new_occurrences = Vec::with_capacity(current.len());
    let mut changes = ChangeSummary {
        compared: true,
        ..Default::default()
    };
    for violation in current {
        let previous_index = positions
            .get_mut(&violation.fingerprint())
            .and_then(VecDeque::pop_front);
        if let Some(index) = previous_index {
            matched_previous[index] = true;
            changes.unchanged += 1;
            new_occurrences.push(false);
        } else {
            changes.new.push(violation.clone());
            new_occurrences.push(true);
        }
    }
    changes.fixed = previous
        .iter()
        .zip(matched_previous)
        .filter(|(_, matched)| !matched)
        .map(|(violation, _)| violation.clone())
        .collect();
    (changes, new_occurrences)
}

pub fn diff(previous: &[Violation], current: &[Violation]) -> ChangeSummary {
    classify(previous, current).0
}

fn touches_files(violation: &Violation, changed_files: &BTreeSet<String>) -> bool {
    let includes = |path: &str| !path.is_empty() && changed_files.contains(path);
    let evidence_touches = |evidence: &crate::architecture::Evidence| {
        includes(evidence.range.file.as_ref())
            || evidence.dependency.as_ref().is_some_and(|dependency| {
                includes(&dependency.from_file) || includes(&dependency.to_file)
            })
    };
    includes(&violation.file)
        || violation.module_cycle.as_ref().is_some_and(|cycle| {
            cycle.files.iter().any(|file| includes(file))
                || cycle
                    .affected_ranges()
                    .any(|range| includes(range.file.as_ref()))
                || cycle.evidence.iter().any(evidence_touches)
        })
        || violation.architecture.as_ref().is_some_and(|diagnostic| {
            diagnostic
                .affected_ranges()
                .any(|range| includes(range.file.as_ref()))
                || diagnostic.evidence.iter().any(evidence_touches)
        })
}

/// Compare the complete, baseline-applied report, then retain findings touching
/// changed files or representing a new occurrence in the comparison.
pub fn apply_changes(
    report: &mut AnalysisReport,
    previous: Option<&PreviousReport>,
    changed_files: Option<&BTreeSet<String>>,
) {
    if previous.is_none() && changed_files.is_none() {
        report.changes = None;
        return;
    }
    let (mut changes, new_occurrences) = match previous {
        Some(previous) => classify(&previous.violations, &report.violations),
        None => (
            ChangeSummary::default(),
            vec![false; report.violations.len()],
        ),
    };
    if let Some(changed_files) = changed_files {
        changes.changed_files = changed_files.iter().cloned().collect();
        let mut index = 0;
        report.violations.retain(|violation| {
            let keep = new_occurrences[index] || touches_files(violation, changed_files);
            index += 1;
            if !keep {
                changes.filtered_out.add(violation);
            }
            keep
        });
    }
    let mut counts = FindingCounts::default();
    let mut architecture_rules = BTreeMap::<&str, usize>::new();
    for violation in &report.violations {
        counts.add(violation);
        if violation.architecture.is_some() {
            *architecture_rules.entry(&violation.rule).or_default() += 1;
        }
    }
    report.summary.errors = counts.errors;
    report.summary.warnings = counts.warnings;
    report.summary.architecture_warnings = counts.architecture_warnings;
    report.architecture.warnings = counts.architecture_warnings;
    for rule in &mut report.architecture.rules {
        rule.findings = architecture_rules
            .get(rule.rule.as_str())
            .copied()
            .unwrap_or(0);
    }
    report.changes = Some(changes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::architecture::{
        ArchitectureDiagnostic, CycleCoupling, CycleDetails, DependencyEvidence, Evidence,
        ModuleCycleDiagnostic, RuleCoverage,
    };
    use crate::ir::SourceRange;

    fn finding(file: &str, rule: &str, line: usize, severity: Severity) -> Violation {
        Violation {
            rule: rule.into(),
            severity,
            file: file.into(),
            line,
            entity: format!("{file}::work"),
            value: Some(9.0),
            limit: Some(7.0),
            scope: "function".into(),
            algorithmic_complexity: None,
            architecture: None,
            module_cycle: None,
        }
    }

    fn diagnostic(participants: &[&str]) -> ArchitectureDiagnostic {
        ArchitectureDiagnostic {
            principle: "ADP".into(),
            title: "Cycle".into(),
            fact: String::new(),
            hypothesis: String::new(),
            evidence_kind: "structural_fact".into(),
            cycle: None,
            measurements: BTreeMap::new(),
            evidence: Vec::new(),
            affected_locations: Vec::new(),
            evidence_total: 0,
            participants: participants.iter().map(|name| (*name).into()).collect(),
            recommendations: Vec::new(),
            limitations: Vec::new(),
        }
    }

    fn cycle() -> ModuleCycleDiagnostic {
        ModuleCycleDiagnostic {
            participants: vec!["php:A".into(), "php:B".into()],
            files: vec!["a.php".into(), "b.php".into()],
            cycle: CycleDetails {
                path: vec!["php:A".into(), "php:B".into(), "php:A".into()],
                coupling: CycleCoupling::CodeCycle,
            },
            evidence: Vec::new(),
            evidence_total: 0,
            affected_locations: Vec::new(),
        }
    }

    fn report(violations: Vec<Violation>) -> AnalysisReport {
        AnalysisReport {
            version: "0.1.3".into(),
            schema_version: "3".into(),
            root: "/project".into(),
            config_path: None,
            baseline_path: None,
            diagnostics: Vec::new(),
            changes: None,
            strategy_capabilities: BTreeMap::new(),
            summary: Default::default(),
            aggregates: Vec::new(),
            files: Vec::new(),
            metrics: Vec::new(),
            violations,
            parse_errors: Vec::new(),
            graph: Default::default(),
            deferred_vendor: Vec::new(),
            resolution_diagnostics: Vec::new(),
            architecture: Default::default(),
        }
    }

    fn previous(violations: Vec<Violation>) -> PreviousReport {
        PreviousReport {
            schema_version: String::new(),
            version: String::new(),
            violations,
        }
    }

    #[test]
    fn comparison_pairs_duplicate_occurrences_in_source_order() {
        let repeated = finding("repeated.php", "function_params", 1, Severity::Error);
        let at = |line| Violation {
            line,
            ..repeated.clone()
        };
        let added = finding("added.php", "function_params", 4, Severity::Warn);
        let removed = finding("fixed.php", "function_params", 5, Severity::Warn);
        let changes = diff(
            &[at(1), at(2), removed.clone()],
            &[at(10), added, at(11), at(12)],
        );
        assert!(changes.compared);
        assert_eq!(changes.unchanged, 2);
        assert_eq!(
            changes.new.iter().map(|v| v.line).collect::<Vec<_>>(),
            [4, 12]
        );
        assert_eq!(changes.fixed.len(), 1);
        assert_eq!(changes.fixed[0].fingerprint(), removed.fingerprint());
        let fewer = diff(&[at(1), at(2), at(3)], &[at(10)]);
        assert_eq!(fewer.unchanged, 1);
        assert_eq!(
            fewer.fixed.iter().map(|v| v.line).collect::<Vec<_>>(),
            [2, 3]
        );
    }

    #[test]
    fn comparison_uses_existing_numeric_and_sha_fingerprints() {
        let numeric = finding("method.php", "function_params", 1, Severity::Warn);
        let mut module = finding("a.php", "module_cycle", 1, Severity::Error);
        module.module_cycle = Some(cycle());
        let mut architecture = finding("a.php", "arch_cycle", 1, Severity::Warn);
        architecture.architecture = Some(diagnostic(&["php:A", "php:B"]));
        let mut current = vec![numeric.clone(), module.clone(), architecture.clone()];
        current[0].line = 100;
        current[0].value = Some(42.0);
        current[0].limit = Some(20.0);
        current[1].file = "another-representative.php".into();
        current[1]
            .module_cycle
            .as_mut()
            .unwrap()
            .participants
            .reverse();
        current[2].file = "another-witness.php".into();
        assert!(module.fingerprint().starts_with("module_cycle:"));
        assert!(architecture
            .fingerprint()
            .starts_with("architecture:arch_cycle:"));
        let before = [numeric, module, architecture];
        let changes = diff(&before, &current);
        assert_eq!(changes.unchanged, 3);
        assert!(changes.new.is_empty() && changes.fixed.is_empty());
        current[1]
            .module_cycle
            .as_mut()
            .unwrap()
            .participants
            .push("php:C".into());
        let changes = diff(&before, &current);
        assert_eq!(changes.unchanged, 2);
        assert_eq!(changes.new.len(), 1);
        assert_eq!(changes.fixed.len(), 1);
    }

    #[test]
    fn previous_reports_accept_full_and_violations_only_json_but_require_findings() {
        let minimal: PreviousReport = serde_json::from_str(r#"{"violations":[]}"#).unwrap();
        assert!(minimal.version.is_empty() && minimal.schema_version.is_empty());
        let full = serde_json::to_value(report(Vec::new())).unwrap();
        assert!(serde_json::from_value::<PreviousReport>(full)
            .unwrap()
            .violations
            .is_empty());
        let limited: PreviousReport = serde_json::from_str(
            r#"{"report_type":"violations","version":"old","summary":{},"violations":[]}"#,
        )
        .unwrap();
        assert_eq!(limited.version, "old");
        assert!(serde_json::from_str::<PreviousReport>(r#"{"version":"old"}"#).is_err());
        assert!(serde_json::from_str::<PreviousReport>(r#"{"violations":null}"#).is_err());
        let mut legacy = serde_json::to_value(report(Vec::new())).unwrap();
        legacy.as_object_mut().unwrap().remove("changes");
        assert!(serde_json::from_value::<AnalysisReport>(legacy)
            .unwrap()
            .changes
            .is_none());
    }

    #[test]
    fn an_empty_file_filter_keeps_only_new_occurrences_after_full_comparison() {
        let repeated = finding("unchanged.php", "function_params", 1, Severity::Error);
        let other = finding("other.php", "function_lines", 3, Severity::Warn);
        let mut extra = repeated.clone();
        extra.line = 2;
        let previous = previous(vec![repeated.clone(), other.clone()]);
        let mut report = report(vec![repeated, extra, other]);
        apply_changes(&mut report, Some(&previous), Some(&BTreeSet::new()));
        assert_eq!(report.violations.len(), 1);
        assert_eq!(report.violations[0].line, 2);
        assert_eq!(report.summary.errors, 1);
        assert_eq!(report.summary.warnings, 0);
        let changes = report.changes.unwrap();
        assert_eq!(changes.new.len(), 1);
        assert_eq!(changes.unchanged, 2);
        assert!(changes.fixed.is_empty());
        assert_eq!(changes.filtered_out.total, 2);
        assert_eq!(changes.filtered_out.errors, 1);
        assert_eq!(changes.filtered_out.warnings, 1);
    }

    #[test]
    fn file_only_mode_does_not_label_findings_and_absent_filter_keeps_everything() {
        let selected = finding("selected.php", "function_lines", 1, Severity::Warn);
        let outside = finding("outside.php", "function_lines", 2, Severity::Error);
        let mut report = report(vec![selected, outside]);
        apply_changes(&mut report, None, None);
        assert!(report.changes.is_none());
        assert_eq!(report.violations.len(), 2);
        apply_changes(
            &mut report,
            None,
            Some(&BTreeSet::from(["selected.php".into()])),
        );
        assert_eq!(report.violations.len(), 1);
        let changes = report.changes.as_ref().unwrap();
        assert!(!changes.compared);
        assert!(changes.new.is_empty() && changes.fixed.is_empty());
        assert_eq!(changes.unchanged, 0);
        assert_eq!(changes.changed_files, ["selected.php"]);
        assert_eq!(changes.filtered_out.errors, 1);
        apply_changes(&mut report, None, Some(&BTreeSet::new()));
        assert!(report.violations.is_empty());
        assert_eq!(report.summary.warnings, 0);
    }

    #[test]
    fn filters_keep_complete_cycle_membership_private_ranges_and_both_dependency_ends() {
        let mut module = finding("a.php", "module_cycle", 1, Severity::Error);
        module.module_cycle = Some(cycle());
        assert!(touches_files(&module, &BTreeSet::from(["b.php".into()])));
        let mut architecture = finding("representative.php", "arch_cycle", 1, Severity::Warn);
        let mut complete = diagnostic(&["php:A", "php:B"]);
        complete.affected_locations.push(SourceRange {
            file: "untruncated.php".into(),
            ..Default::default()
        });
        assert!(serde_json::to_value(&complete)
            .unwrap()
            .get("affected_locations")
            .is_none());
        architecture.architecture = Some(complete);
        let mut boundary = finding("representative.php", "arch_boundary", 1, Severity::Warn);
        let mut edge = diagnostic(&["application", "infrastructure"]);
        edge.evidence.push(Evidence {
            range: SourceRange {
                file: "range.php".into(),
                ..Default::default()
            },
            description: String::new(),
            dependency: Some(DependencyEvidence {
                from_module: "application".into(),
                to_module: "infrastructure".into(),
                from_file: "from.php".into(),
                to_file: "to.php".into(),
                kind: "call".into(),
                source_entity: String::new(),
                target_entity: String::new(),
            }),
        });
        boundary.architecture = Some(edge);
        for file in ["range.php", "from.php", "to.php"] {
            assert!(
                touches_files(&boundary, &BTreeSet::from([file.into()])),
                "{file}"
            );
        }
        let mut skipped_arch = finding("outside.php", "arch_dry", 1, Severity::Warn);
        skipped_arch.architecture = Some(diagnostic(&["outside"]));
        let skipped_error = finding("outside.php", "function_params", 1, Severity::Error);
        let mut report = report(vec![
            module,
            architecture,
            boundary,
            skipped_arch,
            skipped_error,
        ]);
        report.architecture.rules = ["arch_cycle", "arch_boundary", "arch_dry"]
            .map(|rule| RuleCoverage {
                rule: rule.into(),
                title: String::new(),
                principle: String::new(),
                status: "complete".into(),
                reason: String::new(),
                findings: 99,
                calibration: String::new(),
            })
            .into();
        apply_changes(
            &mut report,
            None,
            Some(&BTreeSet::from([
                "b.php".into(),
                "untruncated.php".into(),
                "to.php".into(),
            ])),
        );
        assert_eq!(report.violations.len(), 3);
        assert_eq!(report.summary.errors, 1);
        assert_eq!(report.summary.warnings, 2);
        assert_eq!(report.summary.architecture_warnings, 2);
        assert_eq!(report.architecture.warnings, 2);
        assert_eq!(
            report
                .architecture
                .rules
                .iter()
                .map(|r| r.findings)
                .collect::<Vec<_>>(),
            [1, 1, 0]
        );
        let hidden = &report.changes.unwrap().filtered_out;
        assert_eq!(hidden.total, 2);
        assert_eq!(hidden.errors, 1);
        assert_eq!(hidden.warnings, 1);
        assert_eq!(hidden.architecture_warnings, 1);
    }
}
