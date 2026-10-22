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
    pub(super) fn node(&self, n: Node<'_>, env: &mut Env, in_loop: bool, depth: usize) -> CostNode {
        if depth > 256 {
            return self.unknown(n, "cost extraction depth limit exceeded");
        }
        if let Some(cost) = self.value_node(n, env, in_loop, depth) {
            return cost;
        }
        if let Some(cost) = self.binding_node(n, env, in_loop, depth) {
            return cost;
        }
        if let Some(cost) = self.control_node(n, env, in_loop, depth) {
            return cost;
        }
        self.syntax_fallback(n, env, in_loop, depth)
    }
    fn syntax_function_definition(&self, n: Node<'_>) -> CostNode {
        if self.text(n).contains('=') || self.text(n).trim_start().starts_with('@') {
            self.unknown(
                n,
                "nested function defaults/decorators may execute unmodeled code",
            )
        } else {
            self.unit(n)
        }
    }
    fn syntax_block(&self, n: Node<'_>, env: &mut Env, in_loop: bool, depth: usize) -> CostNode {
        let mut local = env.clone();
        let mut nodes = Vec::new();
        for c in named_children(n) {
            nodes.push(self.node(c, &mut local, in_loop, depth + 1));
            if matches!(
                c.kind(),
                "return_statement"
                    | "break_statement"
                    | "continue_statement"
                    | "throw_statement"
                    | "raise_statement"
            ) {
                break;
            }
        }
        // JS block declarations do not escape their lexical scope.
        if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript) {
            self.invalidate(n, env);
        } else {
            *env = local;
        }
        self.sequence(n, nodes)
    }
    fn syntax_expression_statement(
        &self,
        n: Node<'_>,
        env: &mut Env,
        in_loop: bool,
        depth: usize,
    ) -> CostNode {
        self.children(n, env, in_loop, depth)
    }
    fn syntax_identifier(&self, n: Node<'_>, env: &mut Env) -> CostNode {
        if env.contains_key(&self.name(n)) {
            self.unit(n)
        } else {
            self.unknown(n, format!("unbound value semantics: {}", self.text(n)))
        }
    }
    fn syntax_this(&self, n: Node<'_>) -> CostNode {
        self.unit(n)
    }
    fn syntax_string(&self, n: Node<'_>) -> CostNode {
        if named_children(n)
            .iter()
            .any(|c| c.kind() == "interpolation")
        {
            self.unknown(
                n,
                "string interpolation can execute unmodeled formatting or calls",
            )
        } else {
            self.unit(n)
        }
    }
    fn syntax_encapsed_string(&self, n: Node<'_>) -> CostNode {
        self.unknown(n, "string interpolation/concatenation cost is not modeled")
    }
    fn syntax_update_expression(
        &self,
        n: Node<'_>,
        env: &mut Env,
        in_loop: bool,
        depth: usize,
    ) -> CostNode {
        let target = child(n, "argument").or_else(|| named_children(n).first().copied());
        if let Some(target) = target {
            if !self.numeric(target, env) {
                return self.unknown(n, "increment may execute user-defined coercion");
            }
            if !matches!(target.kind(), "identifier" | "variable_name") {
                let ordinary_scalar = self
                    .ordinary_field_kind(target, env)
                    .or_else(|| self.php_field_kind(target))
                    .is_some_and(|kind| matches!(kind, Kind::Integer | Kind::Number | Kind::Bool));
                if ordinary_scalar {
                    self.assume("Assume updates of declared numeric/bool receiver fields use ordinary scalar storage, without Proxy/getter/setter/descriptor hooks, custom attribute access, subclass replacements or lifetime finalizers.");
                    return self.node(target, env, in_loop, depth + 1);
                }
                return self.unknown(
                    n,
                    "indexed/property increment may resize storage or invoke hooks",
                );
            }
            if let Some(b) = env.get_mut(&self.name(target)) {
                if !matches!(&b.sizes.value, SizeExpr::Input { parameter, .. } if parameter.starts_with("@iteration:"))
                {
                    b.sizes = ArgumentSize::unknown("mutated counter size");
                }
                b.initial = None;
            }
            self.unit(n)
        } else {
            self.unknown(n, "unsupported increment")
        }
    }
    fn syntax_binary_expression(
        &self,
        n: Node<'_>,
        env: &mut Env,
        in_loop: bool,
        depth: usize,
    ) -> CostNode {
        let cost = self.children(n, env, in_loop, depth);
        if self.dialect == Dialect::Python && self.operator(n) == "**" {
            return self.sequence(n, vec![cost, self.unknown(n, "Python exponentiation algorithm and integer magnitude costs are not modeled")]);
        }
        if named_children(n).iter().all(|c| self.numeric(*c, env)) {
            cost
        } else {
            self.sequence(
                n,
                vec![
                    cost,
                    self.unknown(
                        n,
                        "operator semantics are not established as builtin numeric operations",
                    ),
                ],
            )
        }
    }
    fn syntax_if_statement(
        &self,
        n: Node<'_>,
        env: &mut Env,
        in_loop: bool,
        depth: usize,
    ) -> CostNode {
        self.branch(n, env, in_loop, depth)
    }
    fn syntax_for_statement(&self, n: Node<'_>, env: &mut Env, depth: usize) -> CostNode {
        if self.dialect == Dialect::Python {
            super::loops::LoopExtractor {
                extractor: self.extractor,
            }
            .foreach(n, env, depth)
        } else {
            super::loops::LoopExtractor {
                extractor: self.extractor,
            }
            .counted_for(n, env, depth)
        }
    }
    fn syntax_member_expression(
        &self,
        n: Node<'_>,
        env: &mut Env,
        in_loop: bool,
        depth: usize,
    ) -> CostNode {
        if self.php_field_kind(n).is_some() {
            self.assume("Assume reads of declared $this properties use initialized ordinary storage, without overridden property hooks or magic __get access (including after unset).");
            return self.unit(n);
        }
        if self.ordinary_field_kind(n, env).is_some() {
            self.assume("Assume declared receiver fields use initialized ordinary storage on an instance of this class, with no Proxy, descriptors, overridden attribute access, subclass replacements or lifetime finalizers.");
            return self.unit(n);
        }
        if let Some(receiver) = self.length_receiver(n) {
            if self.collection(receiver, env).is_some() {
                return self.node(receiver, env, in_loop, depth + 1);
            }
        }
        self.unknown(
            n,
            "property access may invoke an unmodeled getter/descriptor",
        )
    }
    fn syntax_subscript(
        &self,
        n: Node<'_>,
        env: &mut Env,
        in_loop: bool,
        depth: usize,
    ) -> CostNode {
        let children = named_children(n);
        let cost = self.children(n, env, in_loop, depth);
        if children
            .first()
            .is_some_and(|r| self.collection(*r, env).is_some())
            && !children.iter().any(|c| c.kind().contains("slice"))
        {
            cost
        } else {
            self.sequence(
                n,
                vec![
                    cost,
                    self.unknown(n, "indexing or slicing semantics are not modeled"),
                ],
            )
        }
    }
    fn syntax_list(&self, n: Node<'_>, env: &mut Env, in_loop: bool, depth: usize) -> CostNode {
        self.children(n, env, in_loop, depth)
    }
    fn syntax_dictionary(
        &self,
        n: Node<'_>,
        env: &mut Env,
        in_loop: bool,
        depth: usize,
    ) -> CostNode {
        self.children(n, env, in_loop, depth)
    }
    fn syntax_pair(&self, n: Node<'_>, env: &mut Env, in_loop: bool, depth: usize) -> CostNode {
        let (Some(key), Some(value)) = (child(n, "key"), child(n, "value")) else {
            return self.unknown(n, "unsupported literal key/value shape");
        };
        let cost = self.node(value, env, in_loop, depth + 1);
        if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
            && self.text(key).trim_matches(['\'', '"']) == "__proto__"
        {
            return self.sequence(
                n,
                vec![
                    cost,
                    self.unknown(n, "object literal prototype initialization is not modeled"),
                ],
            );
        }
        let key_is_literal = if self.dialect == Dialect::Python {
            matches!(
                key.kind(),
                "integer" | "float" | "string" | "true" | "false" | "none"
            )
        } else {
            matches!(key.kind(), "property_identifier" | "number" | "string")
        };
        if key_is_literal {
            let key_cost = if key.kind() == "property_identifier" {
                self.unit(key)
            } else {
                self.node(key, env, in_loop, depth + 1)
            };
            self.sequence(n, vec![key_cost, cost])
        } else {
            self.sequence(
                n,
                vec![
                    cost,
                    self.unknown(
                        n,
                        "computed literal keys may execute unmodeled coercion or hashing",
                    ),
                ],
            )
        }
    }
    fn syntax_shorthand_property_identifier(&self, n: Node<'_>, env: &mut Env) -> CostNode {
        if env.contains_key(&self.name(n)) {
            self.unit(n)
        } else {
            self.unknown(n, "unbound shorthand property value")
        }
    }
    fn syntax_array_element_initializer(
        &self,
        n: Node<'_>,
        env: &mut Env,
        in_loop: bool,
        depth: usize,
    ) -> CostNode {
        let cost = self.children(n, env, in_loop, depth);
        let children = named_children(n);
        if children.len() > 1 && !matches!(children[0].kind(), "integer" | "string") {
            self.sequence(
                n,
                vec![
                    cost,
                    self.unknown(
                        n,
                        "dynamic array key conversion/hashing cost is not modeled",
                    ),
                ],
            )
        } else {
            cost
        }
    }
    fn syntax_return_statement(
        &self,
        n: Node<'_>,
        env: &mut Env,
        in_loop: bool,
        depth: usize,
    ) -> CostNode {
        let cost = self.children(n, env, in_loop, depth);
        if in_loop {
            self.make(n, CostKind::UpperBound(Box::new(cost)))
        } else {
            cost
        }
    }
    fn syntax_arrow_function(&self, n: Node<'_>) -> CostNode {
        self.unknown(n, "closure capture and deferred execution are not modeled")
    }
    fn syntax_await_expression(
        &self,
        n: Node<'_>,
        env: &mut Env,
        in_loop: bool,
        depth: usize,
    ) -> CostNode {
        let cost = self.children(n, env, in_loop, depth);
        self.sequence(
            n,
            vec![
                cost,
                self.unknown(n, "control-flow construct is not modeled"),
            ],
        )
    }
    fn syntax_fallback(&self, n: Node<'_>, env: &mut Env, in_loop: bool, depth: usize) -> CostNode {
        let cost = self.children(n, env, in_loop, depth);
        self.sequence(
            n,
            vec![
                cost,
                self.unknown(
                    n,
                    format!("unsupported dynamic-language cost construct: {}", n.kind()),
                ),
            ],
        )
    }
    fn value_node(
        &self,
        n: Node<'_>,
        env: &mut Env,
        in_loop: bool,
        depth: usize,
    ) -> Option<CostNode> {
        match n.kind() {
            "comment"
            | "line_comment"
            | "block_comment"
            | "empty_statement"
            | "pass_statement"
            | "function_declaration" => Some(self.unit(n)),
            "function_definition" => Some(self.syntax_function_definition(n)),
            "block" | "statement_block" | "compound_statement" => {
                Some(self.syntax_block(n, env, in_loop, depth))
            }
            "expression_statement" | "parenthesized_expression" | "argument" | "else_clause" => {
                Some(self.syntax_expression_statement(n, env, in_loop, depth))
            }
            "identifier" | "variable_name" => Some(self.syntax_identifier(n, env)),
            "this" if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript) => {
                Some(self.syntax_this(n))
            }
            "number" | "integer" | "float" | "integer_literal" | "true" | "false" | "boolean"
            | "null" | "none" | "string_literal" => Some(self.unit(n)),
            "string" => Some(self.syntax_string(n)),
            "encapsed_string" | "template_string" | "concatenated_string" => {
                Some(self.syntax_encapsed_string(n))
            }
            "member_expression" | "attribute" | "member_access_expression" => {
                Some(self.syntax_member_expression(n, env, in_loop, depth))
            }
            "subscript" | "subscript_expression" => {
                Some(self.syntax_subscript(n, env, in_loop, depth))
            }
            "list" | "tuple" | "array" | "array_creation_expression" => {
                Some(self.syntax_list(n, env, in_loop, depth))
            }
            "dictionary" | "object" if self.dialect != Dialect::Php => {
                Some(self.syntax_dictionary(n, env, in_loop, depth))
            }
            "pair" if self.dialect != Dialect::Php => {
                Some(self.syntax_pair(n, env, in_loop, depth))
            }
            "shorthand_property_identifier" => {
                Some(self.syntax_shorthand_property_identifier(n, env))
            }
            "array_element_initializer" => {
                Some(self.syntax_array_element_initializer(n, env, in_loop, depth))
            }
            _ => None,
        }
    }
    fn binding_node(
        &self,
        n: Node<'_>,
        env: &mut Env,
        in_loop: bool,
        depth: usize,
    ) -> Option<CostNode> {
        match n.kind() {
            "lexical_declaration" | "variable_declaration" => {
                Some(self.children(n, env, in_loop, depth))
            }
            "variable_declarator"
            | "assignment"
            | "assignment_expression"
            | "augmented_assignment"
            | "augmented_assignment_expression" => Some(self.assignment(n, env, in_loop, depth)),
            "update_expression" => Some(self.syntax_update_expression(n, env, in_loop, depth)),
            "binary_expression"
            | "binary_operator"
            | "comparison_operator"
            | "unary_expression"
            | "unary_operator"
            | "unary_op_expression" => Some(self.syntax_binary_expression(n, env, in_loop, depth)),
            _ => None,
        }
    }
    fn control_node(
        &self,
        n: Node<'_>,
        env: &mut Env,
        in_loop: bool,
        depth: usize,
    ) -> Option<CostNode> {
        match n.kind() {
            "if_statement" | "elif_clause" | "conditional_expression" | "ternary_expression" => {
                Some(self.syntax_if_statement(n, env, in_loop, depth))
            }
            "for_statement" => Some(self.syntax_for_statement(n, env, depth)),
            "for_in_statement" | "foreach_statement" => Some(
                super::loops::LoopExtractor {
                    extractor: self.extractor,
                }
                .foreach(n, env, depth),
            ),
            "while_statement" => Some(
                super::loops::LoopExtractor {
                    extractor: self.extractor,
                }
                .while_loop(n, env, depth),
            ),
            "call"
            | "call_expression"
            | "function_call_expression"
            | "member_call_expression"
            | "scoped_call_expression" => Some(self.call(n, env, in_loop, depth)),
            "return_statement" | "break_statement" | "continue_statement" => {
                Some(self.syntax_return_statement(n, env, in_loop, depth))
            }
            "arrow_function"
            | "lambda"
            | "anonymous_function"
            | "anonymous_function_creation_expression" => Some(self.syntax_arrow_function(n)),
            "await_expression" | "yield" | "yield_expression" | "yield_from" | "try_statement"
            | "with_statement" | "throw_statement" | "raise_statement" | "switch_statement"
            | "match_statement" | "match_expression" => {
                Some(self.syntax_await_expression(n, env, in_loop, depth))
            }
            _ => None,
        }
    }
}

