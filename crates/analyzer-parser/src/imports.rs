use crate::support::*;
use analyzer_core::ir::*;
use std::path::Path;
use tree_sitter::Node;

pub(super) fn esm_path(resource: &str) -> &str {
    [".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs"]
        .iter()
        .find_map(|extension| resource.strip_suffix(extension))
        .unwrap_or(resource)
}

pub(super) fn scope(node: Node<'_>, path: &Path) -> SourceRange {
    if node
        .parent()
        .is_some_and(|p| p.kind() == "import_statement")
    {
        // Python import pieces use the statement's enclosing scope.
        return scope(node.parent().unwrap(), path);
    }
    if node.kind() == "namespace_use_clause" || node.kind() == "namespace_use_declaration" {
        let mut ancestor = node.parent();
        while let Some(p) = ancestor {
            if p.kind() == "compound_statement" {
                break;
            }
            if p.kind() == "program" {
                let mut result = range(p, path);
                let position = node.start_byte();
                for n in named_children(p)
                    .into_iter()
                    .filter(|n| n.kind() == "namespace_definition" && child(*n, "body").is_none())
                {
                    if n.start_byte() <= position {
                        result.start_line = n.start_position().row + 1;
                        result.start_column = n.start_position().column + 1;
                    } else {
                        result.end_line = n.start_position().row + 1;
                        result.end_column = n.start_position().column + 1;
                        break;
                    }
                }
                return result;
            }
            ancestor = p.parent();
        }
    }
    let mut parent = node.parent();
    while let Some(n) = parent {
        if [
            "program",
            "module",
            "source_file",
            "block",
            "statement_block",
            "compound_statement",
            "declaration_list",
            "class_body",
        ]
        .contains(&n.kind())
            || is_function(n.kind(), Dialect::Python)
        {
            return range(n, path);
        }
        parent = n.parent();
    }
    range(node, path)
}
pub(super) fn lexical_module(node: Node<'_>, base: &str, source: &str, dialect: Dialect) -> String {
    if dialect != Dialect::Rust {
        return base.to_owned();
    }
    let mut names = Vec::new();
    let mut parent = node.parent();
    while let Some(n) = parent {
        if n.kind() == "mod_item" {
            if let Some(name) = child(n, "name") {
                names.push(snippet(name, source).to_owned());
            }
        }
        parent = n.parent();
    }
    names.reverse();
    std::iter::once(base.to_owned())
        .chain(names)
        .collect::<Vec<_>>()
        .join("::")
}
pub(super) fn bindings<'t>(
    node: Node<'t>,
    source: &str,
    dialect: Dialect,
) -> Vec<(String, Option<String>, Node<'t>)> {
    let mut out = Vec::new();
    if dialect == Dialect::Rust {
        fn walk<'t>(
            node: Node<'t>,
            prefix: &str,
            source: &str,
            out: &mut Vec<(String, Option<String>, Node<'t>)>,
        ) {
            let join = |name: &str| {
                if prefix.is_empty() {
                    name.to_owned()
                } else {
                    format!("{prefix}::{name}")
                }
            };
            match node.kind() {
                "use_declaration" => {
                    if let Some(n) = child(node, "argument") {
                        walk(n, prefix, source, out);
                    }
                }
                "scoped_use_list" => {
                    let p = child(node, "path")
                        .map(|n| join(snippet(n, source)))
                        .unwrap_or_else(|| prefix.to_owned());
                    if let Some(list) = child(node, "list") {
                        walk(list, &p, source, out);
                    }
                }
                "use_list" => {
                    for n in named_children(node) {
                        if !n.kind().contains("comment") {
                            walk(n, prefix, source, out);
                        }
                    }
                }
                "use_as_clause" => {
                    if let Some(p) = child(node, "path") {
                        let raw = snippet(p, source);
                        let path = if raw == "self" && !prefix.is_empty() {
                            prefix.to_owned()
                        } else {
                            join(raw)
                        };
                        out.push((path, text_field(node, "alias", source), node));
                    }
                }
                "use_wildcard" => out.push((join(snippet(node, source)), Some("*".into()), node)),
                _ => {
                    let raw = snippet(node, source);
                    let path = if raw == "self" && !prefix.is_empty() {
                        prefix.to_owned()
                    } else {
                        join(raw)
                    };
                    out.push((path, None, node));
                }
            }
        }
        walk(node, "", source, &mut out);
        return out;
    }
    let Some(module) = child(node, "source")
        .or_else(|| first_kind(node, &["string"]))
        .or_else(|| {
            first_kind(node, &["import_require_clause"]).and_then(|n| first_kind(n, &["string"]))
        })
    else {
        return out;
    };
    let Some(raw) = js_module_literal(snippet(module, source)) else {
        return out;
    };
    let module = [".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs"]
        .iter()
        .find_map(|ext| raw.strip_suffix(ext))
        .unwrap_or(&raw);
    let Some(clause) = first_kind(node, &["import_clause"]) else {
        let require = if node.kind() == "import_require_clause" {
            Some(node)
        } else {
            first_kind(node, &["import_require_clause"])
        };
        if let Some(require) = require {
            if let Some(alias) = first_kind(require, &["identifier"]) {
                out.push((
                    module.to_owned(),
                    Some(snippet(alias, source).to_owned()),
                    require,
                ));
            }
        }
        return out;
    };
    for item in named_children(clause) {
        match item.kind() {
            "identifier" => out.push((
                format!("{module}.default"),
                Some(snippet(item, source).to_owned()),
                item,
            )),
            "namespace_import" => {
                if let Some(alias) = first_kind(item, &["identifier"]) {
                    out.push((
                        module.to_owned(),
                        Some(snippet(alias, source).to_owned()),
                        item,
                    ));
                }
            }
            "named_imports" => {
                for spec in named_children(item)
                    .into_iter()
                    .filter(|n| n.kind() == "import_specifier")
                {
                    if let Some(name) = child(spec, "name") {
                        let name = snippet(name, source).trim_matches(['\'', '"']);
                        let alias =
                            text_field(spec, "alias", source).unwrap_or_else(|| name.to_owned());
                        out.push((format!("{module}.{name}"), Some(alias), spec));
                    }
                }
            }
            _ => {}
        }
    }
    out
}
pub(super) fn collect_exports(
    root: Node<'_>,
    source: &str,
    model: &mut FileModel,
    dialect: Dialect,
) {
    if !matches!(dialect, Dialect::JavaScript | Dialect::TypeScript) {
        return;
    }
    for node in named_children(root)
        .into_iter()
        .filter(|n| n.kind() == "export_statement")
    {
        let type_only = dialect == Dialect::TypeScript && has_type_modifier(node);
        let imported = if let Some(literal) = text_field(node, "source", source) {
            let Some(raw) = js_module_literal(&literal) else {
                continue;
            };
            Some(raw)
        } else {
            None
        };
        if imported.is_some()
            && named_children(node)
                .iter()
                .all(|child| child.kind() != "namespace_export")
        {
            let mut cursor = node.walk();
            if node.children(&mut cursor).any(|child| child.kind() == "*") {
                model.exports.push(Export {
                    name: "*".into(),
                    target: "*".into(),
                    source: imported.clone(),
                    type_only,
                });
            }
        }
        if let Some(clause) = first_kind(node, &["export_clause"]) {
            for spec in named_children(clause)
                .into_iter()
                .filter(|n| n.kind() == "export_specifier")
            {
                if let Some(target) = text_field(spec, "name", source) {
                    let name = text_field(spec, "alias", source).unwrap_or_else(|| target.clone());
                    model.exports.push(Export {
                        name,
                        target: if imported.is_none() {
                            format!("{}.{}", model.module, target)
                        } else {
                            target
                        },
                        source: imported.clone(),
                        type_only: type_only
                            || (dialect == Dialect::TypeScript && has_type_modifier(spec)),
                    });
                }
            }
        }
        if let Some(namespace) = first_kind(node, &["namespace_export"]).filter(|_| type_only) {
            if let Some(name) = namespace.named_child(0) {
                model.exports.push(Export {
                    name: snippet(name, source).trim_matches(['\'', '"']).to_owned(),
                    target: String::new(),
                    source: imported.clone(),
                    type_only,
                });
            }
        }
        if let Some(decl) = child(node, "declaration").or_else(|| child(node, "value")) {
            if matches!(decl.kind(), "lexical_declaration" | "variable_declaration") {
                for declarator in named_children(decl) {
                    if let Some(name) =
                        child(declarator, "name").filter(|name| name.kind() == "identifier")
                    {
                        let name = snippet(name, source).to_owned();
                        model.exports.push(Export {
                            target: format!("{}.{}", model.module, name),
                            name,
                            source: None,
                            type_only,
                        });
                    }
                }
                continue;
            }
            let default = {
                let mut c = node.walk();
                let found = node.children(&mut c).any(|n| n.kind() == "default");
                found
            };
            let target = model
                .types
                .iter()
                .find(|t| {
                    t.range.start_line == decl.start_position().row + 1
                        && t.range.start_column == decl.start_position().column + 1
                })
                .map(|t| t.fqn.clone())
                .or_else(|| {
                    model
                        .functions
                        .iter()
                        .find(|f| {
                            f.range.start_line == decl.start_position().row + 1
                                && f.range.start_column == decl.start_position().column + 1
                        })
                        .map(|f| f.fqn.clone())
                })
                .or_else(|| {
                    text_field(decl, "name", source)
                        .map(|name| format!("{}.{}", model.module, name))
                })
                .or_else(|| {
                    (decl.kind() == "identifier")
                        .then(|| format!("{}.{}", model.module, snippet(decl, source)))
                });
            if let Some(target) = target {
                let name = if default {
                    "default".to_owned()
                } else {
                    target.rsplit('.').next().unwrap_or(&target).to_owned()
                };
                model.exports.push(Export {
                    name,
                    target,
                    source: None,
                    type_only,
                });
            }
        }
    }
}

