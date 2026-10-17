//! Human-readable renderers for the same report and graph query values exported as JSON.
use crate::graph::{Direction, GraphEdge, GraphNode, GraphQuery};
use crate::ir::{FileModel, FunctionModel};
use crate::metrics::MetricValue;
use crate::report::AnalysisReport;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

pub fn scan_markdown(report: &AnalysisReport, only_violations: bool) -> String {
    let mut out = String::new();
    writeln!(out, "# Codefactor report\n").unwrap();
    writeln!(out, "- Version: {}", cell(&report.version)).unwrap();
    writeln!(out, "- Date: {} UTC", utc_date()).unwrap();
    writeln!(out, "- Root: `{}`", inline(&report.root)).unwrap();
    writeln!(
        out,
        "- Configuration: {}",
        report
            .config_path
            .as_deref()
            .map(|s| format!("`{}`", inline(s)))
            .unwrap_or_else(|| "defaults".into())
    )
    .unwrap();
    writeln!(out, "- Schema version: {}\n", cell(&report.schema_version)).unwrap();
    writeln!(out, "## Summary\n").unwrap();
    writeln!(out, "| Files | Functions | Types | Warnings | Errors | Parse errors | Vendor files | Parsed files | Cache hits |\n|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n| {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",report.summary.files,report.summary.functions,report.summary.types,report.summary.warnings,report.summary.errors,report.summary.parse_errors,report.summary.vendor_files,report.summary.parsed_files,report.summary.cache_hits).unwrap();
    if !report.resolution_diagnostics.is_empty() {
        writeln!(out, "## Resolution diagnostics\n").unwrap();
        for diagnostic in &report.resolution_diagnostics {
            writeln!(out, "- {}", cell(diagnostic)).unwrap();
        }
        out.push('\n');
    }
    if !report.strategy_capabilities.is_empty() {
        writeln!(out, "## Strategy capabilities\n\n| Language | Static types | Generics | Inheritance / traits | Closures | Guards |\n|---|---|---|---|---|---|").unwrap();
        for (language, caps) in &report.strategy_capabilities {
            let mark = |supported| if supported { "yes" } else { "no" };
            writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} |",
                cell(language),
                mark(caps.static_types),
                mark(caps.generics),
                mark(caps.inheritance),
                mark(caps.closures),
                mark(caps.guards)
            )
            .unwrap();
        }
        writeln!(out, "\nStrategy declarations describe supported constructs, not a guarantee of complete type or call resolution.\n").unwrap();
    }
    writeln!(out, "## Violations\n").unwrap();
    if report.violations.is_empty() {
        writeln!(out, "None.\n").unwrap();
    } else {
        writeln!(out,"| Severity | Rule | Scope | Entity | File:line | Value | Limit |\n|---|---|---|---|---|---:|---:|").unwrap();
        for v in &report.violations {
            writeln!(
                out,
                "| {:?} | {} | {} | {} | {}:{} | {} | {} |",
                v.severity,
                cell(&v.rule),
                cell(&v.scope),
                cell(&v.entity),
                cell(&v.file),
                v.line,
                cell(&v.value_text()),
                cell(&v.limit_text())
            )
            .unwrap();
        }
        out.push('\n');
        for v in &report.violations {
            if v.algorithmic_complexity.is_some() {
                writeln!(out, "- {}\n", cell(&v.message())).unwrap();
            }
        }
    }
    writeln!(out, "## Architecture\n").unwrap();
    writeln!(
        out,
        "Profile: {} · Warnings: {} · Non-blocking, including --fail-on-warn.\n",
        cell(&report.architecture.profile),
        report.architecture.warnings
    )
    .unwrap();
    for rule in &report.architecture.rules {
        writeln!(
            out,
            "- {}: {} — {}",
            cell(&rule.rule),
            cell(&rule.status),
            cell(&rule.reason)
        )
        .unwrap();
    }
    for issue in &report.violations {
        if let Some(d) = &issue.architecture {
            writeln!(
                out,
                "\n### {} · {}\n\n**Факт:** {}\n\n**Оценка:** {}\n",
                cell(&d.title),
                cell(&d.principle),
                cell(&d.fact),
                cell(&d.hypothesis)
            )
            .unwrap();
            if let Some(cycle) = &d.cycle {
                writeln!(out, "**Замкнутый путь:**\n").unwrap();
                for (index, module) in cycle.path.iter().enumerate() {
                    writeln!(out, "{}. `{}`", index + 1, inline(module)).unwrap();
                }
                out.push('\n');
            }
            writeln!(out, "**Основания · места в коде:**\n").unwrap();
            for e in &d.evidence {
                writeln!(
                    out,
                    "- {}:{} — {}",
                    cell(&e.range.file),
                    e.range.start_line,
                    cell(&e.description)
                )
                .unwrap();
            }
            if d.evidence_total > d.evidence.len() {
                writeln!(
                    out,
                    "- Evidence shown: {} / {}",
                    d.evidence.len(),
                    d.evidence_total
                )
                .unwrap();
            }
            for r in &d.recommendations {
                writeln!(out, "\n**{}:** {}\n", cell(&r.pattern), cell(&r.proposal)).unwrap();
                for c in &r.conditions {
                    writeln!(out, "- {}", cell(c)).unwrap();
                }
            }
            for limit in &d.limitations {
                writeln!(out, "- Limitation: {}", cell(limit)).unwrap();
            }
        }
    }
    if only_violations {
        return out;
    }
    writeln!(out, "## Aggregates\n").unwrap();
    for group in &report.aggregates {
        writeln!(out, "### {}\n", heading(&group.name)).unwrap();
        writeln!(out,"| Metric | Average | Median | Maximum | p90 | p95 | Histogram (start–end: count) |\n|---|---:|---:|---:|---:|---:|---|").unwrap();
        for (id, d) in &group.metrics {
            let bins = d
                .histogram
                .iter()
                .map(|b| format!("{:.2}–{:.2}: {}", b.start, b.end, b.count))
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(
                out,
                "| {} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {} |",
                cell(id),
                d.avg,
                d.median,
                d.max,
                d.p90,
                d.p95,
                cell(&bins)
            )
            .unwrap();
        }
        out.push('\n');
    }
    let metric_index = metric_index(report.metrics.iter());
    writeln!(out, "## Source hierarchy\n").unwrap();
    let mut directories: BTreeMap<String, Vec<&FileModel>> = BTreeMap::new();
    for file in &report.files {
        let dir = std::path::Path::new(&file.path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| ".".into());
        directories.entry(dir).or_default().push(file);
    }
    for (dir, files) in directories {
        writeln!(out, "### Directory `{}`\n", inline(&dir)).unwrap();
        for file in files {
            writeln!(
                out,
                "#### File `{}`{}\n",
                inline(&file.path),
                if file.vendor { " (vendor)" } else { "" }
            )
            .unwrap();
            writeln!(
                out,
                "Language: {} · Module: `{}` · LOC: {} total, {} code, {} comment, {} blank.\n",
                cell(&file.language),
                inline(&file.module),
                file.loc.total,
                file.loc.code,
                file.loc.comment,
                file.loc.blank
            )
            .unwrap();
            entity_metrics(&mut out, &metric_index, &file.path, &file.path, "file");
            for ty in &file.types {
                writeln!(
                    out,
                    "##### {:?} `{}` (line {})\n",
                    ty.kind,
                    inline(&ty.fqn),
                    ty.range.start_line
                )
                .unwrap();
                writeln!(
                    out,
                    "Fields: {} · Methods: {}.\n",
                    ty.fields.len(),
                    ty.methods.len()
                )
                .unwrap();
                entity_metrics(&mut out, &metric_index, &file.path, &ty.fqn, "class");
                for fun in file
                    .functions
                    .iter()
                    .filter(|f| f.parent_type.as_deref() == Some(&ty.fqn))
                {
                    function_section(&mut out, &metric_index, file, fun, 6);
                }
            }
            for fun in file.functions.iter().filter(|f| f.parent_type.is_none()) {
                function_section(&mut out, &metric_index, file, fun, 5);
            }
        }
    }
    writeln!(out, "## Parse errors\n").unwrap();
    if report.parse_errors.is_empty() {
        writeln!(out, "None.\n").unwrap();
    } else {
        for error in &report.parse_errors {
            writeln!(
                out,
                "- `{}`:{}:{} — {}",
                inline(&error.range.file),
                error.range.start_line,
                error.range.start_column,
                cell(&error.message)
            )
            .unwrap();
        }
        out.push('\n');
    }
    writeln!(out, "## Call graph\n").unwrap();
    graph_nodes(&mut out, &report.graph.nodes);
    graph_edges(&mut out, &report.graph.nodes, &report.graph.edges);
    out.push_str("\n```mermaid\n");
    mermaid_edges(&mut out, &report.graph.nodes, &report.graph.edges);
    out.push_str("```\n");
    if !report.deferred_vendor.is_empty() {
        writeln!(out, "\n## Deferred vendor files\n").unwrap();
        for entry in &report.deferred_vendor {
            writeln!(
                out,
                "- `{}` · {} · {}",
                inline(&entry.path),
                cell(&entry.package),
                cell(&entry.language)
            )
            .unwrap();
        }
    }
    out
}

