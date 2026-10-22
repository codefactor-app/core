//! Serializable diagnostics shared by independent rule evaluators.
use serde::{Deserialize, Serialize};
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
    pub algorithmic_complexity: Option<crate::algorithmic::model::ComplexityDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architecture: Option<crate::architecture::model::ArchitectureDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_cycle: Option<crate::architecture::model::ModuleCycleDiagnostic>,
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
        if let Some(cycle) = &self.module_cycle {
            return format!(
                "{} is {} (limit {}); {} modules, {} files; {}",
                self.rule,
                self.value.map(|v| v.to_string()).unwrap_or_default(),
                self.limit.map(|v| v.to_string()).unwrap_or_default(),
                cycle.participants.len(),
                cycle.files.len(),
                cycle.cycle.path.join(" → ")
            );
        }
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
        if let Some(cycle) = &self.module_cycle {
            use sha2::{Digest, Sha256};
            let mut participants = cycle.participants.clone();
            participants.sort();
            participants.dedup();
            let input = serde_json::to_vec(&(&participants, &self.severity))
                .expect("serializable cycle participants");
            return format!("module_cycle:{:x}", Sha256::digest(input));
        }
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
