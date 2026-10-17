//! Exact lexical callable/object proofs for JS/TS. No project code is executed.
use super::*;

#[derive(Clone, Copy)]
enum Lexical<'a> {
    Declaration(Node<'a>),
    Shadowed,
    Missing,
}
fn binds(node: Node<'_>, name: &str, source: &str) -> bool {
    if matches!(
        node.kind(),
        "identifier" | "shorthand_property_identifier_pattern"
    ) {
        return snippet(node, source) == name;
    }
    if matches!(node.kind(), "pair_pattern" | "assignment_pattern") {
        return child(node, "value")
            .or_else(|| child(node, "left"))
            .is_some_and(|node| binds(node, name, source));
    }
    named_children(node)
        .into_iter()
        .any(|node| binds(node, name, source))
}
fn declaration<'a>(at: Node<'a>, name: &str, source: &str) -> Lexical<'a> {
    let mut ancestor = at.parent();
    while let Some(scope) = ancestor {
        if scope.kind() == "with_statement" {
            return Lexical::Shadowed;
        }
        if is_function(scope.kind(), Dialect::JavaScript) {
            if child(scope, "parameters")
                .or_else(|| child(scope, "parameter"))
                .is_some_and(|node| binds(node, name, source))
            {
                return Lexical::Shadowed;
            }
            if child(scope, "name").is_some_and(|node| snippet(node, source) == name) {
                return Lexical::Declaration(scope);
            }
        }
        if matches!(
            scope.kind(),
            "catch_clause" | "for_in_statement" | "for_statement"
        ) {
            if child(scope, "parameter")
                .or_else(|| child(scope, "left"))
                .or_else(|| child(scope, "initializer"))
                .is_some_and(|node| binds(node, name, source))
            {
                return Lexical::Shadowed;
            }
        }
        if matches!(scope.kind(), "program" | "statement_block" | "switch_body") {
            for mut statement in named_children(scope) {
                if statement.kind() == "export_statement" {
                    statement = child(statement, "declaration").unwrap_or(statement);
                }
                if matches!(
                    statement.kind(),
                    "lexical_declaration" | "variable_declaration"
                ) {
                    for node in named_children(statement) {
                        if child(node, "name").is_some_and(|node| binds(node, name, source)) {
                            return Lexical::Declaration(node);
                        }
                    }
                }
                if matches!(
                    statement.kind(),
                    "function_declaration" | "generator_function_declaration" | "class_declaration"
                ) && child(statement, "name").is_some_and(|node| snippet(node, source) == name)
                {
                    return Lexical::Declaration(statement);
                }
                if statement.kind() == "import_statement"
                    && named_children(statement)
                        .into_iter()
                        .filter(|node| node.kind() == "import_clause")
                        .any(|node| binds(node, name, source))
                {
                    return Lexical::Shadowed;
                }
            }
            // `var` belongs to the containing function even in a nested block.
            if scope.kind() == "program"
                || scope
                    .parent()
                    .is_some_and(|node| is_function(node.kind(), Dialect::JavaScript))
            {
                let mut pending = named_children(scope);
                while let Some(node) = pending.pop() {
                    if is_function(node.kind(), Dialect::JavaScript)
                        || is_type(node.kind(), Dialect::JavaScript)
                    {
                        continue;
                    }
                    if node.kind() == "variable_declaration" {
                        for declarator in named_children(node) {
                            if child(declarator, "name")
                                .is_some_and(|node| binds(node, name, source))
                            {
                                return Lexical::Declaration(declarator);
                            }
                        }
                    }
                    pending.extend(named_children(node));
                }
            }
        }
        ancestor = scope.parent();
    }
    Lexical::Missing
}
fn value<'a>(node: Node<'a>, at: Node<'a>, source: &str, depth: usize) -> Option<Node<'a>> {
    if depth > 16 {
        return None;
    }
    if matches!(
        node.kind(),
        "parenthesized_expression"
            | "as_expression"
            | "satisfies_expression"
            | "non_null_expression"
    ) {
        return value(*named_children(node).first()?, at, source, depth + 1);
    }
    if matches!(node.kind(), "identifier" | "shorthand_property_identifier") {
        let Lexical::Declaration(binding) = declaration(node, snippet(node, source), source) else {
            return None;
        };
        if matches!(
            binding.kind(),
            "function_declaration" | "generator_function_declaration"
        ) {
            return Some(binding);
        }
        if binding.kind() != "variable_declarator" {
            return None;
        }
        let parent = binding.parent()?;
        if parent.kind() != "lexical_declaration"
            || !snippet(parent, source).trim_start().starts_with("const ")
        {
            return None;
        }
        let initializer = child(binding, "value")?;
        // The object's methods execute after its initialization, so their own
        // receiver may refer to the containing const object. Other later
        // declarations do not establish source-order callable evidence.
        let inside_object_method = initializer.kind() == "object"
            && initializer.start_byte() <= at.start_byte()
            && at.end_byte() <= initializer.end_byte()
            && ancestors(at).any(|node| is_function(node.kind(), Dialect::JavaScript));
        if binding.end_byte() > at.start_byte() && !inside_object_method {
            return None;
        }
        return value(initializer, at, source, depth + 1);
    }
    (node.kind() == "object" || is_function(node.kind(), Dialect::JavaScript)).then_some(node)
}
fn ancestors(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    std::iter::successors(node.parent(), |node| node.parent())
}
fn owner(node: Node<'_>) -> Option<usize> {
    ancestors(node)
        .find(|node| is_function(node.kind(), Dialect::JavaScript))
        .map(|node| node.id())
}
fn dynamic_scope(at: Node<'_>, scopes: &BTreeSet<usize>) -> bool {
    ancestors(at).any(|scope| scope.kind() == "with_statement" || scopes.contains(&scope.id()))
}
fn member_key(node: Node<'_>, source: &str) -> Option<String> {
    match node.kind() {
        "member_expression" => Some(snippet(child(node, "property")?, source).into()),
        "subscript_expression" => static_js::string(child(node, "index")?, source),
        _ => None,
    }
}
fn object_member<'a>(object: Node<'a>, key: &str, source: &str) -> Option<Node<'a>> {
    let mut found = None;
    for property in named_children(object) {
        let raw = child(property, "key").or_else(|| child(property, "name"))?;
        let name = match raw.kind() {
            "string" => properties::literal_key(raw, source),
            "computed_property_name" => named_children(raw)
                .first()
                .and_then(|node| static_js::string(*node, source)),
            _ => Some(snippet(raw, source).into()),
        };
        if name.as_deref() == Some(key) {
            found = if property.kind() == "method_definition" {
                (accessor_keyword(property).is_none()).then_some(property)
            } else {
                child(property, "value")
            };
        }
        // Unknown spread/computed properties can replace a preceding member.
        if property.kind() == "spread_element" || name.is_none() {
            found = None;
        }
    }
    found
}
#[derive(Clone)]
struct ObjectEffect {
    key: Option<String>,
    end: usize,
    owner: Option<usize>,
}
type ObjectEffects = BTreeMap<usize, Vec<ObjectEffect>>;
fn effects(nodes: &[Node<'_>], source: &str) -> ObjectEffects {
    let mut effects = ObjectEffects::new();
    for node in nodes {
        let effect = if matches!(
            node.kind(),
            "assignment_expression"
                | "augmented_assignment_expression"
                | "update_expression"
                | "unary_expression"
        ) {
            if node.kind() == "unary_expression"
                && !snippet(*node, source).trim_start().starts_with("delete ")
            {
                continue;
            }
            child(*node, "left")
                .or_else(|| child(*node, "argument"))
                .and_then(|left| {
                    let object = value(child(left, "object")?, *node, source, 0)?;
                    (object.kind() == "object").then(|| (object.id(), member_key(left, source)))
                })
        } else if node.kind() == "call_expression" {
            child(*node, "function").and_then(|function| {
                let raw = snippet(function, source);
                if !matches!(
                    raw,
                    "Object.assign"
                        | "Object.defineProperty"
                        | "Object.defineProperties"
                        | "Reflect.defineProperty"
                        | "Reflect.deleteProperty"
                        | "Reflect.set"
                ) {
                    return None;
                }
                let args = named_children(child(*node, "arguments")?);
                let object = value(*args.first()?, *node, source, 0)?;
                if object.kind() != "object" {
                    return None;
                }
                let key = if matches!(raw, "Object.assign" | "Object.defineProperties") {
                    None
                } else {
                    args.get(1).and_then(|arg| static_js::string(*arg, source))
                };
                Some((object.id(), key))
            })
        } else {
            None
        };
        if let Some((object, key)) = effect {
            effects.entry(object).or_default().push(ObjectEffect {
                key,
                end: node.end_byte(),
                owner: owner(*node),
            });
        }
        let mut escapes = Vec::new();
        if node.kind() == "call_expression" {
            let raw = child(*node, "function")
                .map(|function| snippet(function, source))
                .unwrap_or("");
            if !matches!(
                raw,
                "Object.assign"
                    | "Object.defineProperty"
                    | "Object.defineProperties"
                    | "Reflect.defineProperty"
                    | "Reflect.deleteProperty"
                    | "Reflect.set"
            ) {
                if let Some(arguments) = child(*node, "arguments") {
                    escapes.extend(named_children(arguments));
                }
            }
        } else if node.kind() == "return_statement" {
            escapes.extend(named_children(*node));
        } else if node.kind() == "variable_declarator"
            && node.parent().is_none_or(|parent| {
                parent.kind() != "lexical_declaration"
                    || !snippet(parent, source).trim_start().starts_with("const ")
            })
        {
            escapes.extend(child(*node, "value"));
        } else if node.kind() == "assignment_expression" {
            if child(*node, "left").is_some_and(|left| {
                !static_path(left, source).is_some_and(|path| {
                    path.starts_with("window.") || path.starts_with("globalThis.")
                })
            }) {
                escapes.extend(child(*node, "right"));
            }
        } else if matches!(
            node.kind(),
            "pair" | "array" | "shorthand_property_identifier"
        ) {
            if node.kind() == "pair" {
                escapes.extend(child(*node, "value"));
            } else if node.kind() == "array" {
                escapes.extend(named_children(*node));
            } else {
                escapes.push(*node);
            }
        }
        for escape in escapes {
            if let Some(object) =
                value(escape, *node, source, 0).filter(|object| object.kind() == "object")
            {
                effects.entry(object.id()).or_default().push(ObjectEffect {
                    key: None,
                    end: node.end_byte(),
                    owner: owner(*node),
                });
            }
        }
    }
    effects
}
fn object_mutated(
    object: Node<'_>,
    key: &str,
    at: Option<Node<'_>>,
    effects: &ObjectEffects,
) -> bool {
    effects.get(&object.id()).is_some_and(|effects| {
        effects.iter().any(|effect| {
            if at.is_some_and(|at| effect.owner == owner(at) && effect.end > at.start_byte()) {
                return false;
            }
            effect.key.as_deref().is_none_or(|name| name == key)
        })
    })
}
fn target_of(node: Node<'_>, functions: &BTreeMap<(usize, usize), String>) -> Option<String> {
    let start = node.start_position();
    functions.get(&(start.row + 1, start.column + 1)).cloned()
}
fn static_path(node: Node<'_>, source: &str) -> Option<String> {
    if node.kind() == "identifier" {
        return Some(snippet(node, source).into());
    }
    Some(format!(
        "{}.{}",
        static_path(child(node, "object")?, source)?,
        member_key(node, source)?
    ))
}
fn root_unshadowed(at: Node<'_>, name: &str, source: &str) -> bool {
    if !matches!(declaration(at, name, source), Lexical::Missing) {
        return false;
    }
    for scope in ancestors(at)
        .filter(|scope| scope.kind() == "program" || is_function(scope.kind(), Dialect::JavaScript))
    {
        let mut pending = child(scope, "body")
            .map(named_children)
            .unwrap_or_else(|| named_children(scope));
        while let Some(node) = pending.pop() {
            if is_function(node.kind(), Dialect::JavaScript)
                || is_type(node.kind(), Dialect::JavaScript)
            {
                continue;
            }
            if matches!(
                node.kind(),
                "assignment_expression" | "augmented_assignment_expression" | "update_expression"
            ) && child(node, "left")
                .or_else(|| child(node, "argument"))
                .is_some_and(|left| left.kind() == "identifier" && snippet(left, source) == name)
            {
                return false;
            }
            pending.extend(named_children(node));
        }
    }
    true
}
fn script_write_lookup(node: Node<'_>, source: &str) -> Option<String> {
    let path = static_path(node, source)?;
    let root = path.split('.').next()?;
    if !matches!(declaration(node, root, source), Lexical::Missing) {
        return None;
    }
    if matches!(root, "window" | "globalThis") {
        path.split_once('.').map(|(_, rest)| rest.into())
    } else {
        Some(path)
    }
}
fn script_lookup(node: Node<'_>, source: &str) -> Option<String> {
    let path = static_path(node, source)?;
    let root = path.split('.').next()?;
    if !root_unshadowed(node, root, source) {
        return None;
    }
    if matches!(root, "window" | "globalThis") {
        path.split_once('.').map(|(_, rest)| rest.into())
    } else {
        Some(path)
    }
}
fn browser_script(root: Node<'_>, nodes: &[Node<'_>], source: &str, model: &FileModel) -> bool {
    if model.language != "javascript"
        || matches!(
            Path::new(&model.path).extension().and_then(|x| x.to_str()),
            Some("mjs" | "cjs")
        )
    {
        return false;
    }
    if named_children(root)
        .iter()
        .any(|node| matches!(node.kind(), "import_statement" | "export_statement"))
    {
        return false;
    }
    if nodes.iter().any(|node| {
        if node.kind() == "call_expression" {
            return child(*node, "function").is_some_and(|function| {
                snippet(function, source) == "require"
                    && static_js::unshadowed(function, "require", source)
            });
        }
        static_path(*node, source).is_some_and(|path| {
            (path == "module.exports" || path.starts_with("exports."))
                && root_unshadowed(*node, path.split('.').next().unwrap_or(""), source)
        })
    }) {
        return false;
    }
    nodes.iter().any(|node| {
        node.kind() == "identifier"
            && matches!(snippet(*node, source), "window" | "document" | "globalThis")
            && root_unshadowed(*node, snippet(*node, source), source)
    })
}
fn publish(
    name: &str,
    node: Node<'_>,
    source: &str,
    functions: &BTreeMap<(usize, usize), String>,
    effects: &ObjectEffects,
    globals: &mut Vec<analyzer_core::ScriptGlobal>,
) {
    if node.kind() != "object" {
        globals.push(analyzer_core::ScriptGlobal {
            name: name.into(),
            target: target_of(node, functions),
        });
        return;
    }
    let mut keys = BTreeSet::new();
    for property in named_children(node) {
        if let Some(raw) = child(property, "key").or_else(|| child(property, "name")) {
            let key = match raw.kind() {
                "string" => properties::literal_key(raw, source),
                "computed_property_name" => named_children(raw)
                    .first()
                    .and_then(|key| static_js::string(*key, source)),
                _ => Some(snippet(raw, source).into()),
            };
            if let Some(key) = key {
                keys.insert(key);
            }
        }
    }
    for key in keys {
        let target = if object_mutated(node, &key, None, effects) {
            None
        } else {
            object_member(node, &key, source)
                .and_then(|member| value(member, node, source, 0))
                .and_then(|member| target_of(member, functions))
        };
        globals.push(analyzer_core::ScriptGlobal {
            name: format!("{name}.{key}"),
            target,
        });
    }
}
fn globals(
    root: Node<'_>,
    nodes: &[Node<'_>],
    source: &str,
    functions: &BTreeMap<(usize, usize), String>,
    effects: &ObjectEffects,
) -> Vec<analyzer_core::ScriptGlobal> {
    let mut globals = Vec::new();
    let mut published = BTreeSet::new();
    let mut published_assignments = BTreeSet::new();
    // Plain top-level blocks execute in sequence. Their lexical declarations
    // remain private, but explicit window publications are browser globals.
    let mut statements = named_children(root);
    let mut position = 0;
    while position < statements.len() {
        if statements[position].kind() == "statement_block" {
            statements.extend(named_children(statements[position]));
        }
        position += 1;
    }
    for statement in statements {
        if statement
            .parent()
            .is_some_and(|parent| parent.kind() == "program")
            && matches!(
                statement.kind(),
                "function_declaration" | "generator_function_declaration"
            )
        {
            if let Some(name) = child(statement, "name") {
                let name = snippet(name, source);
                published.insert(name.to_owned());
                publish(name, statement, source, functions, effects, &mut globals);
            }
        }
        if statement
            .parent()
            .is_some_and(|parent| parent.kind() == "program")
            && statement.kind() == "lexical_declaration"
            && snippet(statement, source)
                .trim_start()
                .starts_with("const ")
        {
            for declarator in named_children(statement) {
                if let (Some(name), Some(initializer)) =
                    (child(declarator, "name"), child(declarator, "value"))
                {
                    if name.kind() == "identifier" {
                        let name = snippet(name, source);
                        published.insert(name.to_owned());
                        if let Some(initializer) = value(initializer, declarator, source, 0) {
                            publish(name, initializer, source, functions, effects, &mut globals);
                        } else {
                            globals.push(analyzer_core::ScriptGlobal {
                                name: format!("{name}.*"),
                                target: None,
                            });
                        }
                    }
                }
            }
        }
        if statement.kind() == "expression_statement" {
            for assignment in named_children(statement)
                .into_iter()
                .filter(|node| node.kind() == "assignment_expression")
            {
                let Some(left) = child(assignment, "left") else {
                    continue;
                };
                let Some(path) = static_path(left, source) else {
                    continue;
                };
                let Some((root, name)) = path.split_once('.') else {
                    continue;
                };
                if !matches!(root, "window" | "globalThis")
                    || name.contains('.')
                    || !root_unshadowed(left, root, source)
                {
                    continue;
                }
                if let Some(initializer) =
                    child(assignment, "right").and_then(|node| value(node, assignment, source, 0))
                {
                    published.insert(name.into());
                    published_assignments.insert(assignment.id());
                    publish(name, initializer, source, functions, effects, &mut globals);
                }
            }
        }
    }
    // Any other global write can replace a published callable. Unknown member writes
    // invalidate the entire object. Conditional and nested writers are conservative.
    for node in nodes {
        if published_assignments.contains(&node.id()) {
            continue;
        }
        if matches!(
            node.kind(),
            "assignment_expression"
                | "augmented_assignment_expression"
                | "update_expression"
                | "unary_expression"
        ) {
            if node.kind() == "unary_expression"
                && !snippet(*node, source).trim_start().starts_with("delete ")
            {
                continue;
            }
            let Some(left) = child(*node, "left").or_else(|| child(*node, "argument")) else {
                continue;
            };
            let path = static_path(left, source);
            let root = path.as_deref().and_then(|path| path.split('.').next());
            let name = if root.is_some_and(|root| published.contains(root))
                && matches!(declaration(left, root.unwrap(), source), Lexical::Declaration(binding) if owner(binding).is_none())
            {
                path
            } else {
                script_write_lookup(left, source)
            };
            if let Some(name) = name {
                globals.push(analyzer_core::ScriptGlobal { name, target: None });
            } else if let Some(receiver) =
                child(left, "object").and_then(|node| script_write_lookup(node, source))
            {
                globals.push(analyzer_core::ScriptGlobal {
                    name: format!("{receiver}.*"),
                    target: None,
                });
            }
        }
        if node.kind() == "call_expression" {
            let Some(function) = child(*node, "function") else {
                continue;
            };
            let raw = snippet(function, source);
            if !matches!(
                raw,
                "Object.assign"
                    | "Object.defineProperty"
                    | "Object.defineProperties"
                    | "Reflect.defineProperty"
                    | "Reflect.deleteProperty"
                    | "Reflect.set"
            ) {
                continue;
            }
            let Some(arguments) = child(*node, "arguments") else {
                continue;
            };
            let args = named_children(arguments);
            let Some(receiver) = args.first().and_then(|node| {
                if node.kind() == "identifier"
                    && matches!(snippet(*node, source), "window" | "globalThis")
                    && matches!(
                        declaration(*node, snippet(*node, source), source),
                        Lexical::Missing
                    )
                {
                    Some(String::new())
                } else {
                    script_write_lookup(*node, source)
                }
            }) else {
                continue;
            };
            let key = if matches!(raw, "Object.assign" | "Object.defineProperties") {
                None
            } else {
                args.get(1)
                    .and_then(|node| static_js::string(*node, source))
            };
            globals.push(analyzer_core::ScriptGlobal {
                name: if receiver.is_empty() {
                    key.unwrap_or_else(|| "*".into())
                } else {
                    format!("{receiver}.{}", key.as_deref().unwrap_or("*"))
                },
                target: None,
            });
        }
    }
    globals
}

pub(super) fn annotate(root: Node<'_>, source: &str, model: &mut FileModel) {
    let mut nodes = Vec::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        nodes.push(node);
        pending.extend(named_children(node));
    }
    let dynamic_scopes = nodes
        .iter()
        .filter(|node| {
            node.kind() == "call_expression"
                && child(**node, "function").is_some_and(|function| {
                    function.kind() == "identifier"
                        && snippet(function, source) == "eval"
                        && matches!(declaration(function, "eval", source), Lexical::Missing)
                })
        })
        .filter_map(|node| {
            ancestors(*node)
                .find(|scope| {
                    scope.kind() == "program" || is_function(scope.kind(), Dialect::JavaScript)
                })
                .map(|scope| scope.id())
        })
        .collect::<BTreeSet<_>>();
    let functions = model
        .functions
        .iter()
        .filter(|function| !function.is_module_initializer())
        .map(|function| {
            (
                (function.range.start_line, function.range.start_column),
                function.fqn.clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let effects = effects(&nodes, source);
    model.browser_script = dynamic_scopes.is_empty() && browser_script(root, &nodes, source, model);
    if model.browser_script {
        model.script_globals = globals(root, &nodes, source, &functions, &effects);
    }
    let calls = nodes
        .iter()
        .filter(|node| node.kind() == "call_expression")
        .map(|node| {
            let start = node.start_position();
            let end = node.end_position();
            (
                (start.row + 1, start.column + 1, end.row + 1, end.column + 1),
                *node,
            )
        })
        .collect::<BTreeMap<_, _>>();
    for function in &mut model.functions {
        for call in &mut function.calls {
            let Some(node) = calls
                .get(&(
                    call.range.start_line,
                    call.range.start_column,
                    call.range.end_line,
                    call.range.end_column,
                ))
                .copied()
            else {
                continue;
            };
            let Some(callee) = child(node, "function") else {
                continue;
            };
            if dynamic_scope(node, &dynamic_scopes) {
                continue;
            }
            if model.browser_script {
                call.script_global = script_lookup(callee, source);
            }
            let candidate = if let Some(receiver) = child(callee, "object") {
                let Some(object) =
                    value(receiver, node, source, 0).filter(|node| node.kind() == "object")
                else {
                    continue;
                };
                let Some(key) = member_key(callee, source) else {
                    continue;
                };
                if object_mutated(object, &key, Some(node), &effects) {
                    call.targets.clear();
                    call.confidence = Confidence::Dynamic;
                    call.binding_unknown = true;
                    call.local_binding = true;
                    continue;
                }
                object_member(object, &key, source)
                    .and_then(|member| value(member, node, source, 0))
            } else if callee.kind() == "identifier" {
                value(callee, node, source, 0)
            } else {
                None
            };
            let Some(candidate) = candidate.filter(|node| {
                is_function(node.kind(), Dialect::JavaScript)
                    && (call.receiver.is_some()
                        || !matches!(
                            node.kind(),
                            "function_declaration" | "generator_function_declaration"
                        ))
            }) else {
                continue;
            };
            let start = candidate.start_position();
            if let Some(target) = functions.get(&(start.row + 1, start.column + 1)) {
                call.targets = vec![target.clone()];
                call.confidence = Confidence::Resolved;
                call.binding_unknown = false;
                call.local_binding = true;
                call.script_global = None;
            }
        }
    }
    // Local proofs for an exported object share the same mutable browser value.
    // Keep their publication identity so project-wide writes can invalidate them.
    if model.browser_script {
        let published = model
            .script_globals
            .iter()
            .filter_map(|binding| {
                binding
                    .target
                    .as_ref()
                    .map(|target| (target.clone(), binding.name.clone()))
            })
            .collect::<BTreeMap<_, _>>();
        for function in &mut model.functions {
            for call in &mut function.calls {
                if call.script_global.is_none() && call.targets.len() == 1 {
                    call.script_global = published.get(&call.targets[0]).cloned();
                }
            }
        }
    }
}
