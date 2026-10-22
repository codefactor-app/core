//! PHP/Python/JavaScript/TypeScript adapters for the shared symbolic cost IR.
//! Dynamic semantics are assumptions or unknowns, never implicit constant costs.
mod loops;
mod nodes;
use crate::support::{child, named_children, range, snippet, Dialect};
use analyzer_core::algorithmic::*;
use analyzer_core::ir::FunctionModel;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use tree_sitter::Node;

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Integer,
    Number,
    Bool,
    String,
    Array(Box<Kind>),
    Receiver,
    Input,
    Opaque,
}
#[derive(Clone)]
struct Binding {
    kind: Kind,
    sizes: ArgumentSize,
    initial: Option<u64>,
}
type Env = BTreeMap<String, Binding>;
struct Extractor<'a> {
    source: &'a str,
    path: &'a Path,
    dialect: Dialect,
    php_namespace: bool,
    php_fields: BTreeMap<String, Kind>,
    receiver_name: Option<String>,
    ordinary_fields: BTreeMap<String, Kind>,
    shadows: &'a BTreeSet<String>,
    assumptions: RefCell<BTreeSet<String>>,
    writes: RefCell<HashMap<usize, BTreeSet<String>>>,
}

pub(super) fn shadows(root: Node<'_>, source: &str, _dialect: Dialect) -> BTreeSet<String> {
    let modeled = [
        "len", "range", "int", "float", "bool", "str", "list", "tuple", "count", "strlen",
        "intdiv", "Math", "Array",
    ];
    let mut out = BTreeSet::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if matches!(
            n.kind(),
            "function_definition"
                | "function_declaration"
                | "class_definition"
                | "class_declaration"
        ) {
            if let Some(name) = child(n, "name") {
                let name = snippet(name, source);
                if modeled.contains(&name) {
                    out.insert(name.into());
                }
            }
        }
        // A conservative file-wide binding guard also includes future declarations.
        if matches!(
            n.kind(),
            "assignment"
                | "assignment_expression"
                | "variable_declarator"
                | "augmented_assignment"
                | "simple_parameter"
                | "required_parameter"
                | "optional_parameter"
                | "default_parameter"
                | "typed_parameter"
                | "for_statement"
                | "for_in_statement"
        ) {
            if let Some(left) = child(n, "left")
                .or_else(|| child(n, "name"))
                .or_else(|| child(n, "pattern"))
            {
                for name in binding_names(left, source) {
                    if modeled.contains(&name.as_str()) {
                        out.insert(name);
                    }
                }
            }
        }
        if n.kind().contains("import") || n.kind().contains("namespace_use") {
            let words = snippet(n, source).split(|c: char| !c.is_alphanumeric() && c != '_');
            for word in words {
                if modeled.contains(&word) {
                    out.insert(word.into());
                }
            }
        }
        if matches!(
            n.kind(),
            "parameters" | "formal_parameters" | "lambda_parameters"
        ) {
            for p in named_children(n) {
                if p.kind() == "identifier" && modeled.contains(&snippet(p, source)) {
                    out.insert(snippet(p, source).into());
                }
            }
        }
        stack.extend(named_children(n));
    }
    out
}

