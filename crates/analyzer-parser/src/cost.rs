//! Rust cost extraction over the existing tree. Deliberately conservative without rustc.
mod nodes;
mod progress;
use crate::support::{child, named_children, range, snippet};
use analyzer_core::algorithmic::*;
use analyzer_core::ir::FunctionModel;
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use tree_sitter::Node;

#[derive(Clone)]
struct Binding {
    ty: String,
    sizes: ArgumentSize,
    initial: Option<u64>,
}
type Environment = BTreeMap<String, Arc<Binding>>;
struct Extractor<'a> {
    source: &'a str,
    path: &'a Path,
    mutations: RefCell<HashMap<usize, HashMap<&'a str, usize>>>,
    fields: BTreeMap<String, BTreeMap<String, String>>,
    shadows: &'a BTreeSet<String>,
}

pub(super) fn extract(
    body: Node<'_>,
    function: &FunctionModel,
    source: &str,
    path: &Path,
    shadows: &BTreeSet<String>,
) -> CostNode {
    let mut env = Environment::new();
    let mut prelude = Vec::new();
    let extractor = Extractor {
        source,
        path,
        mutations: RefCell::new(HashMap::new()),
        fields: if !has_field_expression(body) {
            BTreeMap::new()
        } else {
            declared_fields(body, source)
        },
        shadows,
    };
    for p in &function.parameters {
        let ty = if p.implicit_receiver {
            receiver_type(body, source)
                .or_else(|| p.type_name.clone())
                .unwrap_or_default()
        } else {
            p.type_name.clone().unwrap_or_default()
        };
        let mut sizes = ArgumentSize::unknown("parameter size is not known for this type");
        if scalar(&ty) {
            sizes.value = SizeExpr::value(&p.name);
        }
        if collection(&ty) {
            sizes.length = fixed_array_length(&ty).unwrap_or_else(|| SizeExpr::length(&p.name));
        }
        let clean = clean_type(&ty);
        let shadowed = clean
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .any(|token| {
                (scalar(token) || matches!(token, "Vec" | "String"))
                    && (shadows.contains(token) || shadows.contains("*"))
            })
            || ((clean.starts_with("std::") || clean.starts_with("alloc::"))
                && (shadows.contains("std") || shadows.contains("alloc")));
        if shadowed {
            prelude.push(extractor.unknown(
                body,
                format!("modeled type name may be shadowed in this file: {ty}"),
            ));
        }
        if !safe_type(&ty) {
            prelude.push(extractor.unknown(
                body,
                format!(
                    "type semantics or implicit destructor are not modeled: {}: {}",
                    p.name, ty
                ),
            ));
        }
        env.insert(
            p.name.clone(),
            Arc::new(Binding {
                ty,
                sizes,
                initial: None,
            }),
        );
    }
    if function
        .modifiers
        .iter()
        .any(|m| matches!(m.as_str(), "async" | "unsafe"))
        || function.signature.contains("async fn")
        || function.signature.contains("unsafe fn")
    {
        prelude.push(extractor.unknown(body, "async/unsafe function semantics are not modeled"));
    }
    prelude.push(extractor.node(body, &mut env, false, 0));
    extractor.sequence(body, prelude)
}

