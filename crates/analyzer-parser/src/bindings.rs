//! Lexical, source-ordered bindings. Never infer a receiver from a later assignment.
use crate::support::import_applies as applies;
use crate::{php_names, properties, support::*, syntax::*, type_index::*};
use analyzer_core::ir::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
use tree_sitter::Node;
#[derive(Clone, Debug, PartialEq, Eq)]
enum Binding {
    Type(String),
    Object(String, String),
    Types(BTreeSet<String>),
    Import(String),
    ErasedImport,
    Function(String),
    Pending,
    Unknown,
}
fn merge_types(values: impl IntoIterator<Item = Binding>) -> Binding {
    let values = values.into_iter().collect::<Vec<_>>();
    if values.is_empty() {
        return Binding::Unknown;
    }
    if values.iter().all(|value| value == &values[0]) {
        return values[0].clone();
    }
    let mut types = BTreeSet::new();
    for value in values {
        match value {
            Binding::Type(ty) => {
                types.insert(ty);
            }
            Binding::Object(ty, _) => {
                types.insert(ty);
            }
            Binding::Types(known) => types.extend(known),
            _ => return Binding::Unknown,
        }
    }
    match types.len() {
        1 => Binding::Type(types.into_iter().next().unwrap()),
        2..=4 => Binding::Types(types),
        _ => Binding::Unknown,
    }
}

pub(super) struct FunctionScope {
    pub fqn: String,
    pub parent_type: Option<String>,
    pub parent_function: Option<String>,
    pub parameters: Vec<Parameter>,
    pub kind: String,
    pub declaration_scope: SourceRange,
}
// Preserve the replacement order, borrowing ordinary receiver names.
fn receiver_path(raw: &str) -> std::borrow::Cow<'_, str> {
    let mut path = std::borrow::Cow::Borrowed(raw);
    for separator in ["?->", "->", "?.", "::"] {
        if path.contains(separator) {
            path = std::borrow::Cow::Owned(path.replace(separator, "."));
        }
    }
    path
}
fn typed_in_scope(
    raw: &str,
    dialect: Dialect,
    owner: &str,
    imports: &[Import],
    range: &SourceRange,
) -> Binding {
    if dialect == Dialect::Php {
        return php_names::reference(raw, owner, imports, range)
            .map(Binding::Type)
            .unwrap_or(Binding::Unknown);
    }
    typed(raw, dialect)
}
fn typed(raw: &str, dialect: Dialect) -> Binding {
    crate::types::reference(language_name(dialect), raw)
        .map(Binding::Type)
        .unwrap_or(Binding::Unknown)
}