pub(super) fn extract(
    body: Node<'_>,
    function: &FunctionModel,
    source: &str,
    path: &Path,
    dialect: Dialect,
    shadows: &BTreeSet<String>,
    php_namespace: bool,
) -> CostNode {
    let mut e = Extractor {
        source,
        path,
        dialect,
        php_namespace,
        php_fields: BTreeMap::new(),
        receiver_name: None,
        ordinary_fields: BTreeMap::new(),
        shadows,
        assumptions: RefCell::new(BTreeSet::new()),
        writes: RefCell::new(HashMap::new()),
    };
    let mut env = Env::new();
    if dialect == Dialect::Php
        && function.parent_type.is_some()
        && function.parent_function.is_none()
        && !function.modifiers.iter().any(|m| m == "static")
    {
        env.insert(
            "this".into(),
            Binding {
                kind: Kind::Opaque,
                sizes: ArgumentSize::unknown("object receiver dimensions are not modeled"),
                initial: None,
            },
        );
        e.php_fields = e.declared_php_fields(body);
    }
    if function.parent_type.is_some()
        && function.parent_function.is_none()
        && !function.modifiers.iter().any(|m| m == "static")
    {
        e.receiver_name = if dialect == Dialect::Python {
            function
                .parameters
                .iter()
                .find(|p| p.implicit_receiver)
                .map(|p| p.name.clone())
        } else if matches!(dialect, Dialect::JavaScript | Dialect::TypeScript) {
            Some("this".into())
        } else {
            None
        };
        if let Some(name) = &e.receiver_name {
            env.insert(
                name.clone(),
                Binding {
                    kind: Kind::Receiver,
                    sizes: ArgumentSize::unknown("object receiver dimensions are not modeled"),
                    initial: None,
                },
            );
            e.ordinary_fields = e.declared_ordinary_fields(body);
        }
    }
    let mut prelude = Vec::new();
    if function.body_range == function.range {
        prelude.push(e.unknown(body, "function implementation is unavailable"));
    }
    let definition = if function.body_range == function.range {
        body
    } else {
        body.parent().unwrap_or(body)
    };
    if let Some(parameters) = child(definition, "parameters") {
        let mut stack = vec![parameters];
        while let Some(parameter) = stack.pop() {
            if matches!(
                parameter.kind(),
                "object_pattern" | "array_pattern" | "tuple_pattern" | "pattern_list"
            ) {
                prelude.push(e.unknown(
                    parameter,
                    "parameter destructuring can execute unmodeled getters/iteration",
                ));
                break;
            }
            stack.extend(named_children(parameter));
        }
    }
    for p in &function.parameters {
        if p.implicit_receiver {
            continue;
        }
        let kind = e.type_kind(p.type_name.as_deref().unwrap_or(""));
        // Object annotations do not execute operations on a parameter. Python may
        // release its last reference on return, so its lifetime needs an assumption.
        if kind == Kind::Opaque && dialect == Dialect::Python {
            e.assume(format!("Assume releasing parameter {} does not execute user-defined finalizers or input-sized cleanup.", p.name));
        }
        if p.variadic || !plain_name(&p.name) {
            prelude.push(e.unknown(
                body,
                format!("parameter semantics are not modeled: {}", p.name),
            ));
        }
        if p.default.as_deref().is_some_and(|v| !constant_default(v)) {
            prelude.push(e.unknown(
                body,
                "nonconstant default parameter evaluation is not modeled",
            ));
        }
        let mut sizes = ArgumentSize::unknown("size is unavailable for this type");
        if matches!(kind, Kind::Integer | Kind::Number | Kind::Input) {
            sizes.value = SizeExpr::value(&p.name);
        }
        if matches!(kind, Kind::Array(_) | Kind::String | Kind::Input) {
            sizes.length = SizeExpr::length(&p.name);
        }
        env.insert(
            p.name.clone(),
            Binding {
                kind,
                sizes,
                initial: None,
            },
        );
    }
    if function
        .modifiers
        .iter()
        .any(|m| m.contains("async") || m.contains("generator"))
        || function.signature.trim_start().starts_with("async ")
        || function.signature.contains("function*")
    {
        prelude.push(e.unknown(body, "async/generator execution is not modeled"));
    }
    if function.signature.contains('&') && dialect == Dialect::Php {
        prelude.push(e.unknown(body, "PHP reference parameters/returns are not modeled"));
    }
    if function.modifiers.iter().any(|m| m.starts_with('@')) {
        prelude.push(e.unknown(body, "decorator may replace function execution"));
    }
    prelude.push(e.node(body, &mut env, false, 0));
    let result = e.sequence(body, prelude);
    let assumptions = e.assumptions.into_inner().into_iter().collect::<Vec<_>>();
    if assumptions.is_empty() {
        result
    } else {
        CostNode {
            range: range(body, path),
            kind: CostKind::Heuristic {
                body: Box::new(result),
                assumptions,
            },
        }
    }
}

fn plain_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')
}
fn constant_default(value: &str) -> bool {
    let value = value.trim().trim_start_matches('=').trim();
    value.parse::<f64>().is_ok()
        || matches!(value, "true" | "false" | "True" | "False" | "null" | "None")
}
fn binding_names(n: Node<'_>, source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![n];
    while let Some(n) = stack.pop() {
        if matches!(n.kind(), "identifier" | "variable_name") {
            out.push(snippet(n, source).trim_start_matches('$').into());
        } else {
            stack.extend(named_children(n));
        }
    }
    out
}

