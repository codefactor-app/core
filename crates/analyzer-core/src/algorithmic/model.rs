//! Cost IR and symbolic values, independent of the evaluator.
use crate::source::SourceRange;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
pub(super) const MAX_TERMS: usize = 128;
pub(super) const MAX_POWER: u32 = 128;
pub(super) const MAX_DEPTH: usize = 256;
pub const MODEL: &str = "Sequential unit-cost source operations; modeled builtin scalar operations cost O(1), compilation/runtime optimizations are excluded. Numeric input sizes denote nonnegative magnitudes, not bit lengths. The source must be well typed. Tightness is conditional on the stated size relations and normal arithmetic execution. Dynamic numeric/collection semantics, including Python arbitrary-precision unit costs and mutable JavaScript globals, require explicit heuristic assumptions; unmodeled operations have unknown cost.";

pub fn supported_language(language: &str) -> bool {
    matches!(
        language,
        "rust" | "php" | "python" | "javascript" | "typescript"
    )
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum EstimateStatus {
    Tight,
    UpperBound,
    Heuristic,
    #[default]
    Unknown,
    Unsupported,
    Disabled,
}
impl EstimateStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Tight => "точный класс в модели",
            Self::UpperBound => "верхняя граница",
            Self::Heuristic => "эвристическая оценка",
            Self::Unknown => "стоимость неизвестна",
            Self::Unsupported => "язык пока не поддерживается",
            Self::Disabled => "анализ отключён",
        }
    }
}

