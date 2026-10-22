//! Symbolic execution-cost evaluator. Public data paths remain compatible.
pub(crate) mod model;
pub(crate) mod settings;
pub use crate::config::register_algorithmic_config as register_config;
use crate::{
    config::Config,
    graph::{CallGraph, NodeKind},
    ir::{Confidence, FileModel, FunctionModel, SourceRange},
};
pub use model::*;
use model::{MAX_DEPTH, MAX_TERMS};
pub use settings::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
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
pub fn violations(files: &[FileModel], config: &Config) -> Vec<crate::diagnostics::Violation> {
    let paths = config.algorithmic_complexity.compiled_paths();
    use crate::diagnostics::{Severity, Violation};
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
                module_cycle: None,
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
    use crate::ConfigLoader;
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
