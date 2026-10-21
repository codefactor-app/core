//! UI metadata is indexed once per report, so navigation does not rescan all metrics.
use analyzer_core::{
    ir::{AccessorKind, FileModel, FunctionModel, TypeKind},
    rules::Severity,
    AnalysisReport,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub fn owner(file: &FileModel, f: &FunctionModel) -> Option<String> {
    if f.is_module_initializer() || f.parent_function.is_some() {
        return None;
    }
    let eligible = |kind: &TypeKind| {
        !matches!(
            kind,
            TypeKind::Namespace | TypeKind::AssociatedType | TypeKind::Other
        )
    };
    if let Some(parent) = f.parent_type.as_ref() {
        if file
            .types
            .iter()
            .find(|t| &t.fqn == parent)
            .is_none_or(|t| eligible(&t.kind))
        {
            return Some(parent.clone());
        }
    }
    // Recover an omitted owner only from an actual enclosing type range, not the FQN spelling.
    file.types
        .iter()
        .filter(|t| {
            eligible(&t.kind)
                && (t.range.start_line, t.range.start_column)
                    <= (f.range.start_line, f.range.start_column)
                && (t.range.end_line, t.range.end_column) >= (f.range.end_line, f.range.end_column)
                && !f.name.starts_with('<')
        })
        .min_by_key(|t| t.range.end_line.saturating_sub(t.range.start_line))
        .map(|t| t.fqn.clone())
}

pub fn index(report: &AnalysisReport) -> BTreeMap<String, Value> {
    let mut metrics = BTreeMap::<(&str, &str), BTreeMap<&str, f64>>::new();
    for m in &report.metrics {
        if m.scope.as_ref() == "function" {
            metrics
                .entry((&m.path, &m.entity))
                .or_default()
                .insert(&m.id, m.value);
        }
    }
    let mut inbound = BTreeMap::<&str, BTreeSet<&str>>::new();
    let mut outbound = BTreeMap::<&str, BTreeSet<&str>>::new();
    let mut edges_in = BTreeMap::<&str, usize>::new();
    let mut edges_out = BTreeMap::<&str, usize>::new();
    let mut graph_sites = BTreeMap::<&str, BTreeSet<&analyzer_core::SourceRange>>::new();
    let standard = report
        .graph
        .nodes
        .iter()
        .filter(|n| n.category.as_deref() == Some("runtime"))
        .map(|n| n.id.as_str())
        .collect::<BTreeSet<_>>();
    let source_ids = report
        .graph
        .nodes
        .iter()
        .filter(|node| node.source_fqn.is_some())
        .map(|node| (node.id.as_str(), node.source_id().into_owned()))
        .collect::<BTreeMap<_, _>>();
    let mut source_edges = BTreeSet::new();
    for e in &report.graph.edges {
        graph_sites.entry(&e.from).or_default().insert(&e.callsite);
        if standard.contains(e.from.as_ref()) || standard.contains(e.to.as_ref()) {
            continue;
        }
        inbound.entry(&e.to).or_default().insert(&e.from);
        outbound.entry(&e.from).or_default().insert(&e.to);
        *edges_in.entry(&e.to).or_default() += 1;
        *edges_out.entry(&e.from).or_default() += 1;
        let from = source_ids
            .get(e.from.as_ref())
            .map(String::as_str)
            .unwrap_or(&e.from);
        let to = source_ids
            .get(e.to.as_ref())
            .map(String::as_str)
            .unwrap_or(&e.to);
        if source_edges.insert((from, to, &e.callsite)) {
            if source_ids.contains_key(e.to.as_ref()) {
                inbound.entry(to).or_default().insert(from);
                *edges_in.entry(to).or_default() += 1;
            }
            if source_ids.contains_key(e.from.as_ref()) {
                graph_sites.entry(from).or_default().insert(&e.callsite);
                outbound.entry(from).or_default().insert(to);
                *edges_out.entry(from).or_default() += 1;
            }
        }
    }
    let mut violations = BTreeMap::<(&str, &str), (usize, usize)>::new();
    for v in &report.violations {
        if v.scope == "function" {
            let counts = violations.entry((&v.file, &v.entity)).or_default();
            if v.severity == Severity::Error {
                counts.0 += 1;
            } else {
                counts.1 += 1;
            }
        }
    }
    let graph_owners = report
        .graph
        .nodes
        .iter()
        .filter_map(|n| n.parent_type.as_ref().map(|owner| (n.id.as_str(), owner)))
        .collect::<BTreeMap<_, _>>();
    let mut result = BTreeMap::new();
    for file in &report.files {
        let with_nested = file
            .functions
            .iter()
            .filter_map(|f| f.parent_function.as_deref())
            .collect::<BTreeSet<_>>();
        for f in &file.functions {
            let id = format!("fn:{}:{}:{}", file.language, file.path, f.fqn);
            let owner = if !f.is_module_initializer() && f.parent_function.is_none() {
                graph_owners
                    .get(id.as_str())
                    .map(|owner| (*owner).clone())
                    .or_else(|| owner(file, f))
            } else {
                None
            };
            let m = metrics.get(&(file.path.as_str(), f.fqn.as_str()));
            let value =
                |key: &str, fallback: f64| m.and_then(|m| m.get(key)).copied().unwrap_or(fallback);
            let params = f.parameters.iter().filter(|p| !p.implicit_receiver).count();
            let (body_limit, param_limit) = match file.language.as_str() {
                "rust" => (4, 3),
                "php" | "python" | "javascript" | "typescript" => (3, 2),
                _ => (2, 1),
            };
            let body_lines = f
                .body_range
                .end_line
                .saturating_sub(f.body_range.start_line)
                + 1;
            let callsites = f
                .calls
                .iter()
                .map(|call| &call.range)
                .chain(
                    graph_sites
                        .get(id.as_str())
                        .into_iter()
                        .flat_map(|sites| sites.iter().copied()),
                )
                .collect::<BTreeSet<_>>()
                .len();
            let simple = !f.is_module_initializer()
                && callsites == 0
                && body_lines <= body_limit
                && params <= param_limit
                && f.complexity <= 1
                && f.nesting_depth == 0
                && file.parse_errors.is_empty()
                && !with_nested.contains(f.fqn.as_str());
            let behavior = if f.is_module_initializer() {
                "module_initializer"
            } else {
                match f.accessor {
                    Some(AccessorKind::Getter) => "getter",
                    Some(AccessorKind::Setter) => "setter",
                    None if simple => "simple_leaf",
                    _ => "regular",
                }
            };
            let counts = violations
                .get(&(file.path.as_str(), f.fqn.as_str()))
                .copied()
                .unwrap_or_default();
            result.insert(id.clone(),json!({"id":id,"name":f.name,"label":f.fqn,"language":file.language,"parent_type":owner,"owner_source":if owner.is_none() {None}else if f.parent_type==owner {Some("parser")}else if graph_owners.contains_key(id.as_str()){Some("resolved_type")}else{Some("enclosing_type")},"behavior":behavior,"purity":"not_proven","simple_threshold":{"body_lines":body_limit,"parameters":param_limit},"metrics":{
            "lines":value("function_lines",(f.range.end_line.saturating_sub(f.range.start_line)+1) as f64),"body_lines":body_lines,"parameters":value("function_params",params as f64),"complexity":value("cyclomatic_complexity",f.complexity as f64),"cognitive_complexity":m.and_then(|m|m.get("cognitive_complexity")),"nesting_depth":f.nesting_depth,"fan_in":edges_in.get(id.as_str()).copied().unwrap_or(0),"fan_out":edges_out.get(id.as_str()).copied().unwrap_or(0),"callers":inbound.get(id.as_str()).map(BTreeSet::len).unwrap_or(0),"callees":outbound.get(id.as_str()).map(BTreeSet::len).unwrap_or(0),"callsites":callsites,"errors":counts.0,"warnings":counts.1,"maintainability_index":m.and_then(|m|m.get("maintainability_index"))}}));
            if f.is_module_initializer() {
                let metadata = result.get_mut(&id).unwrap();
                metadata["name"] = json!("Инициализация модуля");
                metadata["simple_threshold"] = Value::Null;
                for key in [
                    "lines",
                    "body_lines",
                    "parameters",
                    "complexity",
                    "cognitive_complexity",
                    "nesting_depth",
                    "maintainability_index",
                ] {
                    metadata["metrics"][key] = Value::Null;
                }
            }
        }
    }
    let sites = report
        .files
        .iter()
        .flat_map(|file| {
            file.functions.iter().flat_map(move |f| {
                f.calls.iter().map(move |c| {
                    (
                        (
                            file.path.as_str(),
                            c.range.start_line,
                            c.range.start_column,
                            c.range.end_line,
                            c.range.end_column,
                        ),
                        c,
                    )
                })
            })
        })
        .collect::<BTreeMap<_, _>>();
    let known_types = report
        .files
        .iter()
        .flat_map(|file| {
            file.types
                .iter()
                .map(|t| (file.language.as_str(), t.fqn.as_str()))
        })
        .collect::<BTreeSet<_>>();
    let nodes = report
        .graph
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n))
        .collect::<BTreeMap<_, _>>();
    for edge in &report.graph.edges {
        let Some(node) = nodes
            .get(edge.to.as_ref())
            .filter(|n| n.kind == analyzer_core::graph::NodeKind::Unknown)
        else {
            continue;
        };
        let call = sites.get(&(
            edge.callsite.file.as_ref(),
            edge.callsite.start_line,
            edge.callsite.start_column,
            edge.callsite.end_line,
            edge.callsite.end_column,
        ));
        let ty = node.parent_type.as_deref().or_else(|| {
            node.label.rsplit_once("::").map(|(ty, _)| ty).filter(|_| {
                call.is_some_and(|c| {
                    c.receiver.is_some() || c.kind == analyzer_core::ir::CallKind::Constructor
                })
            })
        });
        let reason = if call.is_some_and(|c| c.binding_unknown) {
            "binding_unknown"
        } else if let Some(ty) = ty {
            if known_types.contains(&(node.language.as_deref().unwrap_or(""), ty)) {
                "method_not_found"
            } else {
                "type_not_analyzed"
            }
        } else if call.is_some_and(|c| c.receiver.is_some()) {
            "receiver_unknown"
        } else {
            "function_not_found"
        };
        let item = result.entry(node.id.clone()).or_insert_with(
            || json!({"parent_type":ty,"name":call.map(|c|c.name.as_str()),"target_status":if reason=="type_not_analyzed" {"outside_scope"}else{"unresolved"},"unresolved":{"reason":reason,"receiver_type":ty,"callsites":[],"total":0}}),
        );
        let detail = &mut item["unresolved"];
        detail["total"] = json!(detail["total"].as_u64().unwrap_or(0) + 1);
        if detail["callsites"].as_array().unwrap().len() < 50 {
            detail["callsites"].as_array_mut().unwrap().push(json!({"receiver":call.and_then(|c| c.receiver.as_ref()),"file":edge.callsite.file,"line":edge.callsite.start_line}));
        }
    }
    for node in report
        .graph
        .nodes
        .iter()
        .filter(|node| node.source_fqn.is_some())
    {
        let Some(mut metadata) = result.get(node.source_id().as_ref()).cloned() else {
            continue;
        };
        metadata["id"] = json!(node.id);
        metadata["label"] = json!(node.label);
        metadata["parent_type"] = json!(node.parent_type);
        metadata["cargo_target"] = json!(node.cargo_target);
        metadata["cargo_target_id"] = json!(node.cargo_target_id);
        metadata["source_fqn"] = json!(node.source_fqn);
        metadata["metrics"]["fan_in"] = json!(edges_in.get(node.id.as_str()).copied().unwrap_or(0));
        metadata["metrics"]["fan_out"] =
            json!(edges_out.get(node.id.as_str()).copied().unwrap_or(0));
        metadata["metrics"]["callers"] = json!(inbound
            .get(node.id.as_str())
            .map(BTreeSet::len)
            .unwrap_or(0));
        metadata["metrics"]["callees"] = json!(outbound
            .get(node.id.as_str())
            .map(BTreeSet::len)
            .unwrap_or(0));
        result.insert(node.id.clone(), metadata);
    }
    result
}