pub fn graph_markdown(query: &GraphQuery) -> String {
    let mut out = String::new();
    writeln!(out, "# Call graph: {:?}\n", query.direction).unwrap();
    writeln!(
        out,
        "Depth: {}\n",
        query
            .depth
            .map(|d| d.to_string())
            .unwrap_or_else(|| "unlimited".into())
    )
    .unwrap();
    coverage(&mut out, query);
    writeln!(out, "## Call chains\n").unwrap();
    let nodes = query
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n))
        .collect::<BTreeMap<_, _>>();
    let mut adjacency: BTreeMap<&str, Vec<(&str, &GraphEdge)>> = BTreeMap::new();
    for edge in &query.edges {
        let (a, b) = if query.direction == Direction::Callers {
            (&edge.to, &edge.from)
        } else {
            (&edge.from, &edge.to)
        };
        adjacency.entry(a).or_default().push((b, edge));
        if query.direction == Direction::Both {
            adjacency.entry(b).or_default().push((a, edge));
        }
    }
    for links in adjacency.values_mut() {
        links.sort_by(|a, b| {
            a.0.cmp(b.0)
                .then(a.1.callsite.start_line.cmp(&b.1.callsite.start_line))
        });
    }
    for root in &query.roots {
        let mut path = BTreeSet::new();
        write_chain(&mut out, root, 0, None, &nodes, &adjacency, &mut path);
    }
    if query.roots.is_empty() {
        out.push_str("No matching function.\n");
    }
    writeln!(out, "\n## Edges\n").unwrap();
    graph_edges(&mut out, &query.nodes, &query.edges);
    writeln!(out, "\n## Nodes\n").unwrap();
    graph_nodes(&mut out, &query.nodes);
    if !query.cycles.is_empty() {
        writeln!(out, "\n## Cycles\n").unwrap();
        graph_edges(&mut out, &query.nodes, &query.cycles);
    }
    if !query.hidden_vendor_edges.is_empty() {
        writeln!(out, "\n## Hidden vendor edges\n").unwrap();
        graph_edges(&mut out, &query.nodes, &query.hidden_vendor_edges);
    }
    out.push_str("\n## Diagram\n\n```mermaid\n");
    mermaid_edges(&mut out, &query.nodes, &query.edges);
    out.push_str("```\n");
    out
}

