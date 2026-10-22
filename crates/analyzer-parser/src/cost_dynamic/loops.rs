//! Loop progress, bounds and induction handling, separate from expression dispatch.
use super::*;
pub(super) struct LoopExtractor<'r, 'source> {
    pub(super) extractor: &'r Extractor<'source>,
}
impl<'source> std::ops::Deref for LoopExtractor<'_, 'source> {
    type Target = Extractor<'source>;
    fn deref(&self) -> &Self::Target {
        self.extractor
    }
}

struct CollectionLoop<'tree> {
    value: Node<'tree>,
    pattern: Node<'tree>,
    key: Option<Node<'tree>>,
    body: Node<'tree>,
}
struct Iteration {
    count: SizeExpr,
    induction: Option<String>,
    status: EstimateStatus,
    assumptions: Vec<String>,
}
struct WhileCounter<'tree> {
    test: Node<'tree>,
    body: Node<'tree>,
    left: Node<'tree>,
    right: Node<'tree>,
    counter: String,
    comparison: String,
    op: String,
    step: u64,
    initial: Option<Binding>,
}
struct WhileBound {
    count: SizeExpr,
    status: EstimateStatus,
    induction_bound: Option<SizeExpr>,
}
impl WhileBound {
    fn unknown() -> Self {
        Self {
            count: SizeExpr::Unknown("while-loop progress or starting bound is not modeled".into()),
            status: EstimateStatus::Tight,
            induction_bound: None,
        }
    }
}
impl LoopExtractor<'_, '_> {
    fn loop_cost(
        &self,
        n: Node<'_>,
        count: SizeExpr,
        induction: Option<String>,
        setup: CostNode,
        check: CostNode,
        body: CostNode,
        status: EstimateStatus,
        assumptions: Vec<String>,
    ) -> CostNode {
        self.make(
            n,
            CostKind::Repeat(Box::new(LoopCost {
                count,
                induction,
                induction_bound: None,
                setup: Box::new(setup),
                check: Box::new(check),
                body: Box::new(body),
                status,
                assumptions,
            })),
        )
    }
    fn body<'t>(&self, n: Node<'t>) -> Option<Node<'t>> {
        child(n, "body")
    }
    fn counter_binding(&self, n: Node<'_>, name: String) -> Binding {
        Binding {
            kind: Kind::Integer,
            sizes: ArgumentSize {
                value: SizeExpr::value(name),
                ..ArgumentSize::unknown("counter has no length")
            },
            initial: self.literal(n),
        }
    }
    pub(super) fn counted_for(&self, n: Node<'_>, env: &mut Env, depth: usize) -> CostNode {
        let (Some(init), Some(test), Some(update), Some(body)) = (
            child(n, "initializer").or_else(|| child(n, "initialize")),
            child(n, "condition"),
            child(n, "increment").or_else(|| child(n, "update")),
            self.body(n),
        ) else {
            return self.unknown(
                n,
                "for-loop initialization, condition or increment is missing",
            );
        };
        let mut local = env.clone();
        let setup = self.node(init, &mut local, false, depth + 1);
        let test = self.unwrap(test);
        let Some(left) = child(test, "left") else {
            return self.sequence(
                n,
                vec![setup, self.unknown(n, "unsupported for-loop condition")],
            );
        };
        let Some(right) = child(test, "right") else {
            return self.sequence(
                n,
                vec![setup, self.unknown(n, "unsupported for-loop bound")],
            );
        };
        let counter = self.name(left);
        let op = self.operator(test);
        let step = self.step(update, &counter);
        let start = local.get(&counter).and_then(|b| b.initial);
        let guarded = matches!(op.as_str(), "<" | "<=")
            && start.is_some()
            && matches!(step, Some((ref op, v)) if (op == "+" || op == "++") && v > 0)
            && self.numeric(left, &local)
            && self.numeric(right, &local)
            && self.write_count(body, &counter) == 0
            && self.stable(right, body);
        if !guarded {
            return self.sequence(
                n,
                vec![
                    setup,
                    self.unknown(
                        n,
                        "for-loop bound/progress is not established or changes in its body",
                    ),
                ],
            );
        }
        let step_value = step.unwrap().1;
        let mut count = self.sizes(right, &local).value;
        if let Some(stop) = self.literal(right) {
            let initial = start.unwrap();
            let distance = if op == "<=" {
                stop.saturating_sub(initial).saturating_add(1)
            } else {
                stop.saturating_sub(initial)
            };
            count = SizeExpr::Constant(if stop < initial {
                0
            } else {
                distance.div_ceil(step_value)
            });
        }
        let induction = format!("@iteration:{}", n.start_byte());
        local.insert(counter, self.counter_binding(left, induction.clone()));
        let check = self.node(test, &mut local, true, depth + 1);
        let body_cost = self.node(body, &mut local, true, depth + 1);
        let increment_cost = self.node(update, &mut local, true, depth + 1);
        let body_cost = self.sequence(body, vec![body_cost, increment_cost]);
        let mut status = EstimateStatus::Tight;
        let mut assumptions = Vec::new();
        if !matches!(count, SizeExpr::Constant(_))
            && (matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
                || (self.dialect == Dialect::Php && self.kind(right, &local) != Kind::Integer))
        {
            status = EstimateStatus::Heuristic;
            assumptions.push("Assume finite nonnegative numeric bounds, exact progressing counter increments, no floating-point stagnation/overflow and an unchanged bound.".into());
        }
        // Nonzero starts count the same order asymptotically, but induction is an upper size bound.
        if start != Some(0) {
            status = status.max(EstimateStatus::UpperBound);
        }
        self.invalidate(n, env);
        self.loop_cost(
            n,
            count,
            Some(induction),
            setup,
            check,
            body_cost,
            status,
            assumptions,
        )
    }
    pub(super) fn foreach(&self, n: Node<'_>, env: &mut Env, depth: usize) -> CostNode {
        let shape = match self.collection_loop(n) {
            Ok(shape) => shape,
            Err(error) => return error,
        };
        let CollectionLoop {
            value,
            pattern,
            key,
            body,
        } = shape;
        let mut local = env.clone();
        if let Some(key) = key {
            local.insert(
                self.name(key),
                Binding {
                    kind: Kind::Opaque, // PHP array keys may be integers or strings.
                    sizes: ArgumentSize::unknown("array key dimensions are not modeled"),
                    initial: None,
                },
            );
        }
        let mut iteration = match self.collection_iteration(n, value, pattern, env, &mut local) {
            Ok(iteration) => iteration,
            Err(error) => return error,
        };
        if !self.stable(value, body) {
            iteration.count =
                SizeExpr::Unknown("collection/range bound changes inside loop".into());
        }
        let setup = self.node(value, env, false, depth + 1);
        let body_cost = self.node(body, &mut local, true, depth + 1);
        self.invalidate(n, env);
        let result = self.loop_cost(
            n,
            iteration.count,
            iteration.induction,
            setup,
            self.unit(n),
            body_cost,
            iteration.status,
            iteration.assumptions,
        );
        self.loop_alternative(n, result, env, depth)
    }
    fn collection_loop<'tree>(&self, n: Node<'tree>) -> Result<CollectionLoop<'tree>, CostNode> {
        if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
            && self.operator(n) != "of"
        {
            return Err(self.unknown(
                n,
                "for-in property enumeration is not a modeled collection traversal",
            ));
        }
        let value = child(n, "right")
            .or_else(|| child(n, "value"))
            .or_else(|| child(n, "traversable"));
        let pattern = child(n, "left")
            .or_else(|| child(n, "value"))
            .or_else(|| child(n, "key"));
        let Some(body) = self.body(n) else {
            return Err(self.unknown(n, "missing collection loop body"));
        };
        // PHP fields differ from the other grammars: first named child is the traversable.
        let children = named_children(n);
        let value = if self.dialect == Dialect::Php {
            children.first().copied()
        } else {
            value
        };
        let pattern = if self.dialect == Dialect::Php {
            children.iter().copied().find(|c| {
                matches!(c.kind(), "variable_name" | "pair")
                    && Some(c.id()) != value.map(|v| v.id())
            })
        } else {
            pattern
        };
        let (Some(value), Some(mut pattern)) = (value, pattern) else {
            return Err(self.unknown(n, "unsupported collection loop shape"));
        };
        let mut key = None;
        if self.dialect == Dialect::Php && pattern.kind() == "pair" {
            let bindings = named_children(pattern);
            if bindings.len() != 2 || bindings.iter().any(|n| n.kind() != "variable_name") {
                return Err(
                    self.unknown(n, "loop destructuring or reference binding is not modeled")
                );
            }
            key = Some(bindings[0]);
            pattern = bindings[1];
        }
        if matches!(
            pattern.kind(),
            "lexical_declaration" | "variable_declarator"
        ) {
            pattern = child(pattern, "name")
                .or_else(|| named_children(pattern).first().copied())
                .unwrap_or(pattern);
        }
        if !matches!(pattern.kind(), "identifier" | "variable_name")
            || self.text(n).contains(" as &")
        {
            return Err(self.unknown(n, "loop destructuring or reference binding is not modeled"));
        }
        Ok(CollectionLoop {
            value,
            pattern,
            key,
            body,
        })
    }
    fn collection_iteration(
        &self,
        n: Node<'_>,
        value: Node<'_>,
        pattern: Node<'_>,
        env: &Env,
        local: &mut Env,
    ) -> Result<Iteration, CostNode> {
        if self.dialect == Dialect::Python && self.builtin_name(value, "range") {
            return self.range_iteration(n, value, pattern, env, local);
        }
        let mut status = EstimateStatus::Tight;
        let mut assumptions = Vec::new();

        let Some(kind) = self.collection(value, env) else {
            return Err(self.unknown(
                n,
                "custom iterable/iterator cardinality and next() cost are not modeled",
            ));
        };
        let count = self.sizes(value, env).length;
        if self.dialect != Dialect::Php {
            status = EstimateStatus::Heuristic;
            assumptions.push("Assume finite builtin traversal with constant-cost next(), an unchanged collection and no custom iterator hooks.".into());
        }
        local.insert(
            self.name(pattern),
            Binding {
                kind,
                sizes: ArgumentSize::unknown("element dimensions are not modeled"),
                initial: None,
            },
        );
        Ok(Iteration {
            count,
            induction: None,
            status,
            assumptions,
        })
    }
    fn range_iteration(
        &self,
        n: Node<'_>,
        value: Node<'_>,
        pattern: Node<'_>,
        env: &Env,
        local: &mut Env,
    ) -> Result<Iteration, CostNode> {
        let mut status = EstimateStatus::Tight;

        let args = self.arguments(value);
        if args.is_empty() || args.len() > 3 {
            return Err(self.unknown(n, "range needs one to three positional arguments"));
        }
        let (start, end) = if args.len() == 1 {
            (None, args[0])
        } else {
            (Some(args[0]), args[1])
        };
        if args.iter().any(|a| !self.numeric(self.unwrap(*a), env)) {
            return Err(self.unknown(n, "range input types are not known as builtin integers"));
        }
        let step = args
            .get(2)
            .map(|a| self.literal(self.unwrap(*a)))
            .unwrap_or(Some(1));
        if !step.is_some_and(|s| s > 0) {
            return Err(self.unknown(n, "range step must be a positive constant integer"));
        }
        let mut count = self.sizes(end, env).value;
        if let (Some(a), Some(b)) = (
            start.map(|n| self.literal(n)).unwrap_or(Some(0)),
            self.literal(end),
        ) {
            count = SizeExpr::Constant(b.saturating_sub(a).div_ceil(step.unwrap()));
        } else if start.is_some_and(|n| self.literal(n).is_none()) {
            status = EstimateStatus::UpperBound;
        }
        let name = format!("@iteration:{}", n.start_byte());
        local.insert(
            self.name(pattern),
            self.counter_binding(pattern, name.clone()),
        );
        let induction = Some(name);
        Ok(Iteration {
            count,
            induction,
            status,
            assumptions: Vec::new(),
        })
    }

    fn loop_alternative(
        &self,
        n: Node<'_>,
        result: CostNode,
        env: &mut Env,
        depth: usize,
    ) -> CostNode {
        if self.dialect != Dialect::Python {
            return result;
        }
        let alternatives = named_children(n)
            .into_iter()
            .filter(|c| c.kind() == "else_clause")
            .collect::<Vec<_>>();
        if alternatives.is_empty() {
            return result;
        }
        let mut nodes = vec![result];
        for arm in alternatives {
            nodes.push(self.node(arm, env, false, depth + 1));
        }
        self.make(n, CostKind::UpperBound(Box::new(self.sequence(n, nodes))))
    }
    fn stable(&self, bound: Node<'_>, body: Node<'_>) -> bool {
        let writes = self.write_names(body);
        let mut stack = vec![bound];
        while let Some(n) = stack.pop() {
            if matches!(n.kind(), "identifier" | "variable_name") && writes.contains(&self.name(n))
            {
                return false;
            }
            stack.extend(named_children(n));
        }
        true
    }

    fn contains_continue(&self, root: Node<'_>) -> bool {
        let mut stack = vec![root];
        while let Some(n) = stack.pop() {
            if matches!(
                n.kind(),
                "function_definition" | "function_declaration" | "arrow_function" | "lambda"
            ) {
                continue;
            }
            if n.kind() == "continue_statement" {
                return true;
            }
            stack.extend(named_children(n));
        }
        false
    }
    fn step(&self, update: Node<'_>, counter: &str) -> Option<(String, u64)> {
        let update = self.unwrap(update);
        let target = child(update, "left")
            .or_else(|| child(update, "argument"))
            .or_else(|| {
                (update.kind() == "update_expression")
                    .then(|| named_children(update).first().copied())
                    .flatten()
            })?;
        if self.name(target) != counter {
            return None;
        }
        let op = self.operator(update);
        if op == "++" {
            return Some(("++".into(), 1));
        }
        if op == "--" {
            return Some(("-".into(), 1));
        }
        let right = child(update, "right")?;
        if ["+=", "-=", "*=", "//="].contains(&op.as_str()) {
            return Some((op.trim_end_matches('=').into(), self.literal(right)?));
        }
        if op == "=" || op.is_empty() {
            if self.floor_builtin(right) {
                let args = self.arguments(right);
                if args.len() == 1 {
                    let division = self.unwrap(args[0]);
                    if self.operator(division) == "/"
                        && child(division, "left").is_some_and(|a| self.name(a) == counter)
                    {
                        return Some(("//".into(), self.literal(child(division, "right")?)?));
                    }
                }
            }
            if self.dialect == Dialect::Php && self.builtin_name(right, "intdiv") {
                let args = self.arguments(right);
                if args.len() == 2 && self.name(self.unwrap(args[0])) == counter {
                    return Some(("//".into(), self.literal(self.unwrap(args[1]))?));
                }
            }
            if let (Some(a), Some(b)) = (child(right, "left"), child(right, "right")) {
                if self.name(a) == counter {
                    return Some((self.operator(right), self.literal(b)?));
                }
            }
        }
        None
    }
    pub(super) fn while_loop(&self, n: Node<'_>, env: &mut Env, depth: usize) -> CostNode {
        let counter = match self.while_counter(n, env) {
            Ok(counter) => counter,
            Err(error) => return error,
        };
        let mut bound = self
            .increasing_while_bound(&counter, env)
            .or_else(|| self.decreasing_while_bound(&counter))
            .or_else(|| self.geometric_while_bound(&counter, env))
            .unwrap_or_else(WhileBound::unknown);
        let right = counter.right;
        let initial = &counter.initial;
        if (matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
            || (self.dialect == Dialect::Php
                && (self.kind(right, env) != Kind::Integer
                    || initial.as_ref().is_some_and(|b| b.kind != Kind::Integer))))
            && !matches!(bound.count, SizeExpr::Constant(_))
        {
            bound.status = bound.status.max(EstimateStatus::Heuristic);
        }
        let induction = format!("@iteration:{}", n.start_byte());
        let mut local = env.clone();
        local.insert(
            counter.counter.clone(),
            self.counter_binding(counter.left, induction.clone()),
        );
        let check = self.node(counter.test, &mut local, true, depth + 1);
        let body_cost = self.node(counter.body, &mut local, true, depth + 1);
        self.invalidate(n, env);
        let result = self.make(n, CostKind::Repeat(Box::new(LoopCost {
            count:bound.count, induction: Some(induction), induction_bound:bound.induction_bound,
            setup: Box::new(self.unit(n)), check: Box::new(check), body: Box::new(body_cost), status:bound.status,
            assumptions: vec!["Assume nonnegative finite numeric magnitudes, normal arithmetic without overflow/stagnation, and the modeled counter reaching its termination bound.".into()],
        })));
        self.loop_alternative(n, result, env, depth)
    }
    fn while_counter<'tree>(
        &self,
        n: Node<'tree>,
        env: &Env,
    ) -> Result<WhileCounter<'tree>, CostNode> {
        let (Some(test), Some(body)) = (child(n, "condition"), self.body(n)) else {
            return Err(self.unknown(n, "missing while-loop condition/body"));
        };
        let test = self.unwrap(test);
        let (left, right) = if self.dialect == Dialect::Python {
            let children = named_children(test);
            (children.first().copied(), children.last().copied())
        } else {
            (child(test, "left"), child(test, "right"))
        };
        let (Some(left), Some(right)) = (left, right) else {
            return Err(self.unknown(n, "unsupported while-loop condition"));
        };
        let counter = self.name(left);
        let comparison = self.operator(test);
        let updates = named_children(body)
            .into_iter()
            .filter_map(|c| {
                let c = if c.kind() == "expression_statement" {
                    named_children(c).first().copied()?
                } else {
                    c
                };
                self.step(c, &counter)
            })
            .collect::<Vec<_>>();
        if updates.len() != 1
            || self.contains_continue(body)
            || self.write_count(body, &counter) != 1
            || !self.stable(right, body)
            || !self.numeric(left, env)
            || !self.numeric(right, env)
        {
            return Err(self.unknown(n, "while-loop counter must have one unconditional progressing update and an unchanged bound"));
        }
        let (op, step) = updates.into_iter().next().unwrap();
        let initial = env.get(&counter).cloned();
        Ok(WhileCounter {
            test,
            body,
            left,
            right,
            counter,
            comparison,
            op,
            step,
            initial,
        })
    }
    fn increasing_while_bound(&self, counter: &WhileCounter<'_>, env: &Env) -> Option<WhileBound> {
        let right = counter.right;
        let comparison = &counter.comparison;
        let op = &counter.op;
        let step = &counter.step;
        let initial = &counter.initial;
        if !(["<", "<="].contains(&comparison.as_str())
            && (op == "+" || op == "++")
            && *step > 0
            && initial.as_ref().and_then(|b| b.initial).is_some())
        {
            return None;
        }
        let mut status = EstimateStatus::Tight;
        let induction_bound = None;

        let mut count = self.sizes(right, env).value;
        if let Some(start) = initial.as_ref().and_then(|b| b.initial) {
            if let Some(end) = self.literal(right) {
                let distance = end
                    .saturating_sub(start)
                    .saturating_add(u64::from(comparison == "<="));
                count = SizeExpr::Constant(if end < start {
                    0
                } else {
                    distance.div_ceil(*step)
                });
            } else if start > 0 {
                status = EstimateStatus::UpperBound;
            }
        }
        Some(WhileBound {
            count,
            status,
            induction_bound,
        })
    }
    fn decreasing_while_bound(&self, counter: &WhileCounter<'_>) -> Option<WhileBound> {
        let right = counter.right;
        let comparison = &counter.comparison;
        let op = &counter.op;
        let step = &counter.step;
        let initial = &counter.initial;
        if !([">", ">="].contains(&comparison.as_str())
            && (op == "-" || op == "//")
            && *step > 0
            && self.literal(right).is_some()
            && !(op == "//"
                && (comparison == ">=" && self.literal(right) == Some(0) || *step <= 1)))
        {
            return None;
        }
        let mut count =
            SizeExpr::Unknown("while-loop progress or starting bound is not modeled".into());
        let mut status = EstimateStatus::Tight;
        let mut induction_bound = None;

        if let Some(initial) = &initial {
            count = initial.sizes.value.clone();
            if op == "//" && *step > 1 {
                induction_bound = Some(count.clone());
                count = SizeExpr::Log(Box::new(count));
                status = EstimateStatus::UpperBound;
            } else if let (Some(start), Some(end)) = (initial.initial, self.literal(right)) {
                let distance = start
                    .saturating_sub(end)
                    .saturating_add(u64::from(comparison == ">="));
                count = SizeExpr::Constant(if start < end {
                    0
                } else {
                    distance.div_ceil(*step)
                });
            } else if self.literal(right) != Some(0) || comparison == ">=" {
                status = EstimateStatus::UpperBound;
            }
        }
        Some(WhileBound {
            count,
            status,
            induction_bound,
        })
    }
    fn geometric_while_bound(&self, counter: &WhileCounter<'_>, env: &Env) -> Option<WhileBound> {
        let right = counter.right;
        let comparison = &counter.comparison;
        let op = &counter.op;
        let step = &counter.step;
        let initial = &counter.initial;
        if !(["<", "<="].contains(&comparison.as_str())
            && op == "*"
            && *step > 1
            && initial
                .as_ref()
                .and_then(|b| b.initial)
                .is_some_and(|n| n > 0))
        {
            return None;
        }
        let bound = self.sizes(right, env).value;
        let induction_bound = Some(bound.clone());
        let count = SizeExpr::Log(Box::new(bound));
        let status = EstimateStatus::Heuristic;
        Some(WhileBound {
            count,
            status,
            induction_bound,
        })
    }
}