fn has_type_modifier(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    let has_type = node
        .children(&mut cursor)
        .any(|child| child.kind() == "type");
    has_type
}

pub(super) fn is_type_only_binding(node: Node<'_>) -> bool {
    let mut current = Some(node);
    while let Some(node) = current {
        if matches!(node.kind(), "import_statement" | "import_specifier") && has_type_modifier(node)
        {
            return true;
        }
        if node.kind() == "import_statement" {
            break;
        }
        current = node.parent();
    }
    false
}

/// Literal namespace require bindings only. Lexical annotation still decides
/// whether require or its assigned variable is shadowed at a particular call.
pub(super) fn collect_requires(
    root: Node<'_>,
    source: &str,
    path: &Path,
    imports: &mut Vec<Import>,
    dialect: Dialect,
) {
    if !matches!(dialect, Dialect::JavaScript | Dialect::TypeScript) {
        return;
    }
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        pending.extend(named_children(node));
        if node.kind() != "variable_declarator" {
            continue;
        }
        let Some(alias) = child(node, "name").filter(|node| node.kind() == "identifier") else {
            continue;
        };
        let Some(value) = child(node, "value").filter(|node| node.kind() == "call_expression")
        else {
            continue;
        };
        if child(value, "function")
            .is_none_or(|node| node.kind() != "identifier" || snippet(node, source) != "require")
        {
            continue;
        }
        let Some(arguments) = child(value, "arguments") else {
            continue;
        };
        let arguments = named_children(arguments);
        let [argument] = arguments.as_slice() else {
            continue;
        };
        if argument.kind() != "string" {
            continue;
        }
        let resource = snippet(*argument, source).trim_matches(['\'', '"']);
        if resource.contains('\\') {
            continue;
        }
        let normalized = esm_path(resource);
        imports.push(Import {
            path: normalized.to_owned(),
            source: Some(resource.to_owned()),
            require: true,
            alias: Some(snippet(alias, source).to_owned()),
            scope: Some(scope(node, path)),
            range: range(node, path),
            ..Import::default()
        });
    }
}