fn function_section(
    out: &mut String,
    metrics: &MetricIndex<'_>,
    file: &FileModel,
    fun: &FunctionModel,
    level: usize,
) {
    let hashes = "#".repeat(level);
    writeln!(
        out,
        "{hashes} Function `{}` (lines {}–{})\n",
        inline(&fun.fqn),
        fun.range.start_line,
        fun.range.end_line
    )
    .unwrap();
    writeln!(out, "Signature: `{}`\n", inline(&fun.signature)).unwrap();
    if !fun.parameters.is_empty() {
        writeln!(
            out,
            "Parameters: {}.\n",
            fun.parameters
                .iter()
                .map(|p| format!(
                    "`{}`{}{}",
                    inline(&p.name),
                    p.type_name
                        .as_deref()
                        .map(|t| format!(": `{}`", inline(t)))
                        .unwrap_or_default(),
                    if p.variadic { " (variadic)" } else { "" }
                ))
                .collect::<Vec<_>>()
                .join(", ")
        )
        .unwrap();
    }
    if let Some(e) = &fun.algorithmic_complexity {
        writeln!(
            out,
            "Algorithmic execution cost: **{}** · {}.\n",
            cell(e.formula.as_deref().unwrap_or(e.status.label())),
            cell(e.status.label())
        )
        .unwrap();
        if let Some(local) = &e.local_formula {
            writeln!(out, "Local cost: {}.\n", cell(local)).unwrap();
        }
        if let Some(n) = &e.normalized {
            writeln!(out, "Common input scale: {}.\n", cell(n)).unwrap();
        }
        for v in &e.variables {
            writeln!(out, "- {}: {}", cell(&v.symbol), cell(&v.definition)).unwrap();
        }
        for reason in &e.unknown_reasons {
            writeln!(out, "- Unknown: {}", cell(reason)).unwrap();
        }
        for assumption in &e.assumptions {
            writeln!(out, "- Assumption: {}", cell(assumption)).unwrap();
        }
        for step in &e.explanations {
            writeln!(
                out,
                "- {}:{} · {}",
                cell(&step.range.file),
                step.range.start_line,
                cell(&step.detail)
            )
            .unwrap();
        }
        writeln!(out, "\nCost model: {}.\n", cell(&e.model)).unwrap();
    }
    entity_metrics(out, metrics, &file.path, &fun.fqn, "function");
    if !fun.calls.is_empty() {
        writeln!(out, "Calls:\n").unwrap();
        for c in &fun.calls {
            writeln!(
                out,
                "- `{}` at line {}{}{}",
                inline(&c.name),
                c.range.start_line,
                condition_text(&c.guard),
                loop_text(&c.in_loop)
            )
            .unwrap();
        }
        out.push('\n');
    }
}
type MetricIndex<'a> = BTreeMap<(&'a str, &'a str, &'a str), Vec<&'a MetricValue>>;
fn metric_index<'a>(values: impl IntoIterator<Item = &'a MetricValue>) -> MetricIndex<'a> {
    let mut index = MetricIndex::new();
    for metric in values {
        index
            .entry((
                metric.path.as_ref(),
                metric.entity.as_ref(),
                metric.scope.as_ref(),
            ))
            .or_default()
            .push(metric);
    }
    index
}