fn clean_type(ty: &str) -> Cow<'_, str> {
    let mut ty = ty.trim();
    while let Some(rest) = ty.strip_prefix('&') {
        ty = rest.trim_start();
        if ty.starts_with('\'') {
            ty = ty
                .split_once(char::is_whitespace)
                .map(|(_, s)| s)
                .unwrap_or("")
                .trim_start();
        }
        ty = ty.strip_prefix("mut ").unwrap_or(ty).trim_start();
    }
    if ty.contains(' ') {
        Cow::Owned(ty.replace(' ', ""))
    } else {
        Cow::Borrowed(ty)
    }
}
fn referent_type(ty: &str) -> Option<&str> {
    let mut ty = ty.trim().strip_prefix('&')?.trim_start();
    if ty.starts_with('\'') {
        ty = ty.split_once(char::is_whitespace)?.1.trim_start();
    }
    Some(ty.strip_prefix("mut ").unwrap_or(ty).trim_start())
}
fn scalar(ty: &str) -> bool {
    matches!(
        clean_type(ty).as_ref(),
        "u8" | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "f32"
            | "f64"
            | "bool"
            | "char"
            | "()"
    )
}
fn integer(ty: &str) -> bool {
    matches!(
        clean_type(ty).as_ref(),
        "u8" | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
    )
}
fn unsigned(ty: &str) -> bool {
    matches!(
        clean_type(ty).as_ref(),
        "u8" | "u16" | "u32" | "u64" | "u128" | "usize"
    )
}
fn scalar_bits(ty: &str) -> u64 {
    match clean_type(ty).as_ref() {
        "u8" => 8,
        "u16" | "usize" => 16, // Conservative across supported pointer widths.
        "u32" => 32,
        "u64" => 64,
        "u128" => 128,
        _ => 0,
    }
}
fn element(ty: &str) -> Option<String> {
    let ty = clean_type(ty);
    if let Some(inner) = ty.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        return Some(inner.split(';').next().unwrap_or(inner).into());
    }
    for prefix in ["Vec<", "std::vec::Vec<", "alloc::vec::Vec<"] {
        if let Some(inner) = ty.strip_prefix(prefix).and_then(|s| s.strip_suffix('>')) {
            return Some(inner.into());
        }
    }
    None
}
fn collection(ty: &str) -> bool {
    element(ty).is_some()
        || matches!(
            clean_type(ty).as_ref(),
            "str" | "String" | "std::string::String"
        )
}
fn safe_type(ty: &str) -> bool {
    // A reference itself has no destructor, even when its referent is opaque.
    // Operations on the referent still require their own concrete type model.
    if ty.trim_start().starts_with('&') {
        return true;
    }
    let clean = clean_type(ty);
    if !ty.trim_start().starts_with('&')
        && (matches!(clean.as_ref(), "String" | "std::string::String")
            || ["Vec<", "std::vec::Vec<", "alloc::vec::Vec<"]
                .iter()
                .any(|p| clean.starts_with(p)))
    {
        // Owned buffers invoke allocator cleanup; that cost has no certified model.
        return false;
    }
    scalar(ty)
        || matches!(
            clean_type(ty).as_ref(),
            "str" | "String" | "std::string::String"
        )
        || element(ty).is_some_and(|t| scalar(&t))
}
fn literal(node: Node<'_>, source: &str) -> Option<u64> {
    if node.kind() != "integer_literal" {
        return None;
    }
    let full = snippet(node, source).replace('_', "");
    let digits = full
        .strip_prefix("0x")
        .map(|s| (s, 16))
        .or_else(|| full.strip_prefix("0b").map(|s| (s, 2)))
        .or_else(|| full.strip_prefix("0o").map(|s| (s, 8)));
    if let Some((s, base)) = digits {
        let digits = s.split(|c: char| !c.is_digit(base)).next()?;
        return u64::from_str_radix(digits, base).ok();
    }
    let digits = full.split(|c: char| !c.is_ascii_digit()).next()?;
    digits.parse().ok()
}

