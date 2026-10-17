//! PHP/Python/JavaScript/TypeScript adapters for the shared symbolic cost IR.
//! Dynamic semantics are assumptions or unknowns, never implicit constant costs.
use super::{child, named_children, range, snippet, Dialect};
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
        let root = ty.split(['[', '<']).next().unwrap_or(&ty);
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
        if depth > 256 {
            return self.unknown(n, "cost extraction depth limit exceeded");
        }
        match n.kind() {
            "comment"
            | "line_comment"
            | "block_comment"
            | "empty_statement"
            | "pass_statement"
            | "function_declaration" => self.unit(n),
            "function_definition" => {
                if self.text(n).contains('=') || self.text(n).trim_start().starts_with('@') {
                    self.unknown(
                        n,
                        "nested function defaults/decorators may execute unmodeled code",
                    )
                } else {
                    self.unit(n)
                }
            }
            "block" | "statement_block" | "compound_statement" => {
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
            "expression_statement" | "parenthesized_expression" | "argument" | "else_clause" => {
                self.children(n, env, in_loop, depth)
            }
            "identifier" | "variable_name" => {
                if env.contains_key(&self.name(n)) {
                    self.unit(n)
                } else {
                    self.unknown(n, format!("unbound value semantics: {}", self.text(n)))
                }
            }
            "this" if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript) => {
                self.unit(n)
            }
            "number" | "integer" | "float" | "integer_literal" | "true" | "false" | "boolean"
            | "null" | "none" | "string_literal" => self.unit(n),
            "string" => {
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
            "encapsed_string" | "template_string" | "concatenated_string" => {
                self.unknown(n, "string interpolation/concatenation cost is not modeled")
            }
            "lexical_declaration" | "variable_declaration" => self.children(n, env, in_loop, depth),
            "variable_declarator"
            | "assignment"
            | "assignment_expression"
            | "augmented_assignment"
            | "augmented_assignment_expression" => self.assignment(n, env, in_loop, depth),
            "update_expression" => {
                let target = child(n, "argument").or_else(|| named_children(n).first().copied());
                if let Some(target) = target {
                    if !self.numeric(target, env) {
                        return self.unknown(n, "increment may execute user-defined coercion");
                    }
                    if !matches!(target.kind(), "identifier" | "variable_name") {
                        let ordinary_scalar = self
                            .ordinary_field_kind(target, env)
                            .or_else(|| self.php_field_kind(target))
                            .is_some_and(|kind| {
                                matches!(kind, Kind::Integer | Kind::Number | Kind::Bool)
                            });
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
            "binary_expression"
            | "binary_operator"
            | "comparison_operator"
            | "unary_expression"
            | "unary_operator"
            | "unary_op_expression" => {
                let cost = self.children(n, env, in_loop, depth);
                if self.dialect == Dialect::Python && self.operator(n) == "**" {
                    return self.sequence(n, vec![cost, self.unknown(n, "Python exponentiation algorithm and integer magnitude costs are not modeled")]);
                }
                if named_children(n).iter().all(|c| self.numeric(*c, env)) {
                    cost
                } else {
                    self.sequence(n, vec![cost, self.unknown(n, "operator semantics are not established as builtin numeric operations")])
                }
            }
            "if_statement" | "elif_clause" | "conditional_expression" | "ternary_expression" => {
                self.branch(n, env, in_loop, depth)
            }
            "for_statement" => {
                if self.dialect == Dialect::Python {
                    self.foreach(n, env, depth)
                } else {
                    self.counted_for(n, env, depth)
                }
            }
            "for_in_statement" | "foreach_statement" => self.foreach(n, env, depth),
            "while_statement" => self.while_loop(n, env, depth),
            "call"
            | "call_expression"
            | "function_call_expression"
            | "member_call_expression"
            | "scoped_call_expression" => self.call(n, env, in_loop, depth),
            "member_expression" | "attribute" | "member_access_expression" => {
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
            "subscript" | "subscript_expression" => {
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
            "list" | "tuple" | "array" | "array_creation_expression" => {
                self.children(n, env, in_loop, depth)
            }
            "dictionary" | "object" if self.dialect != Dialect::Php => {
                self.children(n, env, in_loop, depth)
            }
            "pair" if self.dialect != Dialect::Php => {
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
                            self.unknown(
                                n,
                                "object literal prototype initialization is not modeled",
                            ),
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
            "shorthand_property_identifier" => {
                if env.contains_key(&self.name(n)) {
                    self.unit(n)
                } else {
                    self.unknown(n, "unbound shorthand property value")
                }
            }
            "array_element_initializer" => {
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
            "return_statement" | "break_statement" | "continue_statement" => {
                let cost = self.children(n, env, in_loop, depth);
                if in_loop {
                    self.make(n, CostKind::UpperBound(Box::new(cost)))
                } else {
                    cost
                }
            }
            "arrow_function"
            | "lambda"
            | "anonymous_function"
            | "anonymous_function_creation_expression" => {
                self.unknown(n, "closure capture and deferred execution are not modeled")
            }
            "await_expression" | "yield" | "yield_expression" | "yield_from" | "try_statement"
            | "with_statement" | "throw_statement" | "raise_statement" | "switch_statement"
            | "match_statement" | "match_expression" => {
                let cost = self.children(n, env, in_loop, depth);
                self.sequence(
                    n,
                    vec![
                        cost,
                        self.unknown(n, "control-flow construct is not modeled"),
                    ],
                )
            }
            _ => {
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
        }
    }
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
        let mut builtin = None;
        if floor_builtin && args.len() == 1 && self.numeric(self.unwrap(args[0]), env) {
            self.assume("Assume the standard Math.floor global has not been replaced or mutated through aliases.");
            builtin = Some(LibraryCost {
                expression: SizeExpr::Constant(1),
                status: EstimateStatus::Tight,
                description: "Standard Math.floor of a modeled finite builtin number.".into(),
            });
        }
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
    fn counted_for(&self, n: Node<'_>, env: &mut Env, depth: usize) -> CostNode {
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
    fn foreach(&self, n: Node<'_>, env: &mut Env, depth: usize) -> CostNode {
        if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
            && self.operator(n) != "of"
        {
            return self.unknown(
                n,
                "for-in property enumeration is not a modeled collection traversal",
            );
        }
        let value = child(n, "right")
            .or_else(|| child(n, "value"))
            .or_else(|| child(n, "traversable"));
        let pattern = child(n, "left")
            .or_else(|| child(n, "value"))
            .or_else(|| child(n, "key"));
        let Some(body) = self.body(n) else {
            return self.unknown(n, "missing collection loop body");
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
            return self.unknown(n, "unsupported collection loop shape");
        };
        let mut key = None;
        if self.dialect == Dialect::Php && pattern.kind() == "pair" {
            let bindings = named_children(pattern);
            if bindings.len() != 2 || bindings.iter().any(|n| n.kind() != "variable_name") {
                return self.unknown(n, "loop destructuring or reference binding is not modeled");
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
            return self.unknown(n, "loop destructuring or reference binding is not modeled");
        }
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
        let mut count;
        let mut induction = None;
        let mut status = EstimateStatus::Tight;
        let mut assumptions = Vec::new();
        if self.dialect == Dialect::Python && self.builtin_name(value, "range") {
            let args = self.arguments(value);
            if args.is_empty() || args.len() > 3 {
                return self.unknown(n, "range needs one to three positional arguments");
            }
            let (start, end) = if args.len() == 1 {
                (None, args[0])
            } else {
                (Some(args[0]), args[1])
            };
            if args.iter().any(|a| !self.numeric(self.unwrap(*a), env)) {
                return self.unknown(n, "range input types are not known as builtin integers");
            }
            let step = args
                .get(2)
                .map(|a| self.literal(self.unwrap(*a)))
                .unwrap_or(Some(1));
            if !step.is_some_and(|s| s > 0) {
                return self.unknown(n, "range step must be a positive constant integer");
            }
            count = self.sizes(end, env).value;
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
            induction = Some(name);
        } else {
            let Some(kind) = self.collection(value, env) else {
                return self.unknown(
                    n,
                    "custom iterable/iterator cardinality and next() cost are not modeled",
                );
            };
            count = self.sizes(value, env).length;
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
        }
        if !self.stable(value, body) {
            count = SizeExpr::Unknown("collection/range bound changes inside loop".into());
        }
        let setup = self.node(value, env, false, depth + 1);
        let body_cost = self.node(body, &mut local, true, depth + 1);
        self.invalidate(n, env);
        let result = self.loop_cost(
            n,
            count,
            induction,
            setup,
            self.unit(n),
            body_cost,
            status,
            assumptions,
        );
        self.loop_alternative(n, result, env, depth)
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
    fn while_loop(&self, n: Node<'_>, env: &mut Env, depth: usize) -> CostNode {
        let (Some(test), Some(body)) = (child(n, "condition"), self.body(n)) else {
            return self.unknown(n, "missing while-loop condition/body");
        };
        let test = self.unwrap(test);
        let (left, right) = if self.dialect == Dialect::Python {
            let children = named_children(test);
            (children.first().copied(), children.last().copied())
        } else {
            (child(test, "left"), child(test, "right"))
        };
        let (Some(left), Some(right)) = (left, right) else {
            return self.unknown(n, "unsupported while-loop condition");
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
            return self.unknown(n, "while-loop counter must have one unconditional progressing update and an unchanged bound");
        }
        let (op, step) = &updates[0];
        let initial = env.get(&counter).cloned();
        let mut count =
            SizeExpr::Unknown("while-loop progress or starting bound is not modeled".into());
        let mut status = EstimateStatus::Tight;
        let mut induction_bound = None;
        if ["<", "<="].contains(&comparison.as_str())
            && (op == "+" || op == "++")
            && *step > 0
            && initial.as_ref().and_then(|b| b.initial).is_some()
        {
            count = self.sizes(right, env).value;
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
        } else if [">", ">="].contains(&comparison.as_str())
            && (op == "-" || op == "//")
            && *step > 0
            && self.literal(right).is_some()
            && !(op == "//" && (comparison == ">=" && self.literal(right) == Some(0) || *step <= 1))
        {
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
        } else if ["<", "<="].contains(&comparison.as_str())
            && op == "*"
            && *step > 1
            && initial
                .as_ref()
                .and_then(|b| b.initial)
                .is_some_and(|n| n > 0)
        {
            let bound = self.sizes(right, env).value;
            induction_bound = Some(bound.clone());
            count = SizeExpr::Log(Box::new(bound));
            status = EstimateStatus::Heuristic;
        }
        if (matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
            || (self.dialect == Dialect::Php
                && (self.kind(right, env) != Kind::Integer
                    || initial.as_ref().is_some_and(|b| b.kind != Kind::Integer))))
            && !matches!(count, SizeExpr::Constant(_))
        {
            status = status.max(EstimateStatus::Heuristic);
        }
        let induction = format!("@iteration:{}", n.start_byte());
        let mut local = env.clone();
        local.insert(counter, self.counter_binding(left, induction.clone()));
        let check = self.node(test, &mut local, true, depth + 1);
        let body_cost = self.node(body, &mut local, true, depth + 1);
        self.invalidate(n, env);
        let result = self.make(n, CostKind::Repeat(Box::new(LoopCost {
            count, induction: Some(induction), induction_bound,
            setup: Box::new(self.unit(n)), check: Box::new(check), body: Box::new(body_cost), status,
            assumptions: vec!["Assume nonnegative finite numeric magnitudes, normal arithmetic without overflow/stagnation, and the modeled counter reaching its termination bound.".into()],
        })));
        self.loop_alternative(n, result, env, depth)
    }
}