fn import_name(i: &Import) -> &str {
    i.alias.as_deref().unwrap_or_else(|| {
        i.path
            .rsplit(['.', ':', '/', '\\'])
            .find(|s| !s.is_empty())
            .unwrap_or(&i.path)
    })
}
fn import_binding(import: &Import) -> Binding {
    if import.type_only {
        Binding::ErasedImport
    } else {
        Binding::Import(import.binding_path.as_ref().unwrap_or(&import.path).clone())
    }
}
fn name(node: Node<'_>, source: &str) -> Option<String> {
    ["identifier", "variable_name", "self", "self_parameter"]
        .contains(&node.kind())
        .then(|| {
            snippet(node, source)
                .trim()
                .trim_start_matches('$')
                .to_owned()
        })
}
fn pattern_names(node: Node<'_>, source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        if let Some(n) = name(node, source) {
            names.push(n);
        } else if [
            "shorthand_property_identifier_pattern",
            "shorthand_property_identifier",
        ]
        .contains(&node.kind())
        {
            names.push(snippet(node, source).to_owned());
        } else if [
            "pair_pattern",
            "assignment_pattern",
            "object_assignment_pattern",
        ]
        .contains(&node.kind())
        {
            pending.extend(child(node, "value").or_else(|| child(node, "left")));
        } else if node.kind().contains("pattern")
            || [
                "pattern_list",
                "list_literal",
                "tuple",
                "list",
                "by_ref",
                "pair",
            ]
            .contains(&node.kind())
        {
            pending.extend(named_children(node).into_iter().rev());
        }
    }
    names
}
fn assignment(node: Node<'_>) -> Option<(Node<'_>, Option<Node<'_>>)> {
    if ![
        "assignment",
        "assignment_expression",
        "variable_declarator",
        "let_declaration",
    ]
    .contains(&node.kind())
    {
        return None;
    }
    let left = child(node, "left")
        .or_else(|| child(node, "name"))
        .or_else(|| child(node, "pattern"))?;
    Some((left, child(node, "right").or_else(|| child(node, "value"))))
}
fn exception(node: Node<'_>, source: &str, dialect: Dialect) -> Option<(String, Binding)> {
    if dialect == Dialect::Php && node.kind() == "catch_clause" {
        let n = name(child(node, "name")?, source)?;
        let ty = child(node, "type").and_then(|n| {
            let children = named_children(n);
            (children.len() == 1).then(|| snippet(children[0], source).trim().to_owned())
        });
        return Some((n, ty.map(Binding::Type).unwrap_or(Binding::Unknown)));
    }
    if matches!(dialect, Dialect::JavaScript | Dialect::TypeScript) && node.kind() == "catch_clause"
    {
        return Some((name(child(node, "parameter")?, source)?, Binding::Unknown));
    }
    if dialect == Dialect::Python && node.kind() == "except_clause" {
        let value = child(node, "value")?;
        let alias = child(node, "alias").or_else(|| child(value, "alias"));
        let alias = alias.map(|n| snippet(n, source).to_owned())?;
        let ty = if value.kind() == "as_pattern" {
            named_children(value)
                .into_iter()
                .find(|n| Some(n.id()) != child(value, "alias").map(|n| n.id()))
        } else {
            Some(value)
        };
        let ty = ty
            .filter(|n| ["identifier", "attribute"].contains(&n.kind()))
            .map(|n| snippet(n, source).to_owned());
        return Some((alias, ty.map(Binding::Type).unwrap_or(Binding::Unknown)));
    }
    None
}
type BindingEnv = BTreeMap<String, Binding>;
enum BindingWork<'tree> {
    Visit(Node<'tree>, usize, bool),
    Finish(Node<'tree>, usize, Vec<(String, Option<Binding>)>),
    Function(Node<'tree>, usize),
    ClassBody(Node<'tree>, usize),
    Discard(usize, Option<(usize, String)>),
    Loop(usize, usize, Vec<String>, bool),
    Branches(Node<'tree>, usize),
    Merge(usize, usize, usize),
    LogicalRight(Node<'tree>, usize),
    LogicalDone(Node<'tree>, usize, usize),
    Control(usize, Vec<usize>),
}
struct BindingTraversal<'tree> {
    environments: Vec<Option<BindingEnv>>,
    pending: Vec<BindingWork<'tree>>,
}
struct Walker<'a> {
    dialect: Dialect,
    source: &'a str,
    types: &'a ScopedTypes<'a, 'a>,
    imports: &'a [Import],
    nested: &'a BTreeMap<(String, String), String>,
    owner: &'a str,
    calls: BTreeMap<(usize, usize, usize, usize), Vec<usize>>,
    function: &'a mut FunctionModel,
}
impl Walker<'_> {
    fn visit(&mut self, node: Node<'_>, env: &mut BTreeMap<String, Binding>, root: bool) {
        let mut traversal = BindingTraversal {
            environments: vec![Some(std::mem::take(env))],
            pending: vec![BindingWork::Visit(node, 0, root)],
        };
        while let Some(work) = traversal.pending.pop() {
            if let Some((node, slot, root)) = self.resume_binding(work, &mut traversal) {
                self.visit_binding_node(node, slot, root, &mut traversal);
            }
        }
        *env = traversal.environments[0].take().unwrap();
    }
    fn resume_binding<'tree>(
        &mut self,
        work: BindingWork<'tree>,
        traversal: &mut BindingTraversal<'tree>,
    ) -> Option<(Node<'tree>, usize, bool)> {
        match work {
            BindingWork::Visit(node, slot, root) => Some((node, slot, root)),
            BindingWork::Finish(node, slot, before) => {
                self.resume_finish(node, slot, before, traversal);
                None
            }
            BindingWork::Function(node, slot) => {
                self.resume_function(node, slot, traversal);
                None
            }
            BindingWork::ClassBody(node, slot) => {
                self.resume_class_body(node, slot, traversal);
                None
            }
            BindingWork::Discard(local, invalidated) => {
                self.resume_discard(local, invalidated, traversal);
                None
            }
            BindingWork::Loop(slot, local, bound, scoped) => {
                self.resume_loop(slot, local, bound, scoped, traversal);
                None
            }
            BindingWork::Branches(node, slot) => {
                self.resume_branches(node, slot, traversal);
                None
            }
            BindingWork::Merge(slot, consequent, alternative) => {
                self.resume_merge(slot, consequent, alternative, traversal);
                None
            }
            BindingWork::LogicalRight(node, slot) => {
                self.resume_logical_right(node, slot, traversal);
                None
            }
            BindingWork::LogicalDone(node, slot, right) => {
                self.resume_logical_done(node, slot, right, traversal);
                None
            }
            BindingWork::Control(slot, locals) => {
                self.resume_control(slot, locals, traversal);
                None
            }
        }
    }
    fn resume_finish<'tree>(
        &mut self,
        node: Node<'tree>,
        slot: usize,
        before: Vec<(String, Option<Binding>)>,
        traversal: &mut BindingTraversal<'tree>,
    ) {
        let environments = &mut traversal.environments;

        let env = environments[slot].as_mut().unwrap();
        if let Some((left, value)) = assignment(node) {
            if let Some(n) = name(left, self.source) {
                let callable = value
                    .filter(|v| is_function(v.kind(), self.dialect))
                    .and_then(|_| self.nested.get(&(self.owner.to_owned(), n.clone())))
                    .map(|fqn| Binding::Function(fqn.clone()));
                let ty = callable
                    .or_else(|| {
                        child(node, "type").and_then(|n| {
                            let binding = typed(snippet(n, self.source), self.dialect);
                            (!(matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
                                && binding == Binding::Unknown
                                && value.is_some()))
                            .then_some(binding)
                        })
                    })
                    .or_else(|| value.map(|v| self.value(v, env)))
                    .unwrap_or(Binding::Unknown);
                env.insert(n, ty);
            }
        }
        if let Some((left, _)) = assignment(node) {
            if name(left, self.source).is_none() {
                for n in pattern_names(left, self.source) {
                    env.insert(n, Binding::Unknown);
                }
            }
        }
        for (n, value) in before {
            if let Some(value) = value {
                env.insert(n, value);
            } else {
                env.remove(&n);
            }
        }
    }
    fn resume_function<'tree>(
        &mut self,
        node: Node<'tree>,
        slot: usize,
        traversal: &mut BindingTraversal<'tree>,
    ) {
        let environments = &mut traversal.environments;

        if let Some(n) = text_field(node, "name", self.source) {
            if let Some(fqn) = self.nested.get(&(self.owner.to_owned(), n.clone())) {
                environments[slot]
                    .as_mut()
                    .unwrap()
                    .insert(n, Binding::Function(fqn.clone()));
            }
        }
    }
    fn resume_class_body<'tree>(
        &mut self,
        node: Node<'tree>,
        slot: usize,
        traversal: &mut BindingTraversal<'tree>,
    ) {
        let pending = &mut traversal.pending;
        let environments = &mut traversal.environments;

        if let Some(body) = child(node, "body") {
            let local = environments.len();
            environments.push(environments[slot].clone());
            pending.push(BindingWork::Discard(local, None));
            pending.push(BindingWork::Visit(body, local, false));
        }
    }
    fn resume_discard<'tree>(
        &mut self,
        local: usize,
        invalidated: Option<(usize, String)>,
        traversal: &mut BindingTraversal<'tree>,
    ) {
        let environments = &mut traversal.environments;

        environments[local] = None;
        if let Some((slot, n)) = invalidated {
            environments[slot]
                .as_mut()
                .unwrap()
                .insert(n, Binding::Unknown);
        }
    }
    fn resume_loop<'tree>(
        &mut self,
        slot: usize,
        local: usize,
        bound: Vec<String>,
        scoped: bool,
        traversal: &mut BindingTraversal<'tree>,
    ) {
        let environments = &mut traversal.environments;

        let local = environments[local].take().unwrap();
        let env = environments[slot].as_mut().unwrap();
        for (n, value) in local {
            if env.get(&n) != Some(&value) && !(scoped && bound.contains(&n)) {
                env.insert(n, Binding::Unknown);
            }
        }
    }
    fn resume_branches<'tree>(
        &mut self,
        node: Node<'tree>,
        slot: usize,
        traversal: &mut BindingTraversal<'tree>,
    ) {
        let pending = &mut traversal.pending;
        let environments = &mut traversal.environments;

        let consequent = environments.len();
        environments.push(environments[slot].clone());
        let alternative = environments.len();
        environments.push(environments[slot].clone());
        pending.push(BindingWork::Merge(slot, consequent, alternative));
        if let Some(branch) = child(node, "alternative") {
            pending.push(BindingWork::Visit(branch, alternative, false));
        }
        if let Some(branch) = child(node, "consequence") {
            pending.push(BindingWork::Visit(branch, consequent, false));
        }
    }
    fn resume_merge<'tree>(
        &mut self,
        slot: usize,
        consequent: usize,
        alternative: usize,
        traversal: &mut BindingTraversal<'tree>,
    ) {
        let environments = &mut traversal.environments;

        let consequent = environments[consequent].take().unwrap();
        let alternative = environments[alternative].take().unwrap();
        let env = environments[slot].as_mut().unwrap();
        for key in env.keys().cloned().collect::<Vec<_>>() {
            env.insert(
                key.clone(),
                merge_types([
                    consequent.get(&key).cloned().unwrap_or(Binding::Unknown),
                    alternative.get(&key).cloned().unwrap_or(Binding::Unknown),
                ]),
            );
        }
    }
    fn resume_logical_right<'tree>(
        &mut self,
        node: Node<'tree>,
        slot: usize,
        traversal: &mut BindingTraversal<'tree>,
    ) {
        let pending = &mut traversal.pending;
        let environments = &mut traversal.environments;

        let right = environments.len();
        environments.push(environments[slot].clone());
        pending.push(BindingWork::LogicalDone(node, slot, right));
        if let Some(value) = child(node, "right") {
            pending.push(BindingWork::Visit(value, right, false));
        }
    }
    fn resume_logical_done<'tree>(
        &mut self,
        node: Node<'tree>,
        slot: usize,
        right: usize,
        traversal: &mut BindingTraversal<'tree>,
    ) {
        let environments = &mut traversal.environments;

        let mut right = environments[right].take().unwrap();
        if node.kind() == "augmented_assignment_expression" {
            if let (Some(left), Some(value)) = (
                child(node, "left").and_then(|left| name(left, self.source)),
                child(node, "right"),
            ) {
                right.insert(left, self.value(value, &right));
            }
        }
        let env = environments[slot].as_mut().unwrap();
        for key in env.keys().cloned().collect::<Vec<_>>() {
            env.insert(
                key.clone(),
                merge_types([
                    env.get(&key).cloned().unwrap_or(Binding::Unknown),
                    right.get(&key).cloned().unwrap_or(Binding::Unknown),
                ]),
            );
        }
    }
    fn resume_control<'tree>(
        &mut self,
        slot: usize,
        locals: Vec<usize>,
        traversal: &mut BindingTraversal<'tree>,
    ) {
        let environments = &mut traversal.environments;

        let mut changed = BTreeSet::new();
        for local in locals {
            for (n, value) in environments[local].take().unwrap() {
                if environments[slot].as_ref().unwrap().get(&n) != Some(&value) {
                    changed.insert(n);
                }
            }
        }
        for n in changed {
            environments[slot]
                .as_mut()
                .unwrap()
                .insert(n, Binding::Unknown);
        }
    }
    fn visit_declaration<'tree>(
        &mut self,
        node: Node<'tree>,
        slot: usize,
        traversal: &mut BindingTraversal<'tree>,
    ) -> bool {
        let pending = &mut traversal.pending;
        if self.function.is_module_initializer() {
            let r = range(node, Path::new(""));
            let scope = &self.function.range;
            if (r.end_line, r.end_column) < (scope.start_line, scope.start_column)
                || (r.start_line, r.start_column) > (scope.end_line, scope.end_column)
            {
                return true;
            }
        }
        if is_type(node.kind(), self.dialect) {
            if self.function.is_module_initializer() && self.dialect == Dialect::Python {
                pending.push(BindingWork::ClassBody(node, slot));
                if let Some(bases) = child(node, "superclasses") {
                    pending.push(BindingWork::Visit(bases, slot, false));
                }
            }
            return true;
        }
        if is_function(node.kind(), self.dialect) {
            pending.push(BindingWork::Function(node, slot));
            if self.function.is_module_initializer() && self.dialect == Dialect::Python {
                if let Some(annotation) = child(node, "return_type") {
                    pending.push(BindingWork::Visit(annotation, slot, false));
                }
                if let Some(parameters) = child(node, "parameters") {
                    pending.push(BindingWork::Visit(parameters, slot, false));
                }
            }
            return true;
        }
        false
    }
    fn visit_import<'tree>(
        &mut self,
        node: Node<'tree>,
        slot: usize,
        traversal: &mut BindingTraversal<'tree>,
    ) -> bool {
        let environments = &mut traversal.environments;
        let env = environments[slot].as_mut().unwrap();
        if [
            "import_statement",
            "import_from_statement",
            "namespace_use_declaration",
            "use_declaration",
        ]
        .contains(&node.kind())
        {
            let r = range(node, Path::new(""));
            for i in self
                .imports
                .iter()
                .filter(|i| contains(&r, i.range.start_line, i.range.start_column))
            {
                env.insert(import_name(i).to_owned(), import_binding(i));
            }
            return true;
        }
        false
    }
    fn visit_catch<'tree>(
        &mut self,
        node: Node<'tree>,
        slot: usize,
        traversal: &mut BindingTraversal<'tree>,
    ) -> bool {
        let pending = &mut traversal.pending;
        let environments = &mut traversal.environments;
        let env = environments[slot].as_mut().unwrap();
        if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
            && node.kind() == "catch_clause"
        {
            let mut local = env.clone();
            if let Some(parameter) = child(node, "parameter") {
                for n in pattern_names(parameter, self.source) {
                    local.insert(n, Binding::Unknown);
                }
            }
            let local_slot = environments.len();
            environments.push(Some(local));
            pending.push(BindingWork::Discard(local_slot, None));
            pending.extend(
                named_children(node)
                    .into_iter()
                    .rev()
                    .map(|c| BindingWork::Visit(c, local_slot, false)),
            );
            return true;
        }
        false
    }
    fn visit_exception<'tree>(
        &mut self,
        node: Node<'tree>,
        slot: usize,
        traversal: &mut BindingTraversal<'tree>,
    ) -> bool {
        let pending = &mut traversal.pending;
        let environments = &mut traversal.environments;
        let env = environments[slot].as_mut().unwrap();
        if let Some((n, ty)) = exception(node, self.source, self.dialect) {
            let ty = if self.dialect == Dialect::Php {
                match ty {
                    Binding::Type(raw) => typed_in_scope(
                        &raw,
                        self.dialect,
                        self.owner,
                        self.imports,
                        &range(node, Path::new("")),
                    ),
                    binding => binding,
                }
            } else {
                ty
            };
            let mut local = env.clone();
            local.insert(n.clone(), ty);
            let local_slot = environments.len();
            environments.push(Some(local));
            pending.push(BindingWork::Discard(
                local_slot,
                (self.dialect == Dialect::Python).then_some((slot, n)),
            ));
            pending.extend(
                named_children(node)
                    .into_iter()
                    .rev()
                    .map(|c| BindingWork::Visit(c, local_slot, false)),
            );
            return true;
        }
        false
    }
    fn visit_loop<'tree>(
        &mut self,
        node: Node<'tree>,
        slot: usize,
        traversal: &mut BindingTraversal<'tree>,
    ) -> bool {
        let pending = &mut traversal.pending;
        let environments = &mut traversal.environments;
        let env = environments[slot].as_mut().unwrap();
        if is_loop(node.kind()) {
            let mut local = env.clone();
            let mut bound = child(node, "left")
                .or_else(|| child(node, "pattern"))
                .map(|n| pattern_names(n, self.source))
                .unwrap_or_default();
            if node.kind() == "foreach_statement" {
                for n in named_children(node)
                    .into_iter()
                    .filter(|n| child(node, "body").is_none_or(|b| b.id() != n.id()))
                    .skip(1)
                {
                    bound.extend(pattern_names(n, self.source));
                }
            }
            let lexical_initializer =
                child(node, "initializer").filter(|n| n.kind() == "lexical_declaration");
            if let Some(initializer) = lexical_initializer {
                let mut children_cursor = initializer.walk();
                for d in initializer.named_children(&mut children_cursor) {
                    if let Some(pattern) = child(d, "name") {
                        bound.extend(pattern_names(pattern, self.source));
                    }
                }
            }
            let scoped = self.dialect == Dialect::Rust
                || (matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
                    && (lexical_initializer.is_some()
                        || child(node, "kind")
                            .is_some_and(|n| ["let", "const"].contains(&snippet(n, self.source)))));
            for n in &bound {
                local.insert(n.clone(), Binding::Unknown);
            }
            let local_slot = environments.len();
            environments.push(Some(local));
            pending.push(BindingWork::Loop(slot, local_slot, bound, scoped));
            let body = child(node, "body");
            if let Some(body) = body {
                pending.push(BindingWork::Visit(body, local_slot, false));
            }
            pending.extend(
                named_children(node)
                    .into_iter()
                    .filter(|c| body.is_none_or(|b| b.id() != c.id()))
                    .rev()
                    .map(|c| BindingWork::Visit(c, local_slot, false)),
            );
            return true;
        }
        false
    }
    fn visit_branch<'tree>(
        &mut self,
        node: Node<'tree>,
        slot: usize,
        traversal: &mut BindingTraversal<'tree>,
    ) -> bool {
        let pending = &mut traversal.pending;
        if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
            && matches!(node.kind(), "if_statement" | "ternary_expression")
        {
            pending.push(BindingWork::Branches(node, slot));
            if let Some(condition) = child(node, "condition") {
                pending.push(BindingWork::Visit(condition, slot, false));
            }
            return true;
        }
        false
    }
    fn visit_logical<'tree>(
        &mut self,
        node: Node<'tree>,
        slot: usize,
        traversal: &mut BindingTraversal<'tree>,
    ) -> bool {
        let pending = &mut traversal.pending;
        if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
            && matches!(
                node.kind(),
                "augmented_assignment_expression" | "binary_expression"
            )
            && child(node, "operator").is_some_and(|operator| {
                matches!(
                    snippet(operator, self.source),
                    "&&=" | "||=" | "??=" | "&&" | "||" | "??"
                )
            })
        {
            pending.push(BindingWork::LogicalRight(node, slot));
            if let Some(left) = child(node, "left") {
                pending.push(BindingWork::Visit(left, slot, false));
            }
            return true;
        }
        false
    }
    fn visit_control<'tree>(
        &mut self,
        node: Node<'tree>,
        slot: usize,
        traversal: &mut BindingTraversal<'tree>,
    ) -> bool {
        let pending = &mut traversal.pending;
        let environments = &mut traversal.environments;
        let control = [
            "if_statement",
            "try_statement",
            "match_statement",
            "match_expression",
            "switch_statement",
            "conditional_expression",
            "ternary_expression",
        ]
        .contains(&node.kind())
            || (node.kind() == "augmented_assignment_expression"
                && child(node, "operator").is_some_and(|operator| {
                    matches!(snippet(operator, self.source), "&&=" | "||=" | "??=")
                }));
        if control {
            let mut visits = Vec::new();
            let mut locals = Vec::new();
            for c in named_children(node) {
                let local = environments.len();
                environments.push(environments[slot].clone());
                locals.push(local);
                visits.push(BindingWork::Visit(c, local, false));
            }
            pending.push(BindingWork::Control(slot, locals));
            pending.extend(visits.into_iter().rev());
            return true;
        }
        false
    }
    fn visit_children<'tree>(
        &mut self,
        node: Node<'tree>,
        slot: usize,
        root: bool,
        traversal: &mut BindingTraversal<'tree>,
    ) {
        let pending = &mut traversal.pending;
        let environments = &mut traversal.environments;
        let env = environments[slot].as_mut().unwrap();
        let block = !root
            && matches!(
                self.dialect,
                Dialect::JavaScript | Dialect::TypeScript | Dialect::Rust
            )
            && ["block", "statement_block"].contains(&node.kind());
        let mut declared = Vec::new();
        if block {
            let mut children_cursor = node.walk();
            for c in node.named_children(&mut children_cursor) {
                if c.kind() == "let_declaration" {
                    if let Some(n) = child(c, "pattern").and_then(|n| name(n, self.source)) {
                        declared.push(n);
                    }
                }
                if c.kind() == "lexical_declaration" {
                    let mut children_cursor = c.walk();
                    for d in c.named_children(&mut children_cursor) {
                        if let Some(n) = child(d, "name").and_then(|n| name(n, self.source)) {
                            declared.push(n);
                        }
                    }
                }
            }
        }
        // Only declarations in this block need restoration. Other assignments escape.
        let before: Vec<_> = declared
            .iter()
            .map(|name| (name.clone(), env.get(name).cloned()))
            .collect();
        for n in &declared {
            env.insert(n.clone(), Binding::Unknown);
        }
        pending.push(BindingWork::Finish(node, slot, before));
        pending.extend(
            named_children(node)
                .into_iter()
                .rev()
                .map(|c| BindingWork::Visit(c, slot, false)),
        );
    }
    fn visit_binding_node<'tree>(
        &mut self,
        node: Node<'tree>,
        slot: usize,
        root: bool,
        traversal: &mut BindingTraversal<'tree>,
    ) {
        if self.visit_declaration(node, slot, traversal) {
            return;
        }
        self.bind_call(node, traversal.environments[slot].as_ref().unwrap());
        if self.visit_import(node, slot, traversal) {
            return;
        }
        if self.visit_catch(node, slot, traversal) {
            return;
        }
        if self.visit_exception(node, slot, traversal) {
            return;
        }
        if self.visit_loop(node, slot, traversal) {
            return;
        }
        if self.visit_branch(node, slot, traversal) {
            return;
        }
        if self.visit_logical(node, slot, traversal) {
            return;
        }
        if self.visit_control(node, slot, traversal) {
            return;
        }
        self.visit_children(node, slot, root, traversal);
    }
    fn bind_call(&mut self, node: Node<'_>, env: &BTreeMap<String, Binding>) {
        let start = node.start_position();
        let end = node.end_position();
        if let Some(indices) = self
            .calls
            .get(&(start.row + 1, start.column + 1, end.row + 1, end.column + 1))
            .cloned()
        {
            for index in indices {
                let call = &mut self.function.calls[index];
                let lookup = call.receiver.as_deref().unwrap_or(&call.name);
                let parts = receiver_path(lookup);
                let (first, rest) = parts
                    .split_once('.')
                    .map_or((parts.as_ref(), None), |(first, rest)| (first, Some(rest)));
                let first = first.trim_start_matches('$');
                let reserved = self.dialect == Dialect::Php
                    && call.kind == CallKind::Static
                    && matches!(call.receiver.as_deref(), Some("self" | "static" | "parent"));
                let class_binding = reserved
                    .then(|| self.function.parent_type.clone())
                    .flatten()
                    .map(|ty| Binding::Type(format!("\\{ty}")));
                let binding = if reserved {
                    class_binding.as_ref().filter(|_| first != "parent")
                } else {
                    env.get(first)
                };
                if let Some(binding) = binding {
                    call.local_binding = !matches!(binding, Binding::Import(_));
                    call.receiver_type = None;
                    call.receiver_types.clear();
                    call.receiver_identity = None;
                    call.receiver_root_type = None;
                    call.targets.clear();
                    call.binding_unknown = false;
                    call.confidence = Confidence::Dynamic;
                    match binding {
                        Binding::Unknown | Binding::ErasedImport => call.binding_unknown = true,
                        Binding::Pending => {}
                        Binding::Function(target) if call.receiver.is_none() => {
                            call.targets.push(target.clone());
                            call.confidence = Confidence::Resolved;
                        }
                        Binding::Type(ty) | Binding::Import(ty) | Binding::Object(ty, _)
                            if call.receiver.is_some() =>
                        {
                            if let Binding::Object(_, identity) = binding {
                                if rest.is_none() {
                                    call.receiver_identity = Some(identity.clone());
                                }
                            }
                            // Properties are resolved using the actual class's field metadata later.
                            if rest.is_some()
                                && matches!(binding, Binding::Type(_) | Binding::Object(_, _))
                            {
                                call.receiver_root_type = Some(ty.clone());
                            }
                            if rest.is_none() || matches!(binding, Binding::Import(_)) {
                                call.receiver_type = Some(if rest.is_none() {
                                    ty.clone()
                                } else {
                                    format!("{ty}.{}", rest.unwrap_or_default())
                                });
                            }
                        }
                        Binding::Types(types) if call.receiver.is_some() => {
                            call.receiver_types = types.iter().cloned().collect();
                            call.confidence = Confidence::Ambiguous;
                        }
                        Binding::Import(target) => {
                            call.targets.push(target.clone());
                        }
                        _ => call.binding_unknown = true,
                    }
                } else if call.kind == CallKind::Dynamic {
                    call.local_binding = true;
                    call.binding_unknown = true;
                }
            }
        }
    }
    fn value(&self, node: Node<'_>, env: &BTreeMap<String, Binding>) -> Binding {
        enum Work<'tree> {
            Value(Option<Node<'tree>>),
            Merge,
            LazyMerge,
        }
        let mut pending = vec![Work::Value(Some(node))];
        let mut values = Vec::new();
        while let Some(work) = pending.pop() {
            match work {
                Work::Merge | Work::LazyMerge => {
                    let right = values.pop().unwrap();
                    let left = values.pop().unwrap();
                    let merged = if matches!(work, Work::LazyMerge) {
                        match (left, right) {
                            (
                                Binding::Pending | Binding::Unknown,
                                Binding::Type(ty) | Binding::Object(ty, _),
                            ) => Binding::Type(ty),
                            (left, right) => merge_types([left, right]),
                        }
                    } else {
                        merge_types([left, right])
                    };
                    values.push(merged);
                }
                Work::Value(None) => values.push(Binding::Unknown),
                Work::Value(Some(node)) => {
                    if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
                        && node.kind() == "ternary_expression"
                    {
                        pending.push(Work::Merge);
                        pending.push(Work::Value(child(node, "alternative")));
                        pending.push(Work::Value(child(node, "consequence")));
                    } else if matches!(
                        node.kind(),
                        "parenthesized_expression"
                            | "as_expression"
                            | "satisfies_expression"
                            | "non_null_expression"
                    ) {
                        pending.push(Work::Value(named_children(node).first().copied()));
                    } else if let Some(n) = name(node, self.source) {
                        values.push(env.get(&n).cloned().unwrap_or(Binding::Unknown));
                    } else if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
                        && node.kind() == "binary_expression"
                        && child(node, "operator").is_some_and(|operator| {
                            matches!(snippet(operator, self.source), "||" | "??")
                        })
                    {
                        // A typed fallback establishes a lazy singleton's type.
                        pending.push(Work::LazyMerge);
                        pending.push(Work::Value(child(node, "right")));
                        pending.push(Work::Value(child(node, "left")));
                    } else {
                        values.push(self.value_leaf(node, env));
                    }
                }
            }
        }
        values.pop().unwrap_or(Binding::Unknown)
    }
    fn value_leaf(&self, node: Node<'_>, env: &BTreeMap<String, Binding>) -> Binding {
        if ["new_expression", "object_creation_expression"].contains(&node.kind()) {
            let raw = snippet(node, self.source)
                .trim_start_matches("new ")
                .split('(')
                .next()
                .unwrap_or("")
                .trim();
            return if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript) {
                let constructor = child(node, "constructor")
                    .map(|node| snippet(node, self.source))
                    .unwrap_or(raw);
                if constructor.split('.').next().and_then(|name| env.get(name))
                    == Some(&Binding::ErasedImport)
                {
                    return Binding::Unknown;
                }
                Binding::Object(
                    raw.to_owned(),
                    format!(
                        "{}:{}",
                        node.start_position().row + 1,
                        node.start_position().column + 1
                    ),
                )
            } else if self.dialect == Dialect::Php {
                php_names::reference(raw, self.owner, self.imports, &range(node, Path::new("")))
                    .map(Binding::Type)
                    .unwrap_or(Binding::Unknown)
            } else {
                Binding::Type(raw.to_owned())
            };
        }
        if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
            && properties::member(node, self.source).is_some()
        {
            // A typed native getter can establish this binding in graph source
            // order; ordinary/untyped property values provide no type evidence.
            return Binding::Pending;
        }
        if is_call(node.kind(), self.dialect) {
            let target = child(node, "function")
                .map(|n| snippet(n, self.source))
                .unwrap_or("");
            if matches!(self.dialect, Dialect::JavaScript | Dialect::TypeScript)
                && target == "require"
                && !env.contains_key("require")
            {
                if let Some(arguments) = child(node, "arguments") {
                    let arguments = named_children(arguments);
                    if arguments.len() == 1 {
                        if let Some(module) = properties::literal_key(arguments[0], self.source) {
                            let path = [".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs"]
                                .iter()
                                .find_map(|extension| module.strip_suffix(extension))
                                .unwrap_or(&module);
                            return Binding::Import(path.to_owned());
                        }
                    }
                }
            }
            if self.dialect == Dialect::Python {
                if let Some(ty) = self.types.get(target) {
                    return Binding::Type(ty.clone());
                }
            }
            if self.dialect == Dialect::Rust {
                if let Some((ty, method)) = target.rsplit_once("::") {
                    if ["new", "default"].contains(&method) {
                        return Binding::Type(ty.to_owned());
                    }
                }
            }
            return Binding::Pending;
        }
        Binding::Unknown
    }
}
fn declared_names(
    body: Node<'_>,
    source: &str,
    dialect: Dialect,
    imports: &[Import],
) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut pending = vec![body];
    while let Some(node) = pending.pop() {
        if is_function(node.kind(), dialect) || is_type(node.kind(), dialect) {
            continue;
        }
        if dialect == Dialect::Python {
            if let Some((left, _)) = assignment(node) {
                if let Some(n) = name(left, source) {
                    names.insert(n);
                } else {
                    names.extend(pattern_names(left, source));
                }
            }
            if let Some(left) = child(node, "left").filter(|_| node.kind() == "for_statement") {
                names.extend(pattern_names(left, source));
            }
            if let Some((n, _)) = exception(node, source, dialect) {
                names.insert(n);
            }
            if ["import_statement", "import_from_statement"].contains(&node.kind()) {
                let r = range(node, Path::new(""));
                for i in imports
                    .iter()
                    .filter(|i| contains(&r, i.range.start_line, i.range.start_column))
                {
                    names.insert(import_name(i).to_owned());
                }
                continue;
            }
        } else if matches!(dialect, Dialect::JavaScript | Dialect::TypeScript)
            && node.kind() == "variable_declaration"
        {
            let mut children_cursor = node.walk();
            for d in node.named_children(&mut children_cursor) {
                if let Some(n) = child(d, "name").and_then(|n| name(n, source)) {
                    names.insert(n);
                }
            }
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    names
}
/// JS/TS module-scope `const` declarations with one declared or constructed
/// type. A `const` is never reassigned, so every function of the module and
/// every importer may use that type.
pub(super) fn module_constants(
    root: Node<'_>,
    types: &ScopedTypes<'_, '_>,
    imports: &[Import],
    dialect: Dialect,
    source: &str,
    path: &Path,
) -> Vec<Field> {
    let module = range(root, path);
    let mut env = imports
        .iter()
        .filter(|i| applies(i, &module))
        .map(|i| (import_name(i).to_owned(), import_binding(i)))
        .collect::<BTreeMap<_, _>>();
    let mut function = FunctionModel::default();
    let walker = Walker {
        dialect,
        source,
        types,
        imports,
        nested: &BTreeMap::new(),
        owner: "",
        calls: BTreeMap::new(),
        function: &mut function,
    };
    let mut constants = Vec::new();
    for statement in named_children(root) {
        let declaration = if statement.kind() == "export_statement" {
            child(statement, "declaration")
        } else {
            Some(statement)
        };
        let Some(declaration) = declaration.filter(|d| {
            d.kind() == "lexical_declaration"
                && child(*d, "kind").is_some_and(|kind| snippet(kind, source) == "const")
        }) else {
            continue;
        };
        for declarator in named_children(declaration) {
            let Some(n) = child(declarator, "name").and_then(|n| name(n, source)) else {
                continue;
            };
            let declared = child(declarator, "type")
                .map(|ty| typed(snippet(ty, source), dialect))
                .filter(|binding| *binding != Binding::Unknown);
            let binding = declared
                .or_else(|| child(declarator, "value").map(|v| walker.value(v, &env)))
                .unwrap_or(Binding::Unknown);
            let ty = match &binding {
                Binding::Type(ty) | Binding::Object(ty, _) => Some(ty.clone()),
                _ => None,
            };
            env.insert(n.clone(), ty.clone().map_or(binding, Binding::Type));
            if let Some(ty) = ty {
                constants.push(Field {
                    name: n,
                    type_name: Some(ty),
                    range: range(declarator, path),
                    ..Field::default()
                });
            }
        }
    }
    constants
}
pub(super) fn annotate(
    node: Node<'_>,
    f: &mut FunctionModel,
    constants: &[Field],
    types: &ScopedTypes<'_, '_>,
    imports: &[Import],
    nested: &BTreeMap<(String, String), String>,
    scopes: &BTreeMap<(usize, usize), FunctionScope>,
    dialect: Dialect,
    source: &str,
) {
    if f.calls.is_empty() && f.property_accesses.is_empty() && f.property_mutations.is_empty() {
        return;
    }
    let Some(body) = child(node, "body").or_else(|| f.is_module_initializer().then_some(node))
    else {
        return;
    };
    // Property reads/writes share the explicit call binder and source ordering.
    // Compound operations can have multiple operations at exactly the same range.
    let explicit_calls = f.calls.len();
    f.calls
        .extend(f.property_accesses.iter().map(|access| access.site.clone()));
    f.calls.extend(
        f.property_mutations
            .iter()
            .map(|mutation| mutation.site.clone()),
    );
    // Module constants are the outermost scope; everything below shadows them.
    // The module initializer binds them itself in source order.
    let mut env = constants
        .iter()
        .filter(|_| !f.is_module_initializer())
        .filter_map(|c| Some((c.name.clone(), Binding::Type(c.type_name.clone()?))))
        .collect::<BTreeMap<_, _>>();
    let mut ancestors = Vec::new();
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        if is_function(parent.kind(), dialect) {
            if let Some(scope) = scopes.get(&(
                parent.start_position().row + 1,
                parent.start_position().column + 1,
            )) {
                ancestors.push(scope);
            }
        }
        ancestor = parent.parent();
    }
    // Rust function items cannot capture; closures can. JS/TS nested functions
    // capture lexical names, while only arrows inherit an enclosing `this`.
    let capture_names = dialect != Dialect::Rust || node.kind() == "closure_expression";
    if capture_names {
        for scope in ancestors.iter().rev() {
            for parameter in &scope.parameters {
                if !matches!(parameter.name.as_str(), "this" | "self") {
                    env.insert(
                        parameter.name.clone(),
                        parameter
                            .type_name
                            .as_ref()
                            .map(|ty| {
                                typed_in_scope(ty, dialect, &scope.fqn, imports, &parameter.range)
                            })
                            .unwrap_or(Binding::Unknown),
                    );
                }
            }
            for ((owner, name), target) in nested {
                if owner == &scope.fqn
                    && scopes.values().any(|scope| {
                        scope.fqn == *target
                            && contains(
                                &scope.declaration_scope,
                                node.start_position().row + 1,
                                node.start_position().column + 1,
                            )
                            && matches!(
                                scope.kind.as_str(),
                                "function_declaration"
                                    | "generator_function_declaration"
                                    | "function_item"
                            )
                    })
                {
                    env.insert(name.clone(), Binding::Function(target.clone()));
                }
            }
        }
    }
    if matches!(dialect, Dialect::JavaScript | Dialect::TypeScript)
        && node.kind() == "arrow_function"
    {
        if let Some(scope) = ancestors
            .iter()
            .find(|scope| scope.kind != "arrow_function" || scope.parent_function.is_none())
        {
            if scope.kind == "method_definition"
                || (scope.kind == "arrow_function" && scope.parent_function.is_none())
            {
                if let Some(owner) = &scope.parent_type {
                    env.insert("this".into(), Binding::Type(owner.clone()));
                }
            }
        }
    } else if dialect == Dialect::Rust && node.kind() == "closure_expression" {
        if let Some(scope) = ancestors
            .iter()
            .find(|scope| scope.kind != "closure_expression")
        {
            if let Some(owner) = &scope.parent_type {
                if scope
                    .parameters
                    .iter()
                    .any(|parameter| parameter.implicit_receiver)
                {
                    env.insert("self".into(), Binding::Type(owner.clone()));
                }
            }
        }
    }
    for i in imports.iter().filter(|i| applies(i, &f.range)) {
        env.insert(import_name(i).to_owned(), import_binding(i));
    }
    for n in declared_names(body, source, dialect, imports) {
        env.insert(n, Binding::Unknown);
    }
    if matches!(dialect, Dialect::JavaScript | Dialect::TypeScript) {
        for c in named_children(body)
            .into_iter()
            .filter(|n| n.kind() == "lexical_declaration")
        {
            let mut children_cursor = c.walk();
            for d in c.named_children(&mut children_cursor) {
                if let Some(n) = child(d, "name").and_then(|n| name(n, source)) {
                    env.insert(n, Binding::Unknown);
                }
            }
        }
    }
    for p in &f.parameters {
        env.insert(
            p.name.clone(),
            p.type_name
                .as_ref()
                .map(|t| typed_in_scope(t, dialect, &f.fqn, imports, &p.range))
                .unwrap_or(Binding::Unknown),
        );
    }
    if let Some(params) = child(node, "parameters") {
        let mut children_cursor = params.walk();
        for p in params.named_children(&mut children_cursor) {
            let pattern = child(p, "pattern")
                .or_else(|| child(p, "name"))
                .or_else(|| child(p, "left"))
                .unwrap_or(p);
            if name(pattern, source).is_none() {
                for n in pattern_names(pattern, source) {
                    env.insert(n, Binding::Unknown);
                }
            }
        }
    }
    if f.parent_function.is_none() {
        if let Some(ty) = &f.parent_type {
            for n in receiver_names(f, dialect) {
                env.insert(
                    n.to_owned(),
                    Binding::Type(if dialect == Dialect::Php {
                        format!("\\{ty}")
                    } else {
                        ty.clone()
                    }),
                );
            }
        }
    }
    if dialect != Dialect::Python {
        for ((owner, name), fqn) in nested {
            if owner == &f.fqn
                && scopes.values().any(|scope| {
                    scope.fqn == *fqn
                        && contains(
                            &scope.declaration_scope,
                            body.start_position().row + 1,
                            body.start_position().column + 1,
                        )
                        && matches!(
                            scope.kind.as_str(),
                            "function_declaration"
                                | "generator_function_declaration"
                                | "function_item"
                        )
                })
            {
                env.insert(name.clone(), Binding::Function(fqn.clone()));
            }
        }
    }
    let mut calls = BTreeMap::<_, Vec<usize>>::new();
    for (i, c) in f.calls.iter().enumerate() {
        calls
            .entry((
                c.range.start_line,
                c.range.start_column,
                c.range.end_line,
                c.range.end_column,
            ))
            .or_default()
            .push(i);
    }
    let owner = f.fqn.clone();
    let mut walker = Walker {
        dialect,
        source,
        types,
        imports,
        nested,
        owner: &owner,
        calls,
        function: f,
    };
    walker.visit(body, &mut env, true);
    let mut sites = f.calls.drain(explicit_calls..);
    for access in &mut f.property_accesses {
        access.site = sites.next().unwrap();
    }
    for mutation in &mut f.property_mutations {
        mutation.site = sites.next().unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::receiver_path;

    #[test]
    fn receiver_paths_preserve_order_and_empty_segments() {
        for raw in [
            "value",
            "$value",
            "value.field",
            "value..field",
            "value.",
            "",
            "?->",
            "?->->",
            "??->",
            "value?->field::method",
            "value?.field->method",
            "тип?.поле::метод",
            "?::->",
            "value:::field",
        ] {
            let old = raw
                .replace("?->", ".")
                .replace("->", ".")
                .replace("?.", ".")
                .replace("::", ".");
            let new = receiver_path(raw);
            assert_eq!(new, old, "{raw}");
            let old_parts: Vec<_> = old.split('.').collect();
            let (first, rest) = new
                .split_once('.')
                .map_or((new.as_ref(), None), |(first, rest)| (first, Some(rest)));
            assert_eq!(first, old_parts[0]);
            assert_eq!(
                rest,
                (old_parts.len() > 1)
                    .then(|| old_parts[1..].join("."))
                    .as_deref()
            );
        }
        assert!(matches!(
            receiver_path("value.field"),
            std::borrow::Cow::Borrowed(_)
        ));
    }
}