impl Extractor<'_> {
    fn writes(&self, node: Node<'_>, name: &str) -> usize {
        self.mutations
            .borrow_mut()
            .entry(node.id())
            .or_insert_with(|| mutation_counts(node, self.source))
            .get(name)
            .copied()
            .unwrap_or(0)
    }
    fn make(&self, node: Node<'_>, kind: CostKind) -> CostNode {
        CostNode {
            range: range(node, self.path),
            kind,
        }
    }
    fn unit(&self, node: Node<'_>) -> CostNode {
        self.make(node, CostKind::Unit)
    }
    fn unknown(&self, node: Node<'_>, reason: impl Into<String>) -> CostNode {
        self.make(node, CostKind::Unknown(reason.into().into()))
    }
    fn sequence(&self, node: Node<'_>, items: Vec<CostNode>) -> CostNode {
        let mut flattened = Vec::new();
        for item in items {
            match item.kind {
                CostKind::Unit => {}
                CostKind::Sequence(children) => flattened.extend(children),
                _ => flattened.push(item),
            }
        }
        let mut unknowns = BTreeSet::new();
        let mut compact = flattened
            .into_iter()
            .filter(|item| match &item.kind {
                CostKind::Unknown(reason) => unknowns.insert(reason_category(reason).to_string()),
                _ => true,
            })
            .collect::<Vec<_>>();
        match compact.len() {
            0 => self.unit(node),
            1 => compact.pop().unwrap(),
            _ => self.make(node, CostKind::Sequence(compact)),
        }
    }
    fn bound(&self, node: Node<'_>, body: CostNode) -> CostNode {
        self.make(node, CostKind::UpperBound(Box::new(body)))
    }
    fn text<'a>(&self, node: Node<'a>) -> &str {
        snippet(node, self.source).trim()
    }
    fn infer_type(&self, node: Node<'_>, env: &Environment) -> String {
        match node.kind() {
            "identifier" | "self" => env
                .get(self.text(node))
                .map(|b| b.ty.clone())
                .unwrap_or_default(),
            "integer_literal" => {
                let s = self.text(node);
                for t in [
                    "usize", "isize", "u128", "i128", "u64", "i64", "u32", "i32", "u16", "i16",
                    "u8", "i8",
                ] {
                    if s.ends_with(t) {
                        return t.into();
                    }
                }
                "usize".into()
            }
            "boolean_literal" => "bool".into(),
            "char_literal" => "char".into(),
            "float_literal" => "f64".into(),
            "string_literal" | "raw_string_literal" => "&str".into(),
            "reference_expression" => child(node, "value")
                .or_else(|| named_children(node).last().copied())
                .map(|n| {
                    let inner = self.infer_type(n, env);
                    if inner.is_empty() {
                        inner
                    } else if self.text(node).starts_with("&mut") {
                        format!("&mut {inner}")
                    } else {
                        format!("&{inner}")
                    }
                })
                .unwrap_or_default(),
            "parenthesized_expression" => child(node, "value")
                .or_else(|| named_children(node).first().copied())
                .map(|n| self.infer_type(n, env))
                .unwrap_or_default(),
            "field_expression" => self.field_type(node, env).unwrap_or_default(),
            "array_expression" => {
                let values = named_children(node);
                let length = child(node, "length");
                let values = values
                    .into_iter()
                    .filter(|n| Some(*n) != length && n.kind() != "attribute_item")
                    .collect::<Vec<_>>();
                let Some(first) = values.first() else {
                    return "[();0]".into();
                };
                let ty = self.infer_type(*first, env);
                if !scalar(&ty) || values.iter().any(|n| self.infer_type(*n, env) != ty) {
                    return String::new();
                }
                let length = match length {
                    Some(length) => match literal(length, self.source) {
                        Some(length) => length,
                        None => return String::new(),
                    },
                    None => values.len() as u64,
                };
                format!("[{ty};{length}]")
            }
            "type_cast_expression" => child(node, "type")
                .map(|n| self.text(n).into())
                .unwrap_or_default(),
            "unary_expression" => named_children(node)
                .first()
                .map(|n| {
                    let ty = self.infer_type(*n, env);
                    if self.text(node).starts_with('*') {
                        referent_type(&ty).unwrap_or("").to_string()
                    } else {
                        ty
                    }
                })
                .unwrap_or_default(),
            "binary_expression" => child(node, "left")
                .or_else(|| named_children(node).first().copied())
                .map(|n| self.infer_type(n, env))
                .unwrap_or_default(),
            "index_expression" => {
                let nodes = named_children(node);
                if nodes.get(1).is_some_and(|n| n.kind() == "range_expression") {
                    return nodes
                        .first()
                        .and_then(|n| element(&self.infer_type(*n, env)))
                        .map(|e| format!("&[{e}]"))
                        .unwrap_or_default();
                }
                nodes
                    .first()
                    .and_then(|n| element(&self.infer_type(*n, env)))
                    .unwrap_or_default()
            }
            "call_expression" => {
                if let Some((receiver, method)) = self.method(node) {
                    if method == "len" && collection(&self.infer_type(receiver, env)) {
                        return "usize".into();
                    }
                    if method == "contains" && element(&self.infer_type(receiver, env)).is_some() {
                        return "bool".into();
                    }
                    if matches!(
                        method.as_str(),
                        "wrapping_add"
                            | "wrapping_sub"
                            | "wrapping_mul"
                            | "saturating_add"
                            | "saturating_sub"
                            | "saturating_mul"
                            | "abs"
                    ) {
                        return self.infer_type(receiver, env);
                    }
                }
                String::new()
            }
            _ => String::new(),
        }
    }
    fn sizes(&self, node: Node<'_>, env: &Environment) -> ArgumentSize {
        let unknown = || {
            ArgumentSize::unknown(&format!(
                "size expression is not supported: {}",
                self.text(node).chars().take(120).collect::<String>()
            ))
        };
        match node.kind() {
            "identifier" | "self" => env
                .get(self.text(node))
                .map(|b| b.sizes.clone())
                .unwrap_or_else(unknown),
            "integer_literal" => ArgumentSize {
                status: EstimateStatus::Tight,
                value: literal(node, self.source)
                    .map(SizeExpr::Constant)
                    .unwrap_or_else(|| {
                        SizeExpr::Unknown("integer literal exceeds supported magnitude".into())
                    }),
                length: SizeExpr::Unknown("numeric value has no collection length".into()),
            },
            "array_expression" => fixed_array_length(&self.infer_type(node, env))
                .map(|length| ArgumentSize {
                    status: EstimateStatus::Tight,
                    value: SizeExpr::Unknown("array value is not a scalar".into()),
                    length,
                })
                .unwrap_or_else(unknown),
            "reference_expression" | "parenthesized_expression" => child(node, "value")
                .or_else(|| named_children(node).last().copied())
                .map(|n| self.sizes(n, env))
                .unwrap_or_else(unknown),
            "unary_expression" if self.text(node).starts_with('*') => named_children(node)
                .first()
                .filter(|n| referent_type(&self.infer_type(**n, env)).is_some())
                .map(|n| self.sizes(*n, env))
                .unwrap_or_else(unknown),
            "call_expression" => {
                if let Some((receiver, method)) = self.method(node) {
                    if method == "len" && collection(&self.infer_type(receiver, env)) {
                        let sizes = self.sizes(receiver, env);
                        return ArgumentSize {
                            status: sizes.status,
                            value: sizes.length,
                            length: SizeExpr::Unknown("length result is scalar".into()),
                        };
                    }
                }
                unknown()
            }
            "binary_expression" => {
                let Some(left) = child(node, "left") else {
                    return unknown();
                };
                let Some(right) = child(node, "right") else {
                    return unknown();
                };
                let left_size = self.sizes(left, env);
                let right_size = self.sizes(right, env);
                let status = left_size.status.max(right_size.status);
                let a = left_size.value;
                let b = right_size.value;
                let op = child(node, "operator").map(|n| self.text(n)).unwrap_or("");
                let value = match op {
                    "+" => SizeExpr::Sum(vec![a, b]),
                    "*" => SizeExpr::Product(vec![a, b]),
                    _ => return unknown(),
                };
                ArgumentSize {
                    status,
                    value,
                    length: SizeExpr::Unknown("arithmetic expression is scalar".into()),
                }
            }
            "index_expression" => {
                let children = named_children(node);
                if let [value, index] = children.as_slice() {
                    if index.kind() == "range_expression" && self.text(*index) == ".." {
                        return self.sizes(*value, env);
                    }
                    if index.kind() == "range_expression" {
                        let raw = self.text(*index);
                        let bounds = named_children(*index);
                        let end = if raw.ends_with("..") {
                            None
                        } else {
                            bounds.last().copied()
                        };
                        let mut length = end
                            .map(|n| self.sizes(n, env).value)
                            .unwrap_or_else(|| self.sizes(*value, env).length);
                        if raw.contains("..=") {
                            length = SizeExpr::Sum(vec![length, SizeExpr::Constant(1)]);
                        }
                        return ArgumentSize {
                            status: EstimateStatus::UpperBound,
                            value: SizeExpr::Unknown("slice is not a scalar value".into()),
                            length,
                        };
                    }
                }
                unknown()
            }
            _ => unknown(),
        }
    }
    fn method<'a>(&self, node: Node<'a>) -> Option<(Node<'a>, String)> {
        let function = child(node, "function")?;
        let function = if function.kind() == "generic_function" {
            child(function, "function")?
        } else {
            function
        };
        if function.kind() != "field_expression" {
            return None;
        }
        Some((
            child(function, "value")?,
            child(function, "field").map(|n| self.text(n).to_string())?,
        ))
    }
    fn field_type(&self, node: Node<'_>, env: &Environment) -> Option<String> {
        let receiver = child(node, "value")?;
        let ty = self.infer_type(receiver, env);
        // A declared field uses built-in projection. Looking through an unknown
        // smart pointer would instead invoke a potentially arbitrary Deref impl.
        let fields = self.fields.get(clean_type(&ty).as_ref())?;
        let ty = fields.get(self.text(child(node, "field")?))?;
        if self.shadows.contains("*")
            || clean_type(ty)
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .any(|token| self.shadows.contains(token))
        {
            return None;
        }
        Some(ty.clone())
    }
    fn builtin(&self, node: Node<'_>, env: &Environment) -> Option<LibraryCost> {
        let (receiver, method) = self.method(node)?;
        let ty = self.infer_type(receiver, env);
        let constant = |description: &str| LibraryCost {
            expression: SizeExpr::Constant(1),
            status: EstimateStatus::Tight,
            description: description.into(),
        };
        if method == "len" && collection(&ty) {
            return Some(constant("Built-in collection length is constant time."));
        }
        if matches!(method.as_str(), "iter" | "iter_mut") && element(&ty).is_some() {
            return Some(constant("Creating a slice iterator is constant time; consumption is charged by the enclosing loop."));
        }
        if scalar(&ty)
            && matches!(
                method.as_str(),
                "wrapping_add"
                    | "wrapping_sub"
                    | "wrapping_mul"
                    | "saturating_add"
                    | "saturating_sub"
                    | "saturating_mul"
                    | "abs"
            )
        {
            return Some(constant(
                "Built-in fixed-width scalar method has constant unit cost.",
            ));
        }
        let elem = element(&ty)?;
        if !scalar(&elem) {
            return None;
        }
        let n = self.sizes(receiver, env).length;
        let (expression,description)=match method.as_str() {
            "contains"=>(n,"Slice membership performs at most n built-in scalar comparisons."),
            "binary_search"=>(SizeExpr::Log(Box::new(n)),"Slice binary search performs O(log(n)) built-in scalar comparisons."),
            "sort"|"sort_unstable"=>(SizeExpr::Product(vec![n.clone(),SizeExpr::Log(Box::new(n))]),"Standard slice sorting has O(n*log(n)) worst-case comparisons; built-in scalar comparisons have constant cost."),
            _=>return None
        };
        Some(LibraryCost {
            expression,
            status: EstimateStatus::UpperBound,
            description: description.into(),
        })
    }
    fn call(&self, node: Node<'_>, env: &mut Environment, in_loop: bool, depth: usize) -> CostNode {
        let args = child(node, "arguments")
            .map(named_children)
            .unwrap_or_default();
        let mut arguments = Vec::new();
        let receiver = self
            .method(node)
            .map(|(receiver, _)| self.sizes(receiver, env));
        let builtin = self.builtin(node, env);
        let mut items = Vec::new();
        if let Some((receiver, _)) = self.method(node) {
            items.push(self.node(receiver, env, in_loop, depth + 1));
        } else if let Some(function) = child(node, "function") {
            if !matches!(
                function.kind(),
                "identifier" | "scoped_identifier" | "generic_function"
            ) {
                items.push(self.node(function, env, in_loop, depth + 1));
            }
        }
        for arg in args {
            arguments.push(self.sizes(arg, env));
            items.push(self.node(arg, env, in_loop, depth + 1));
        }
        items.push(self.make(
            node,
            CostKind::Call(Box::new(CallCost {
                arguments,
                receiver,
                builtin,
            })),
        ));
        self.sequence(node, items)
    }
    fn node(&self, node: Node<'_>, env: &mut Environment, in_loop: bool, depth: usize) -> CostNode {
        nodes::NodeExtractor { extractor: self }.node(node, env, in_loop, depth)
    }
    fn children(
        &self,
        node: Node<'_>,
        env: &mut Environment,
        in_loop: bool,
        depth: usize,
    ) -> CostNode {
        self.sequence(
            node,
            named_children(node)
                .into_iter()
                .map(|n| self.node(n, env, in_loop, depth + 1))
                .collect(),
        )
    }
    fn for_loop(&self, node: Node<'_>, env: &mut Environment, depth: usize) -> CostNode {
        let Some(original_value) = child(node, "value") else {
            return self.unknown(node, "missing iterator");
        };
        let value = if let Some((receiver, method)) = self.method(original_value) {
            if method == "step_by" {
                let step = child(original_value, "arguments")
                    .map(named_children)
                    .and_then(|n| n.first().copied());
                if !step
                    .and_then(|n| literal(n, self.source))
                    .is_some_and(|n| n > 0)
                {
                    return self
                        .unknown(original_value, "step_by requires a positive constant step");
                }
                receiver
            } else {
                original_value
            }
        } else {
            original_value
        };
        let mut value = value;
        while value.kind() == "parenthesized_expression" {
            value = named_children(value).first().copied().unwrap_or(value);
        }
        let Some(body) = child(node, "body") else {
            return self.unknown(node, "missing loop body");
        };
        let Some(pattern) = child(node, "pattern") else {
            return self.unknown(node, "missing loop binding");
        };
        let mut local = env.clone();
        let mut status = EstimateStatus::Tight;
        let mut assumptions = Vec::new();
        let mut induction = None;
        let mut count = SizeExpr::Unknown("iterator cardinality is not supported".into());
        let mut item_ty = String::new();
        let setup;
        if value.kind() == "range_expression" {
            let raw = self.text(value);
            let children = named_children(value);
            let start = children.first().copied();
            let end = children.last().copied();
            if children.len() == 2 {
                let start = start.unwrap();
                let end = end.unwrap();
                item_ty = self.infer_type(end, env);
                if let (Some(a), Some(b)) = (literal(start, self.source), literal(end, self.source))
                {
                    count = SizeExpr::Constant(if b < a {
                        0
                    } else {
                        (b - a).saturating_add(u64::from(raw.contains("..=")))
                    });
                } else if self.text(start) == self.text(end) && !raw.contains("..=") {
                    count = SizeExpr::Constant(0);
                } else if integer(&item_ty) {
                    let end_size = self.sizes(end, env);
                    count = end_size.value;
                    status = status.max(end_size.status);
                    if literal(start, self.source).is_none() {
                        if unsigned(&self.infer_type(start, env)) && unsigned(&item_ty) {
                            status = status.max(EstimateStatus::UpperBound);
                        } else {
                            count = SizeExpr::Unknown(
                                "signed or unmodeled range starting bound is not supported".into(),
                            );
                        }
                    }
                }
                let name = format!("@iteration:{}", node.start_byte());
                induction = Some(name.clone());
                if pattern.kind() == "identifier" {
                    local.insert(
                        self.text(pattern).into(),
                        Arc::new(Binding {
                            ty: item_ty.clone(),
                            sizes: ArgumentSize {
                                status: EstimateStatus::Tight,
                                value: SizeExpr::value(name),
                                length: SizeExpr::Unknown(
                                    "induction variable has no length".into(),
                                ),
                            },
                            initial: None,
                        }),
                    );
                }
            }
            setup = self.children(value, env, false, depth + 1);
        } else {
            let (receiver, known) = if let Some((receiver, method)) = self.method(value) {
                (
                    receiver,
                    matches!(method.as_str(), "iter" | "iter_mut")
                        && element(&self.infer_type(receiver, env)).is_some(),
                )
            } else {
                (value, element(&self.infer_type(value, env)).is_some())
            };
            if known {
                let size = self.sizes(receiver, env);
                count = size.length;
                status = status.max(size.status);
                let receiver_ty = self.infer_type(receiver, env);
                let element_ty = element(&receiver_ty).unwrap_or_default();
                item_ty =
                    if self.method(value).is_some() || receiver_ty.trim_start().starts_with('&') {
                        format!("&{element_ty}")
                    } else {
                        element_ty
                    };
            } else if self
                .method(value)
                .is_some_and(|(_, method)| matches!(method.as_str(), "iter" | "iter_mut"))
            {
                let name = self.text(receiver).to_string();
                if env.contains_key(&name) {
                    count = SizeExpr::length(name);
                    status = EstimateStatus::Heuristic;
                    assumptions.push("Assume this untyped iter()/iter_mut() is a finite standard collection traversal with constant-cost next().".into());
                }
            }
            setup = if known {
                self.call_or_value(value, env, depth + 1)
            } else {
                self.unit(value)
            };
        }
        if pattern.kind() != "identifier" && self.text(pattern) != "_" {
            return self.sequence(
                node,
                vec![
                    setup,
                    self.unknown(node, "loop destructuring pattern is not modeled"),
                ],
            );
        }
        let name = self.text(pattern).to_string();
        if !local.contains_key(&name) || induction.is_none() {
            local.insert(
                name,
                Arc::new(Binding {
                    ty: item_ty,
                    sizes: ArgumentSize::unknown("element sizes are not modeled"),
                    initial: None,
                }),
            );
        }
        let body_cost = self.node(body, &mut local, true, depth + 1);
        let cost = self.make(
            node,
            CostKind::Repeat(Box::new(LoopCost {
                count,
                induction,
                induction_bound: None,
                setup: Box::new(setup),
                check: Box::new(self.unit(value)),
                body: Box::new(body_cost),
                status,
                assumptions,
            })),
        );
        for (name, binding) in env.iter_mut() {
            if self.writes(body, name) > 0 {
                let binding = Arc::make_mut(binding);
                binding.sizes = ArgumentSize::unknown("size changed inside a loop");
                binding.initial = None;
            }
        }
        cost
    }
    fn call_or_value(&self, node: Node<'_>, env: &mut Environment, depth: usize) -> CostNode {
        self.node(node, env, false, depth)
    }
    fn while_loop(&self, node: Node<'_>, env: &mut Environment, depth: usize) -> CostNode {
        let Some(condition) = child(node, "condition") else {
            return self.unknown(node, "missing while condition");
        };
        let Some(body) = child(node, "body") else {
            return self.unknown(node, "missing while body");
        };
        let mut local = env.clone();
        let check = self.node(condition, &mut local, true, depth + 1);
        let progress::WhileProgress {
            count,
            status,
            assumptions,
            induction,
            induction_bound,
        } = self.while_progress(node, condition, body, env, &mut local);

        let body_cost = self.node(body, &mut local, true, depth + 1);
        let cost = self.make(
            node,
            CostKind::Repeat(Box::new(LoopCost {
                count,
                induction,
                induction_bound,
                setup: Box::new(self.unit(condition)),
                check: Box::new(check),
                body: Box::new(body_cost),
                status,
                assumptions,
            })),
        );
        for (name, binding) in env.iter_mut() {
            if self.writes(body, name) > 0 {
                let binding = Arc::make_mut(binding);
                binding.sizes = ArgumentSize::unknown("size changed inside a while loop");
                binding.initial = None;
            }
        }
        cost
    }
}
fn direct_terminator(node: Node<'_>) -> bool {
    if matches!(
        node.kind(),
        "return_expression" | "break_expression" | "continue_expression"
    ) {
        return true;
    }
    node.kind() == "expression_statement"
        && named_children(node).first().is_some_and(|n| {
            matches!(
                n.kind(),
                "return_expression" | "break_expression" | "continue_expression"
            )
        })
}
fn mutation_counts<'a>(node: Node<'_>, source: &'a str) -> HashMap<&'a str, usize> {
    let mut counts = HashMap::new();
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        let target = if matches!(
            node.kind(),
            "assignment_expression" | "compound_assignment_expr"
        ) {
            child(node, "left")
        } else if node.kind() == "reference_expression"
            && snippet(node, source).trim().starts_with("&mut")
        {
            Some(node)
        } else {
            None
        };
        if let Some(target) = target {
            let mut identifiers = HashSet::new();
            let mut pending = vec![target];
            while let Some(value) = pending.pop() {
                if matches!(value.kind(), "identifier" | "self") {
                    identifiers.insert(snippet(value, source).trim());
                }
                pending.extend(named_children(value));
            }
            for name in identifiers {
                *counts.entry(name).or_default() += 1;
            }
        }
        if !matches!(node.kind(), "closure_expression" | "function_item") {
            stack.extend(named_children(node));
        }
    }
    counts
}
fn contains_identifier(node: Node<'_>, name: &str, source: &str) -> bool {
    (matches!(node.kind(), "identifier" | "self") && snippet(node, source).trim() == name)
        || named_children(node)
            .iter()
            .any(|n| contains_identifier(*n, name, source))
}
fn top_updates(body: Node<'_>, name: &str, source: &str) -> Option<(String, u64)> {
    for node in named_children(body) {
        let node = if node.kind() == "expression_statement" {
            named_children(node).first().copied().unwrap_or(node)
        } else {
            node
        };
        if node.kind() == "compound_assignment_expr"
            && child(node, "left").is_some_and(|n| snippet(n, source).trim() == name)
        {
            return Some((
                child(node, "operator").map(|n| snippet(n, source).into())?,
                literal(child(node, "right")?, source)?,
            ));
        }
    }
    None
}
fn has_conditional_update(node: Node<'_>, name: &str, extractor: &Extractor<'_>) -> bool {
    if matches!(
        node.kind(),
        "if_expression"
            | "match_expression"
            | "for_expression"
            | "while_expression"
            | "loop_expression"
    ) && extractor.writes(node, name) > 0
    {
        return true;
    }
    named_children(node)
        .iter()
        .any(|n| has_conditional_update(*n, name, extractor))
}