/// The order is asymptotic, not an arbitrary numerical severity score.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct GrowthClass {
    pub polynomial: u32,
    pub logarithmic: u32,
}
impl GrowthClass {
    pub fn expression(self) -> String {
        let mut parts = Vec::new();
        if self.polynomial > 0 {
            parts.push(power_text("N", self.polynomial));
        }
        if self.logarithmic > 0 {
            parts.push(power_text("log(N)", self.logarithmic));
        }
        if parts.is_empty() {
            "1".into()
        } else {
            parts.join("·")
        }
    }
}
pub fn parse_limit(input: &str) -> Result<GrowthClass, String> {
    let compact = input
        .replace(' ', "")
        .replace('·', "*")
        .replace('²', "^2")
        .replace('³', "^3");
    let compact = compact
        .strip_prefix("O(")
        .and_then(|s| s.strip_suffix(')'))
        .unwrap_or(&compact);
    let mut out = GrowthClass::default();
    for factor in compact.split('*') {
        if factor == "1" {
            continue;
        }
        let (base, power) = if let Some((base, exponent)) = factor.rsplit_once('^') {
            (
                base,
                exponent
                    .parse::<u32>()
                    .map_err(|_| "invalid growth exponent".to_string())?,
            )
        } else {
            (factor, 1)
        };
        if power > MAX_POWER {
            return Err(format!("growth exponent must be <= {MAX_POWER}"));
        }
        let target = match base {
            "N" => &mut out.polynomial,
            "log(N)" => &mut out.logarithmic,
            _ => {
                return Err(
                    "expected 1, N^p or N^p*log(N)^q with nonnegative integer exponents".into(),
                )
            }
        };
        *target = target
            .checked_add(power)
            .filter(|v| *v <= MAX_POWER)
            .ok_or("growth exponent limit exceeded")?;
    }
    Ok(out)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SizeMeasure {
    Length,
    Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum SizeExpr {
    Constant(u64),
    Input {
        parameter: String,
        measure: SizeMeasure,
    },
    Sum(Vec<SizeExpr>),
    Product(Vec<SizeExpr>),
    Log(Box<SizeExpr>),
    Unknown(Arc<str>),
}
impl SizeExpr {
    pub(super) fn intern_reasons(&mut self, intern: &mut impl FnMut(&mut Arc<str>)) {
        match self {
            Self::Unknown(reason) => intern(reason),
            Self::Sum(items) | Self::Product(items) => {
                for item in items {
                    item.intern_reasons(intern);
                }
            }
            Self::Log(value) => value.intern_reasons(intern),
            _ => {}
        }
    }
    pub fn value(name: impl Into<String>) -> Self {
        Self::Input {
            parameter: name.into(),
            measure: SizeMeasure::Value,
        }
    }
    pub fn length(name: impl Into<String>) -> Self {
        Self::Input {
            parameter: name.into(),
            measure: SizeMeasure::Length,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArgumentSize {
    #[serde(default)]
    pub status: EstimateStatus,
    pub value: SizeExpr,
    pub length: SizeExpr,
}
impl ArgumentSize {
    pub fn unknown(reason: &str) -> Self {
        let reason: Arc<str> = reason.into();
        Self {
            status: EstimateStatus::Tight,
            value: SizeExpr::Unknown(reason.clone()),
            length: SizeExpr::Unknown(reason),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CostNode {
    pub range: SourceRange,
    pub kind: CostKind,
}
impl CostNode {
    pub(crate) fn intern_ranges(
        &mut self,
        intern: &mut impl FnMut(&mut SourceRange),
        reason: &mut impl FnMut(&mut Arc<str>),
    ) {
        intern(&mut self.range);
        match &mut self.kind {
            CostKind::Sequence(items) => {
                for item in items.iter_mut() {
                    item.intern_ranges(intern, reason);
                }
                items.shrink_to_fit();
            }
            CostKind::Branch { test, arms } => {
                test.intern_ranges(intern, reason);
                for arm in arms.iter_mut() {
                    arm.intern_ranges(intern, reason);
                }
                arms.shrink_to_fit();
            }
            CostKind::Repeat(repeat) => {
                let LoopCost {
                    setup,
                    check,
                    body,
                    count,
                    induction_bound,
                    ..
                } = repeat.as_mut();
                count.intern_reasons(reason);
                if let Some(bound) = induction_bound {
                    bound.intern_reasons(reason);
                }
                setup.intern_ranges(intern, reason);
                check.intern_ranges(intern, reason);
                body.intern_ranges(intern, reason);
            }
            CostKind::UpperBound(body) | CostKind::Heuristic { body, .. } => {
                body.intern_ranges(intern, reason);
            }
            CostKind::Call(call) => {
                for arg in call.arguments.iter_mut().chain(call.receiver.iter_mut()) {
                    arg.value.intern_reasons(reason);
                    arg.length.intern_reasons(reason);
                }
                call.arguments.shrink_to_fit();
                if let Some(builtin) = &mut call.builtin {
                    builtin.expression.intern_reasons(reason);
                }
            }
            CostKind::Unknown(text) => reason(text),
            _ => {}
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum CostKind {
    Unit,
    Sequence(Vec<CostNode>),
    Branch {
        test: Box<CostNode>,
        arms: Vec<CostNode>,
    },
    Repeat(Box<LoopCost>),
    Call(Box<CallCost>),
    UpperBound(Box<CostNode>),
    Heuristic {
        body: Box<CostNode>,
        assumptions: Vec<String>,
    },
    Unknown(Arc<str>),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoopCost {
    pub count: SizeExpr,
    pub induction: Option<String>,
    pub induction_bound: Option<SizeExpr>,
    pub setup: Box<CostNode>,
    pub check: Box<CostNode>,
    pub body: Box<CostNode>,
    pub status: EstimateStatus,
    pub assumptions: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CallCost {
    pub arguments: Vec<ArgumentSize>,
    pub receiver: Option<ArgumentSize>,
    pub builtin: Option<LibraryCost>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LibraryCost {
    pub expression: SizeExpr,
    pub status: EstimateStatus,
    pub description: String,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Powers {
    pub polynomial: u32,
    pub logarithmic: u32,
}
/// A sum of positive monomials; constants and coefficients are omitted asymptotically.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Growth {
    pub terms: Vec<BTreeMap<String, Powers>>,
}
impl Growth {
    pub(super) fn one() -> Self {
        Self {
            terms: vec![BTreeMap::new()],
        }
    }
    pub(super) fn symbol(name: String) -> Self {
        Self {
            terms: vec![BTreeMap::from([(
                name,
                Powers {
                    polynomial: 1,
                    logarithmic: 0,
                },
            )])],
        }
    }
    pub(super) fn sum(&self, other: &Self) -> Result<Self, String> {
        let mut terms = self.terms.clone();
        terms.extend(other.terms.iter().cloned());
        Self { terms }.normalize()
    }
    pub(super) fn product(&self, other: &Self) -> Result<Self, String> {
        if self.terms.len().saturating_mul(other.terms.len()) > MAX_TERMS {
            return Err("symbolic term budget exceeded".into());
        }
        let mut terms = Vec::new();
        for a in &self.terms {
            for b in &other.terms {
                let mut term = a.clone();
                for (symbol, power) in b {
                    let p = term.entry(symbol.clone()).or_default();
                    p.polynomial = p
                        .polynomial
                        .checked_add(power.polynomial)
                        .filter(|p| *p <= MAX_POWER)
                        .ok_or("symbolic exponent budget exceeded")?;
                    p.logarithmic = p
                        .logarithmic
                        .checked_add(power.logarithmic)
                        .filter(|p| *p <= MAX_POWER)
                        .ok_or("symbolic exponent budget exceeded")?;
                }
                terms.push(term);
            }
        }
        Self { terms }.normalize()
    }
    pub(super) fn pow(&self, power: u32) -> Result<Self, String> {
        let mut out = Self::one();
        for _ in 0..power {
            out = out.product(self)?;
        }
        Ok(out)
    }
    pub(super) fn log(&self) -> Result<Self, String> {
        let mut terms = Vec::new();
        for term in &self.terms {
            for (symbol, p) in term {
                if p.polynomial == 0 && p.logarithmic > 0 {
                    return Err("iterated logarithms are not supported".into());
                }
                if p.polynomial > 0 {
                    terms.push(BTreeMap::from([(
                        symbol.clone(),
                        Powers {
                            polynomial: 0,
                            logarithmic: 1,
                        },
                    )]));
                }
            }
        }
        if terms.is_empty() {
            Ok(Self::one())
        } else {
            Self { terms }.normalize()
        }
    }
    pub(super) fn normalize(mut self) -> Result<Self, String> {
        self.terms.sort();
        self.terms.dedup();
        let dominated = |a: &BTreeMap<String, Powers>, b: &BTreeMap<String, Powers>| {
            a.iter()
                .all(|(s, p)| b.get(s).copied().unwrap_or_default() >= *p)
        };
        let terms = self
            .terms
            .iter()
            .enumerate()
            .filter(|(i, a)| {
                !self
                    .terms
                    .iter()
                    .enumerate()
                    .any(|(j, b)| i != &j && dominated(a, b))
            })
            .map(|(_, t)| t.clone())
            .collect::<Vec<_>>();
        if terms.len() > MAX_TERMS {
            Err("symbolic term budget exceeded".into())
        } else {
            self.terms = terms;
            Ok(self)
        }
    }
    pub(super) fn substitute(&self, bindings: &BTreeMap<String, Growth>) -> Result<Self, String> {
        let mut out = Self::default();
        for term in &self.terms {
            let mut product = Self::one();
            for (symbol, p) in term {
                let value = bindings
                    .get(symbol)
                    .ok_or_else(|| format!("size substitution unavailable: {symbol}"))?;
                product = product.product(&value.pow(p.polynomial)?)?;
                if p.logarithmic > 0 {
                    product = product.product(&value.log()?.pow(p.logarithmic)?)?;
                }
            }
            out = out.sum(&product)?;
        }
        Ok(out)
    }
    pub fn class(&self) -> GrowthClass {
        self.terms
            .iter()
            .map(|t| {
                t.values().fold(GrowthClass::default(), |mut c, p| {
                    c.polynomial += p.polynomial;
                    c.logarithmic += p.logarithmic;
                    c
                })
            })
            .max()
            .unwrap_or_default()
    }
    pub fn expression(&self) -> String {
        if self.terms.is_empty() {
            return "0".into();
        }
        let mut terms = self
            .terms
            .iter()
            .map(|t| {
                let mut factors = Vec::new();
                for (s, p) in t {
                    if p.polynomial > 0 {
                        factors.push(power_text(s, p.polynomial));
                    }
                    if p.logarithmic > 0 {
                        factors.push(power_text(&format!("log({s})"), p.logarithmic));
                    }
                }
                if factors.is_empty() {
                    "1".into()
                } else {
                    factors.join("·")
                }
            })
            .collect::<Vec<_>>();
        terms.sort();
        terms.join(" + ")
    }
    pub(super) fn symbols(&self) -> BTreeSet<String> {
        self.terms.iter().flat_map(|t| t.keys().cloned()).collect()
    }
}
pub fn reason_category(reason: &str) -> &str {
    reason
        .split_once(": ")
        .map(|(category, _)| category)
        .unwrap_or(reason)
}
pub(super) fn shared_model() -> Arc<str> {
    static SHARED: std::sync::OnceLock<Arc<str>> = std::sync::OnceLock::new();
    SHARED.get_or_init(|| Arc::from(MODEL)).clone()
}
pub(super) fn power_text(base: &str, power: u32) -> String {
    if power == 1 {
        base.into()
    } else {
        format!("{base}^{power}")
    }
}
pub(super) fn size_growth(expr: &SizeExpr, depth: usize) -> Result<Growth, String> {
    if depth > MAX_DEPTH {
        return Err("size expression depth limit exceeded".into());
    }
    match expr {
        SizeExpr::Constant(0) => Ok(Growth::default()),
        SizeExpr::Constant(_) => Ok(Growth::one()),
        SizeExpr::Input { parameter, measure } => Ok(Growth::symbol(match measure {
            SizeMeasure::Length => format!("{parameter}.len()"),
            SizeMeasure::Value => parameter.clone(),
        })),
        SizeExpr::Sum(items) => items.iter().try_fold(Growth::default(), |out, e| {
            out.sum(&size_growth(e, depth + 1)?)
        }),
        SizeExpr::Product(items) => items.iter().try_fold(Growth::one(), |out, e| {
            out.product(&size_growth(e, depth + 1)?)
        }),
        SizeExpr::Log(e) => size_growth(e, depth + 1)?.log(),
        SizeExpr::Unknown(reason) => Err(reason.to_string()),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CostExplanation {
    pub range: SourceRange,
    pub rule: Arc<str>,
    pub detail: Arc<str>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SizeVariable {
    pub symbol: String,
    pub definition: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ComplexityEstimate {
    pub status: EstimateStatus,
    pub formula: Option<String>,
    pub normalized: Option<String>,
    pub local_formula: Option<String>,
    pub known_formula: Option<String>,
    pub variables: Vec<SizeVariable>,
    pub explanations: Vec<Arc<CostExplanation>>,
    pub assumptions: Vec<Arc<str>>,
    pub unknown_reasons: Vec<Arc<str>>,
    pub model: Arc<str>,
    /// Formal expression for machine consumers; absent when the total is unknown.
    pub expression: Option<Growth>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ComplexityDiagnostic {
    pub estimate: Arc<ComplexityEstimate>,
    pub threshold: Option<String>,
    pub message: String,
}
