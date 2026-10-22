use super::*;

/// Assigned graph identities remain separate from source module names and FQNs.
pub(super) type ModuleAssignments = BTreeMap<String, String>;

pub(super) fn assign_modules(
    files: &[&FileModel],
    calls: &CallGraph,
    config: &ArchitectureConfig,
) -> Result<ModuleAssignments, String> {
    config.validate_modules()?;
    let mut assignments = BTreeMap::new();
    for file in files {
        let matches = config
            .modules
            .iter()
            .filter(|group| {
                group.selectors.iter().any(|selector| {
                    graph::selector_matches(selector, file, "", calls.packages_of(Some(&file.path)))
                })
            })
            .map(|group| group.name.as_str())
            .collect::<BTreeSet<_>>();
        if matches.len() > 1 {
            return Err(format!(
                "architecture.modules: file {} matches multiple groups: {}",
                file.path,
                matches.into_iter().collect::<Vec<_>>().join(", ")
            ));
        }
        let key = if let Some(name) = matches.first() {
            format!("{}:group:{name}", file.language)
        } else if config.modules.is_empty() && !config.module_depth.contains_key(&file.language) {
            rules::module_key(file)
        } else {
            let module = match config.module_depth.get(&file.language) {
                Some(depth) => {
                    let separator = module_separator(&file.language).unwrap();
                    file.module
                        .split(separator)
                        .take(*depth)
                        .collect::<Vec<_>>()
                        .join(separator)
                }
                None => file.module.clone(),
            };
            if config.modules.is_empty() {
                format!("{}:{module}", file.language)
            } else {
                format!("{}:native:{module}", file.language)
            }
        };
        assignments.insert(file.path.clone(), key);
    }
    Ok(assignments)
}

pub(crate) fn validate_module_assignments(
    files: &[FileModel],
    calls: &CallGraph,
    config: &ArchitectureConfig,
) -> Result<(), String> {
    assign_modules(&selected_files(files, config), calls, config).map(|_| ())
}