fn has_indirect_write(node: Node<'_>) -> bool {
    (matches!(
        node.kind(),
        "assignment_expression" | "compound_assignment_expr"
    ) && child(node, "left").is_some_and(|n| n.kind() != "identifier"))
        || named_children(node).iter().any(|n| has_indirect_write(*n))
}

fn fixed_array_length(ty: &str) -> Option<SizeExpr> {
    let clean = clean_type(ty);
    let inner = clean.strip_prefix('[')?.strip_suffix(']')?;
    let (_, length) = inner.rsplit_once(';')?;
    Some(
        length
            .replace('_', "")
            .parse::<u64>()
            .map(SizeExpr::Constant)
            .unwrap_or_else(|_| {
                SizeExpr::Unknown("const-generic array length is not modeled".into())
            }),
    )
}

fn receiver_type(body: Node<'_>, source: &str) -> Option<String> {
    let function = body.parent()?;
    let parameters = child(function, "parameters")?;
    let receiver = named_children(parameters).into_iter().find(|n| {
        n.kind() == "self_parameter"
            || child(*n, "pattern").is_some_and(|pattern| snippet(pattern, source).trim() == "self")
    })?;
    let mut ancestor = function.parent();
    while let Some(owner) = ancestor {
        if owner.kind() == "impl_item" {
            let owner_ty = snippet(child(owner, "type")?, source);
            if let Some(ty) = child(receiver, "type") {
                let ty = snippet(ty, source);
                return Some(if clean_type(ty) == "Self" {
                    ty.replace("Self", owner_ty)
                } else {
                    ty.to_string()
                });
            }
            let receiver = snippet(receiver, source).trim();
            return Some(if receiver.starts_with('&') {
                format!("&{owner_ty}")
            } else {
                owner_ty.to_string()
            });
        }
        if owner.kind() == "function_item" {
            break;
        }
        ancestor = owner.parent();
    }
    None
}

