//! Narrow Rust expression typing for collection elements. This does not model
//! arbitrary Index/IntoIterator/Deref implementations or infer by method name.
use crate::{support::*, type_index::*};
use analyzer_core::ir::*;
use std::collections::{BTreeMap, BTreeSet};
use tree_sitter::Node;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Shape {
    Named(String),
    Vec(Box<Shape>),
    Slice(Box<Shape>),
    Iter(Box<Shape>),
    Ref(Box<Shape>),
    Option(Box<Shape>),
    Result(Box<Shape>, Box<Shape>),
    Tuple(Vec<Shape>),
    Integer,
    Never,
    Unknown,
}
impl Shape {
    fn dereferenced(&self) -> &Self {
        match self {
            Self::Ref(value) => value.dereferenced(),
            value => value,
        }
    }
}
type Env = BTreeMap<String, Shape>;
type Position = (usize, usize);
#[derive(Clone)]
struct Scope {
    index: usize,
    fqn: String,
    owner: Option<String>,
    parameters: Vec<Parameter>,
    range: SourceRange,
    type_parameters: BTreeSet<String>,
}
struct Typer<'a> {
    source: &'a str,
    types: TypeIndex<'a>,
    fields: BTreeMap<(String, String), (String, SourceRange, BTreeSet<String>)>,
    returns: BTreeMap<(String, String), Option<(String, String, SourceRange, BTreeSet<String>)>>,
    imports: &'a [Import],
    scopes: BTreeMap<Position, Scope>,
    functions: &'a mut [FunctionModel],
}

