//! Counter recognition and progress proofs are separate from loop body extraction.
use super::*;

pub(super) struct WhileProgress {
    pub count: SizeExpr,
    pub status: EstimateStatus,
    pub assumptions: Vec<String>,
    pub induction: Option<String>,
    pub induction_bound: Option<SizeExpr>,
}

struct WhileCounter<'tree> {
    start: usize,
    body: Node<'tree>,
    right: Node<'tree>,
    comparison: String,
    name: String,
    binding: Arc<Binding>,
    update: String,
    step: u64,
}

impl Extractor<'_> {
    pub(super) fn while_progress(
        &self,
        node: Node<'_>,
        condition: Node<'_>,
        body: Node<'_>,
        env: &Environment,
        local: &mut Environment,
    ) -> WhileProgress {
        let mut progress = WhileProgress {
            count: SizeExpr::Unknown("while loop termination or progress is not proven".into()),
            status: EstimateStatus::Tight,
            assumptions: Vec::new(),
            induction: None,
            induction_bound: None,
        };
        let Some(counter) = self.while_counter(node, condition, body, env) else {
            return progress;
        };
        if !self.linear_while(&counter, env, &mut progress) {
            self.geometric_while(&counter, env, &mut progress);
        }
        if let Some(symbol) = &progress.induction {
            if let Some(binding) = local.get_mut(&counter.name) {
                Arc::make_mut(binding).sizes.value = SizeExpr::value(symbol);
            }
        }
        progress
    }

    fn while_counter<'tree>(
        &self,
        node: Node<'tree>,
        condition: Node<'tree>,
        body: Node<'tree>,
        env: &Environment,
    ) -> Option<WhileCounter<'tree>> {
        if condition.kind() != "binary_expression" {
            return None;
        }
        let left = child(condition, "left")?;
        let right = child(condition, "right")?;
        let operator = child(condition, "operator")?;
        if left.kind() != "identifier" {
            return None;
        }
        let name = self.text(left);
        let binding = env.get(name)?;
        let (update, step) = top_updates(body, name, self.source)?;
        if self.writes(body, name) != 1
            || !unsigned(&binding.ty)
            || has_conditional_update(body, name, self)
            || has_indirect_write(body)
        {
            return None;
        }
        Some(WhileCounter {
            start: node.start_byte(),
            body,
            right,
            comparison: self.text(operator).into(),
            name: name.into(),
            binding: binding.clone(),
            update,
            step,
        })
    }

    fn linear_while(
        &self,
        counter: &WhileCounter<'_>,
        env: &Environment,
        progress: &mut WhileProgress,
    ) -> bool {
        let binding = &counter.binding;
        if counter.comparison == "<"
            && binding.initial == Some(0)
            && counter.update == "+="
            && counter.step == 1
        {
            let bound = self.sizes(counter.right, env);
            let stable = env.keys().all(|key| {
                !contains_identifier(counter.right, key, self.source)
                    || self.writes(counter.body, key) == 0
            });
            if stable && !has_indirect_write(counter.body) {
                progress.status = progress.status.clone().max(bound.status);
                progress.count = bound.value;
                progress.induction = Some(format!("@iteration:{}", counter.start));
            }
            return true;
        }
        if counter.comparison == ">"
            && literal(counter.right, self.source) == Some(0)
            && counter.update == "-="
            && counter.step == 1
        {
            progress.status = progress.status.clone().max(binding.sizes.status.clone());
            progress.count = binding.sizes.value.clone();
            progress.induction = Some(format!("@iteration:{}", counter.start));
            return true;
        }
        false
    }

    fn geometric_while(
        &self,
        counter: &WhileCounter<'_>,
        env: &Environment,
        progress: &mut WhileProgress,
    ) {
        let binding = &counter.binding;
        let shift = counter.step >= 1 && counter.step < scalar_bits(&binding.ty);
        let decreasing = counter.comparison == ">"
            && literal(counter.right, self.source) == Some(0)
            && ((counter.update == "/=" && counter.step >= 2)
                || (counter.update == ">>=" && shift));
        if decreasing {
            progress.status = progress.status.clone().max(binding.sizes.status.clone());
            progress.count = SizeExpr::Log(Box::new(binding.sizes.value.clone()));
            progress.induction = Some(format!("@iteration:{}", counter.start));
            progress.induction_bound = Some(binding.sizes.value.clone());
            return;
        }
        if counter.comparison == "<"
            && matches!(counter.update.as_str(), "*=" | "<<=")
            && binding.initial.is_some_and(|n| n > 0)
            && env.keys().all(|key| {
                !contains_identifier(counter.right, key, self.source)
                    || self.writes(counter.body, key) == 0
            })
            && ((counter.update == "*=" && counter.step >= 2) || (counter.update == "<<=" && shift))
        {
            progress.count = SizeExpr::Log(Box::new(self.sizes(counter.right, env).value));
            progress.status = EstimateStatus::Heuristic;
            progress.induction = Some(format!("@iteration:{}", counter.start));
            progress.induction_bound = Some(self.sizes(counter.right, env).value);
            progress.assumptions.push("Assume geometric counter growth never overflows, the bound remains unchanged and the counter reaches it.".into());
        }
    }
}
