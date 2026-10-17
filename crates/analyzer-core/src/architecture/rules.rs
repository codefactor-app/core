use super::graph::{components, is_abstract, selector_matches, witness, Symbols};
use super::*;
use crate::graph::{CallGraph, NodeKind};
use sha2::{Digest, Sha256};

fn module_key(file: &FileModel) -> String {
    format!("{}:{}", file.language, file.module)
}
fn module_graph(deps: &[Dependency], symbols: &Symbols<'_>) -> BTreeMap<String, BTreeSet<String>> {
    let keys = symbols
        .files
        .iter()
        .map(|(path, file)| (*path, module_key(file)))
        .collect::<BTreeMap<_, _>>();
    let mut edges = BTreeMap::<_, BTreeSet<_>>::new();
    for d in deps {
        let a = &keys[d.from];
        let b = &keys[d.to];
        if a != b {
            if !edges.contains_key(a) {
                edges.insert(a.clone(), BTreeSet::new());
            }
            let targets = edges.get_mut(a).unwrap();
            if !targets.contains(b) {
                targets.insert(b.clone());
            }
        }
    }
    edges
}
pub(super) fn cycles(
    deps: &[Dependency],
    symbols: &Symbols<'_>,
    config: &ArchitectureConfig,
    out: &mut Vec<Violation>,
) {
    // Only operations on values establish this rule's cycle. The complete static
    // dependency graph remains available to explicit boundaries and stability.
    let runtime = deps
        .iter()
        .filter(|d| matches!(d.kind, "call" | "member_access" | "variable_access"))
        .cloned()
        .collect::<Vec<_>>();
    let deps = runtime.as_slice();
    let edges = module_graph(deps, symbols);
    let keys = symbols
        .files
        .iter()
        .map(|(path, file)| (*path, module_key(file)))
        .collect::<BTreeMap<_, _>>();
    let groups = components(&edges)
        .into_iter()
        .filter(|g| g.len() > 1)
        .collect::<Vec<_>>();
    if groups.is_empty() {
        return;
    }
    let membership = groups
        .iter()
        .enumerate()
        .flat_map(|(index, group)| group.iter().map(move |name| (name.as_str(), index)))
        .collect::<BTreeMap<_, _>>();
    // Only lookup uses this hash index; output still follows sorted components and
    // the original dependency order, independent of randomized map iteration.
    let file_components = keys
        .iter()
        .filter_map(|(file, module)| membership.get(module.as_str()).map(|index| (*file, *index)))
        .collect::<std::collections::HashMap<_, _>>();
    let mut by_pair = BTreeMap::new();
    let mut participant_edges = vec![Vec::new(); groups.len()];
    let mut internal_edges = vec![Vec::new(); groups.len()];
    let mut indexed_sources = BTreeSet::new();
    for d in deps {
        let (Some(&from), Some(&to)) = (file_components.get(d.from), file_components.get(d.to))
        else {
            continue;
        };
        if from != to {
            continue;
        }
        let pair = (keys[d.from].as_str(), keys[d.to].as_str());
        if pair.0 == pair.1 {
            continue;
        }
        if pair.0 != pair.1 {
            internal_edges[from].push(d);
            let representative = by_pair.entry(pair).or_insert(d);
            if dependency_rank(d.kind) < dependency_rank(representative.kind) {
                *representative = d;
            }
        }
        // One real outgoing edge suffices for each file's related-source witness.
        // Partition these once instead of rescanning all dependencies for every SCC.
        if indexed_sources.insert(d.from) {
            participant_edges[from].push(d);
        }
    }
    for (index, group) in groups.iter().enumerate() {
        let internal = &internal_edges[index];
        let path = witness(&edges, group);
        let coupling = CycleCoupling::CodeCycle;
        let imports = 0;
        let metadata = 0;
        let code = internal.len();
        let mut proof = path
            .windows(2)
            .filter_map(|p| by_pair.get(&(p[0].as_str(), p[1].as_str())))
            .map(|d| dependency_evidence(d, &keys, false))
            .collect::<Vec<_>>();
        // The shortest cycle need not visit every member of the SCC. Retain one
        // actual outgoing dependency per remaining participant for related-source
        // indexing, while keeping the cycle itself as the primary witness.
        let covered = proof
            .iter()
            .map(|e| e.range.file.to_string())
            .collect::<BTreeSet<_>>();
        for d in &participant_edges[index] {
            if !covered.contains(d.from) {
                proof.push(dependency_evidence(d, &keys, true));
            }
        }
        let Some(first) = proof.first() else {
            continue;
        };
        let range = first.range.clone();
        let files = internal
            .iter()
            .flat_map(|d| [d.from, d.to])
            .collect::<BTreeSet<_>>()
            .len();
        let pairs = internal
            .iter()
            .map(|d| (&keys[d.from], &keys[d.to]))
            .collect::<BTreeSet<_>>()
            .len();
        let php = group.iter().all(|m| m.starts_with("php:"));
        let unit = if php {
            "PHP namespace"
        } else {
            "модулей"
        };
        let mut assessment = "Замкнутый путь подтверждён вызовами и обращениями к свойствам или переменным. Импорты, атрибуты, аннотации, наследование и ссылки на типы сами по себе не образуют этот цикл. Разрыв нужен, если участники задуманы как независимые компоненты.".to_string();
        if config.layers.is_empty() || config.forbidden_dependencies.is_empty() {
            assessment.push_str(
                " Границы независимых компонентов не заданы; нарушение границы не установлено.",
            );
        } else {
            assessment.push_str(
                " Нарушения заданных границ проверяются отдельно правилом arch_boundary.",
            );
        }
        let proposals = vec![recommendation(
            "Разрыв обратной зависимости",
            "Выберите показанный обратный вызов или обращение к значению. Перенесите используемый контракт на сторону потребителя либо объедините участников, если они представляют один компонент.",
            &["Контракт должен описывать реально используемые операции и не зависеть от реализации.", "После изменения проверьте, что замкнутый путь исчез, включая транзитные зависимости."],
        )];
        emit(out, "arch_cycle", &range, &group[0],
            format!("Цикл вызовов и обращений к значениям между {} {unit}; затронуто файлов: {files}, направленных связей: {pairs}.", group.len()),
            &assessment, group.clone(), proof,
            BTreeMap::from([
                ("modules".into(), group.len() as f64),
                ("files".into(), files as f64),
                ("dependency_pairs".into(), pairs as f64),
                ("import_sites".into(), imports as f64),
                ("metadata_sites".into(), metadata as f64),
                ("code_reference_sites".into(), code as f64),
                ("call_sites".into(), internal.iter().filter(|d| d.kind == "call").count() as f64),
                ("value_access_sites".into(), internal.iter().filter(|d| matches!(d.kind, "member_access" | "variable_access")).count() as f64),
            ]), proposals, config);
        let diagnostic = out.last_mut().unwrap().architecture.as_mut().unwrap();
        diagnostic.cycle = Some(CycleDetails { path, coupling });
        diagnostic.limitations = vec![
            "Анализ охватывает разрешённые статические зависимости выбранных файлов; динамическая загрузка и внешние потребители не проверены.".into(),
            if php { "Участники сгруппированы по PHP namespace; namespace не обязательно является отдельным архитектурным компонентом." } else { "Модуль языка не обязательно является отдельным архитектурным компонентом." }.into(),
        ];
    }
}
fn dependency_rank(kind: &str) -> u8 {
    match kind {
        "import" => 2,
        "metadata_reference" => 1,
        _ => 0,
    }
}
fn dependency_evidence(
    d: &Dependency<'_>,
    keys: &BTreeMap<&str, String>,
    related: bool,
) -> Evidence {
    Evidence {
        range: d.range.clone(),
        description: format!(
            "{}{} → {} ({})",
            if related {
                "Участник компоненты: "
            } else {
                ""
            },
            d.from,
            d.to,
            d.kind
        ),
        dependency: Some(DependencyEvidence {
            from_module: keys[d.from].clone(),
            to_module: keys[d.to].clone(),
            from_file: d.from.into(),
            to_file: d.to.into(),
            kind: d.kind.into(),
            source_entity: d.source_entity.into(),
            target_entity: d.target_entity.into(),
        }),
    }
}
pub(super) fn boundaries(
    deps: &[Dependency],
    symbols: &Symbols<'_>,
    config: &ArchitectureConfig,
    diagnostics: &mut Vec<String>,
    out: &mut Vec<Violation>,
) {
    let layer = |file: &FileModel, entity: &str| -> Option<String> {
        let matched = config
            .layers
            .iter()
            .filter(|l| {
                l.selectors
                    .iter()
                    .any(|s| selector_matches(s, file, entity))
            })
            .map(|l| l.name.clone())
            .collect::<Vec<_>>();
        (matched.len() == 1).then(|| matched[0].clone())
    };
    let mut overlaps = BTreeSet::new();
    let mut groups = BTreeMap::<(String, String), Vec<&Dependency>>::new();
    for d in deps {
        let a = symbols.files[d.from];
        let b = symbols.files[d.to];
        for (f, e) in [(a, d.source_entity), (b, d.target_entity)] {
            if config
                .layers
                .iter()
                .filter(|l| l.selectors.iter().any(|s| selector_matches(s, f, e)))
                .count()
                > 1
            {
                overlaps.insert(format!("{}:{e}", f.path));
            }
        }
        if config
            .composition_roots
            .iter()
            .any(|s| selector_matches(s, a, &d.source_entity))
        {
            continue;
        }
        let (Some(from), Some(to)) = (layer(a, &d.source_entity), layer(b, &d.target_entity))
        else {
            continue;
        };
        if config
            .forbidden_dependencies
            .iter()
            .any(|r| r.from == from && r.to == to)
        {
            groups.entry((from, to)).or_default().push(d);
        }
    }
    diagnostics.extend(
        overlaps
            .into_iter()
            .take(config.max_evidence)
            .map(|s| format!("Сущность назначена нескольким слоям и исключена из проверки: {s}")),
    );
    for ((from, to), ds) in groups {
        let first = ds[0];
        let proof = ds
            .iter()
            .map(|d| {
                evidence(
                    &d.range,
                    format!("{} → {} ({})", d.source_entity, d.target_entity, d.kind),
                )
            })
            .collect();
        let dip = config
            .forbidden_dependencies
            .iter()
            .any(|r| r.from == from && r.to == to && r.principle.as_deref() == Some("DIP"))
            && ds.iter().any(|d| d.concrete);
        emit(
            out,
            "arch_boundary",
            &first.range,
            &format!("{from} → {to}"),
            format!(
                "{} подтверждённых зависимостей нарушают заданный запрет {from} → {to}.",
                ds.len()
            ),
            "Возможно, реализация противоречит выбранной архитектуре слоёв.",
            vec![from.clone(), to.clone()],
            proof,
            BTreeMap::from([("dependencies".into(), ds.len() as f64)]),
            vec![recommendation(
                if dip {
                    "Dependency inversion"
                } else {
                    "Layer boundary"
                },
                if dip {
                    "Перенести контракт в разрешённый слой и передавать реализацию через него."
                } else {
                    "Пересмотреть зависимость либо явно скорректировать правило границ."
                },
                &["Проверить владельца контракта и место сборки приложения."],
            )],
            config,
        );
        if dip {
            out.last_mut()
                .unwrap()
                .architecture
                .as_mut()
                .unwrap()
                .principle = "DIP".into();
        }
    }
}
pub(super) fn duplicates(
    files: &[&FileModel],
    calls: &CallGraph,
    symbols: &Symbols<'_>,
    config: &ArchitectureConfig,
    out: &mut Vec<Violation>,
) {
    let threshold = config.rule("arch_dry");
    let tokens = threshold.min_tokens.unwrap_or(100);
    let statements = threshold.min_statements.unwrap_or(5);
    let mut call_context = BTreeMap::<&str, Vec<_>>::new();
    for e in &calls.edges {
        call_context
            .entry(e.callsite.file.as_ref())
            .or_default()
            .push(e);
    }
    for edges in call_context.values_mut() {
        edges.sort_by(|a, b| (&a.callsite, &a.to).cmp(&(&b.callsite, &b.to)));
    }
    let mut buckets = BTreeMap::<[u8; 32], Vec<(&FileModel, &Fragment, String)>>::new();
    for &file in files {
        if !file.parse_errors.is_empty() {
            continue;
        }
        let Some(facts) = &file.architecture_facts else {
            continue;
        };
        let sites = file
            .functions
            .iter()
            .flat_map(|f| f.calls.iter().map(move |c| (&c.range, (f, c))))
            .collect::<BTreeMap<_, _>>();
        for frag in &facts.fragments {
            if frag.tokens < tokens || frag.statements < statements {
                continue;
            }
            // Same spelling in two files can refer to different external symbols.
            // Call identities belong to syntax occurrences, not to an unordered
            // set: swapping two alias bindings must change the verified key.
            let mut context = Vec::new();
            let mut ambiguous = false;
            let edges = call_context
                .get(file.path.as_str())
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let first = edges.partition_point(|e| {
                (e.callsite.start_line, e.callsite.start_column)
                    < (frag.range.start_line, frag.range.start_column)
            });
            for e in edges[first..]
                .iter()
                .take_while(|e| e.callsite.start_line <= frag.range.end_line)
                .filter(|e| in_range(&e.callsite, &frag.range))
            {
                if e.confidence != Confidence::Resolved {
                    let site = sites.get(&e.callsite);
                    if let Some((f, c)) = site {
                        if c.binding_unknown || c.local_binding {
                            // No imported/project name fallback is justified for
                            // a shadowed lexical callable.
                            context.push(format!("lexical:{}:{}:{}", file.path, f.fqn, c.name));
                            continue;
                        }
                        let symbol = c
                            .receiver
                            .as_ref()
                            .map(|r| {
                                let separator = if file.language == "php" { "::" } else { "." };
                                format!("{r}{separator}{}", c.name)
                            })
                            .unwrap_or_else(|| c.name.clone());
                        match symbols.imported_callable(file, &symbol, &c.range) {
                            Ok(Some(target)) => {
                                context.push(format!("import:{}:{target}", file.language));
                                continue;
                            }
                            Err(()) => {
                                ambiguous = true;
                                break;
                            }
                            Ok(None) => {}
                        }
                    }
                    context.push(format!("unresolved:{}:{}", file.module, e.to));
                    continue;
                }
                if let Some(target) = calls.node(&e.to) {
                    if target.kind != NodeKind::Unknown {
                        context.push(format!("resolved:{}", target.id));
                    }
                }
            }
            if ambiguous {
                continue;
            }
            let function = file
                .functions
                .iter()
                .find(|f| f.fqn == frag.entity && in_range(&frag.range, &f.range));
            let signature = function
                .map(|f| {
                    f.parameters
                        .iter()
                        .filter(|p| !p.implicit_receiver)
                        .map(|p| {
                            p.type_name
                                .as_deref()
                                .map(|name| symbols.signature_type(file, name, &p.range))
                                .unwrap_or_default()
                        })
                        .chain(std::iter::once(
                            f.return_type
                                .as_deref()
                                .map(|name| symbols.signature_type(file, name, &f.range))
                                .unwrap_or_default(),
                        ))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            // Exact fallback may observe a binding that never occurs among body
            // tokens (locals()/vars(), opaque macro expansion). Keep input names
            // as well as types when alpha-renaming was not certified.
            let exact_inputs = (!frag.renamed).then(|| {
                function.map(|f| {
                    f.parameters
                        .iter()
                        .map(|p| p.name.as_str())
                        .collect::<Vec<_>>()
                })
            });
            let key =
                serde_json::to_string(&(&file.language, context, signature, exact_inputs)).unwrap();
            let mut hasher = Sha256::new();
            hasher.update(frag.canonical.len().to_le_bytes());
            hasher.update(frag.canonical.as_bytes());
            hasher.update(key.as_bytes());
            let hash = hasher.finalize().into();
            buckets.entry(hash).or_default().push((file, frag, key));
        }
    }
    let mut emitted = Vec::<Vec<SourceRange>>::new();
    // Larger groups first ensure nested identical blocks do not duplicate whole-body findings.
    let mut buckets = buckets
        .into_values()
        .filter(|b| b.len() > 1)
        .collect::<Vec<_>>();
    buckets.sort_by_key(|b| std::cmp::Reverse(b[0].1.tokens));
    for bucket in buckets {
        let mut exact: Vec<Vec<(&FileModel, &Fragment, String)>> = Vec::new();
        for item in bucket {
            if let Some(group) = exact
                .iter_mut()
                .find(|g| g[0].1.canonical == item.1.canonical && g[0].2 == item.2)
            {
                group.push(item);
            } else {
                exact.push(vec![item]);
            }
        }
        for mut group in exact {
            group.sort_by(|a, b| {
                (&a.0.path, &a.1.range, a.1.renamed).cmp(&(&b.0.path, &b.1.range, b.1.renamed))
            });
            group.dedup_by(|a, b| a.1.range == b.1.range);
            let mut kept: Vec<(&FileModel, &Fragment, String)> = Vec::new();
            for item in group {
                if !kept.iter().any(|(_, f, _)| {
                    in_range(&item.1.range, &f.range) || in_range(&f.range, &item.1.range)
                }) {
                    kept.push(item);
                }
            }
            if kept.len() < 2 {
                continue;
            }
            let ranges = kept
                .iter()
                .map(|(_, f, _)| f.range.clone())
                .collect::<Vec<_>>();
            if emitted
                .iter()
                .any(|old| ranges.iter().all(|r| old.iter().any(|o| in_range(r, o))))
            {
                continue;
            }
            let first = kept[0].1;
            let proof = kept
                .iter()
                .map(|(_, f, _)| {
                    evidence(
                        &f.range,
                        format!(
                            "{}: {} токенов, {} операторов, {} сравнение",
                            f.entity,
                            f.tokens,
                            f.statements,
                            if f.renamed {
                                "с привязками переменных"
                            } else {
                                "точное"
                            }
                        ),
                    )
                })
                .collect();
            let mut occurrence = BTreeMap::new();
            let participants = kept
                .iter()
                .map(|(file, f, verified_key)| {
                    let mut hasher = Sha256::new();
                    hasher.update(f.canonical.len().to_le_bytes());
                    hasher.update(f.canonical.as_bytes());
                    hasher.update(verified_key.as_bytes());
                    let hash = format!("{:x}", hasher.finalize());
                    let key = format!("{}:{}:{}:{hash}", file.language, file.path, f.entity);
                    let n = occurrence.entry(key.clone()).or_insert(0usize);
                    *n += 1;
                    format!("{key}:{}", *n)
                })
                .collect();
            emit(out,"arch_dry",&first.range,&first.entity,format!("{} непересекающихся фрагментов имеют одинаковое нормализованное представление ({} токенов).",kept.len(),first.tokens),"Возможно, одна реализация может заменить эти копии.",participants,proof,BTreeMap::from([("copies".into(),kept.len() as f64),("tokens".into(),first.tokens as f64)]),vec![recommendation("Extract function","Рассмотреть выделение общего фрагмента в функцию или компонент.",&["Проверить, должны ли копии изменяться совместно.","Проверить входные переменные, побочные эффекты, выходы и ранние возвраты.","Совпадение структуры не доказывает эквивалентность поведения."])],config);
            emitted.push(ranges);
        }
    }
}
pub(super) fn cohesion(
    files: &[&FileModel],
    calls: &CallGraph,
    symbols: &Symbols<'_>,
    config: &ArchitectureConfig,
    out: &mut Vec<Violation>,
) {
    let rule = config.rule("arch_srp");
    let mut method_index = BTreeMap::<(String, String), Vec<_>>::new();
    for &file in files {
        let Some(facts) = &file.architecture_facts else {
            continue;
        };
        let by_range = facts
            .functions
            .iter()
            .map(|ff| ((&ff.fqn, &ff.range), ff))
            .collect::<BTreeMap<_, _>>();
        for m in &file.functions {
            if let Some(owner) = &m.parent_type {
                if let Some(ff) = by_range.get(&(&m.fqn, &m.range)) {
                    method_index
                        .entry((file.language.clone(), owner.clone()))
                        .or_default()
                        .push((file, m, *ff));
                }
            }
        }
    }
    let mut internal = BTreeMap::<String, BTreeSet<String>>::new();
    let mut unknown = BTreeSet::new();
    for e in &calls.edges {
        if e.confidence != Confidence::Resolved {
            unknown.insert(e.from.to_string());
            continue;
        }
        if let (Some(a), Some(b)) = (calls.node(&e.from), calls.node(&e.to)) {
            if a.parent_type.is_some() && a.parent_type == b.parent_type && a.file == b.file {
                internal
                    .entry(a.id.clone())
                    .or_default()
                    .insert(b.id.clone());
            }
        }
    }
    for &file in files {
        if file.architecture_facts.is_none() || !file.parse_errors.is_empty() {
            continue;
        }
        for t in &file.types {
            if symbols.declaration(&file.language, &t.fqn).is_none() {
                continue;
            }
            if is_abstract(file, t)
                || matches!(
                    t.kind,
                    TypeKind::Enum | TypeKind::Impl | TypeKind::Namespace
                )
            {
                continue;
            }
            // Rust impl methods can be associated with the concrete type rather than the impl declaration.
            let methods = method_index
                .get(&(file.language.clone(), t.fqn.clone()))
                .into_iter()
                .flatten()
                .copied()
                .filter(|(_, m, ff)| {
                    m.parent_function.is_none()
                        && !m.modifiers.iter().any(|s| {
                            matches!(
                                s.as_str(),
                                "static"
                                    | "@staticmethod"
                                    | "@builtins.staticmethod"
                                    | "@classmethod"
                                    | "@builtins.classmethod"
                            ) || s.contains("abstract")
                        })
                        && !matches!(
                            m.name.as_str(),
                            "constructor" | "__init__" | "__construct" | "new"
                        )
                        && m.accessor.is_none()
                        && ff.body_present
                })
                .collect::<Vec<_>>();
            if methods.len() < rule.min_methods.unwrap_or(8) {
                continue;
            }
            // Dropping a dynamic method could erase the bridge between otherwise disconnected groups.
            if methods.iter().any(|(_, _, ff)| ff.dynamic_access) {
                continue;
            }
            let mut edges = BTreeMap::<String, BTreeSet<String>>::new();
            let mut fields = BTreeMap::<&str, Vec<String>>::new();
            let mut unusable = false;
            for (f, m, ff) in &methods {
                let id = format!("fn:{}:{}:{}", f.language, f.path, m.fqn);
                edges.entry(id.clone()).or_default();
                if unknown.contains(&id)
                    && m.calls
                        .iter()
                        .any(|c| matches!(c.receiver.as_deref(), Some("self" | "this" | "$this")))
                {
                    unusable = true;
                }
                for field in &ff.own_fields {
                    fields.entry(field).or_default().push(id.clone());
                }
            }
            if unusable {
                continue;
            }
            for group in fields.values() {
                for pair in group.windows(2) {
                    edges
                        .entry(pair[0].clone())
                        .or_default()
                        .insert(pair[1].clone());
                    edges
                        .entry(pair[1].clone())
                        .or_default()
                        .insert(pair[0].clone());
                }
            }
            let member_ids = edges.keys().cloned().collect::<Vec<_>>();
            for id in &member_ids {
                let Some(targets) = internal.get(id) else {
                    continue;
                };
                for target in targets {
                    if edges.contains_key(target) {
                        edges.get_mut(id).unwrap().insert(target.clone());
                        edges.get_mut(target).unwrap().insert(id.clone());
                    }
                }
            }
            let groups = components(&edges)
                .into_iter()
                .filter(|g| {
                    g.len() >= rule.min_group_methods.unwrap_or(3)
                        && fields
                            .values()
                            .any(|users| users.iter().any(|id| g.contains(id)))
                })
                .collect::<Vec<_>>();
            if groups.len() < 2 {
                continue;
            }
            let proof = groups
                .iter()
                .enumerate()
                .flat_map(|(i, g)| {
                    methods
                        .iter()
                        .filter(move |(f, m, _)| {
                            g.contains(&format!("fn:{}:{}:{}", f.language, f.path, m.fqn))
                        })
                        .map(move |(_, m, ff)| {
                            evidence(
                                &m.range,
                                format!(
                                    "Группа {}: {} · поля {}",
                                    i + 1,
                                    m.fqn,
                                    ff.own_fields.join(", ")
                                ),
                            )
                        })
                })
                .collect();
            emit(out,"arch_srp",&t.range,&t.fqn,format!("{} содержательных методов образуют {} раздельных групп с собственным состоянием.",methods.len(),groups.len()),"Возможно, класс объединяет несколько независимо изменяемых обязанностей.",vec![type_id(file,t)],proof,BTreeMap::from([("methods".into(),methods.len() as f64),("groups".into(),groups.len() as f64)]),vec![recommendation("Extract class","Рассмотреть выделение показанных групп в отдельные компоненты.",&["Группы состояния должны представлять отдельные обязанности, а не намеренный общий фасад."])],config);
        }
    }
    let _ = symbols;
}
pub(super) fn decisions(
    id: &str,
    files: &[&FileModel],
    symbols: &Symbols<'_>,
    config: &ArchitectureConfig,
    out: &mut Vec<Violation>,
) {
    let rule = config.rule(id);
    let mut groups = BTreeMap::<
        String,
        Vec<(
            &FileModel,
            &FunctionFacts,
            &Decision,
            Vec<Vec<(String, String)>>,
        )>,
    >::new();
    for &file in files {
        if !file.parse_errors.is_empty() {
            continue;
        }
        let Some(facts) = &file.architecture_facts else {
            continue;
        };
        for f in &facts.functions {
            if f.dynamic_access {
                continue;
            }
            for d in &f.decisions {
                if d.cases.len() < rule.min_alternatives.unwrap_or(3) {
                    continue;
                }
                let mut matrix = Vec::new();
                let mut types = Vec::new();
                let mut valid = true;
                for case in &d.cases {
                    if id == "arch_ocp" {
                        let Some(name) = &case.tested_type else {
                            valid = false;
                            break;
                        };
                        let Some((tf, t)) = symbols.resolve_type(file, name, &case.range) else {
                            valid = false;
                            break;
                        };
                        types.push((tf, t));
                    }
                    let mut row = Vec::new();
                    for creation in case.creations.iter().filter(|_| id == "arch_factory") {
                        if let Some((tf, t)) =
                            symbols.resolve_type(file, &creation.name, &creation.range)
                        {
                            row.push((type_id(tf, t), t.fqn.clone()));
                        } else if creation.kind != "construction_candidate" {
                            valid = false;
                        }
                    }
                    row.sort();
                    row.dedup();
                    matrix.push(row);
                }
                if !valid {
                    continue;
                }
                if id == "arch_ocp" {
                    let mut common: Option<BTreeSet<String>> = None;
                    for (tf, t) in &types {
                        let ancestors = symbols.ancestors(tf, t);
                        common = Some(match common {
                            Some(old) => old.intersection(&ancestors).cloned().collect(),
                            None => ancestors,
                        });
                    }
                    let open = common
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|name| {
                            symbols
                                .resolve_type(file, name, &d.range)
                                .is_some_and(|(f, t)| {
                                    !f.architecture_facts
                                        .as_ref()
                                        .is_some_and(|a| a.closed_types.contains(&t.fqn))
                                })
                        })
                        .collect::<Vec<_>>();
                    if open.is_empty() {
                        continue;
                    }
                    let cases = types
                        .iter()
                        .map(|(f, t)| type_id(f, t))
                        .collect::<BTreeSet<_>>();
                    if cases.len() < rule.min_alternatives.unwrap_or(3) {
                        continue;
                    }
                    let key = serde_json::to_string(&(&file.language, open, cases)).unwrap();
                    groups.entry(key).or_default().push((file, f, d, matrix));
                } else {
                    if matrix.iter().any(Vec::is_empty) {
                        continue;
                    }
                    if product_contracts(
                        &matrix,
                        symbols,
                        file,
                        &d.range,
                        rule.min_alternatives.unwrap_or(3),
                    )
                    .is_empty()
                    {
                        continue;
                    }
                    // Preserve selector contracts and case labels, not merely constructor counts.
                    let subject = d
                        .subject_type
                        .as_ref()
                        .and_then(|name| symbols.resolve_type(file, name, &d.range))
                        .map(|(f, t)| type_id(f, t))
                        .unwrap_or_else(|| format!("{}:{}", file.language, d.subject));
                    let key = serde_json::to_string(&(
                        subject,
                        d.cases.iter().map(|c| &c.label).collect::<Vec<_>>(),
                        &matrix,
                    ))
                    .unwrap();
                    groups.entry(key).or_default().push((file, f, d, matrix));
                }
            }
        }
    }
    for group in groups.into_values().filter(|g| {
        g.len()
            >= rule
                .min_sites
                .unwrap_or(if id == "arch_ocp" { 3 } else { 2 })
    }) {
        let first = &group[0];
        let proof = group
            .iter()
            .flat_map(|(_, f, d, _)| {
                d.cases
                    .iter()
                    .map(move |c| evidence(&c.range, format!("{}: {}", f.fqn, c.label)))
            })
            .collect();
        let mut occurrence = BTreeMap::new();
        let participants = group
            .iter()
            .map(|(file, f, d, _)| {
                let contract = serde_json::to_vec(&(
                    &d.subject,
                    d.cases
                        .iter()
                        .map(|c| (&c.label, &c.tested_type))
                        .collect::<Vec<_>>(),
                ))
                .unwrap();
                let site = format!(
                    "{}:{}:{}:{:x}",
                    file.language,
                    file.path,
                    f.fqn,
                    Sha256::digest(contract)
                );
                let n = occurrence.entry(site.clone()).or_insert(0usize);
                *n += 1;
                format!("{site}:{n}")
            })
            .collect();
        let abstract_factory =
            id == "arch_factory" && product_families(&group[0].3, symbols, first.0, &first.2.range);
        let suggestion = if id == "arch_ocp" {
            recommendation(
                "Strategy",
                "Вынести выбор поведения в реализации общего контракта.",
                &[
                    "Варианты должны расширяться независимо.",
                    "Сравнить с простотой существующего ветвления; паттерн не обязателен.",
                ],
            )
        } else if abstract_factory {
            recommendation(
                "Abstract Factory",
                "Сосредоточить создание согласованных семейств продуктов в фабриках.",
                &[
                    "Семейства должны действительно выбираться совместно.",
                    "Сохранить порядок создания, владение и жизненный цикл объектов.",
                ],
            )
        } else {
            recommendation(
                "Factory",
                "Выделить повторяющийся выбор и создание объектов в фабрику.",
                &[
                    "Создаваемые реализации должны иметь общий контракт.",
                    "Проверить конфигурацию, владение и побочные эффекты создания.",
                ],
            )
        };
        emit(
            out,
            id,
            &first.2.range,
            &first.1.fqn,
            format!(
                "В {} местах повторяются {} альтернатив {}.",
                group.len(),
                first.2.cases.len(),
                if id == "arch_ocp" {
                    "открытой иерархии"
                } else {
                    "создания объектов"
                }
            ),
            "Возможно, изменения вариантов приходится синхронизировать в нескольких местах.",
            participants,
            proof,
            BTreeMap::from([
                ("sites".into(), group.len() as f64),
                ("alternatives".into(), first.2.cases.len() as f64),
            ]),
            vec![suggestion],
            config,
        );
    }
}
fn product_contracts(
    matrix: &[Vec<(String, String)>],
    symbols: &Symbols<'_>,
    file: &FileModel,
    range: &SourceRange,
    min_variants: usize,
) -> BTreeSet<String> {
    let mut shared: Option<BTreeSet<String>> = None;
    let mut variants = BTreeMap::<String, BTreeSet<String>>::new();
    for row in matrix {
        let mut contracts = BTreeSet::new();
        for (id, name) in row {
            let Some((f, t)) = symbols.resolve_type(file, name, range) else {
                continue;
            };
            for parent in symbols.ancestors(f, t) {
                if symbols
                    .resolve_type(file, &parent, range)
                    .is_some_and(|(f, t)| is_abstract(f, t))
                {
                    contracts.insert(parent.clone());
                    variants.entry(parent).or_default().insert(id.clone());
                }
            }
        }
        shared = Some(match shared {
            Some(old) => old.intersection(&contracts).cloned().collect(),
            None => contracts,
        });
    }
    shared
        .unwrap_or_default()
        .into_iter()
        .filter(|c| variants.get(c).is_some_and(|v| v.len() >= min_variants))
        .collect()
}
fn product_families(
    matrix: &[Vec<(String, String)>],
    symbols: &Symbols<'_>,
    file: &FileModel,
    range: &SourceRange,
) -> bool {
    if matrix.len() < 3 || matrix.iter().any(|r| r.len() < 2) {
        return false;
    }
    let contracts = product_contracts(matrix, symbols, file, range, 3)
        .into_iter()
        .collect::<Vec<_>>();
    for (i, a) in contracts.iter().enumerate() {
        for b in contracts.iter().skip(i + 1) {
            let Some((af, at)) = symbols.resolve_type(file, a, range) else {
                continue;
            };
            let Some((bf, bt)) = symbols.resolve_type(file, b, range) else {
                continue;
            };
            if symbols.ancestors(af, at).contains(b) || symbols.ancestors(bf, bt).contains(a) {
                continue;
            }
            if matrix.iter().all(|row| {
                let products = row
                    .iter()
                    .filter_map(|(id, name)| {
                        symbols
                            .resolve_type(file, name, range)
                            .map(|(f, t)| (id, symbols.ancestors(f, t)))
                    })
                    .collect::<Vec<_>>();
                products.iter().any(|(aid, parents)| {
                    parents.contains(a)
                        && products
                            .iter()
                            .any(|(bid, parents)| aid != bid && parents.contains(b))
                })
            }) {
                return true;
            }
        }
    }
    false
}
pub(super) fn substitution(
    files: &[&FileModel],
    symbols: &Symbols<'_>,
    config: &ArchitectureConfig,
    out: &mut Vec<Violation>,
) {
    // A qualified macro may name a project module or crate alias declared in a
    // different file. Without Rust edition/crate-origin facts, spelling alone
    // cannot establish a standard-library operation across that namespace.
    let qualified_rust_origin_ambiguous = files.iter().filter(|f| f.language == "rust").any(|f| {
        f.types
            .iter()
            .any(|t| matches!(t.name.as_str(), "core" | "std"))
            || f.imports
                .iter()
                .any(|i| matches!(crate::scoped_imports::alias(i), "core" | "std"))
            || f.architecture_facts.as_ref().is_some_and(|facts| {
                facts
                    .rust_namespace_shadows
                    .iter()
                    .any(|name| matches!(name.as_str(), "core" | "std"))
            })
    });
    for &file in files {
        if !file.parse_errors.is_empty() {
            continue;
        }
        let Some(facts) = &file.architecture_facts else {
            continue;
        };
        // A shadowed builtin is not an unsupported-operation witness.
        let bare_stub_shadowed = facts.unsupported_operation_shadowed
            || file.imports.iter().any(|i| {
                crate::scoped_imports::alias(i) == "NotImplementedError"
                    || file.language == "rust" && crate::scoped_imports::alias(i) == "unimplemented"
            })
            || file.types.iter().any(|t| t.name == "NotImplementedError")
            || file
                .functions
                .iter()
                .any(|f| f.name == "NotImplementedError");
        for ff in facts.functions.iter().filter(|f| f.unsupported_stub) {
            if file.language == "rust"
                && ff.unsupported_stub_qualified
                && qualified_rust_origin_ambiguous
            {
                continue;
            }
            if bare_stub_shadowed && !(file.language == "rust" && ff.unsupported_stub_qualified) {
                continue;
            }
            let Some(m) = file.functions.iter().find(|m| {
                m.fqn == ff.fqn
                    && m.range == ff.range
                    && m.parent_function.is_none()
                    && !m.modifiers.iter().any(|x| x.contains("abstract"))
            }) else {
                continue;
            };
            let Some(owner) = m
                .parent_type
                .as_deref()
                .and_then(|n| symbols.resolve_type(file, n, &m.range))
            else {
                continue;
            };
            for ancestor in symbols.ancestors(owner.0, owner.1) {
                let Some((bf, base)) = symbols.resolve_type(file, &ancestor, &m.range) else {
                    continue;
                };
                let Some(base_method) = files
                    .iter()
                    .filter(|f| f.language == file.language)
                    .flat_map(|f| f.functions.iter().map(move |m| (*f, m)))
                    .find(|(_, f)| {
                        f.parent_type.as_deref() == Some(&base.fqn)
                            && f.parent_function.is_none()
                            && f.name == m.name
                            && !f.modifiers.iter().any(|x| x.contains("abstract"))
                    })
                else {
                    continue;
                };
                let Some(base_facts) = base_method.0.architecture_facts.as_ref().and_then(|a| {
                    a.functions
                        .iter()
                        .find(|ff| ff.range == base_method.1.range && ff.normal_exit)
                }) else {
                    continue;
                };
                emit(out,"arch_lsp",&m.range,&m.fqn,"Переопределение полностью отказывается от операции, для которой существует конкретная базовая реализация.".into(),"Возможно, использование этого наследника через базовый контракт нарушает ожидания вызывающего кода.",vec![format!("{}:{}",type_id(owner.0,owner.1),m.name),type_id(bf,base)],vec![evidence(&m.range,"Отказ от операции"),evidence(&base_facts.range,"Конкретная базовая реализация")],BTreeMap::new(),vec![recommendation("Interface segregation / composition","Проверить границы контракта; рассмотреть более узкий интерфейс либо композицию.",&["Уточнить предусловия и допустимые исключения базового контракта.","Отказ может быть намеренно предусмотрен контрактом."])],config);
                break;
            }
        }
    }
}
pub(super) fn interfaces(
    files: &[&FileModel],
    symbols: &Symbols<'_>,
    config: &ArchitectureConfig,
    out: &mut Vec<Violation>,
) {
    let rule = config.rule("arch_isp");
    let mut declared = BTreeMap::<(String, String), BTreeSet<String>>::new();
    for &file in files {
        for m in &file.functions {
            if m.parent_function.is_some() {
                continue;
            }
            if let Some(owner) = &m.parent_type {
                if !m
                    .modifiers
                    .iter()
                    .any(|s| s == "private" || s == "protected")
                {
                    declared
                        .entry((file.language.clone(), owner.clone()))
                        .or_default()
                        .insert(m.name.clone());
                }
            }
        }
    }

    let mut consumers =
        BTreeMap::<String, BTreeMap<String, (BTreeSet<String>, SourceRange)>>::new();
    let mut dynamic = BTreeSet::new();
    for &file in files {
        for f in &file.functions {
            let Some(owner) = f.parent_type.as_ref() else {
                continue;
            };
            for c in &f.calls {
                let Some(name) = &c.receiver_type else {
                    continue;
                };
                let Some((tf, t)) = symbols.resolve_type(file, name, &c.range) else {
                    continue;
                };
                if !is_abstract(tf, t) {
                    continue;
                }
                let id = type_id(tf, t);
                if c.binding_unknown || c.kind == CallKind::Dynamic {
                    dynamic.insert(id);
                    continue;
                }
                let entry = consumers
                    .entry(id)
                    .or_default()
                    .entry(format!("{}:{}:{owner}", file.language, file.path))
                    .or_insert_with(|| (BTreeSet::new(), c.range.clone()));
                entry.0.insert(c.name.clone());
            }
            if file.architecture_facts.as_ref().is_some_and(|a| {
                a.functions
                    .iter()
                    .any(|ff| ff.range == f.range && ff.dynamic_access)
            }) {
                for p in &f.parameters {
                    if let Some((tf, t)) = p
                        .type_name
                        .as_deref()
                        .and_then(|n| symbols.resolve_type(file, n, &p.range))
                    {
                        dynamic.insert(type_id(tf, t));
                    }
                }
            }
        }
    }
    for &file in files {
        for t in &file.types {
            if !is_abstract(file, t) {
                continue;
            }
            let id = type_id(file, t);
            if dynamic.contains(&id) {
                continue;
            }
            let methods = declared
                .get(&(file.language.clone(), t.fqn.clone()))
                .cloned()
                .unwrap_or_default();
            if methods.len() < rule.min_methods.unwrap_or(8) {
                continue;
            }
            let Some(clients) = consumers.get(&id) else {
                continue;
            };
            let mut groups = BTreeMap::<BTreeSet<String>, Vec<(&String, &SourceRange)>>::new();
            for (client, (used, range)) in clients {
                if used.len() >= 2
                    && used.is_subset(&methods)
                    && used.len() as f64 / methods.len() as f64
                        <= rule.max_usage_ratio.unwrap_or(0.25)
                {
                    groups
                        .entry(used.clone())
                        .or_default()
                        .push((client, range));
                }
            }
            let groups = groups
                .into_iter()
                .filter(|(_, g)| g.len() >= 2)
                .collect::<Vec<_>>();
            let Some((a, b)) = groups.iter().enumerate().find_map(|(i, a)| {
                groups
                    .iter()
                    .skip(i + 1)
                    .find(|b| a.0.is_disjoint(&b.0))
                    .map(|b| (a, b))
            }) else {
                continue;
            };
            let proof =
                a.1.iter()
                    .chain(&b.1)
                    .map(|(client, range)| evidence(range, format!("Потребитель {client}")))
                    .collect();
            let participants =
                a.1.iter()
                    .chain(&b.1)
                    .map(|(s, _)| (*s).clone())
                    .chain(std::iter::once(id))
                    .collect();
            emit(out,"arch_isp",&t.range,&t.fqn,format!("Интерфейс содержит {} методов. Раздельные группы потребителей используют {{{}}} и {{{}}}.",methods.len(),a.0.iter().cloned().collect::<Vec<_>>().join(", "),b.0.iter().cloned().collect::<Vec<_>>().join(", ")),"Возможно, потребителям полезны отдельные узкие контракты.",participants,proof,BTreeMap::from([("methods".into(),methods.len() as f64),("consumers".into(),(a.1.len()+b.1.len()) as f64)]),vec![recommendation("Extract interface","Рассмотреть интерфейсы по показанным наборам операций.",&["Это наблюдаемое использование внутри анализируемого scope, а не все внешние потребители.","Проверить совместимость публичного API и реализаций."])],config);
        }
    }
}
pub(super) fn demeter(
    files: &[&FileModel],
    symbols: &Symbols<'_>,
    config: &ArchitectureConfig,
    out: &mut Vec<Violation>,
) {
    for &file in files {
        let Some(facts) = &file.architecture_facts else {
            continue;
        };
        for f in &facts.functions {
            let mut groups = BTreeMap::<String, Vec<&MemberChain>>::new();
            for c in &f.member_chains {
                let Some((tf, root)) = c
                    .root_type
                    .as_deref()
                    .and_then(|n| symbols.resolve_type(file, n, &c.range))
                else {
                    continue;
                };
                let mut current = (tf, root);
                let mut valid = true;
                for name in c.members.iter().take(c.members.len() - 1) {
                    let Some(field) = current.1.fields.iter().find(|f| &f.name == name) else {
                        valid = false;
                        break;
                    };
                    let Some(next) = field
                        .type_name
                        .as_deref()
                        .and_then(|n| symbols.resolve_type(current.0, n, &field.range))
                    else {
                        valid = false;
                        break;
                    };
                    if next.1.methods.is_empty() || next.1.fqn == current.1.fqn {
                        valid = false;
                        break;
                    }
                    current = next;
                }
                if valid {
                    groups
                        .entry(format!("{}:{}", type_id(tf, root), c.members.join(".")))
                        .or_default()
                        .push(c);
                }
            }
            for (key, group) in groups.into_iter().filter(|(_, g)| g.len() >= 2) {
                emit(out,"arch_demeter",&group[0].range,&f.fqn,format!("В {} местах повторяется доступ через известные промежуточные объекты: {}.",group.len(),group[0].members.join(".")),"Возможно, функция зависит от внутреннего устройства своих непосредственных зависимостей.",vec![format!("{}:{}:{}:{key}",file.language,file.path,f.fqn)],group.iter().map(|c|evidence(&c.range,c.members.join(" → "))).collect(),BTreeMap::from([("sites".into(),group.len() as f64)]),vec![recommendation("Encapsulate navigation","Рассмотреть передачу непосредственной зависимости или операцию на ближайшем объекте.",&["Проверить, является ли цепочка частью намеренно публичного API."])],config);
            }
        }
    }
}
pub(super) fn stability(
    deps: &[Dependency],
    symbols: &Symbols<'_>,
    config: &ArchitectureConfig,
    out: &mut Vec<Violation>,
) {
    let edges = module_graph(deps, symbols);
    let mut incoming = BTreeMap::<String, BTreeSet<String>>::new();
    for (a, bs) in &edges {
        for b in bs {
            incoming.entry(b.clone()).or_default().insert(a.clone());
        }
    }
    let metrics = |m: &str| {
        let ce = edges.get(m).map_or(0, BTreeSet::len);
        let ca = incoming.get(m).map_or(0, BTreeSet::len);
        (
            ca,
            ce,
            if ca + ce == 0 {
                0.0
            } else {
                ce as f64 / (ca + ce) as f64
            },
        )
    };
    let mut seen = BTreeSet::new();
    for d in deps {
        let a = module_key(symbols.files[d.from]);
        let b = module_key(symbols.files[d.to]);
        if a == b || !seen.insert((a.clone(), b.clone())) {
            continue;
        }
        let (ca, ce, ai) = metrics(&a);
        let (bca, bce, bi) = metrics(&b);
        if ca < 3
            || ca + ce < 5
            || bca + bce < 5
            || bi - ai
                <= config
                    .rule("arch_stability")
                    .instability_delta
                    .unwrap_or(0.3)
        {
            continue;
        }
        emit(
            out,
            "arch_stability",
            &d.range,
            &a,
            format!("По наблюдаемому графу I({a})={ai:.3}, I({b})={bi:.3}; I=Ce/(Ca+Ce)."),
            "Возможно, более устойчивый модуль зависит от менее устойчивого.",
            vec![a.clone(), b],
            vec![evidence(&d.range, format!("{} → {}", d.from, d.to))],
            BTreeMap::from([
                ("source_instability".into(), ai),
                ("target_instability".into(), bi),
                ("source_ca".into(), ca as f64),
                ("source_ce".into(), ce as f64),
                ("target_ca".into(), bca as f64),
                ("target_ce".into(), bce as f64),
            ]),
            vec![recommendation(
                "Dependency inversion",
                "Проверить направление зависимости и владельца общего контракта.",
                &[
                    "Числа относятся только к разрешённым зависимостям анализируемого scope.",
                    "Метрика не измеряет частоту изменений и не доказывает качество архитектуры.",
                ],
            )],
            config,
        );
    }
}
