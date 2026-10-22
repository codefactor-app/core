//! Construction of deterministic rule witnesses.
use super::{model::*, spec::*};
use crate::{
    config::architecture::ArchitectureConfig,
    diagnostics::{Severity, Violation},
    ir::*,
};
use std::collections::{BTreeMap, BTreeSet};
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
    // Keep the complete file index before bounding public witnesses. A changed
    // dependency target matters even when its source range is in another file
    // or its witness is later deduplicated or truncated.
    let ranged_files = evidence
        .iter()
        .map(|proof| proof.range.file.as_ref())
        .collect::<BTreeSet<_>>();
    let mut affected_locations = Vec::new();
    for proof in &evidence {
        affected_locations.push(proof.range.clone());
        if let Some(dependency) = &proof.dependency {
            for file in [&dependency.from_file, &dependency.to_file] {
                if !file.is_empty() && !ranged_files.contains(file.as_str()) {
                    affected_locations.push(SourceRange {
                        file: file.as_str().into(),
                        start_line: 1,
                        start_column: 1,
                        end_line: 1,
                        end_column: 1,
                    });
                }
            }
        }
    }
    affected_locations.sort();
    affected_locations.dedup();
    let mut evidence = evidence;
    evidence.sort_by(|a, b| (&a.range, &a.description).cmp(&(&b.range, &b.description)));
    evidence.dedup_by(|a, b| a.range == b.range && a.description == b.description);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_endpoints_survive_witness_deduplication_and_truncation() {
        let source = SourceRange {
            file: "source.py".into(),
            start_line: 10,
            end_line: 10,
            ..Default::default()
        };
        let proof = |range: SourceRange, target: &str| Evidence {
            dependency: Some(DependencyEvidence {
                from_module: "python:source".into(),
                to_module: format!("python:{target}"),
                from_file: range.file.to_string(),
                to_file: target.into(),
                kind: "call".into(),
                source_entity: "run".into(),
                target_entity: "work".into(),
            }),
            range,
            description: "same retained witness description".into(),
        };
        let evidence = vec![
            proof(source.clone(), "target1.py"),
            proof(source.clone(), "target2.py"),
            proof(
                SourceRange {
                    file: "target1.py".into(),
                    start_line: 40,
                    end_line: 40,
                    ..Default::default()
                },
                "source.py",
            ),
        ];
        let config = ArchitectureConfig {
            max_evidence: 1,
            ..Default::default()
        };
        let mut violations = Vec::new();
        emit(
            &mut violations,
            "arch_boundary",
            &source,
            "domain → infrastructure",
            String::new(),
            "",
            vec!["domain".into(), "infrastructure".into()],
            evidence,
            BTreeMap::new(),
            Vec::new(),
            &config,
        );
        let diagnostic = violations[0].architecture.as_ref().unwrap();
        assert_eq!(diagnostic.evidence.len(), 1);
        assert_eq!(diagnostic.evidence_total, 3);
        assert_eq!(diagnostic.affected_locations.len(), 3);
        assert!(diagnostic
            .affected_ranges()
            .any(|range| range.file.as_ref() == "target2.py"));
        assert!(diagnostic
            .affected_ranges()
            .any(|range| range.file.as_ref() == "target1.py" && range.start_line == 40));
        assert!(!diagnostic
            .affected_ranges()
            .any(|range| range.file.as_ref() == "target1.py" && range.start_line == 1));
    }
}