fn has_field_expression(node: Node<'_>) -> bool {
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        if node.kind() == "field_expression" {
            return true;
        }
        if !matches!(node.kind(), "function_item" | "closure_expression") {
            pending.extend(named_children(node));
        }
    }
    false
}

fn declared_fields(body: Node<'_>, source: &str) -> BTreeMap<String, BTreeMap<String, String>> {
    // Only use direct declarations in the same lexical module. Resolving aliases,
    // foreign modules, generic substitutions and macro-generated storage needs rustc.
    let mut scope = body;
    loop {
        if scope != body && scope.kind() == "block" {
            // Nested functions can see local types that shadow module declarations.
            return BTreeMap::new();
        }
        if scope.kind() == "source_file"
            || (scope.kind() == "declaration_list"
                && scope.parent().is_some_and(|p| p.kind() == "mod_item"))
        {
            break;
        }
        let Some(parent) = scope.parent() else { break };
        scope = parent;
    }
    let mut fields = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for declaration in named_children(scope) {
        if declaration.kind() != "struct_item" {
            continue;
        }
        let Some(name) = child(declaration, "name") else {
            continue;
        };
        let name = snippet(name, source).to_string();
        if !seen.insert(name.clone()) {
            fields.remove(&name);
            continue;
        }
        let mut previous = declaration.prev_named_sibling();
        while previous.is_some_and(|n| matches!(n.kind(), "line_comment" | "block_comment")) {
            previous = previous.and_then(|n| n.prev_named_sibling());
        }
        if child(declaration, "type_parameters").is_some()
            || previous.is_some_and(|n| n.kind() == "attribute_item")
        {
            continue;
        }
        let Some(body) = child(declaration, "body") else {
            continue;
        };
        if body.kind() != "field_declaration_list" {
            continue;
        }
        let mut storage = BTreeMap::new();
        for field in named_children(body) {
            if field.kind() == "field_declaration" {
                if let (Some(name), Some(ty)) = (child(field, "name"), child(field, "type")) {
                    storage.insert(snippet(name, source).into(), snippet(ty, source).into());
                }
            }
        }
        fields.insert(name, storage);
    }
    fields
}
pub(super) fn type_shadows(root: Node<'_>, source: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "struct_item" | "enum_item" | "type_item" | "mod_item" | "trait_item"
        ) {
            if let Some(name) = child(node, "name") {
                out.insert(snippet(name, source).into());
            }
        }
        if node.kind() == "use_declaration" {
            let text = snippet(node, source)
                .trim()
                .trim_start_matches("pub ")
                .trim_start_matches("use ");
            if ![
                "std::",
                "core::",
                "alloc::",
                "::std::",
                "::core::",
                "::alloc::",
            ]
            .iter()
            .any(|p| text.starts_with(p))
            {
                if text.contains('*') {
                    out.insert("*".into());
                }
                for token in text.split(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
                    if matches!(token, "Vec" | "String") || scalar(token) {
                        out.insert(token.into());
                    }
                }
            }
        }
        stack.extend(named_children(node));
    }
    out
}