pub(super) fn annotate(root: Node<'_>, source: &str, model: &mut FileModel) {
    // Expression typing only writes call-site receiver evidence. Keep the
    // declarations untouched when there are no call sites to annotate.
    if model
        .functions
        .iter()
        .all(|function| function.calls.is_empty())
    {
        return;
    }
    let mut type_parameters = BTreeMap::new();
    let mut nodes = vec![root];
    while let Some(node) = nodes.pop() {
        if is_function(node.kind(), Dialect::Rust) || is_type(node.kind(), Dialect::Rust) {
            let mut owner = Some(node);
            let mut names = BTreeSet::new();
            while let Some(node) = owner {
                if let Some(parameters) = child(node, "type_parameters") {
                    names.extend(
                        named_children(parameters)
                            .into_iter()
                            .filter(|parameter| parameter.kind() == "type_parameter")
                            .filter_map(|parameter| child(parameter, "name"))
                            .map(|name| snippet(name, source).to_owned()),
                    );
                }
                owner = node.parent();
            }
            type_parameters.insert(
                (
                    node.start_position().row + 1,
                    node.start_position().column + 1,
                ),
                names,
            );
        }
        nodes.extend(named_children(node));
    }
    let scopes = model
        .functions
        .iter()
        .enumerate()
        .map(|(index, function)| {
            (
                (function.range.start_line, function.range.start_column),
                Scope {
                    index,
                    fqn: function.fqn.clone(),
                    owner: function.parent_type.clone(),
                    parameters: function.parameters.clone(),
                    range: function.range.clone(),
                    type_parameters: type_parameters
                        .get(&(function.range.start_line, function.range.start_column))
                        .cloned()
                        .unwrap_or_default(),
                },
            )
        })
        .collect();
    let fields = model
        .types
        .iter()
        .flat_map(|ty| {
            let parameters = type_parameters
                .get(&(ty.range.start_line, ty.range.start_column))
                .cloned()
                .unwrap_or_default();
            ty.fields.iter().filter_map(move |field| {
                field.type_name.as_ref().map(|raw| {
                    (
                        (ty.fqn.clone(), field.name.clone()),
                        (raw.clone(), ty.range.clone(), parameters.clone()),
                    )
                })
            })
        })
        .collect();
    let mut returns = BTreeMap::new();
    for function in &model.functions {
        if let (Some(owner), Some(raw)) = (&function.parent_type, &function.return_type) {
            let key = (owner.clone(), function.name.clone());
            let parameters = type_parameters
                .get(&(function.range.start_line, function.range.start_column))
                .cloned()
                .unwrap_or_default();
            let value = returns.entry(key).or_insert_with(|| {
                Some((
                    raw.clone(),
                    function.fqn.clone(),
                    function.range.clone(),
                    parameters,
                ))
            });
            if value.as_ref().map(|value| &value.0) != Some(raw) {
                *value = None;
            }
        }
    }
    let mut typer = Typer {
        source,
        types: TypeIndex::new(&model.types, Dialect::Rust),
        fields,
        returns,
        imports: &model.imports,
        scopes,
        functions: &mut model.functions,
    };
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if node.kind() == "function_item" {
            typer.function(node, &Env::new(), None);
        } else {
            pending.extend(named_children(node));
        }
    }
}
impl Typer<'_> {
    fn scope(&self, node: Node<'_>) -> Option<Scope> {
        self.scopes
            .get(&(
                node.start_position().row + 1,
                node.start_position().column + 1,
            ))
            .cloned()
    }
    fn standard(&self, raw: &str, leaf: &str, qualified: &[&str], scope: &Scope) -> bool {
        if qualified.contains(&raw) {
            return true;
        }
        if raw != leaf
            || scope.type_parameters.contains(raw)
            || self.types.scope(&scope.fqn).get(raw).is_some()
        {
            return false;
        }
        let imported = self
            .imports
            .iter()
            .filter(|import| crate::support::import_applies(import, &scope.range))
            .find(|import| {
                import
                    .alias
                    .as_deref()
                    .unwrap_or_else(|| import.path.rsplit("::").next().unwrap_or(&import.path))
                    == raw
            });
        imported.is_none_or(|import| qualified.contains(&import.path.as_str()))
    }
    fn annotation(&self, raw: &str, scope: &Scope) -> Shape {
        if raw
            .chars()
            .filter(|ch| matches!(ch, '<' | '[' | '('))
            .count()
            > 64
        {
            return Shape::Unknown;
        }
        let mut raw = raw.trim().trim_start_matches(':').trim();
        if let Some(rest) = raw.strip_prefix('&') {
            let mut rest = rest.trim_start();
            if rest.starts_with('\'') {
                let Some((_, tail)) = rest.split_once(char::is_whitespace) else {
                    return Shape::Unknown;
                };
                rest = tail.trim_start();
            }
            rest = rest.strip_prefix("mut ").unwrap_or(rest).trim_start();
            return Shape::Ref(Box::new(self.annotation(rest, scope)));
        }
        raw = raw.strip_prefix("mut ").unwrap_or(raw).trim_start();
        if raw.starts_with('\'') {
            let Some((_, tail)) = raw.split_once(char::is_whitespace) else {
                return Shape::Unknown;
            };
            return self.annotation(tail, scope);
        }
        if let Some(inner) = raw.strip_prefix('[').and_then(|raw| raw.strip_suffix(']')) {
            let element = split(inner, ';').into_iter().next().unwrap_or("");
            return Shape::Slice(Box::new(self.annotation(element, scope)));
        }
        if let Some(inner) = raw.strip_prefix('(').and_then(|raw| raw.strip_suffix(')')) {
            return Shape::Tuple(
                split(inner, ',')
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .map(|part| self.annotation(part, scope))
                    .collect(),
            );
        }
        if let Some((base, inner)) = raw
            .split_once('<')
            .and_then(|(base, rest)| rest.strip_suffix('>').map(|inner| (base.trim(), inner)))
        {
            let args = split(inner, ',');
            if args.len() == 1
                && self.standard(base, "Vec", &["std::vec::Vec", "alloc::vec::Vec"], scope)
            {
                return Shape::Vec(Box::new(self.annotation(args[0], scope)));
            }
            if args.len() == 1
                && self.standard(
                    base,
                    "Option",
                    &["std::option::Option", "core::option::Option"],
                    scope,
                )
            {
                return Shape::Option(Box::new(self.annotation(args[0], scope)));
            }
            if args.len() == 2
                && self.standard(
                    base,
                    "Result",
                    &["std::result::Result", "core::result::Result"],
                    scope,
                )
            {
                return Shape::Result(
                    Box::new(self.annotation(args[0], scope)),
                    Box::new(self.annotation(args[1], scope)),
                );
            }
        }
        if raw == "Self" {
            return scope
                .owner
                .clone()
                .map(Shape::Named)
                .unwrap_or(Shape::Unknown);
        }
        let base = raw.split_once('<').map_or(raw, |(base, _)| base).trim();
        if scope.type_parameters.contains(base)
            || scope
                .type_parameters
                .iter()
                .any(|parameter| raw.starts_with(&format!("{parameter}::")))
        {
            return Shape::Unknown;
        }
        crate::types::reference("rust", raw)
            .map(|raw| {
                if let Some(ty) = self.types.scope(&scope.fqn).get(&raw) {
                    return Shape::Named(ty.clone());
                }
                let mut context = scope.fqn.as_str();
                while let Some((parent, _)) = context.rsplit_once("::") {
                    if let Some(ty) = self.types.qualified(format!("{parent}::{raw}").as_str()) {
                        return Shape::Named((*ty).clone());
                    }
                    context = parent;
                }
                Shape::Named(raw)
            })
            .unwrap_or(Shape::Unknown)
    }
    fn function(&mut self, node: Node<'_>, captured: &Env, item: Option<Shape>) {
        let Some(scope) = self.scope(node) else {
            return;
        };
        let closure = node.kind() == "closure_expression";
        let mut env = if closure {
            captured.clone()
        } else {
            Env::new()
        };
        for parameter in &scope.parameters {
            env.insert(
                parameter.name.clone(),
                parameter
                    .type_name
                    .as_ref()
                    .map(|raw| self.annotation(raw, &scope))
                    .unwrap_or(Shape::Unknown),
            );
        }
        if let Some(owner) = &scope.owner {
            if !closure
                && scope
                    .parameters
                    .iter()
                    .any(|parameter| parameter.implicit_receiver)
            {
                env.insert("self".into(), Shape::Named(owner.clone()));
            }
        }
        if closure {
            if let Some(parameters) = child(node, "parameters") {
                let parameters = named_children(parameters);
                for parameter in &parameters {
                    let pattern = child(*parameter, "pattern").unwrap_or(*parameter);
                    let ty = child(*parameter, "type")
                        .map(|raw| self.annotation(snippet(raw, self.source), &scope))
                        .or_else(|| (parameters.len() == 1).then(|| item.clone()).flatten())
                        .unwrap_or(Shape::Unknown);
                    bind(pattern, ty, &mut env, self.source);
                }
            }
        }
        if let Some(body) = child(node, "body") {
            self.walk(body, &mut env, &scope);
        }
    }
    fn closure_item(&self, node: Node<'_>, env: &Env, scope: &Scope) -> Option<Shape> {
        let args = node
            .parent()
            .filter(|parent| parent.kind() == "arguments")?;
        let call = args
            .parent()
            .filter(|parent| parent.kind() == "call_expression")?;
        let target = call_target(call)?;
        if target.kind() != "field_expression" {
            return None;
        }
        let method = snippet(child(target, "field")?, self.source);
        if ![
            "map",
            "filter_map",
            "filter",
            "find",
            "find_map",
            "any",
            "all",
            "for_each",
            "inspect",
            "position",
        ]
        .contains(&method)
        {
            return None;
        }
        let receiver = self.expression(child(target, "value")?, env, scope);
        let Shape::Iter(item) = receiver.dereferenced() else {
            return None;
        };
        Some((**item).clone())
    }
    fn walk(&mut self, node: Node<'_>, env: &mut Env, scope: &Scope) {
        if node.kind() == "function_item" {
            self.function(node, &Env::new(), None);
            return;
        }
        if node.kind() == "closure_expression" {
            let item = self.closure_item(node, env, scope);
            self.function(node, env, item);
            return;
        }
        if is_type(node.kind(), Dialect::Rust) {
            return;
        }
        if node.kind() == "call_expression" {
            if let Some(target) =
                call_target(node).filter(|target| target.kind() == "field_expression")
            {
                if let Some(receiver) = child(target, "value") {
                    if let Shape::Named(ty) = self.expression(receiver, env, scope).dereferenced() {
                        let position = node.start_position();
                        if let Some(call) =
                            self.functions[scope.index].calls.iter_mut().find(|call| {
                                call.range.start_line == position.row + 1
                                    && call.range.start_column == position.column + 1
                            })
                        {
                            // The lexical binder already follows assignments and
                            // block restoration. Element typing supplements
                            // missing evidence without replacing that proof.
                            if call.receiver_type.is_none() {
                                call.receiver_type = Some(ty.clone());
                                call.receiver_root_type = None;
                                call.binding_unknown = false;
                                call.local_binding = true;
                                call.targets.clear();
                                call.confidence = Confidence::Dynamic;
                            }
                        }
                    }
                }
            }
        }
        if node.kind() == "let_declaration" {
            if let Some(value) = child(node, "value") {
                self.walk(value, env, scope);
            }
            if let Some(pattern) = child(node, "pattern") {
                let ty = child(node, "type")
                    .map(|raw| self.annotation(snippet(raw, self.source), scope))
                    .or_else(|| {
                        child(node, "value").map(|value| self.expression(value, env, scope))
                    })
                    .unwrap_or(Shape::Unknown);
                bind(pattern, ty, env, self.source);
            }
            return;
        }
        if node.kind() == "match_expression" {
            let shape = child(node, "value")
                .map(|value| self.expression(value, env, scope))
                .unwrap_or(Shape::Unknown);
            if let Some(value) = child(node, "value") {
                self.walk(value, env, scope);
            }
            if let Some(body) = child(node, "body") {
                for arm in named_children(body)
                    .into_iter()
                    .filter(|arm| arm.kind() == "match_arm")
                {
                    let mut local = env.clone();
                    if let Some(pattern) = child(arm, "pattern") {
                        bind(pattern, shape.clone(), &mut local, self.source);
                    }
                    if let Some(value) = child(arm, "value") {
                        self.walk(value, &mut local, scope);
                    }
                }
            }
            return;
        }
        if node.kind() == "for_expression" {
            let shape = child(node, "value")
                .map(|value| self.expression(value, env, scope))
                .unwrap_or(Shape::Unknown);
            let item = match shape.dereferenced() {
                Shape::Vec(item) | Shape::Slice(item) | Shape::Iter(item) => (**item).clone(),
                _ => Shape::Unknown,
            };
            let mut local = env.clone();
            if let Some(pattern) = child(node, "pattern") {
                bind(pattern, item, &mut local, self.source);
            }
            if let Some(value) = child(node, "value") {
                self.walk(value, env, scope);
            }
            if let Some(body) = child(node, "body") {
                self.walk(body, &mut local, scope);
            }
            return;
        }
        if node.kind() == "block" {
            let mut local = env.clone();
            for child in named_children(node) {
                self.walk(child, &mut local, scope);
            }
        } else {
            for child in named_children(node) {
                self.walk(child, env, scope);
            }
        }
    }
    fn expression(&self, node: Node<'_>, env: &Env, scope: &Scope) -> Shape {
        match node.kind() {
            "identifier" | "self" => env
                .get(snippet(node, self.source))
                .cloned()
                .unwrap_or(Shape::Unknown),
            "integer_literal" => Shape::Integer,
            "reference_expression" => child(node, "value")
                .map(|value| Shape::Ref(Box::new(self.expression(value, env, scope))))
                .unwrap_or(Shape::Unknown),
            "parenthesized_expression" => named_children(node)
                .first()
                .map(|value| self.expression(*value, env, scope))
                .unwrap_or(Shape::Unknown),
            "unary_expression" => {
                let value = named_children(node)
                    .first()
                    .map(|value| self.expression(*value, env, scope))
                    .unwrap_or(Shape::Unknown);
                if snippet(node, self.source).trim_start().starts_with('*') {
                    if let Shape::Ref(value) = value {
                        *value
                    } else {
                        Shape::Unknown
                    }
                } else {
                    Shape::Unknown
                }
            }
            "type_cast_expression" => child(node, "type")
                .map(|raw| self.annotation(snippet(raw, self.source), scope))
                .unwrap_or(Shape::Unknown),
            "tuple_expression" => Shape::Tuple(
                named_children(node)
                    .into_iter()
                    .map(|value| self.expression(value, env, scope))
                    .collect(),
            ),
            "index_expression" => {
                let children = named_children(node);
                let [collection, index] = children.as_slice() else {
                    return Shape::Unknown;
                };
                let collection = self.expression(*collection, env, scope);
                let (Shape::Vec(item) | Shape::Slice(item)) = collection.dereferenced() else {
                    return Shape::Unknown;
                };
                let index_type = self.expression(*index, env, scope);
                if index.kind() == "range_expression" {
                    Shape::Slice(item.clone())
                } else if index_type == Shape::Integer || index_type == Shape::Named("usize".into())
                {
                    (**item).clone()
                } else {
                    Shape::Unknown
                }
            }
            "field_expression" => {
                let Some(value) = child(node, "value") else {
                    return Shape::Unknown;
                };
                let Some(field) = child(node, "field") else {
                    return Shape::Unknown;
                };
                match self.expression(value, env, scope).dereferenced() {
                    Shape::Named(owner) => self
                        .fields
                        .get(&(owner.clone(), snippet(field, self.source).into()))
                        .map(|(raw, range, type_parameters)| {
                            let declaration = Scope {
                                fqn: owner.clone(),
                                owner: Some(owner.clone()),
                                range: range.clone(),
                                type_parameters: type_parameters.clone(),
                                ..scope.clone()
                            };
                            self.annotation(raw, &declaration)
                        })
                        .unwrap_or(Shape::Unknown),
                    Shape::Tuple(items) => snippet(field, self.source)
                        .parse::<usize>()
                        .ok()
                        .and_then(|index| items.get(index))
                        .cloned()
                        .unwrap_or(Shape::Unknown),
                    _ => Shape::Unknown,
                }
            }
            "call_expression" => self.call(node, env, scope),
            "match_expression" => {
                let value = child(node, "value")
                    .map(|value| self.expression(value, env, scope))
                    .unwrap_or(Shape::Unknown);
                let Some(body) = child(node, "body") else {
                    return Shape::Unknown;
                };
                let mut result = Shape::Never;
                for arm in named_children(body)
                    .into_iter()
                    .filter(|arm| arm.kind() == "match_arm")
                {
                    let mut local = env.clone();
                    if let Some(pattern) = child(arm, "pattern") {
                        bind(pattern, value.clone(), &mut local, self.source);
                    }
                    let shape = child(arm, "value")
                        .map(|value| self.expression(value, &local, scope))
                        .unwrap_or(Shape::Unknown);
                    if shape == Shape::Never {
                        continue;
                    }
                    if result != Shape::Never && result != shape {
                        return Shape::Unknown;
                    }
                    result = shape;
                }
                result
            }
            "return_expression" | "continue_expression" | "break_expression" => Shape::Never,
            "binary_expression" => {
                let values = ["left", "right"].map(|field| {
                    child(node, field)
                        .map(|value| self.expression(value, env, scope))
                        .unwrap_or(Shape::Unknown)
                });
                let op = child(node, "operator")
                    .map(|op| snippet(op, self.source))
                    .unwrap_or("");
                if ["+", "-", "*", "/", "%", "|", "&", "^"].contains(&op)
                    && values.iter().all(|shape| {
                        matches!(shape, Shape::Integer) || *shape == Shape::Named("usize".into())
                    })
                {
                    if values.contains(&Shape::Named("usize".into())) {
                        Shape::Named("usize".into())
                    } else {
                        Shape::Integer
                    }
                } else {
                    Shape::Unknown
                }
            }
            _ => Shape::Unknown,
        }
    }
    fn call(&self, node: Node<'_>, env: &Env, scope: &Scope) -> Shape {
        let Some(target) = call_target(node) else {
            return Shape::Unknown;
        };
        if target.kind() != "field_expression" {
            return Shape::Unknown;
        }
        let Some(receiver) = child(target, "value") else {
            return Shape::Unknown;
        };
        let method = child(target, "field")
            .map(|method| snippet(method, self.source))
            .unwrap_or("");
        let receiver = self.expression(receiver, env, scope);
        match (receiver.dereferenced(), method) {
            (Shape::Vec(item) | Shape::Slice(item), "iter" | "iter_mut" | "into_iter") => {
                Shape::Iter(item.clone())
            }
            (Shape::Vec(_) | Shape::Slice(_), "len") => Shape::Named("usize".into()),
            (Shape::Vec(item), "pop") => Shape::Option(item.clone()),
            (Shape::Option(item), "unwrap" | "expect")
            | (Shape::Result(item, _), "unwrap" | "expect") => (**item).clone(),
            (
                Shape::Iter(item),
                "filter" | "inspect" | "rev" | "take" | "skip" | "cloned" | "copied",
            ) => Shape::Iter(item.clone()),
            (Shape::Named(owner), _) => self
                .returns
                .get(&(owner.clone(), method.to_owned()))
                .and_then(Option::as_ref)
                .map(|(raw, fqn, range, type_parameters)| {
                    let declaration = Scope {
                        fqn: fqn.clone(),
                        owner: Some(owner.clone()),
                        range: range.clone(),
                        type_parameters: type_parameters.clone(),
                        ..scope.clone()
                    };
                    self.annotation(raw, &declaration)
                })
                .unwrap_or(Shape::Unknown),
            _ => Shape::Unknown,
        }
    }
}
fn call_target(node: Node<'_>) -> Option<Node<'_>> {
    let target = child(node, "function")?;
    if target.kind() == "generic_function" {
        child(target, "function")
    } else {
        Some(target)
    }
}
fn bind(pattern: Node<'_>, shape: Shape, env: &mut Env, source: &str) {
    match pattern.kind() {
        "identifier" => {
            env.insert(snippet(pattern, source).into(), shape);
        }
        "match_pattern" | "mut_pattern" | "reference_pattern" => {
            for part in named_children(pattern)
                .into_iter()
                .filter(|part| part.kind() != "mutable_specifier")
            {
                bind(part, shape.clone(), env, source);
            }
        }
        "tuple_pattern" => {
            let parts = named_children(pattern);
            let shapes = match shape.dereferenced() {
                Shape::Tuple(items) if items.len() == parts.len() => Some(items),
                _ => None,
            };
            for (index, part) in parts.into_iter().enumerate() {
                bind(
                    part,
                    shapes
                        .and_then(|items| items.get(index))
                        .cloned()
                        .unwrap_or(Shape::Unknown),
                    env,
                    source,
                );
            }
        }
        "tuple_struct_pattern" => {
            let variant = child(pattern, "type");
            let inner = match (
                variant.map(|node| snippet(node, source)),
                shape.dereferenced(),
            ) {
                (Some("Ok"), Shape::Result(ok, _)) | (Some("Some"), Shape::Option(ok)) => {
                    (**ok).clone()
                }
                (Some("Err"), Shape::Result(_, err)) => (**err).clone(),
                _ => Shape::Unknown,
            };
            for part in named_children(pattern)
                .into_iter()
                .filter(|part| variant.is_none_or(|variant| part.id() != variant.id()))
            {
                bind(part, inner.clone(), env, source);
            }
        }
        _ => {}
    }
}
fn split(raw: &str, separator: char) -> Vec<&str> {
    let mut depth = 0isize;
    let mut start = 0;
    let mut parts = Vec::new();
    for (index, ch) in raw.char_indices() {
        match ch {
            '<' | '[' | '(' => depth += 1,
            '>' | ']' | ')' => depth -= 1,
            _ => {}
        }
        if ch == separator && depth == 0 {
            parts.push(raw[start..index].trim());
            start = index + ch.len_utf8();
        }
    }
    parts.push(raw[start..].trim());
    parts
}