fn entity_metrics(
    out: &mut String,
    index: &MetricIndex<'_>,
    path: &str,
    entity: &str,
    scope: &str,
) {
    let metrics = index
        .get(&(path, entity, scope))
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if !metrics.is_empty() {
        out.push_str("| Metric | Value |\n|---|---:|\n");
        for m in metrics {
            writeln!(out, "| {} | {:.2} |", cell(&m.id), m.value).unwrap();
        }
        out.push('\n');
    }
}
fn coverage(out: &mut String, query: &GraphQuery) {
    let c = &query.coverage;
    writeln!(out, "## Coverage\n").unwrap();
    writeln!(
        out,
        "Functions: {} · Classes: {} · Files: {} · Modules: {} · External: {} · Unresolved: {}.\n",
        c.functions.len(),
        c.classes.len(),
        c.files.len(),
        c.modules.len(),
        c.external.len(),
        c.unresolved.len()
    )
    .unwrap();
    for (label, values) in [
        ("Functions", &c.functions),
        ("Classes", &c.classes),
        ("Files", &c.files),
        ("Modules", &c.modules),
        ("External operations", &c.external),
        ("Unresolved calls", &c.unresolved),
        ("Entry points", &c.entry_points),
        ("Leaves", &c.leaves),
        ("Vendor functions", &c.vendor_functions),
        ("Vendor classes", &c.vendor_classes),
    ] {
        if !values.is_empty() {
            writeln!(
                out,
                "- {}: {}",
                label,
                values
                    .iter()
                    .map(|s| format!("`{}`", inline(s)))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
            .unwrap();
        }
    }
    if c.vendor_boundary {
        out.push_str("- Vendor boundary: some vendor code was not parsed.\n");
    }
    out.push('\n');
}
fn write_chain<'a>(
    out: &mut String,
    id: &'a str,
    depth: usize,
    via: Option<&GraphEdge>,
    nodes: &BTreeMap<&'a str, &GraphNode>,
    adjacency: &BTreeMap<&'a str, Vec<(&'a str, &GraphEdge)>>,
    path: &mut BTreeSet<&'a str>,
) {
    let label = nodes.get(id).map(|n| n.label.as_str()).unwrap_or(id);
    let prefix = "  ".repeat(depth);
    write!(out, "{prefix}- `{}`", inline(label)).unwrap();
    if let Some(edge) = via {
        write!(
            out,
            " — `{}`:{} ({:?}){}{}",
            inline(&edge.callsite.file),
            edge.callsite.start_line,
            edge.confidence,
            condition_text(&edge.guard),
            loop_text(&edge.in_loop)
        )
        .unwrap();
    }
    if !path.insert(id) {
        out.push_str(" (already on this path)\n");
        return;
    }
    out.push('\n');
    if let Some(links) = adjacency.get(id) {
        for (next, edge) in links {
            write_chain(out, next, depth + 1, Some(edge), nodes, adjacency, path);
        }
    }
    path.remove(id);
}
fn graph_edges(out: &mut String, nodes: &[GraphNode], edges: &[GraphEdge]) {
    let names = nodes
        .iter()
        .map(|n| (n.id.as_str(), n.label.as_str()))
        .collect::<BTreeMap<_, _>>();
    if edges.is_empty() {
        out.push_str("No edges.\n");
        return;
    }
    out.push_str("| Caller | Callee | Callsite | Confidence | Conditions | Loop | External argument |\n|---|---|---|---|---|---|---|\n");
    for edge in edges {
        writeln!(
            out,
            "| {} | {} | {}:{} | {:?} | {} | {} | {} |",
            cell(names.get(edge.from.as_ref()).copied().unwrap_or(&edge.from)),
            cell(names.get(edge.to.as_ref()).copied().unwrap_or(&edge.to)),
            cell(&edge.callsite.file),
            edge.callsite.start_line,
            edge.confidence,
            cell(&conditions(&edge.guard)),
            cell(&loops(&edge.in_loop)),
            cell(edge.argument.as_deref().unwrap_or("—"))
        )
        .unwrap();
    }
}
fn graph_nodes(out: &mut String, nodes: &[GraphNode]) {
    if nodes.is_empty() {
        out.push_str("No nodes.\n");
        return;
    }
    out.push_str("| Node | Kind | File:line | Language | Module | Type | Package | External category / operation / argument |\n|---|---|---|---|---|---|---|---|\n");
    for node in nodes {
        let external = [
            node.category.as_deref(),
            node.library.as_deref(),
            node.operation.as_deref(),
            node.argument.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(cell)
        .collect::<Vec<_>>()
        .join(" · ");
        writeln!(
            out,
            "| {} | {:?} | {}:{} | {} | {} | {} | {} | {} |",
            cell(&node.display_label()),
            node.kind,
            cell(node.file.as_deref().unwrap_or("")),
            node.line.map(|n| n.to_string()).unwrap_or_default(),
            cell(node.language.as_deref().unwrap_or("")),
            cell(node.module.as_deref().unwrap_or("")),
            cell(node.parent_type.as_deref().unwrap_or("")),
            cell(node.package.as_deref().unwrap_or("")),
            external
        )
        .unwrap();
    }
    out.push('\n');
}
fn mermaid_edges(out: &mut String, nodes: &[GraphNode], edges: &[GraphEdge]) {
    out.push_str("flowchart LR\n");
    let ids = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), format!("n{i}")))
        .collect::<BTreeMap<_, _>>();
    for node in nodes {
        let id = &ids[node.id.as_str()];
        writeln!(out, "  {id}[\"{}\"]", mermaid_label(&node.display_label())).unwrap();
    }
    for edge in edges {
        let (Some(a), Some(b)) = (ids.get(edge.from.as_ref()), ids.get(edge.to.as_ref())) else {
            continue;
        };
        let label = conditions(&edge.guard);
        if label.is_empty() {
            writeln!(out, "  {a} --> {b}").unwrap();
        } else {
            writeln!(out, "  {a} -->|\"{}\"| {b}", mermaid_label(&label)).unwrap();
        }
    }
}
fn condition_text(guards: &[crate::ir::Guard]) -> String {
    let text = conditions(guards);
    if text.is_empty() {
        String::new()
    } else {
        format!(" · if {text}")
    }
}
fn loop_text(contexts: &[crate::ir::LoopContext]) -> String {
    let text = loops(contexts);
    if text.is_empty() {
        String::new()
    } else {
        format!(" · in loop {text}")
    }
}
fn conditions(guards: &[crate::ir::Guard]) -> String {
    guards
        .iter()
        .map(|g| format!("{}: {} [raw: {}]", g.branch, g.normalized, g.raw))
        .collect::<Vec<_>>()
        .join(" ∧ ")
}
fn loops(contexts: &[crate::ir::LoopContext]) -> String {
    contexts
        .iter()
        .map(|l| format!("{} (line {})", l.header, l.line))
        .collect::<Vec<_>>()
        .join(" → ")
}
fn inline(s: &str) -> String {
    s.replace('`', "'").replace('\n', " ")
}
fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}
fn heading(s: &str) -> String {
    s.replace('\n', " ")
}
fn mermaid_label(s: &str) -> String {
    s.replace('"', "'").replace('|', "/").replace('\n', " ")
}
fn utc_date() -> String {
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 86_400;
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1
    }
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod index_tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn metric_index_visits_input_once_and_preserves_entity_row_order() {
        let values = (0..10_000)
            .flat_map(|index| {
                ["second_metric", "first_metric"].map(move |id| MetricValue {
                    id: id.into(),
                    scope: "function".into(),
                    entity: format!("function_{index}").into(),
                    path: "file.ts".into(),
                    line: 1,
                    value: index as f64,
                })
            })
            .collect::<Vec<_>>();
        let visits = Cell::new(0);
        let index = metric_index(values.iter().inspect(|_| visits.set(visits.get() + 1)));
        assert_eq!(visits.get(), values.len());
        for entity in 0..10_000 {
            let name = format!("function_{entity}");
            let rows = &index[&("file.ts", name.as_str(), "function")];
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].id.as_ref(), "second_metric");
            assert_eq!(rows[1].id.as_ref(), "first_metric");
        }
        assert_eq!(visits.get(), values.len());
        let mut actual = String::new();
        entity_metrics(&mut actual, &index, "file.ts", "function_7", "function");
        assert_eq!(
            actual,
            "| Metric | Value |\n|---|---:|\n| second_metric | 7.00 |\n| first_metric | 7.00 |\n\n"
        );
    }
}