impl NodeExtractor<'_, '_> {
    fn assignment(&self, n: Node<'_>, env: &mut Env, in_loop: bool, depth: usize) -> CostNode {
        let left = child(n, "left").or_else(|| child(n, "name"));
        let right = child(n, "right").or_else(|| child(n, "value"));
        let (Some(left), Some(right)) = (left, right) else {
            return self.unknown(n, "unsupported assignment shape");
        };
        let cost = self.node(right, env, in_loop, depth + 1);
        if !matches!(left.kind(), "identifier" | "variable_name") {
            self.invalidate(n, env);
            // Replacing an object or owned buffer may run destructors or free an
            // input-sized allocation. Only native numeric/bool storage is modeled.
            if n.kind() == "assignment_expression"
                && self.operator(n) == "="
                && self
                    .php_field_kind(left)
                    .is_some_and(|kind| matches!(kind, Kind::Integer | Kind::Number | Kind::Bool))
                && matches!(
                    self.kind(right, env),
                    Kind::Integer | Kind::Number | Kind::Bool
                )
            {
                self.assume("Assume writes of declared numeric/bool $this properties use ordinary storage without overridden property hooks, magic __set access or reference aliases.");
                return cost;
            }
            if matches!(n.kind(), "assignment" | "assignment_expression")
                && self.operator(n) == "="
                && self
                    .ordinary_field_kind(left, env)
                    .is_some_and(|kind| matches!(kind, Kind::Integer | Kind::Number | Kind::Bool))
                && self.numeric(right, env)
            {
                self.assume("Assume writes of declared numeric/bool receiver fields replace only builtin scalar storage, without Proxy/setter/descriptor hooks, custom attribute access, subclass replacements or lifetime finalizers.");
                return cost;
            }
            return self.sequence(
                n,
                vec![
                    cost,
                    self.unknown(
                        n,
                        "destructuring, property or indexed writes may change costs/sizes",
                    ),
                ],
            );
        }
        let name = self.name(left);
        let simple = matches!(
            n.kind(),
            "assignment" | "assignment_expression" | "variable_declarator"
        ) && matches!(self.operator(n).as_str(), "" | "=");
        if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
            && n.kind() != "variable_declarator"
            && !env.contains_key(&name)
        {
            return self.sequence(
                n,
                vec![cost, self.unknown(n, "write to an unbound/global name")],
            );
        }
        let kind = self.kind(right, env);
        if !simple && (!self.numeric(left, env) || !self.numeric(right, env)) {
            return self.sequence(
                n,
                vec![
                    cost,
                    self.unknown(n, "compound assignment may execute unmodeled operators"),
                ],
            );
        }
        let induction = env.get(&name).is_some_and(|b| matches!(&b.sizes.value, SizeExpr::Input {parameter,..} if parameter.starts_with("@iteration:")));
        if !induction {
            let sizes = if simple {
                self.sizes(right, env)
            } else {
                ArgumentSize::unknown("mutated scalar size is not tracked")
            };
            env.insert(
                name,
                Binding {
                    kind,
                    sizes,
                    initial: simple.then(|| self.literal(right)).flatten(),
                },
            );
        }
        cost
    }
    fn branch(&self, n: Node<'_>, env: &mut Env, in_loop: bool, depth: usize) -> CostNode {
        let condition = child(n, "condition");
        let test = condition
            .map(|c| self.node(c, env, in_loop, depth + 1))
            .unwrap_or_else(|| self.unit(n));
        let mut arms = Vec::new();
        for field in ["consequence", "body", "alternative"] {
            if let Some(arm) = child(n, field) {
                arms.push(self.node(arm, &mut env.clone(), in_loop, depth + 1));
            }
        }
        // Python elif/else nodes may be unnamed fields in the grammar.
        for arm in named_children(n)
            .into_iter()
            .filter(|c| matches!(c.kind(), "elif_clause" | "else_clause"))
        {
            arms.push(self.node(arm, &mut env.clone(), in_loop, depth + 1));
        }
        self.invalidate(n, env);
        self.make(
            n,
            CostKind::Branch {
                test: Box::new(test),
                arms,
            },
        )
    }
    fn call(&self, n: Node<'_>, env: &mut Env, in_loop: bool, depth: usize) -> CostNode {
        let args = self.arguments(n);
        let invalid_arguments = args.iter().any(|a| {
            matches!(
                a.kind(),
                "keyword_argument"
                    | "list_splat"
                    | "dictionary_splat"
                    | "spread_element"
                    | "variadic_unpacking"
            ) || child(*a, "name").is_some()
                || self.text(*a).starts_with("...")
        });
        let mut nodes = Vec::new();
        let function = child(n, "function");
        let receiver = child(n, "object").or_else(|| function.and_then(|f| child(f, "object")));
        let floor_builtin = self.floor_builtin(n);
        if let Some(receiver) = receiver.filter(|_| !floor_builtin) {
            nodes.push(self.node(receiver, env, in_loop, depth + 1));
        } else if !floor_builtin
            && function
                .is_some_and(|f| !matches!(f.kind(), "identifier" | "name" | "qualified_name"))
        {
            nodes.push(self.unknown(n, "dynamic callable evaluation is not modeled"));
        }
        let mut arguments = Vec::new();
        for a in &args {
            arguments.push(self.sizes(*a, env));
            nodes.push(self.node(*a, env, in_loop, depth + 1));
        }
        if invalid_arguments {
            nodes.push(self.unknown(
                n,
                "keyword, named, spread or unpacked argument binding is not modeled",
            ));
            return self.sequence(n, nodes);
        }
        let builtin = self.builtin_call(n, &args, env, floor_builtin);
        if builtin.is_none() {
            // A project callee can mutate a shared collection; an old length is no witness.
            for a in &args {
                let name = self.name(self.unwrap(*a));
                if let Some(binding) = env.get_mut(&name) {
                    if matches!(binding.kind, Kind::Array(_) | Kind::Input) {
                        binding.sizes.length = SizeExpr::Unknown(
                            "collection length may change in a project or external call".into(),
                        );
                    }
                }
            }
        }
        nodes.push(self.make(
            n,
            CostKind::Call(Box::new(CallCost {
                arguments,
                receiver: receiver.map(|r| self.sizes(r, env)),
                builtin,
            })),
        ));
        self.sequence(n, nodes)
    }
    fn builtin_call(
        &self,
        n: Node<'_>,
        args: &[Node<'_>],
        env: &Env,
        floor_builtin: bool,
    ) -> Option<LibraryCost> {
        let mut builtin = None;
        if floor_builtin && args.len() == 1 && self.numeric(self.unwrap(args[0]), env) {
            self.assume("Assume the standard Math.floor global has not been replaced or mutated through aliases.");
            builtin = Some(LibraryCost {
                expression: SizeExpr::Constant(1),
                status: EstimateStatus::Tight,
                description: "Standard Math.floor of a modeled finite builtin number.".into(),
            });
        }
        if let Some(length) = self.builtin_length(n, env) {
            builtin = Some(length);
        }
        if self.dialect == Dialect::Python
            && self.builtin_name(n, "range")
            && (1..=3).contains(&args.len())
        {
            if args.iter().all(|a| self.numeric(self.unwrap(*a), env)) {
                self.assume("Assume the standard range builtin has not been replaced at runtime.");
                builtin = Some(LibraryCost { expression: SizeExpr::Constant(1), status: EstimateStatus::Tight,
                    description: "Construct a builtin Python range under the unit-cost integer model; it does not materialize its elements.".into() });
            }
        }
        if self.dialect == Dialect::Php && self.builtin_name(n, "intdiv") && args.len() == 2 {
            if args.iter().all(|a| self.numeric(self.unwrap(*a), env)) {
                builtin = Some(LibraryCost {
                    expression: SizeExpr::Constant(1),
                    status: EstimateStatus::Tight,
                    description: "Builtin PHP integer division of modeled primitive inputs.".into(),
                });
            }
        }
        builtin
    }
    fn builtin_length(&self, n: Node<'_>, env: &Env) -> Option<LibraryCost> {
        let mut builtin = None;
        if let Some(input) = self.length_receiver(n) {
            let name = self.simple_callee(n).unwrap_or("");
            let valid_type = if self.dialect == Dialect::Php
                && name.trim_start_matches('\\') == "strlen"
            {
                match self.kind(input, env) {
                    Kind::String => true,
                    Kind::Input => {
                        self.assume("Assume strlen receives a builtin string without user-defined coercion.");
                        true
                    }
                    _ => false,
                }
            } else {
                self.collection(input, env).is_some()
            };
            if valid_type {
                if self.dialect == Dialect::Python {
                    self.assume(
                        "Assume the standard len builtin has not been replaced at runtime.",
                    );
                }
                builtin = Some(LibraryCost {
                    expression: SizeExpr::Constant(1), status: EstimateStatus::Tight,
                    description: "Native length/count of a modeled builtin collection/string; PHP recursive count and Countable objects are excluded.".into(),
                });
            }
        }
        builtin
    }
}
