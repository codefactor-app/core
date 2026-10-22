//! Expression cost extraction grouped by values, bindings and control flow.
use super::*;
pub(super) struct NodeExtractor<'r, 'source> {
    pub(super) extractor: &'r Extractor<'source>,
}
impl<'source> std::ops::Deref for NodeExtractor<'_, 'source> {
    type Target = Extractor<'source>;
    fn deref(&self) -> &Self::Target {
        self.extractor
    }
}
impl NodeExtractor<'_, '_> {
    pub(super) fn node(
        &self,
        node: Node<'_>,
        env: &mut Environment,
        in_loop: bool,
        depth: usize,
    ) -> CostNode {
        if depth > 256 {
            return self.unknown(node, "cost extraction depth limit exceeded");
        }
        if let Some(cost) = self.value_node(node, env, in_loop, depth) {
            return cost;
        }
        if let Some(cost) = self.binding_node(node, env, in_loop, depth) {
            return cost;
        }
        if let Some(cost) = self.control_node(node, env, in_loop, depth) {
            return cost;
        }
        self.sequence(
            node,
            vec![
                self.children(node, env, in_loop, depth),
                self.unknown(
                    node,
                    format!("unsupported Rust cost construct: {}", node.kind()),
                ),
            ],
        )
    }
    fn syntax_block(
        &self,
        node: Node<'_>,
        env: &mut Environment,
        in_loop: bool,
        depth: usize,
    ) -> CostNode {
        let mut local = env.clone();
        let mut items = Vec::new();
        let mut early = false;
        let children = named_children(node);
        for (index, n) in children.iter().enumerate() {
            items.push(self.node(*n, &mut local, in_loop, depth + 1));
            if direct_terminator(*n) {
                if index + 1 < children.len() || in_loop {
                    early = true;
                }
                break;
            }
        }
        // Mutable aliases surviving the block do not retain an old size.
        for (name, binding) in env.iter_mut() {
            if self.writes(node, name) > 0 {
                let binding = Arc::make_mut(binding);
                binding.sizes = ArgumentSize::unknown("size changed inside a block");
                binding.initial = None;
            }
        }
        let cost = self.sequence(node, items);
        if early {
            self.bound(node, cost)
        } else {
            cost
        }
    }
    fn syntax_let_declaration(
        &self,
        node: Node<'_>,
        env: &mut Environment,
        in_loop: bool,
        depth: usize,
    ) -> CostNode {
        let Some(pattern) = child(node, "pattern") else {
            return self.unknown(node, "unsupported binding pattern");
        };
        let Some(value) = child(node, "value") else {
            return self.unknown(node, "uninitialized binding");
        };
        let cost = self.node(value, env, in_loop, depth + 1);
        if pattern.kind() != "identifier" {
            return self.sequence(
                node,
                vec![
                    cost,
                    self.unknown(node, "destructuring binding is not modeled"),
                ],
            );
        }
        let ty = child(node, "type")
            .map(|n| self.text(n).into())
            .unwrap_or_else(|| self.infer_type(value, env));
        let sizes = self.sizes(value, env);
        let initial = literal(value, self.source);
        env.insert(
            self.text(pattern).into(),
            Arc::new(Binding {
                ty: ty.clone(),
                sizes,
                initial,
            }),
        );
        if !safe_type(&ty) {
            self.sequence(
                node,
                vec![
                    cost,
                    self.unknown(
                        node,
                        "local value type or implicit destructor is not modeled",
                    ),
                ],
            )
        } else {
            cost
        }
    }
    fn syntax_if_expression(
        &self,
        node: Node<'_>,
        env: &mut Environment,
        in_loop: bool,
        depth: usize,
    ) -> CostNode {
        let test = child(node, "condition")
            .map(|n| self.node(n, env, in_loop, depth + 1))
            .unwrap_or_else(|| self.unknown(node, "missing branch condition"));
        let mut arms = Vec::new();
        for field in ["consequence", "alternative"] {
            if let Some(n) = child(node, field) {
                arms.push(self.node(n, &mut env.clone(), in_loop, depth + 1));
            }
        }
        // Do not keep stale sizes after conditionally assigned variables.
        for (name, binding) in env.iter_mut() {
            if self.writes(node, name) > 0 {
                let binding = Arc::make_mut(binding);
                binding.sizes = ArgumentSize::unknown("size changed in a conditional branch");
                binding.initial = None;
            }
        }
        self.make(
            node,
            CostKind::Branch {
                test: Box::new(test),
                arms,
            },
        )
    }
    fn syntax_assignment_expression(
        &self,
        node: Node<'_>,
        env: &mut Environment,
        in_loop: bool,
        depth: usize,
    ) -> CostNode {
        let left = child(node, "left");
        let right = child(node, "right");
        let cost = self.children(node, env, in_loop, depth);
        if let (Some(left), Some(right)) = (left, right) {
            let ty = self.infer_type(left, env);
            let right_ty = self.infer_type(right, env);
            let is_primitive = scalar(&ty) && scalar(&right_ty);
            if left.kind() == "identifier" {
                let name = self.text(left).to_string();
                let sizes = if node.kind() == "assignment_expression" {
                    self.sizes(right, env)
                } else {
                    ArgumentSize::unknown("mutated scalar size is not tracked")
                };
                if let Some(binding) = env.get_mut(&name) {
                    let binding = Arc::make_mut(binding);
                    let induction = matches!(&binding.sizes.value, SizeExpr::Input {parameter,..} if parameter.starts_with("@iteration:"));
                    if !induction {
                        binding.sizes = sizes;
                    }
                    binding.initial = literal(right, self.source)
                        .filter(|_| node.kind() == "assignment_expression");
                }
            }
            if !is_primitive {
                return self.sequence(
                    node,
                    vec![
                        cost,
                        self.unknown(
                            node,
                            "assignment/operator or implicit drop has unmodeled type semantics",
                        ),
                    ],
                );
            }
        }
        cost
    }
    fn syntax_binary_expression(
        &self,
        node: Node<'_>,
        env: &mut Environment,
        in_loop: bool,
        depth: usize,
    ) -> CostNode {
        let cost = self.children(node, env, in_loop, depth);
        let all_primitive = named_children(node)
            .iter()
            .all(|n| scalar(&self.infer_type(*n, env)));
        // Built-in reference projection is constant in a place context.
        // Reading an aggregate value may copy a const-generic array.
        let reference_dereference = node.kind() == "unary_expression"
            && self.text(node).starts_with('*')
            && named_children(node)
                .first()
                .is_some_and(|n| referent_type(&self.infer_type(*n, env)).is_some())
            && node.parent().is_some_and(|p| {
                matches!(
                    p.kind(),
                    "reference_expression" | "field_expression" | "index_expression"
                )
            });
        if all_primitive || reference_dereference {
            cost
        } else {
            self.sequence(
                node,
                vec![
                    cost,
                    self.unknown(
                        node,
                        "operator may execute an unmodeled trait implementation",
                    ),
                ],
            )
        }
    }
    fn syntax_index_expression(
        &self,
        node: Node<'_>,
        env: &mut Environment,
        in_loop: bool,
        depth: usize,
    ) -> CostNode {
        let cost = self.children(node, env, in_loop, depth);
        let children = named_children(node);
        if children
            .first()
            .is_some_and(|n| element(&self.infer_type(*n, env)).is_some())
        {
            cost
        } else {
            self.sequence(
                node,
                vec![
                    cost,
                    self.unknown(node, "indexing type semantics are not known"),
                ],
            )
        }
    }
    fn syntax_field_expression(
        &self,
        node: Node<'_>,
        env: &mut Environment,
        in_loop: bool,
        depth: usize,
    ) -> CostNode {
        let cost = child(node, "value")
            .map(|n| self.node(n, env, in_loop, depth + 1))
            .unwrap_or_else(|| self.unit(node));
        if self.field_type(node, env).is_some() {
            cost
        } else {
            self.sequence(
                node,
                vec![
                    cost,
                    self.unknown(node, "user field/type semantics are not modeled"),
                ],
            )
        }
    }
    fn syntax_return_expression(
        &self,
        node: Node<'_>,
        env: &mut Environment,
        in_loop: bool,
        depth: usize,
    ) -> CostNode {
        let cost = self.children(node, env, in_loop, depth);
        if in_loop {
            self.bound(node, cost)
        } else {
            cost
        }
    }
    fn syntax_identifier(&self, node: Node<'_>, env: &mut Environment) -> CostNode {
        if env.get(self.text(node)).is_some_and(|b| safe_type(&b.ty)) {
            self.unit(node)
        } else {
            self.unknown(
                node,
                format!("value type is not established: {}", self.text(node)),
            )
        }
    }
    fn syntax_reference_expression(
        &self,
        node: Node<'_>,
        env: &mut Environment,
        in_loop: bool,
        depth: usize,
    ) -> CostNode {
        let cost = child(node, "value")
            .or_else(|| named_children(node).last().copied())
            .map(|n| self.node(n, env, in_loop, depth + 1))
            .unwrap_or_else(|| self.unit(node));
        if self.text(node).starts_with("&mut") {
            for (name, binding) in env.iter_mut() {
                if contains_identifier(node, name, self.source) {
                    let binding = Arc::make_mut(binding);
                    binding.sizes =
                        ArgumentSize::unknown("mutable reference may change input size");
                    binding.initial = None;
                }
            }
        }
        cost
    }
    fn syntax_array_expression(
        &self,
        node: Node<'_>,
        env: &mut Environment,
        in_loop: bool,
        depth: usize,
    ) -> CostNode {
        let cost = self.children(node, env, in_loop, depth);
        if safe_type(&self.infer_type(node, env)) {
            cost
        } else {
            self.sequence(
                node,
                vec![
                    cost,
                    self.unknown(
                        node,
                        "array element or repetition semantics are not modeled",
                    ),
                ],
            )
        }
    }
    fn syntax_type_cast_expression(
        &self,
        node: Node<'_>,
        env: &mut Environment,
        in_loop: bool,
        depth: usize,
    ) -> CostNode {
        let cost = child(node, "value")
            .map(|n| self.node(n, env, in_loop, depth + 1))
            .unwrap_or_else(|| self.children(node, env, in_loop, depth));
        self.sequence(
            node,
            vec![
                cost,
                self.unknown(node, "cast size effects are not modeled"),
            ],
        )
    }
    fn value_node(
        &self,
        node: Node<'_>,
        env: &mut Environment,
        in_loop: bool,
        depth: usize,
    ) -> Option<CostNode> {
        match node.kind() {
            "line_comment" | "block_comment" | "function_item" | "attribute_item"
            | "empty_statement" => Some(self.unit(node)),
            "identifier" | "self" => Some(self.syntax_identifier(node, env)),
            "integer_literal" | "float_literal" | "boolean_literal" | "char_literal"
            | "string_literal" | "raw_string_literal" | "unit_expression" => Some(self.unit(node)),
            "reference_expression" => {
                Some(self.syntax_reference_expression(node, env, in_loop, depth))
            }
            "expression_statement"
            | "else_clause"
            | "parenthesized_expression"
            | "range_expression" => Some(self.children(node, env, in_loop, depth)),
            "array_expression" => Some(self.syntax_array_expression(node, env, in_loop, depth)),
            "type_cast_expression" => {
                Some(self.syntax_type_cast_expression(node, env, in_loop, depth))
            }
            _ => None,
        }
    }
    fn binding_node(
        &self,
        node: Node<'_>,
        env: &mut Environment,
        in_loop: bool,
        depth: usize,
    ) -> Option<CostNode> {
        match node.kind() {
            "let_declaration" => Some(self.syntax_let_declaration(node, env, in_loop, depth)),
            "assignment_expression" | "compound_assignment_expr" => {
                Some(self.syntax_assignment_expression(node, env, in_loop, depth))
            }
            "binary_expression" | "unary_expression" => {
                Some(self.syntax_binary_expression(node, env, in_loop, depth))
            }
            "index_expression" => Some(self.syntax_index_expression(node, env, in_loop, depth)),
            "field_expression" => Some(self.syntax_field_expression(node, env, in_loop, depth)),
            _ => None,
        }
    }
    fn control_node(
        &self,
        node: Node<'_>,
        env: &mut Environment,
        in_loop: bool,
        depth: usize,
    ) -> Option<CostNode> {
        match node.kind() {
            "block" => Some(self.syntax_block(node, env, in_loop, depth)),
            "for_expression" => Some(self.for_loop(node, env, depth)),
            "while_expression" => Some(self.while_loop(node, env, depth)),
            "loop_expression" => Some(self.sequence(
                node,
                vec![
                    self.children(node, env, true, depth),
                    self.unknown(node, "unbounded loop has no supported termination proof"),
                ],
            )),
            "if_expression" => Some(self.syntax_if_expression(node, env, in_loop, depth)),
            "call_expression" => Some(self.call(node, env, in_loop, depth)),
            "macro_invocation" => Some(self.unknown(node, "macro expansion is not available")),
            "closure_expression" => Some(self.unknown(
                node,
                "closure execution/destructor semantics are not modeled",
            )),
            "match_expression" | "try_expression" | "unsafe_block" | "async_block"
            | "await_expression" | "yield_expression" => Some(self.sequence(
                node,
                vec![
                    self.children(node, env, in_loop, depth),
                    self.unknown(node, "control-flow construct is not modeled"),
                ],
            )),
            "return_expression" | "break_expression" | "continue_expression" => {
                Some(self.syntax_return_expression(node, env, in_loop, depth))
            }
            _ => None,
        }
    }
}
