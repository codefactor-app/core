use crate::ir::SourceRange;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Private, source-local facts. Persisted in the cache sidecar, never the public FileModel.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FileFacts {
    #[serde(default)]
    pub unsupported_operation_shadowed: bool,
    /// Rust namespace bindings whose origin cannot be established as the standard crate.
    #[serde(default)]
    pub rust_namespace_shadows: Vec<String>,
    pub functions: Vec<FunctionFacts>,
    pub fragments: Vec<Fragment>,
    pub references: Vec<TypeReference>,
    /// Actual value reads/writes, separate from imports, annotations and type metadata.
    #[serde(default)]
    pub value_accesses: Vec<ValueAccess>,
    pub abstract_types: Vec<String>,
    pub closed_types: Vec<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FunctionFacts {
    pub fqn: String,
    pub range: SourceRange,
    pub own_fields: Vec<String>,
    pub unsupported_stub: bool,
    /// A qualified builtin macro whose origin was verified independently of bare-name bindings.
    #[serde(default)]
    pub unsupported_stub_qualified: bool,
    pub normal_exit: bool,
    pub body_present: bool,
    pub dynamic_access: bool,
    pub decisions: Vec<Decision>,
    pub member_chains: Vec<MemberChain>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Decision {
    pub range: SourceRange,
    pub subject: String,
    pub subject_type: Option<String>,
    pub cases: Vec<DecisionCase>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DecisionCase {
    pub label: String,
    pub tested_type: Option<String>,
    pub range: SourceRange,
    pub creations: Vec<TypeReference>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MemberChain {
    pub root_type: Option<String>,
    pub members: Vec<String>,
    pub range: SourceRange,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TypeReference {
    pub name: String,
    pub kind: String,
    pub range: SourceRange,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ValueAccess {
    pub receiver: String,
    pub member: Option<String>,
    pub receiver_type: Option<String>,
    pub binding_unknown: bool,
    pub range: SourceRange,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Fragment {
    pub entity: String,
    pub range: SourceRange,
    pub tokens: usize,
    pub statements: usize,
    /// Length-delimited syntax leaves; binding-aware representation when safely supported.
    pub canonical: String,
    pub renamed: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ArchitectureSummary {
    pub enabled: bool,
    pub profile: String,
    pub warnings: usize,
    pub analyzed_files: usize,
    pub excluded_files: usize,
    pub dependencies: usize,
    pub unresolved_dependencies: usize,
    pub rules: Vec<RuleCoverage>,
    pub diagnostics: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuleCoverage {
    pub rule: String,
    pub title: String,
    pub principle: String,
    pub status: String,
    pub reason: String,
    pub findings: usize,
    pub calibration: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArchitectureDiagnostic {
    pub principle: String,
    pub title: String,
    pub fact: String,
    pub hypothesis: String,
    pub evidence_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle: Option<CycleDetails>,
    pub measurements: BTreeMap<String, f64>,
    pub evidence: Vec<Evidence>,
    /// Complete source index for local navigation; public evidence may be truncated.
    #[serde(default, skip_serializing)]
    pub affected_locations: Vec<SourceRange>,
    pub evidence_total: usize,
    pub participants: Vec<String>,
    pub recommendations: Vec<Recommendation>,
    pub limitations: Vec<String>,
}
impl ArchitectureDiagnostic {
    /// Deserialized public reports can only provide their retained evidence ranges.
    pub fn affected_ranges(&self) -> impl Iterator<Item = &SourceRange> {
        self.affected_locations.iter().chain(
            self.evidence
                .iter()
                .filter(|_| self.affected_locations.is_empty())
                .map(|e| &e.range),
        )
    }
}
/// A numeric module-cycle finding with complete membership and bounded witnesses.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModuleCycleDiagnostic {
    pub participants: Vec<String>,
    pub files: Vec<String>,
    pub cycle: CycleDetails,
    pub evidence: Vec<Evidence>,
    pub evidence_total: usize,
    #[serde(default, skip_serializing)]
    pub affected_locations: Vec<SourceRange>,
}
impl ModuleCycleDiagnostic {
    pub fn affected_ranges(&self) -> impl Iterator<Item = &SourceRange> {
        self.affected_locations.iter().chain(
            self.evidence
                .iter()
                .filter(|_| self.affected_locations.is_empty())
                .map(|e| &e.range),
        )
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Evidence {
    pub range: SourceRange,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependency: Option<DependencyEvidence>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CycleDetails {
    /// A real closed path; the last module repeats the first.
    pub path: Vec<String>,
    pub coupling: CycleCoupling,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CycleCoupling {
    ImportsOnly,
    ImportsWithReferences,
    MetadataLinked,
    CodeCycle,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DependencyEvidence {
    pub from_module: String,
    pub to_module: String,
    pub from_file: String,
    pub to_file: String,
    pub kind: String,
    pub source_entity: String,
    pub target_entity: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Recommendation {
    pub pattern: String,
    pub proposal: String,
    pub conditions: Vec<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Dependency<'a> {
    pub from: &'a str,
    pub to: &'a str,
    pub kind: &'a str,
    pub range: &'a SourceRange,
    pub source_entity: &'a str,
    pub target_entity: &'a str,
    pub concrete: bool,
}