pub(crate) fn module_cycles(
    files: &[FileModel],
    calls: &CallGraph,
    config: &ArchitectureConfig,
) -> Vec<ModuleCycleDiagnostic> {
    let selected = selected_files(files, config);
    // Scan validates this before metrics; standalone advisory calls report it separately.
    let Ok(keys) = assign_modules(&selected, calls, config) else {
        return Vec::new();
    };
    let symbols = graph::Symbols::new(&selected);
    let (dependencies, _) = graph::dependencies(&selected, calls, &symbols);
    let runtime = rules::runtime_dependencies(&dependencies);
    let edges = rules::module_graph(&runtime, &keys);
    let groups = graph::components(&edges)
        .into_iter()
        .filter(|g| g.len() > 1)
        .collect::<Vec<_>>();
    let membership = groups
        .iter()
        .enumerate()
        .flat_map(|(index, group)| group.iter().map(move |name| (name.as_str(), index)))
        .collect::<BTreeMap<_, _>>();
    let mut group_files = vec![Vec::new(); groups.len()];
    for (file, module) in &keys {
        if let Some(&index) = membership.get(module.as_str()) {
            group_files[index].push(file.clone());
        }
    }
    let mut internal_edges = vec![Vec::new(); groups.len()];
    let mut by_pair = BTreeMap::new();
    for dependency in &runtime {
        let a = keys[dependency.from].as_str();
        let b = keys[dependency.to].as_str();
        if a == b {
            continue;
        }
        let (Some(&from), Some(&to)) = (membership.get(a), membership.get(b)) else {
            continue;
        };
        if from == to {
            internal_edges[from].push(dependency);
            by_pair.entry((a, b)).or_insert(dependency);
        }
    }
    let mut out = Vec::new();
    for (index, participants) in groups.into_iter().enumerate() {
        let files = std::mem::take(&mut group_files[index]);
        let internal = &internal_edges[index];
        let path = graph::witness(&edges, &participants);
        // A bounded report retains the actual closed path before extra witnesses.
        let mut ordered = path
            .windows(2)
            .filter_map(|pair| by_pair.get(&(pair[0].as_str(), pair[1].as_str())).copied())
            .collect::<Vec<_>>();
        ordered.extend(internal.iter().copied());
        let mut seen = BTreeSet::new();
        let mut evidence = ordered
            .into_iter()
            .filter_map(|d| {
                seen.insert((
                    d.range,
                    d.from,
                    d.to,
                    d.kind,
                    d.source_entity,
                    d.target_entity,
                ))
                .then(|| rules::dependency_evidence(d, &keys, false))
            })
            .collect::<Vec<_>>();
        let evidence_total = evidence.len();
        let mut affected_locations = evidence.iter().map(|e| e.range.clone()).collect::<Vec<_>>();
        // Some files share a participating module without owning an intermodule edge.
        let covered_files = affected_locations
            .iter()
            .map(|range| range.file.to_string())
            .collect::<BTreeSet<_>>();
        for file in &files {
            if !covered_files.contains(file) {
                affected_locations.push(SourceRange {
                    file: file.as_str().into(),
                    start_line: 1,
                    end_line: 1,
                    start_column: 1,
                    end_column: 1,
                });
            }
        }
        affected_locations.sort();
        affected_locations.dedup();
        evidence.truncate(config.max_evidence);
        out.push(ModuleCycleDiagnostic {
            participants,
            files,
            cycle: CycleDetails {
                path,
                coupling: CycleCoupling::CodeCycle,
            },
            evidence,
            evidence_total,
            affected_locations,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(name: &str, selectors: &[&str]) -> Layer {
        Layer {
            name: name.into(),
            selectors: selectors.iter().map(|s| (*s).into()).collect(),
        }
    }

    #[test]
    fn native_module_depth_uses_each_languages_separator_and_keeps_short_names() {
        for (language, separator) in [
            ("php", "\\"),
            ("python", "."),
            ("rust", "::"),
            ("javascript", "/"),
            ("typescript", "/"),
        ] {
            for (module, expected) in [
                (
                    format!("App{separator}Billing{separator}Store"),
                    format!("App{separator}Billing"),
                ),
                ("App".into(), "App".into()),
                (String::new(), String::new()),
            ] {
                let file = FileModel {
                    path: "source".into(),
                    language: language.into(),
                    module: module.clone(),
                    ..Default::default()
                };
                let config = ArchitectureConfig {
                    module_depth: BTreeMap::from([(language.into(), 2)]),
                    ..Default::default()
                };
                let assignments = assign_modules(&[&file], &CallGraph::default(), &config).unwrap();
                assert_eq!(assignments["source"], format!("{language}:{expected}"));
                assert_eq!(file.module, module);
            }
        }
    }

    #[test]
    fn explicit_module_groups_win_over_depth_and_preserve_native_identity_prefixes() {
        let files = ["selected.php", "native.php"].map(|path| FileModel {
            path: path.into(),
            language: "php".into(),
            module: "App\\Billing\\Store".into(),
            ..Default::default()
        });
        let config = ArchitectureConfig {
            module_depth: BTreeMap::from([("php".into(), 2)]),
            modules: vec![group("store", &["file:selected.php"])],
            ..Default::default()
        };
        let assignments = assign_modules(
            &files.iter().collect::<Vec<_>>(),
            &CallGraph::default(),
            &config,
        )
        .unwrap();
        assert_eq!(assignments["selected.php"], "php:group:store");
        assert_eq!(assignments["native.php"], "php:native:App\\Billing");
    }
    #[test]
    fn file_selectors_share_existing_semantics_and_package_ownership() {
        let file = FileModel {
            path: "src/auth/login.php".into(),
            language: "php".into(),
            module: "App\\Auth\\Session".into(),
            ..Default::default()
        };
        let mut calls = CallGraph::default();
        calls
            .source_packages
            .insert(file.path.clone(), vec!["login-package".into()]);
        for selector in [
            "ns:App\\Auth",
            "mod:App",
            "dir:src/auth",
            "glob:src/**/*.php",
            "file:src/auth/login.php",
            "pkg:login-package",
        ] {
            let config = ArchitectureConfig {
                modules: vec![group("auth", &[selector])],
                ..Default::default()
            };
            let assignments = assign_modules(&[&file], &calls, &config).unwrap();
            assert_eq!(assignments[&file.path], "php:group:auth", "{selector}");
        }
    }
    #[test]
    fn configured_and_native_identities_do_not_collide_or_join_languages() {
        let files = [
            FileModel {
                path: "selected.php".into(),
                language: "php".into(),
                module: "auth".into(),
                ..Default::default()
            },
            FileModel {
                path: "selected.py".into(),
                language: "python".into(),
                module: "auth".into(),
                ..Default::default()
            },
            FileModel {
                path: "native.php".into(),
                language: "php".into(),
                module: "group:auth".into(),
                ..Default::default()
            },
        ];
        let config = ArchitectureConfig {
            modules: vec![group("auth", &["glob:selected.*"])],
            ..Default::default()
        };
        let assignments = assign_modules(
            &files.iter().collect::<Vec<_>>(),
            &CallGraph::default(),
            &config,
        )
        .unwrap();
        assert_eq!(assignments["selected.php"], "php:group:auth");
        assert_eq!(assignments["selected.py"], "python:group:auth");
        assert_eq!(assignments["native.php"], "php:native:group:auth");
        assert_eq!(assignments.values().collect::<BTreeSet<_>>().len(), 3);
    }
    #[test]
    fn excluded_and_vendor_files_do_not_create_assignment_overlap_errors() {
        let files = [
            FileModel {
                path: "vendor/a.php".into(),
                vendor: true,
                ..Default::default()
            },
            FileModel {
                path: "tests/a.php".into(),
                ..Default::default()
            },
            FileModel {
                path: "src/a.php".into(),
                ..Default::default()
            },
        ];
        let config = ArchitectureConfig {
            modules: vec![
                group("first", &["glob:**/a.php"]),
                group("second", &["dir:vendor", "dir:tests"]),
            ],
            ..Default::default()
        };
        validate_module_assignments(&files, &CallGraph::default(), &config).unwrap();
    }
}