/// Keep class selection and frame labels consistent with recovered ownership.
pub fn prepare(report: &mut AnalysisReport) -> BTreeMap<String, Value> {
    let info = index(report);
    for node in &mut report.graph.nodes {
        if let Some(metadata) = info.get(&node.id) {
            node.parent_type = metadata["parent_type"].as_str().map(str::to_owned);
        }
    }
    info
}

#[cfg(test)]
mod tests {
    use super::*;
    use analyzer_core::LanguageStrategy;
    use std::path::Path;

    #[test]
    fn owner_recovery_uses_columns_for_declarations_on_the_same_line() {
        let file = analyzer_javascript::JavaScript.analyze(
            "class First{run(){}} class Second{run(){}} function free(){}",
            Path::new("owners.js"),
        );
        for function in &file.functions {
            let expected = if function.name == "free" {
                None
            } else {
                function.parent_type.clone()
            };
            let mut missing = function.clone();
            missing.parent_type = None;
            assert_eq!(owner(&file, &missing), expected, "{}", function.fqn);
        }
    }

    #[test]
    fn accessor_calls_count_source_sites_and_block_simple_leaf_label() {
        use analyzer_core::{Analyzer, Config, LanguageRegistry};
        let root = std::env::temp_dir().join(format!(
            "codefactor-accessor-metadata-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("store.ts"), "class Store { plain = 0; get value(){return 1;} set value(v:number){} }\nfunction update(s:Store){s.value += 1;}\nfunction ordinary(s:Store){s.plain += 1;}\n").unwrap();
        let mut registry = LanguageRegistry::new();
        registry.register(analyzer_typescript::TypeScript);
        let mut config = Config::default();
        config.cache.enabled = false;
        let report = Analyzer::new(registry, config).scan(&root).unwrap();
        std::fs::remove_dir_all(root).unwrap();
        let info = index(&report);
        let update = info.values().find(|item| item["name"] == "update").unwrap();
        assert_eq!(update["metrics"]["callsites"], 1);
        assert_eq!(update["metrics"]["fan_out"], 2);
        assert_eq!(update["behavior"], "regular");
        let ordinary = info
            .values()
            .find(|item| item["name"] == "ordinary")
            .unwrap();
        assert_eq!(ordinary["metrics"]["callsites"], 0);
        assert_eq!(ordinary["metrics"]["fan_out"], 0);
        assert_eq!(ordinary["behavior"], "simple_leaf");
    }

    #[test]
    fn cargo_context_metadata_keeps_target_identity_and_source_metrics() {
        use analyzer_core::{Analyzer, Config, LanguageRegistry};
        let root = std::env::temp_dir().join(format!(
            "codefactor-cargo-metadata-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname='app'\nversion='0.1.0'\n",
        )
        .unwrap();
        for entry in ["lib", "main"] {
            std::fs::write(
                root.join(format!("src/{entry}.rs")),
                "#[path=\"../shared.rs\"]mod shared;fn helper(){}fn flow(){shared::run();}\n",
            )
            .unwrap();
        }
        std::fs::write(root.join("shared.rs"), "pub fn run(){crate::helper();}\n").unwrap();
        let mut registry = LanguageRegistry::new();
        registry.register(analyzer_rust::Rust);
        let mut config = Config::default();
        config.cache.enabled = false;
        let mut report = Analyzer::new(registry, config).scan(&root).unwrap();
        let info = prepare(&mut report);
        let source = &info["fn:rust:shared.rs:shared::run"];
        assert_eq!(source["metrics"]["fan_in"], 2);
        assert_eq!(source["metrics"]["fan_out"], 2);
        for node in report
            .graph
            .nodes
            .iter()
            .filter(|node| node.source_fqn.is_some())
        {
            let metadata = &info[&node.id];
            assert_eq!(metadata["id"], node.id);
            assert_eq!(metadata["label"], node.label);
            assert_eq!(
                metadata["cargo_target"].as_str(),
                node.cargo_target.as_deref()
            );
            assert_eq!(
                metadata["cargo_target_id"].as_str(),
                node.cargo_target_id.as_deref()
            );
            assert_eq!(metadata["metrics"]["lines"], source["metrics"]["lines"]);
            assert_eq!(metadata["metrics"]["fan_in"], 1);
            assert_eq!(metadata["metrics"]["fan_out"], 1);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