impl Extractor<'_> {
    fn text(&self, n: Node<'_>) -> &str {
        snippet(n, self.source).trim()
    }
    fn name(&self, n: Node<'_>) -> String {
        self.text(n).trim_start_matches('$').into()
    }
    fn make(&self, n: Node<'_>, kind: CostKind) -> CostNode {
        CostNode {
            range: range(n, self.path),
            kind,
        }
    }
    fn unit(&self, n: Node<'_>) -> CostNode {
        self.make(n, CostKind::Unit)
    }
    fn unknown(&self, n: Node<'_>, reason: impl Into<String>) -> CostNode {
        self.make(n, CostKind::Unknown(reason.into().into()))
    }
    fn sequence(&self, n: Node<'_>, nodes: Vec<CostNode>) -> CostNode {
        let mut items = Vec::new();
        let mut reasons = BTreeSet::new();
        for node in nodes {
            match node.kind {
                CostKind::Unit => (),
                CostKind::Sequence(children) => items.extend(children),
                CostKind::Unknown(ref reason)
                    if !reasons.insert(reason_category(reason).to_string()) =>
                {
                    ()
                }
                _ => items.push(node),
            }
        }
        match items.len() {
            0 => self.unit(n),
            1 => items.pop().unwrap(),
            _ => self.make(n, CostKind::Sequence(items)),
        }
    }
    fn assume(&self, assumption: impl Into<String>) {
        self.assumptions.borrow_mut().insert(assumption.into());
    }
    fn type_kind(&self, ty: &str) -> Kind {
        let ty = ty.trim().trim_start_matches(':').trim().replace(' ', "");
        let ty = if self.dialect == Dialect::Php {
            ty.trim_start_matches('?')
        } else {
            &ty
        };
        if ty.is_empty() || ty == "any" || ty == "unknown" {
            return Kind::Input;
        }
        let root = ty.split(['[', '<']).next().unwrap_or(ty);
        if self.dialect == Dialect::Python && self.shadows.contains(root) {
            return Kind::Opaque;
        }
        if ty == "int" {
            return Kind::Integer;
        }
        if matches!(ty, "float" | "number") {
            return Kind::Number;
        }
        if matches!(ty, "bool" | "boolean") {
            return Kind::Bool;
        }
        if matches!(ty, "str" | "string") {
            return Kind::String;
        }
        if let Some(inner) = ty.strip_suffix("[]") {
            return Kind::Array(Box::new(self.type_kind(inner)));
        }
        for (prefix, end) in [
            ("list[", "]"),
            ("tuple[", "]"),
            ("Array<", ">"),
            ("ReadonlyArray<", ">"),
        ] {
            if let Some(inner) = ty.strip_prefix(prefix).and_then(|s| s.strip_suffix(end)) {
                return Kind::Array(Box::new(self.type_kind(inner)));
            }
        }
        if matches!(ty, "array" | "list" | "tuple") {
            return Kind::Array(Box::new(Kind::Input));
        }
        Kind::Opaque
    }
    fn declared_php_fields(&self, body: Node<'_>) -> BTreeMap<String, Kind> {
        let mut owner = body.parent();
        while let Some(node) = owner {
            // Trait fields depend on composition and are not established here.
            if node.kind() == "trait_declaration" {
                return BTreeMap::new();
            }
            if node.kind() == "class_declaration" {
                let mut fields = BTreeMap::new();
                if let Some(body) = child(node, "body") {
                    for declaration in named_children(body) {
                        if declaration.kind() != "property_declaration"
                            || named_children(declaration).iter().any(|c| {
                                matches!(c.kind(), "property_hook_list" | "static_modifier")
                            })
                        {
                            continue;
                        }
                        let kind = child(declaration, "type")
                            .map(|ty| self.type_kind(self.text(ty)))
                            .unwrap_or(Kind::Opaque);
                        for field in named_children(declaration) {
                            if field.kind() == "property_element" {
                                if let Some(name) = child(field, "name") {
                                    fields.insert(self.name(name), kind.clone());
                                }
                            }
                        }
                    }
                }
                return fields;
            }
            owner = node.parent();
        }
        BTreeMap::new()
    }
    fn php_field_kind(&self, n: Node<'_>) -> Option<&Kind> {
        if self.dialect != Dialect::Php
            || n.kind() != "member_access_expression"
            || !child(n, "object").is_some_and(|o| self.text(o) == "$this")
        {
            return None;
        }
        let name = child(n, "name").filter(|n| n.kind() == "name")?;
        self.php_fields.get(self.text(name))
    }
    fn declared_ordinary_fields(&self, body: Node<'_>) -> BTreeMap<String, Kind> {
        let mut owner = body.parent();
        while let Some(node) = owner {
            if matches!(
                node.kind(),
                "class_definition" | "class_declaration" | "class"
            ) {
                // Inherited storage, decorators and metaclasses need their own models.
                if child(node, "superclasses").is_some()
                    || child(node, "decorator").is_some()
                    || node
                        .parent()
                        .is_some_and(|p| p.kind() == "decorated_definition")
                    || named_children(node)
                        .iter()
                        .any(|n| n.kind() == "class_heritage")
                {
                    return BTreeMap::new();
                }
                let Some(body) = child(node, "body") else {
                    return BTreeMap::new();
                };
                if self.dialect == Dialect::Python {
                    let mut stack = vec![body];
                    while let Some(binding) = stack.pop() {
                        let hook_name =
                            if matches!(binding.kind(), "function_definition" | "class_definition")
                            {
                                child(binding, "name")
                            } else if binding.kind() == "identifier" {
                                Some(binding)
                            } else {
                                None
                            };
                        if hook_name.is_some_and(|name| {
                            matches!(
                                self.text(name),
                                "__getattribute__" | "__getattr__" | "__setattr__"
                            )
                        }) {
                            // Aliases, imports and conditional class bindings can install
                            // hooks just as a def can. Do not infer ordinary storage.
                            return BTreeMap::new();
                        }
                        if !matches!(
                            binding.kind(),
                            "function_definition" | "class_definition" | "lambda"
                        ) {
                            stack.extend(named_children(binding));
                        }
                    }
                }
                let mut fields = BTreeMap::new();
                let mut blocked = BTreeSet::new();
                for declaration in named_children(body) {
                    let definition = if declaration.kind() == "decorated_definition" {
                        child(declaration, "definition").unwrap_or(declaration)
                    } else {
                        declaration
                    };
                    if matches!(
                        definition.kind(),
                        "function_definition" | "method_definition"
                    ) {
                        if let Some(name) = child(definition, "name") {
                            let name = self.text(name);
                            if self.dialect == Dialect::Python
                                && matches!(
                                    name,
                                    "__getattribute__" | "__getattr__" | "__setattr__"
                                )
                            {
                                return BTreeMap::new();
                            }
                            if self.dialect == Dialect::Python
                                && name == "__init__"
                                && declaration.kind() != "decorated_definition"
                            {
                                for (name, kind) in self.python_initial_fields(definition) {
                                    if kind == Kind::Opaque {
                                        blocked.insert(name);
                                    } else {
                                        fields.insert(name, kind);
                                    }
                                }
                            }
                            blocked.insert(name.to_string());
                        }
                        continue;
                    }
                    let declaration = if declaration.kind() == "expression_statement" {
                        named_children(declaration)
                            .first()
                            .copied()
                            .unwrap_or(declaration)
                    } else {
                        declaration
                    };
                    let name =
                        if self.dialect == Dialect::Python && declaration.kind() == "assignment" {
                            child(declaration, "left").filter(|n| n.kind() == "identifier")
                        } else if matches!(
                            declaration.kind(),
                            "field_definition" | "public_field_definition"
                        ) {
                            let mut cursor = declaration.walk();
                            if declaration.children(&mut cursor).any(|n| {
                                matches!(n.kind(), "static" | "declare" | "abstract" | "decorator")
                                    || n.kind() == "accessor"
                            }) {
                                continue;
                            }
                            child(declaration, "property")
                                .or_else(|| child(declaration, "name"))
                                .filter(|n| {
                                    matches!(
                                        n.kind(),
                                        "property_identifier" | "private_property_identifier"
                                    )
                                })
                        } else {
                            None
                        };
                    let Some(name) = name else {
                        if self.dialect == Dialect::Python {
                            // Class-body control flow can replace a previously ordinary
                            // field with a descriptor; it cannot establish storage here.
                            blocked.extend(self.write_names(declaration));
                            if declaration.kind() == "delete_statement" {
                                blocked.extend(binding_names(declaration, self.source));
                            }
                        }
                        continue;
                    };
                    let value = child(declaration, "right").or_else(|| child(declaration, "value"));
                    let kind = if let Some(value) = value {
                        self.kind(value, &Env::new())
                    } else if self.dialect == Dialect::TypeScript {
                        child(declaration, "type")
                            .map(|n| self.type_kind(self.text(n)))
                            .unwrap_or(Kind::Opaque)
                    } else {
                        Kind::Opaque
                    };
                    // A declaration initialized by a call may contain a descriptor.
                    // Only visible builtin storage values establish a useful kind.
                    if matches!(
                        kind,
                        Kind::Integer | Kind::Number | Kind::Bool | Kind::String | Kind::Array(_)
                    ) {
                        fields.insert(self.text(name).to_string(), kind);
                    } else {
                        blocked.insert(self.text(name).to_string());
                    }
                }
                for name in blocked {
                    fields.remove(&name);
                }
                return fields;
            }
            owner = node.parent();
        }
        BTreeMap::new()
    }
    fn python_initial_fields(&self, definition: Node<'_>) -> BTreeMap<String, Kind> {
        let mut fields = BTreeMap::new();
        let Some(parameters) = child(definition, "parameters") else {
            return fields;
        };
        let mut parameters_env = Env::new();
        let mut receiver = None;
        for parameter in named_children(parameters) {
            let name = if parameter.kind() == "identifier" {
                Some(parameter)
            } else {
                named_children(parameter)
                    .into_iter()
                    .find(|n| n.kind() == "identifier")
            };
            let Some(name) = name else { continue };
            if receiver.is_none() {
                receiver = Some(self.text(name).to_string());
            }
            let kind = child(parameter, "type")
                .map(|n| self.type_kind(self.text(n)))
                .unwrap_or(Kind::Input);
            parameters_env.insert(
                self.text(name).to_string(),
                Binding {
                    kind,
                    sizes: ArgumentSize::unknown(
                        "constructor parameter dimensions are not modeled",
                    ),
                    initial: None,
                },
            );
        }
        let (Some(receiver), Some(body)) = (receiver, child(definition, "body")) else {
            return fields;
        };
        if self.write_count(body, &receiver) > 0 {
            return fields;
        }
        let mut stack = vec![body];
        while let Some(node) = stack.pop() {
            if matches!(
                node.kind(),
                "function_definition" | "lambda" | "class_definition"
            ) {
                continue;
            }
            if node.kind() == "assignment" {
                if let Some(left) = child(node, "left").filter(|n| n.kind() == "attribute") {
                    if child(left, "object").is_some_and(|n| self.text(n) == receiver) {
                        if let Some(name) = child(left, "attribute") {
                            // Only unconditional initialization establishes ordinary storage.
                            let direct = node.parent().is_some_and(|p| {
                                p.kind() == "expression_statement"
                                    && p.parent().is_some_and(|p| p.id() == body.id())
                            });
                            let kind = if direct {
                                child(node, "right")
                                    .map(|n| self.kind(n, &parameters_env))
                                    .unwrap_or(Kind::Opaque)
                            } else {
                                Kind::Opaque
                            };
                            let kind = if matches!(
                                kind,
                                Kind::Integer
                                    | Kind::Number
                                    | Kind::Bool
                                    | Kind::String
                                    | Kind::Array(_)
                            ) {
                                kind
                            } else {
                                Kind::Opaque
                            };
                            fields
                                .entry(self.text(name).to_string())
                                .and_modify(|old| {
                                    if *old != kind {
                                        *old = Kind::Opaque;
                                    }
                                })
                                .or_insert(kind);
                        }
                    }
                }
            }
            stack.extend(named_children(node));
        }
        fields
    }
    fn ordinary_field_kind(&self, n: Node<'_>, env: &Env) -> Option<&Kind> {
        if !matches!(n.kind(), "member_expression" | "attribute") {
            return None;
        }
        let object = child(n, "object")?;
        let receiver = self.receiver_name.as_deref()?;
        if self.text(object) != receiver || env.get(receiver)?.kind != Kind::Receiver {
            return None;
        }
        let name = child(n, "property").or_else(|| child(n, "attribute"))?;
        if !matches!(
            name.kind(),
            "identifier" | "property_identifier" | "private_property_identifier"
        ) {
            return None;
        }
        self.ordinary_fields.get(self.text(name))
    }
    fn literal(&self, n: Node<'_>) -> Option<u64> {
        if !matches!(n.kind(), "integer" | "integer_literal" | "number") {
            return None;
        }
        let raw = self.text(n).replace('_', "");
        if let Some(raw) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
            u64::from_str_radix(raw, 16).ok()
        } else {
            raw.parse().ok()
        }
    }
    fn unwrap<'t>(&self, mut n: Node<'t>) -> Node<'t> {
        while matches!(n.kind(), "parenthesized_expression" | "argument") {
            let children = named_children(n);
            if children.len() != 1 {
                break;
            }
            n = children[0];
        }
        n
    }
    fn kind(&self, n: Node<'_>, env: &Env) -> Kind {
        let n = self.unwrap(n);
        match n.kind() {
            "this" if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript) => env
                .get("this")
                .map(|b| b.kind.clone())
                .unwrap_or(Kind::Opaque),
            "identifier" | "variable_name" => env
                .get(&self.name(n))
                .map(|b| b.kind.clone())
                .unwrap_or(Kind::Opaque),
            "integer" | "integer_literal" => Kind::Integer,
            "number" if self.literal(n).is_some() => Kind::Integer,
            "number" | "float" => Kind::Number,
            "true" | "false" | "boolean" => Kind::Bool,
            "string" | "string_literal" | "encapsed_string" => Kind::String,
            "list" | "tuple" | "array" | "array_creation_expression" => {
                let mut kinds = named_children(n).into_iter().map(|n| {
                    let value = if n.kind() == "array_element_initializer" {
                        named_children(n).last().copied().unwrap_or(n)
                    } else {
                        n
                    };
                    self.kind(value, env)
                });
                let first = kinds.next().unwrap_or(Kind::Integer);
                let kind = if kinds.all(|k| k == first) {
                    first
                } else {
                    Kind::Opaque
                };
                Kind::Array(Box::new(kind))
            }
            "binary_expression"
            | "binary_operator"
            | "unary_expression"
            | "unary_operator"
            | "unary_op_expression" => {
                if named_children(n).iter().all(|n| self.numeric(*n, env)) {
                    Kind::Number
                } else {
                    Kind::Opaque
                }
            }
            "subscript" | "subscript_expression" => {
                let receiver = child(n, "value")
                    .or_else(|| child(n, "object"))
                    .or_else(|| named_children(n).first().copied());
                match receiver.map(|n| self.kind(n, env)) {
                    Some(Kind::Array(element)) => *element,
                    Some(Kind::String) => Kind::String,
                    Some(Kind::Input) => Kind::Input,
                    _ => Kind::Opaque,
                }
            }
            "member_access_expression" if self.php_field_kind(n).is_some() => {
                self.php_field_kind(n).unwrap().clone()
            }
            "member_expression" | "attribute" if self.ordinary_field_kind(n, env).is_some() => {
                self.ordinary_field_kind(n, env).unwrap().clone()
            }
            _ => {
                if self.length_receiver(n).is_some()
                    || (self.dialect == Dialect::Php && self.builtin_name(n, "intdiv"))
                {
                    Kind::Integer
                } else {
                    Kind::Opaque
                }
            }
        }
    }
    fn numeric(&self, n: Node<'_>, env: &Env) -> bool {
        match self.kind(n, env) {
            Kind::Integer | Kind::Number | Kind::Bool => {
                if self.dialect == Dialect::Python {
                    self.assume("Python numeric operations use a unit-cost model for builtin numbers, excluding arbitrary-precision bit complexity and user subclasses.");
                }
                true
            }
            Kind::Input => {
                self.assume(format!("Assume {} is a finite builtin numeric value, with constant-cost arithmetic/coercion and no user-defined operators.", self.text(n)));
                true
            }
            _ => false,
        }
    }
    fn sizes(&self, n: Node<'_>, env: &Env) -> ArgumentSize {
        let n = self.unwrap(n);
        if let Some(value) = self.literal(n) {
            return ArgumentSize {
                value: SizeExpr::Constant(value),
                ..ArgumentSize::unknown("scalar has no length")
            };
        }
        if matches!(n.kind(), "identifier" | "variable_name") {
            return env
                .get(&self.name(n))
                .map(|b| b.sizes.clone())
                .unwrap_or_else(|| ArgumentSize::unknown("unbound size"));
        }
        if let Some(receiver) = self.length_receiver(n) {
            let sizes = self.sizes(receiver, env);
            return ArgumentSize {
                value: sizes.length,
                status: sizes.status,
                ..ArgumentSize::unknown("length has no collection size")
            };
        }
        if matches!(n.kind(), "binary_expression" | "binary_operator") {
            if let (Some(left), Some(right)) = (child(n, "left"), child(n, "right")) {
                let a = self.sizes(left, env);
                let b = self.sizes(right, env);
                let op = self.operator(n);
                if op == "+" || op == "*" {
                    let value = if op == "+" {
                        SizeExpr::Sum(vec![a.value, b.value])
                    } else {
                        SizeExpr::Product(vec![a.value, b.value])
                    };
                    return ArgumentSize {
                        value,
                        status: a.status.max(b.status),
                        ..ArgumentSize::unknown("numeric expression has no length")
                    };
                }
            }
        }
        if matches!(
            n.kind(),
            "list" | "tuple" | "array" | "array_creation_expression"
        ) {
            return ArgumentSize {
                length: SizeExpr::Constant(named_children(n).len() as u64),
                ..ArgumentSize::unknown("collection has no scalar magnitude")
            };
        }
        ArgumentSize::unknown("argument size expression is not modeled")
    }
    fn simple_callee(&self, n: Node<'_>) -> Option<&str> {
        if !matches!(
            n.kind(),
            "call" | "call_expression" | "function_call_expression"
        ) {
            return None;
        }
        let function = child(n, "function")?;
        matches!(function.kind(), "identifier" | "name" | "qualified_name")
            .then(|| self.text(function))
    }
    fn floor_builtin(&self, n: Node<'_>) -> bool {
        matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
            && !self.shadows.contains("Math")
            && child(n, "function").is_some_and(|f| {
                f.kind() == "member_expression"
                    && child(f, "object").is_some_and(|o| self.text(o) == "Math")
                    && child(f, "property").is_some_and(|p| self.text(p) == "floor")
            })
    }
    fn arguments<'t>(&self, n: Node<'t>) -> Vec<Node<'t>> {
        child(n, "arguments")
            .map(named_children)
            .unwrap_or_default()
    }
    fn builtin_name(&self, n: Node<'_>, name: &str) -> bool {
        self.simple_callee(n).is_some_and(|s| {
            (s == name && !self.shadows.contains(name) && !self.php_namespace)
                || (self.dialect == Dialect::Php && s == format!("\\{name}"))
        })
    }
    fn length_receiver<'t>(&self, n: Node<'t>) -> Option<Node<'t>> {
        let n = self.unwrap(n);
        if (self.dialect == Dialect::Python && self.builtin_name(n, "len"))
            || (self.dialect == Dialect::Php
                && (self.builtin_name(n, "count") || self.builtin_name(n, "strlen")))
        {
            let args = self.arguments(n);
            return (args.len() == 1).then(|| self.unwrap(args[0]));
        }
        if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
            && n.kind() == "member_expression"
            && child(n, "property").is_some_and(|p| self.text(p) == "length")
        {
            return child(n, "object");
        }
        None
    }
    fn collection(&self, n: Node<'_>, env: &Env) -> Option<Kind> {
        match self.kind(n, env) {
            Kind::Array(element) => {
                if self.dialect != Dialect::Php {
                    self.assume("Assume annotated collections are builtin finite arrays/lists/tuples, with no Proxy, subclass hooks or overridden iteration, indexing or length.");
                }
                Some(*element)
            }
            Kind::String => {
                self.assume("Assume standard string traversal/indexing and native string length units for this language.");
                Some(Kind::String)
            }
            Kind::Input => {
                self.assume(format!("Assume {} is a finite builtin collection with constant-cost traversal/length and no custom iterator or property hooks.", self.text(n)));
                Some(Kind::Input)
            }
            _ => None,
        }
    }
    fn operator(&self, n: Node<'_>) -> String {
        if let Some(op) = child(n, "operator") {
            return self.text(op).into();
        }
        if let (Some(a), Some(b)) = (child(n, "left"), child(n, "right")) {
            return self.source[a.end_byte()..b.start_byte()].trim().into();
        }
        if n.kind() == "comparison_operator" {
            let children = named_children(n);
            if children.len() == 2 {
                return self.source[children[0].end_byte()..children[1].start_byte()]
                    .trim()
                    .into();
            }
        }
        let raw = self.text(n);
        for op in ["++", "--", "+=", "-=", "*=", "/=", "//=", ">>="] {
            if raw.contains(op) {
                return op.into();
            }
        }
        String::new()
    }
    fn write_names(&self, n: Node<'_>) -> BTreeSet<String> {
        if let Some(names) = self.writes.borrow().get(&n.id()) {
            return names.clone();
        }
        let mut names = BTreeSet::new();
        let mut stack = vec![n];
        while let Some(n) = stack.pop() {
            if matches!(
                n.kind(),
                "function_definition" | "function_declaration" | "arrow_function" | "lambda"
            ) {
                continue;
            }
            if matches!(
                n.kind(),
                "assignment"
                    | "augmented_assignment"
                    | "assignment_expression"
                    | "augmented_assignment_expression"
                    | "update_expression"
                    | "variable_declarator"
                    | "for_statement"
                    | "for_in_statement"
            ) {
                if let Some(left) = child(n, "left")
                    .or_else(|| child(n, "name"))
                    .or_else(|| child(n, "argument"))
                {
                    names.extend(binding_names(left, self.source));
                }
            }
            stack.extend(named_children(n));
        }
        self.writes.borrow_mut().insert(n.id(), names.clone());
        names
    }
    fn invalidate(&self, node: Node<'_>, env: &mut Env) {
        for name in self.write_names(node) {
            if let Some(binding) = env.get_mut(&name) {
                binding.sizes = ArgumentSize::unknown("size may have changed in a branch or loop");
                binding.initial = None;
                if binding.kind == Kind::Receiver && self.write_count(node, &name) > 0 {
                    binding.kind = Kind::Opaque;
                }
            }
        }
    }
    fn children(&self, n: Node<'_>, env: &mut Env, in_loop: bool, depth: usize) -> CostNode {
        self.sequence(
            n,
            named_children(n)
                .into_iter()
                .map(|c| self.node(c, env, in_loop, depth + 1))
                .collect(),
        )
    }
    fn node(&self, n: Node<'_>, env: &mut Env, in_loop: bool, depth: usize) -> CostNode {
        nodes::NodeExtractor { extractor: self }.node(n, env, in_loop, depth)
    }
}

impl Extractor<'_> {
    fn write_count(&self, root: Node<'_>, name: &str) -> usize {
        let mut count = 0;
        let mut stack = vec![root];
        while let Some(n) = stack.pop() {
            if matches!(
                n.kind(),
                "function_definition" | "function_declaration" | "arrow_function" | "lambda"
            ) {
                continue;
            }
            if matches!(
                n.kind(),
                "assignment"
                    | "augmented_assignment"
                    | "assignment_expression"
                    | "augmented_assignment_expression"
                    | "update_expression"
                    | "variable_declarator"
                    | "for_statement"
                    | "for_in_statement"
            ) {
                let target = child(n, "left")
                    .or_else(|| child(n, "name"))
                    .or_else(|| child(n, "argument"))
                    .or_else(|| {
                        (n.kind() == "update_expression")
                            .then(|| named_children(n).first().copied())
                            .flatten()
                    });
                if target.is_some_and(|t| self.name(t) == name) {
                    count += 1;
                }
            }
            stack.extend(named_children(n));
        }
        count
    }
}
