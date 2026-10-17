//! Symbolic execution-cost analysis. Bounds describe the documented unit-cost model,
//! not wall-clock time. Unknown operations never silently acquire constant cost.
use crate::config::{Config, ConfigLoader, KeySpec};
use crate::graph::{CallGraph, NodeKind};
use crate::ir::{Confidence, FileModel, FunctionModel, SourceRange};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

const MAX_TERMS: usize = 128;
const MAX_POWER: u32 = 128;
const MAX_DEPTH: usize = 256;
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

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UnknownPolicy {
    #[default]
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ComplexityLimits {
    pub warn: Option<String>,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AlgorithmicOverride {
    pub enabled: Option<bool>,
    pub heuristics: Option<bool>,
    pub unknown: Option<UnknownPolicy>,
    pub limits: ComplexityLimits,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AlgorithmicPathOverride {
    pub glob: String,
    pub enabled: Option<bool>,
    pub heuristics: Option<bool>,
    pub unknown: Option<UnknownPolicy>,
    pub limits: ComplexityLimits,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AlgorithmicConfig {
    pub enabled: bool,
    pub heuristics: bool,
    pub unknown: UnknownPolicy,
    pub limits: ComplexityLimits,
    pub by_language: BTreeMap<String, AlgorithmicOverride>,
    pub by_path: Vec<AlgorithmicPathOverride>,
    pub by_entity: BTreeMap<String, AlgorithmicOverride>,
}
impl Default for AlgorithmicConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            heuristics: true,
            unknown: UnknownPolicy::Info,
            limits: ComplexityLimits::default(),
            by_language: BTreeMap::new(),
            by_path: Vec::new(),
            by_entity: BTreeMap::new(),
        }
    }
}
#[derive(Clone)]
pub struct Settings {
    pub enabled: bool,
    pub heuristics: bool,
    pub unknown: UnknownPolicy,
    pub limits: ComplexityLimits,
}
impl AlgorithmicConfig {
    pub fn effective(&self, language: &str, path: &str, entity: &str) -> Settings {
        self.effective_with_paths(
            language,
            entity,
            self.by_path.iter().filter(|o| {
                globset::Glob::new(&o.glob)
                    .ok()
                    .is_some_and(|g| g.compile_matcher().is_match(path))
            }),
        )
    }
    fn compiled_paths(&self) -> Vec<(&AlgorithmicPathOverride, globset::GlobMatcher)> {
        self.by_path
            .iter()
            .filter_map(|o| {
                globset::Glob::new(&o.glob)
                    .ok()
                    .map(|g| (o, g.compile_matcher()))
            })
            .collect()
    }
    fn effective_with_paths<'a>(
        &self,
        language: &str,
        entity: &str,
        paths: impl Iterator<Item = &'a AlgorithmicPathOverride>,
    ) -> Settings {
        let mut s = Settings {
            enabled: self.enabled,
            heuristics: self.heuristics,
            unknown: self.unknown.clone(),
            limits: self.limits.clone(),
        };
        let overlay = |s: &mut Settings,
                       enabled: Option<bool>,
                       heuristics: Option<bool>,
                       unknown: &Option<UnknownPolicy>,
                       limits: &ComplexityLimits| {
            if let Some(v) = enabled {
                s.enabled = v;
            }
            if let Some(v) = heuristics {
                s.heuristics = v;
            }
            if let Some(v) = unknown {
                s.unknown = v.clone();
            }
            if limits.warn.is_some() {
                s.limits.warn = limits.warn.clone();
            }
            if limits.error.is_some() {
                s.limits.error = limits.error.clone();
            }
        };
        if let Some(o) = self.by_language.get(language) {
            overlay(&mut s, o.enabled, o.heuristics, &o.unknown, &o.limits);
        }
        for o in paths {
            overlay(&mut s, o.enabled, o.heuristics, &o.unknown, &o.limits);
        }
        if let Some(o) = self.by_entity.get(entity) {
            overlay(&mut s, o.enabled, o.heuristics, &o.unknown, &o.limits);
        }
        s
    }
}
pub fn register_config(loader: &mut ConfigLoader) {
    let mapping: crate::config::Validator = Arc::new(|v| {
        if v.is_mapping() {
            Ok(())
        } else {
            Err("expected mapping".into())
        }
    });
    loader.register_key(KeySpec::new(
        "algorithmic_complexity",
        "Symbolic worst-case execution cost",
        mapping.clone(),
    ));
    for key in [
        "algorithmic_complexity.by_language",
        "algorithmic_complexity.by_entity",
        "algorithmic_complexity.by_language.*",
        "algorithmic_complexity.by_entity.*",
        "algorithmic_complexity.by_path.*",
    ] {
        loader.register_key(KeySpec::new(key, "Complexity overrides", mapping.clone()));
    }
    loader.register_key(KeySpec::new(
        "algorithmic_complexity.by_path",
        "Ordered path overrides",
        Arc::new(|v| {
            if v.is_sequence() {
                Ok(())
            } else {
                Err("expected list of path overrides".into())
            }
        }),
    ));
    loader.register_key(KeySpec::new(
        "algorithmic_complexity.by_path.*.glob",
        "Path override glob",
        Arc::new(|v| {
            let glob = v.as_str().ok_or("expected glob string")?;
            globset::Glob::new(glob)
                .map(|_| ())
                .map_err(|e| format!("invalid algorithmic path glob: {e}"))
        }),
    ));
    for scope in [
        "algorithmic_complexity",
        "algorithmic_complexity.by_language.*",
        "algorithmic_complexity.by_entity.*",
        "algorithmic_complexity.by_path.*",
    ] {
        let optional = scope != "algorithmic_complexity";
        for field in ["enabled", "heuristics"] {
            loader.register_key(KeySpec::new(
                format!("{scope}.{field}"),
                "Boolean complexity setting",
                Arc::new(move |v| {
                    if v.is_bool() || (optional && v.is_null()) {
                        Ok(())
                    } else {
                        Err("expected boolean".into())
                    }
                }),
            ));
        }
        loader.register_key(KeySpec::new(
            format!("{scope}.unknown"),
            "Unknown cost severity",
            Arc::new(move |v| {
                if (optional && v.is_null())
                    || v.as_str()
                        .is_some_and(|s| ["info", "warn", "error"].contains(&s))
                {
                    Ok(())
                } else {
                    Err("expected info, warn or error".into())
                }
            }),
        ));
        loader.register_key(KeySpec::new(
            format!("{scope}.limits"),
            "Symbolic complexity thresholds",
            mapping.clone(),
        ));
        for field in ["warn", "error"] {
            loader.register_key(KeySpec::new(
                format!("{scope}.limits.{field}"),
                "Symbolic complexity threshold",
                Arc::new(|v| {
                    if v.is_null() {
                        return Ok(());
                    }
                    let limit = v.as_str().ok_or("expected symbolic complexity string")?;
                    parse_limit(limit).map(|_| ())
                }),
            ));
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
    fn intern_reasons(&mut self, intern: &mut impl FnMut(&mut Arc<str>)) {
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
    fn one() -> Self {
        Self {
            terms: vec![BTreeMap::new()],
        }
    }
    fn symbol(name: String) -> Self {
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
    fn sum(&self, other: &Self) -> Result<Self, String> {
        let mut terms = self.terms.clone();
        terms.extend(other.terms.iter().cloned());
        Self { terms }.normalize()
    }
    fn product(&self, other: &Self) -> Result<Self, String> {
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
    fn pow(&self, power: u32) -> Result<Self, String> {
        let mut out = Self::one();
        for _ in 0..power {
            out = out.product(self)?;
        }
        Ok(out)
    }
    fn log(&self) -> Result<Self, String> {
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
    fn normalize(mut self) -> Result<Self, String> {
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
    fn substitute(&self, bindings: &BTreeMap<String, Growth>) -> Result<Self, String> {
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
    fn symbols(&self) -> BTreeSet<String> {
        self.terms.iter().flat_map(|t| t.keys().cloned()).collect()
    }
}
pub fn reason_category(reason: &str) -> &str {
    reason
        .split_once(": ")
        .map(|(category, _)| category)
        .unwrap_or(reason)
}
fn shared_model() -> Arc<str> {
    static SHARED: std::sync::OnceLock<Arc<str>> = std::sync::OnceLock::new();
    SHARED.get_or_init(|| Arc::from(MODEL)).clone()
}
fn power_text(base: &str, power: u32) -> String {
    if power == 1 {
        base.into()
    } else {
        format!("{base}^{power}")
    }
}
fn size_growth(expr: &SizeExpr, depth: usize) -> Result<Growth, String> {
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
#[derive(Clone)]
struct Evaluation {
    growth: Growth,
    status: EstimateStatus,
    explanations: Vec<Arc<CostExplanation>>,
    assumptions: BTreeSet<Arc<str>>,
    unknown_reasons: BTreeSet<Arc<str>>,
}
impl Evaluation {
    fn unit() -> Self {
        Self {
            growth: Growth::one(),
            status: EstimateStatus::Tight,
            explanations: Vec::new(),
            assumptions: BTreeSet::new(),
            unknown_reasons: BTreeSet::new(),
        }
    }
    fn unknown(reason: impl Into<Arc<str>>, range: &SourceRange) -> Self {
        let reason: Arc<str> = reason.into();
        let mut out = Self::unit();
        out.growth = Growth::default();
        out.status = EstimateStatus::Unknown;
        out.unknown_reasons.insert(reason.clone());
        out.explain(range, "unknown", reason.clone());
        out
    }
    fn explain(&mut self, range: &SourceRange, rule: &str, detail: impl Into<Arc<str>>) {
        if self.explanations.len() < MAX_TERMS {
            self.explanations.push(Arc::new(CostExplanation {
                range: range.clone(),
                rule: rule.into(),
                detail: detail.into(),
            }));
        }
    }
    fn merge(&mut self, other: Self, range: &SourceRange) {
        match self.growth.sum(&other.growth) {
            Ok(g) => self.growth = g,
            Err(e) => {
                self.status = EstimateStatus::Unknown;
                self.unknown_reasons.insert(e.into());
            }
        }
        self.status = self.status.clone().max(other.status);
        self.assumptions.extend(other.assumptions);
        for reason in other.unknown_reasons {
            if !self
                .unknown_reasons
                .iter()
                .any(|r| reason_category(r) == reason_category(&reason))
            {
                self.unknown_reasons.insert(reason);
            }
        }
        for explanation in other.explanations {
            let repeated = self.explanations.iter().any(|e| {
                (e.range == explanation.range
                    && e.rule == explanation.rule
                    && e.detail == explanation.detail)
                    || (e.rule.as_ref() == "unknown"
                        && explanation.rule.as_ref() == "unknown"
                        && reason_category(&e.detail) == reason_category(&explanation.detail))
            });
            if !repeated && self.explanations.len() < MAX_TERMS {
                self.explanations.push(explanation);
            }
        }
        if self.status == EstimateStatus::Unknown && self.unknown_reasons.is_empty() {
            self.unknown_reasons
                .insert(format!("incomplete analysis at line {}", range.start_line).into());
        }
    }
    fn finish(self, local: Option<String>) -> ComplexityEstimate {
        let known = self.growth.expression();
        let notation = if self.status == EstimateStatus::Tight {
            "Θ"
        } else {
            "O"
        };
        let known_status = matches!(
            self.status,
            EstimateStatus::Tight | EstimateStatus::UpperBound | EstimateStatus::Heuristic
        );
        let variables = self
            .growth
            .symbols()
            .into_iter()
            .map(|symbol| {
                let definition = if let Some(name) = symbol.strip_suffix(".len()") {
                    format!("Native length of input {name}: elements for collections; string units depend on the language model")
                } else {
                    format!("nonnegative magnitude of input {symbol}, not its bit length")
                };
                SizeVariable { symbol, definition }
            })
            .collect();
        ComplexityEstimate {
            status: self.status,
            formula: known_status.then(|| format!("{notation}({known})")),
            normalized: known_status
                .then(|| format!("{notation}({})", self.growth.class().expression())),
            local_formula: local,
            known_formula: (!self.growth.terms.is_empty()).then(|| format!("O({known})")),
            variables,
            explanations: self.explanations,
            assumptions: self.assumptions.into_iter().collect(),
            unknown_reasons: self.unknown_reasons.into_iter().collect(),
            model: shared_model(),
            expression: known_status.then_some(self.growth),
        }
    }
}

struct FunctionEntry<'a> {
    file: &'a str,
    language: &'a str,
    function: &'a FunctionModel,
    parse_error: bool,
    settings: Settings,
}
struct Engine<'a> {
    functions: Vec<FunctionEntry<'a>>,
    targets: BTreeMap<(Arc<str>, usize, usize, usize, usize), Vec<usize>>,
    results: Vec<Option<Evaluation>>,
    active: BTreeSet<usize>,
    recursive: BTreeSet<usize>,
    stack: Vec<usize>,
    cargo_ambiguous: BTreeSet<usize>,
}
impl Engine<'_> {
    fn function(&mut self, index: usize, depth: usize) -> Evaluation {
        if let Some(result) = &self.results[index] {
            return result.clone();
        }
        let range = self.functions[index].function.range.clone();
        if depth > MAX_DEPTH {
            return Evaluation::unknown("interprocedural analysis depth limit exceeded", &range);
        }
        if self.active.contains(&index) {
            if let Some(start) = self.stack.iter().position(|i| *i == index) {
                self.recursive.extend(self.stack[start..].iter().copied());
            }
            return Evaluation::unknown("recursive call component is not supported", &range);
        }
        let entry = &self.functions[index];
        let settings = &entry.settings;
        let unsupported = if !settings.enabled {
            Some("callee analysis is disabled")
        } else if !supported_language(entry.language) {
            Some("callee language is not supported")
        } else if entry.parse_error {
            Some("function contains a parse error")
        } else {
            None
        };
        if let Some(reason) = unsupported {
            let result = Evaluation::unknown(reason, &range);
            self.results[index] = Some(result.clone());
            return result;
        }
        if self.cargo_ambiguous.contains(&index) {
            let result = Evaluation::unknown("source is compiled for multiple Cargo targets with different call targets; a single source cost cannot represent those contexts", &range);
            self.results[index] = Some(result.clone());
            return result;
        }
        let function = entry.function;
        let Some(ir) = function.cost_ir.as_ref() else {
            return Evaluation::unknown("cost IR is unavailable", &range);
        };
        self.active.insert(index);
        self.stack.push(index);
        let mut result = self.node(ir, depth + 1, settings.heuristics, false);
        self.stack.pop();
        self.active.remove(&index);
        if self.recursive.contains(&index) {
            result.merge(
                Evaluation::unknown("recursive call component is not supported", &range),
                &range,
            );
        }
        self.results[index] = Some(result.clone());
        result
    }
    fn node(&mut self, node: &CostNode, depth: usize, heuristics: bool, local: bool) -> Evaluation {
        if depth > MAX_DEPTH {
            return Evaluation::unknown("cost IR depth limit exceeded", &node.range);
        }
        let mut out = match &node.kind {
            CostKind::Unit => Evaluation::unit(),
            CostKind::Unknown(reason) => Evaluation::unknown(reason.clone(), &node.range),
            CostKind::Sequence(items) => {
                let mut out = Evaluation::unit();
                for item in items {
                    out.merge(self.node(item, depth + 1, heuristics, local), &node.range);
                }
                out
            }
            CostKind::Branch { test, arms } => {
                let mut out = self.node(test, depth + 1, heuristics, local);
                for arm in arms {
                    out.merge(self.node(arm, depth + 1, heuristics, local), &node.range);
                }
                out.status = out.status.max(EstimateStatus::UpperBound);
                out.explain(&node.range,"branch","Test cost plus maximum of branch costs (represented by their asymptotically equivalent sum); branch feasibility is not proven.");
                out
            }
            CostKind::UpperBound(body) => {
                let mut out = self.node(body, depth + 1, heuristics, local);
                out.status = out.status.max(EstimateStatus::UpperBound);
                out.explain(
                    &node.range,
                    "early_exit",
                    "Early exit or a size upper bound prevents a tightness proof.",
                );
                out
            }
            CostKind::Heuristic { body, assumptions } => {
                if !heuristics {
                    Evaluation::unknown("heuristic inference is disabled", &node.range)
                } else {
                    let mut out = self.node(body, depth + 1, heuristics, local);
                    out.status = out.status.max(EstimateStatus::Heuristic);
                    out.assumptions
                        .extend(assumptions.iter().map(|s| Arc::from(s.as_str())));
                    out
                }
            }
            CostKind::Repeat(repeat) => {
                let LoopCost {
                    count,
                    induction,
                    induction_bound,
                    setup,
                    check,
                    body,
                    status,
                    assumptions,
                } = repeat.as_ref();
                let mut out = self.node(setup, depth + 1, heuristics, local);
                if *status == EstimateStatus::Heuristic && !heuristics {
                    out.merge(
                        Evaluation::unknown("heuristic loop inference is disabled", &node.range),
                        &node.range,
                    );
                    return out;
                }
                if matches!(count, SizeExpr::Constant(0)) {
                    out.merge(self.node(check, depth + 1, heuristics, local), &node.range);
                    return out;
                }
                let mut repeated = self.node(check, depth + 1, heuristics, local);
                repeated.merge(self.node(body, depth + 1, heuristics, local), &node.range);
                match size_growth(count, 0) {
                    Ok(iterations) => {
                        if let Some(induction) = induction {
                            let mut bindings = repeated
                                .growth
                                .symbols()
                                .into_iter()
                                .map(|s| (s.clone(), Growth::symbol(s)))
                                .collect::<BTreeMap<_, _>>();
                            let induction_growth = induction_bound
                                .as_ref()
                                .map(|e| size_growth(e, 0))
                                .unwrap_or_else(|| Ok(iterations.clone()));
                            if induction_bound.is_some()
                                && repeated.growth.symbols().contains(induction)
                            {
                                repeated.status = repeated.status.max(EstimateStatus::UpperBound);
                            }
                            match induction_growth {
                                Ok(g) => {
                                    bindings.insert(induction.clone(), g);
                                }
                                Err(e) => {
                                    repeated.merge(Evaluation::unknown(e, &node.range), &node.range)
                                }
                            }
                            // Sum i^p*log(i)^q, i=0..n, is Theta(n^(p+1)*log(n)^q).
                            match repeated.growth.substitute(&bindings) {
                                Ok(g) => repeated.growth = g,
                                Err(e) => {
                                    repeated.merge(Evaluation::unknown(e, &node.range), &node.range)
                                }
                            }
                        }
                        match iterations.product(&repeated.growth) {
                            Ok(g) => repeated.growth = g,
                            Err(e) => {
                                repeated.merge(Evaluation::unknown(e, &node.range), &node.range)
                            }
                        }
                        repeated.status = repeated.status.max(status.clone());
                        repeated
                            .assumptions
                            .extend(assumptions.iter().map(|s| Arc::from(s.as_str())));
                        repeated.explain(&node.range,"loop",format!("Sum iteration costs over {} iterations; includes loop checks and body. {}",iterations.expression(),if induction_bound.is_some() { "Counter-dependent body costs use the maximum counter-size bound for this geometric progression." } else if induction.is_some() { "Polynomial induction-variable sums preserve order." } else { "" }));
                        out.merge(repeated, &node.range);
                        // The final condition check also runs when there are zero iterations.
                        out.merge(self.node(check, depth + 1, heuristics, local), &node.range);
                    }
                    Err(e) => {
                        out.merge(repeated, &node.range);
                        out.merge(Evaluation::unknown(e, &node.range), &node.range);
                    }
                }
                out
            }
            CostKind::Call(call) => {
                let CallCost {
                    arguments,
                    receiver,
                    builtin,
                } = call.as_ref();
                if let Some(builtin) = builtin {
                    match size_growth(&builtin.expression, 0) {
                        Ok(growth) => {
                            let mut out = Evaluation::unit();
                            out.growth = growth.sum(&Growth::one()).unwrap_or(growth);
                            out.status = builtin.status.clone();
                            out.explain(&node.range, "library", builtin.description.as_str());
                            out
                        }
                        Err(e) => Evaluation::unknown(e, &node.range),
                    }
                } else if local {
                    let mut out = Evaluation::unit();
                    out.assumptions.insert("Local cost excludes execution of project callees; argument evaluation is included.".into());
                    out
                } else {
                    let key = range_key(&node.range);
                    let target = self
                        .targets
                        .get(&key)
                        .filter(|targets| targets.len() == 1)
                        .map(|targets| targets[0]);
                    if let Some(target) = target {
                        let callee = self.functions[target].function;
                        let parameters = &callee.parameters;
                        let fqn = &callee.fqn;
                        let mut out = self.function(target, depth + 1);
                        let mut inputs = arguments.iter();
                        let mut bindings = BTreeMap::new();
                        let needed = out.growth.symbols();
                        for p in parameters {
                            let arg = if p.implicit_receiver {
                                // UFCS supplies a method receiver as its first explicit argument.
                                receiver.as_ref().or_else(|| inputs.next())
                            } else {
                                inputs.next()
                            };
                            for (symbol, size) in [
                                (p.name.clone(), arg.map(|a| &a.value)),
                                (format!("{}.len()", p.name), arg.map(|a| &a.length)),
                            ] {
                                if !needed.contains(&symbol) {
                                    continue;
                                }
                                if let Some(arg) = arg {
                                    out.status = out.status.max(arg.status.clone());
                                    if arg.status == EstimateStatus::UpperBound {
                                        out.assumptions.insert("Argument size uses a partial-slice upper bound under successful bounds checks.".into());
                                    }
                                }
                                match size.map(|s| size_growth(s, 0)).unwrap_or_else(|| {
                                    Err(format!("argument size unavailable for {}", p.name))
                                }) {
                                    Ok(g) => {
                                        bindings.insert(symbol, g);
                                    }
                                    Err(e) => {
                                        out.unknown_reasons.insert(e.into());
                                        out.status = EstimateStatus::Unknown;
                                    }
                                }
                            }
                        }
                        match out.growth.substitute(&bindings) {
                            Ok(g) => out.growth = g,
                            Err(e) => {
                                out.status = EstimateStatus::Unknown;
                                out.unknown_reasons.insert(e.into());
                                out.growth = Growth::default();
                            }
                        }
                        out.explain(&node.range,"call",format!("Call {fqn}; substitute callee input sizes using this call's arguments."));
                        out
                    } else {
                        Evaluation::unknown(
                            "unresolved or ambiguous call has unknown execution cost",
                            &node.range,
                        )
                    }
                }
            }
        };
        if out.explanations.len() > MAX_TERMS {
            out.explanations.truncate(MAX_TERMS);
        }
        out
    }
}
fn range_key(range: &SourceRange) -> (Arc<str>, usize, usize, usize, usize) {
    (
        range.file.clone(),
        range.start_line,
        range.start_column,
        range.end_line,
        range.end_column,
    )
}

/// Recompute global summaries after every graph rebuild. Only source cost IR is cached.
pub fn analyze(files: &mut [FileModel], graph: &mut CallGraph, config: &Config) {
    let paths = config.algorithmic_complexity.compiled_paths();
    let functions = files
        .iter()
        .flat_map(|file| {
            let paths = &paths;
            file.functions
                .iter()
                .filter(|f| !f.is_module_initializer())
                .map(move |f| FunctionEntry {
                    settings: config.algorithmic_complexity.effective_with_paths(
                        &file.language,
                        &f.fqn,
                        paths
                            .iter()
                            .filter(|(_, matcher)| matcher.is_match(&file.path))
                            .map(|(o, _)| *o),
                    ),
                    file: &file.path,
                    language: &file.language,
                    function: f,
                    parse_error: file.parse_errors.iter().any(|e| {
                        e.range.start_line <= f.range.end_line
                            && e.range.end_line >= f.range.start_line
                    }),
                })
        })
        .collect::<Vec<_>>();
    let mut by_id = functions
        .iter()
        .enumerate()
        .map(|(i, f)| {
            (
                format!("fn:{}:{}:{}", f.language, f.file, f.function.fqn),
                i,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut cargo_signatures =
        BTreeMap::<usize, Vec<BTreeSet<(SourceRange, String, String)>>>::new();
    for node in graph.nodes.iter().filter(|node| node.source_fqn.is_some()) {
        let source = format!(
            "fn:{}:{}:{}",
            node.language.as_deref().unwrap_or("rust"),
            node.file.as_deref().unwrap_or(""),
            node.source_fqn.as_deref().unwrap()
        );
        if let Some(&index) = by_id.get(&source) {
            by_id.insert(node.id.clone(), index);
            let signature = graph
                .outgoing_edges(&node.id)
                .map(|edge| {
                    let target = graph.node(&edge.to).unwrap();
                    let source_target = target
                        .source_fqn
                        .as_ref()
                        .map(|source| {
                            format!(
                                "fn:{}:{}:{source}",
                                target.language.as_deref().unwrap_or("rust"),
                                target.file.as_deref().unwrap_or("")
                            )
                        })
                        .unwrap_or_else(|| target.id.clone());
                    (
                        edge.callsite.clone(),
                        source_target,
                        format!("{:?}", edge.confidence),
                    )
                })
                .collect();
            cargo_signatures.entry(index).or_default().push(signature);
        }
    }
    let cargo_ambiguous = cargo_signatures
        .into_iter()
        .filter_map(|(index, signatures)| {
            signatures
                .windows(2)
                .any(|pair| pair[0] != pair[1])
                .then_some(index)
        })
        .collect();
    let mut targets = BTreeMap::<_, Vec<usize>>::new();
    let needs_calls = functions
        .iter()
        .any(|f| supported_language(f.language) && f.settings.enabled);
    if needs_calls {
        for edge in &graph.edges {
            if edge.confidence != Confidence::Resolved {
                continue;
            }
            if let Some(index) = by_id.get(edge.to.as_ref()) {
                targets
                    .entry(range_key(&edge.callsite))
                    .or_default()
                    .push(*index);
            }
        }
        // Removing the same key repeatedly is harmless; no temporary set of
        // every unresolved callsite needs to be allocated.
        for edge in graph
            .edges
            .iter()
            .filter(|e| e.confidence != Confidence::Resolved)
        {
            targets.remove(&range_key(&edge.callsite));
        }
    }
    for value in targets.values_mut() {
        value.sort();
        value.dedup();
    }
    let count = functions.len();
    let mut engine = Engine {
        functions,
        targets,
        results: vec![None; count],
        active: BTreeSet::new(),
        recursive: BTreeSet::new(),
        stack: Vec::new(),
        cargo_ambiguous,
    };
    let mut estimates = Vec::new();
    for index in 0..count {
        let entry = &engine.functions[index];
        let settings = &entry.settings;
        let status = if !settings.enabled {
            Some(EstimateStatus::Disabled)
        } else if !supported_language(entry.language) {
            Some(EstimateStatus::Unsupported)
        } else {
            None
        };
        if let Some(status) = status {
            let mut estimate =
                Evaluation::unknown(status.label(), &entry.function.range).finish(None);
            estimate.status = status;
            estimate.known_formula = None;
            estimates.push(estimate);
            continue;
        }
        let function = entry.function;
        let heuristics = settings.heuristics;
        let local = function
            .cost_ir
            .as_ref()
            .map(|ir| engine.node(ir, 0, heuristics, true).finish(None))
            .and_then(|e| e.formula);
        estimates.push(engine.function(index, 0).finish(local));
    }
    drop(engine);
    let estimates = estimates.into_iter().map(Arc::new).collect::<Vec<_>>();
    for node in &mut graph.nodes {
        if node.kind == NodeKind::Function {
            node.algorithmic_complexity =
                by_id.get(&node.id).map(|&index| estimates[index].clone());
        }
    }
    let mut estimates = estimates.into_iter();
    for file in files {
        for function in &mut file.functions {
            function.algorithmic_complexity = if function.is_module_initializer() {
                None
            } else {
                estimates.next()
            };
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ComplexityDiagnostic {
    pub estimate: Arc<ComplexityEstimate>,
    pub threshold: Option<String>,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn limits_compare_actual_growth_orders() {
        assert!(parse_limit("N*log(N)").unwrap() < parse_limit("N²").unwrap());
        assert_eq!(
            parse_limit("O(N^2*log(N)^3)").unwrap(),
            GrowthClass {
                polynomial: 2,
                logarithmic: 3
            }
        );
        for invalid in ["", "n", "N+N", "2^N", "N^-1", "N^129", "log(log(N))"] {
            assert!(parse_limit(invalid).is_err(), "{invalid}");
        }
    }
    #[test]
    fn algebra_preserves_independent_sizes_and_composes() {
        let n = Growth::symbol("n".into());
        let m = Growth::symbol("m".into());
        assert_eq!(n.sum(&m).unwrap().expression(), "m + n");
        assert_eq!(n.product(&m).unwrap().class().polynomial, 2);
        assert_eq!(n.sum(&n.product(&n).unwrap()).unwrap().expression(), "n^2");
        let callee = n.product(&n.log().unwrap()).unwrap();
        let binding = BTreeMap::from([("n".into(), m.clone())]);
        assert_eq!(
            callee.substitute(&binding).unwrap().class(),
            GrowthClass {
                polynomial: 1,
                logarithmic: 1
            }
        );
        assert!(callee.substitute(&BTreeMap::new()).is_err());
    }
}

/// Symbolic rules deliberately bypass the numeric metric registry and aggregations.
pub fn violations(files: &[FileModel], config: &Config) -> Vec<crate::rules::Violation> {
    let paths = config.algorithmic_complexity.compiled_paths();
    use crate::rules::{Severity, Violation};
    let mut out = Vec::new();
    for file in files.iter().filter(|f| !f.vendor) {
        for function in &file.functions {
            let settings = config.algorithmic_complexity.effective_with_paths(
                &file.language,
                &function.fqn,
                paths
                    .iter()
                    .filter(|(_, matcher)| matcher.is_match(&file.path))
                    .map(|(o, _)| *o),
            );
            if !settings.enabled {
                continue;
            }
            let Some(estimate) = &function.algorithmic_complexity else {
                continue;
            };
            let (rule, severity, threshold, message) = if estimate.status == EstimateStatus::Unknown
            {
                let severity = match settings.unknown {
                    UnknownPolicy::Info => continue,
                    UnknownPolicy::Warn => Severity::Warn,
                    UnknownPolicy::Error => Severity::Error,
                };
                ("algorithmic_time_unknown",severity,None,format!("{}: стоимость неизвестна: {}. Это нарушение требования полноты анализа, а не доказательство высокой сложности.",function.fqn,estimate.unknown_reasons.join("; ")))
            } else if let Some(expression) = &estimate.expression {
                let class = expression.class();
                let error = settings
                    .limits
                    .error
                    .as_ref()
                    .filter(|v| parse_limit(v).is_ok_and(|l| class > l));
                let warn = settings
                    .limits
                    .warn
                    .as_ref()
                    .filter(|v| parse_limit(v).is_ok_and(|l| class > l));
                let (severity, threshold) =
                    if estimate.status == EstimateStatus::Tight && error.is_some() {
                        (Severity::Error, error.unwrap())
                    } else if let Some(threshold) = warn.or(error) {
                        (Severity::Warn, threshold)
                    } else {
                        continue;
                    };
                let formula = estimate.formula.as_deref().unwrap_or("unknown");
                let message = format!(
                    "{}: {} — {}, общий масштаб {} превышает допустимый O({}). {}",
                    function.fqn,
                    estimate.status.label(),
                    formula,
                    estimate.normalized.as_deref().unwrap_or("unknown"),
                    parse_limit(threshold).unwrap().expression(),
                    match estimate.status {
                        EstimateStatus::UpperBound =>
                            "Достижение этой верхней границы не доказано.",
                        EstimateStatus::Heuristic =>
                            "Результат основан на перечисленных предположениях.",
                        _ => "Точный класс установлен внутри указанной модели стоимости.",
                    }
                );
                (
                    "algorithmic_time",
                    severity,
                    Some(threshold.clone()),
                    message,
                )
            } else {
                continue;
            };
            let definitions = estimate
                .variables
                .iter()
                .map(|v| format!("{}: {}", v.symbol, v.definition))
                .collect::<Vec<_>>()
                .join("; ");
            let evidence = estimate
                .explanations
                .iter()
                .filter(|e| matches!(e.rule.as_ref(), "call" | "loop" | "library"))
                .map(|e| format!("{}:{}: {}", e.range.file, e.range.start_line, e.detail))
                .collect::<Vec<_>>()
                .join("; ");
            let message = format!("{message} {definitions} {evidence}");
            out.push(Violation {
                rule: rule.into(),
                severity,
                file: file.path.clone(),
                line: function.range.start_line,
                entity: function.fqn.clone(),
                value: None,
                limit: None,
                scope: "function".into(),
                architecture: None,
                algorithmic_complexity: Some(ComplexityDiagnostic {
                    estimate: estimate.clone(),
                    threshold,
                    message: message.trim().into(),
                }),
            });
        }
    }
    out
}

#[cfg(test)]
mod configuration_tests {
    use super::*;
    use std::path::PathBuf;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new(yaml: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "codefactor-cost-config-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("analyzer.yaml"), yaml).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn config_includes_and_overrides_preserve_precedence() {
        let fixture=Fixture::new("include: base.yaml\nalgorithmic_complexity:\n  by_language:\n    rust: {limits: {warn: 'N*log(N)'}}\n  by_path:\n    - glob: 'src/**'\n      limits: {warn: 'N^2'}\n  by_entity:\n    'src::f': {limits: {warn: 'N^3'}}\n");
        std::fs::write(
            fixture.0.join("base.yaml"),
            "algorithmic_complexity:\n  limits: {warn: N, error: 'N^4'}\n  unknown: warn\n",
        )
        .unwrap();
        let load = ConfigLoader::new()
            .load(&fixture.0.join("analyzer.yaml"), true, true)
            .unwrap();
        assert!(load.diagnostics.is_empty(), "{:?}", load.diagnostics);
        let config = &load.config.algorithmic_complexity;
        assert_eq!(
            config
                .effective("rust", "src/f.rs", "src::f")
                .limits
                .warn
                .as_deref(),
            Some("N^3")
        );
        assert_eq!(
            config
                .effective("rust", "src/g.rs", "src::g")
                .limits
                .warn
                .as_deref(),
            Some("N^2")
        );
        assert_eq!(
            config
                .effective("rust", "lib/g.rs", "lib::g")
                .limits
                .warn
                .as_deref(),
            Some("N*log(N)")
        );
        assert_eq!(
            config
                .effective("python", "lib/g.py", "g")
                .limits
                .warn
                .as_deref(),
            Some("N")
        );
        assert_eq!(
            config.effective("rust", "src/f.rs", "src::f").unknown,
            UnknownPolicy::Warn
        );
    }
    #[test]
    fn malformed_cost_config_does_not_discard_unrelated_valid_settings() {
        for config in [
            "{limits: {warn: '2^N'}}",
            "{enabled: maybe}",
            "{unknown: pass}",
            "{limits: {warn: 'N^3', error: N}}",
            "{by_path: [{glob: '['}]}",
        ] {
            let fixture = Fixture::new(&format!(
                "cache: {{enabled: false}}\nalgorithmic_complexity: {config}\n"
            ));
            let load = ConfigLoader::new()
                .load(&fixture.0.join("analyzer.yaml"), false, true)
                .unwrap();
            assert!(!load.config.cache.enabled);
            assert!(!load.diagnostics.is_empty(), "{config}");
            assert!(load.config.algorithmic_complexity.limits.warn.is_none());
        }
        let fixture = Fixture::new("algorithmic_complexity: {limits: {warning: N}}\n");
        assert!(ConfigLoader::new()
            .load(&fixture.0.join("analyzer.yaml"), true, true)
            .is_err());
    }
}
