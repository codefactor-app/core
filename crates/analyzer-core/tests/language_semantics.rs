use analyzer_core::{
    graph::NodeKind, Analyzer, AstKind, AstNode, CallKind, CallSite, Confidence, Config,
    ExternalOperationRule, FileModel, FunctionModel, LanguageRegistry, LanguageStrategy, Parameter,
    RawAst, SourceRange, TypeKind, TypeModel,
};
use std::path::Path;

/// This adapter accepts an IR-shaped input so the test exercises registration,
/// discovery, caching and graph semantics without depending on a bundled parser.
struct PolicyLanguage(bool);
impl LanguageStrategy for PolicyLanguage {
    fn language_id(&self) -> &'static str {
        "policy_fixture"
    }
    fn file_extensions(&self) -> &'static [&'static str] {
        &["policy"]
    }
    fn parse(&self, _: &str, path: &Path) -> RawAst {
        RawAst {
            root: AstNode {
                kind: AstKind::File,
                raw_kind: "file".into(),
                range: range(path, 1),
                text: None,
                children: Vec::new(),
            },
            errors: Vec::new(),
        }
    }
    fn normalize(&self, source: &str, path: &Path, _: &RawAst) -> FileModel {
        let mut file: FileModel = serde_json::from_str(source).unwrap();
        file.path = path.to_string_lossy().into_owned();
        file.language = self.language_id().into();
        file
    }
    fn constructor_name(&self) -> &'static str {
        "create"
    }
    fn permits_constructor_inference(&self) -> bool {
        !self.0
    }
    fn namespace_separator(&self) -> &'static str {
        "#"
    }
    fn method_separator(&self) -> &'static str {
        "#"
    }
    fn type_reference(&self, annotation: &str) -> Option<String> {
        Some(annotation.trim().trim_start_matches('@').to_string())
    }
    fn is_native_function(&self, name: &str) -> bool {
        name == "echo"
    }
    fn is_native_macro(&self, name: &str) -> bool {
        name == "bang!"
    }
    fn is_native_receiver(&self, name: &str) -> bool {
        matches!(name, "Host" | "HostBase")
    }
    fn is_native_global_receiver(&self, name: &str) -> bool {
        name == "Host"
    }
    fn is_native_method(&self, owner: &str, name: &str) -> bool {
        owner == "HostBase" && name == "size"
    }
    fn resolve_call(
        &self,
        call: &CallSite,
        _: &analyzer_core::TypeContext,
        _: &FileModel,
    ) -> Vec<String> {
        call.receiver
            .as_ref()
            .map(|receiver| vec![format!("{receiver}#{}", call.name)])
            .unwrap_or_else(|| call.targets.clone())
    }
    fn external_operations(&self) -> Vec<ExternalOperationRule> {
        vec![ExternalOperationRule {
            pattern: "IO#write".into(),
            category: "shell".into(),
            library: "policy".into(),
            operation: "write".into(),
        }]
    }
    fn resolve_project_call(
        &self,
        call: &CallSite,
        context: &analyzer_core::TypeContext,
        file: &FileModel,
        project: &[FileModel],
    ) -> Vec<String> {
        // This adapter's package locator is deliberately outside built-in syntax.
        // The core needs no policy_fixture branch to link it across files.
        if !call.local_binding && call.receiver.is_none() {
            for import in &file.imports {
                if import.alias.as_deref() == Some(&call.name) {
                    return project
                        .iter()
                        .filter(|other| {
                            other.language == file.language
                                && import.source.as_deref()
                                    == Some(&format!("bundle:{}", other.module))
                        })
                        .flat_map(|other| &other.exports)
                        .filter(|export| export.name == import.path && export.source.is_none())
                        .map(|export| export.target.clone())
                        .collect();
                }
            }
        }
        self.resolve_call(call, context, file)
    }
}

fn range(path: &Path, line: usize) -> SourceRange {
    SourceRange {
        file: path.to_string_lossy().into_owned().into(),
        start_line: line,
        end_line: line,
        start_column: 1,
        end_column: 20,
    }
}
fn method(owner: &str, name: &str) -> FunctionModel {
    FunctionModel {
        name: name.into(),
        fqn: format!("demo#{owner}#{name}"),
        parent_type: Some(format!("demo#{owner}")),
        ..Default::default()
    }
}
fn model() -> FileModel {
    let path = Path::new("main.policy");
    let mut calls = vec![
        CallSite {
            name: "Box".into(),
            kind: CallKind::Constructor,
            targets: vec!["demo#Box".into()],
            assignment_to: Some("created".into()),
            ..Default::default()
        },
        CallSite {
            name: "run".into(),
            kind: CallKind::Method,
            receiver: Some("created".into()),
            ..Default::default()
        },
        CallSite {
            name: "run".into(),
            kind: CallKind::Method,
            receiver: Some("typed".into()),
            receiver_type: Some("@Box".into()),
            local_binding: true,
            ..Default::default()
        },
        CallSite {
            name: "Child".into(),
            kind: CallKind::Constructor,
            targets: vec!["demo#Child".into()],
            ..Default::default()
        },
        CallSite {
            name: "echo".into(),
            ..Default::default()
        },
        CallSite {
            name: "bang!".into(),
            kind: CallKind::Macro,
            ..Default::default()
        },
        CallSite {
            name: "clock".into(),
            receiver: Some("Host".into()),
            kind: CallKind::Static,
            ..Default::default()
        },
        CallSite {
            name: "clock".into(),
            receiver: Some("Random".into()),
            kind: CallKind::Method,
            ..Default::default()
        },
        CallSite {
            name: "size".into(),
            receiver: Some("native_child".into()),
            receiver_type: Some("@NativeChild".into()),
            kind: CallKind::Method,
            ..Default::default()
        },
        CallSite {
            name: "write".into(),
            receiver: Some("IO".into()),
            kind: CallKind::Static,
            ..Default::default()
        },
    ];
    for (index, call) in calls.iter_mut().enumerate() {
        call.range = range(path, index + 10);
    }
    FileModel {
        module: "demo".into(),
        types: [
            ("Box", Vec::new()),
            ("Child", vec!["Box".into()]),
            ("NativeChild", vec!["HostBase".into()]),
        ]
        .into_iter()
        .map(|(name, extends)| TypeModel {
            name: name.into(),
            fqn: format!("demo#{name}"),
            kind: TypeKind::Class,
            extends,
            range: range(path, 1),
            ..Default::default()
        })
        .collect(),
        functions: vec![
            method("Box", "create"),
            method("Box", "run"),
            FunctionModel {
                name: "flow".into(),
                fqn: "demo#flow".into(),
                calls,
                parameters: vec![Parameter {
                    name: "typed".into(),
                    type_name: Some("@Box".into()),
                    ..Default::default()
                }],
                range: range(path, 9),
                body_range: range(path, 9),
                ..Default::default()
            },
        ],
        ..Default::default()
    }
}

#[test]
fn registered_semantics_drive_constructor_native_calls_annotations_and_custom_separators() {
    let root = std::env::temp_dir().join(format!(
        "codefactor-policy-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("main.policy"),
        serde_json::to_string(&model()).unwrap(),
    )
    .unwrap();
    let mut registry = LanguageRegistry::new();
    registry.register(PolicyLanguage(false));
    let mut config = Config::default();
    config.architecture.enabled = false;
    config.algorithmic_complexity.enabled = false;
    let analyzer = Analyzer::new(registry, config);
    let report = analyzer.scan(&root).unwrap();
    let expected = [
        (10, "demo#Box#create", Confidence::Resolved),
        (11, "demo#Box#run", Confidence::Resolved),
        (12, "demo#Box#run", Confidence::Resolved),
        (13, "demo#Box#create", Confidence::Resolved),
        (
            14,
            "external:runtime:policy_fixture:echo",
            Confidence::Resolved,
        ),
        (
            15,
            "external:runtime:policy_fixture:bang!",
            Confidence::Resolved,
        ),
        (
            16,
            "external:runtime:policy_fixture:Host::clock",
            Confidence::Resolved,
        ),
        (17, "Random->clock", Confidence::Dynamic),
        (
            18,
            "external:runtime:policy_fixture:HostBase::size",
            Confidence::Resolved,
        ),
        (19, "external:shell:policy:write", Confidence::Resolved),
    ];
    for (line, target, confidence) in expected {
        let edges = report
            .graph
            .edges
            .iter()
            .filter(|edge| edge.callsite.start_line == line)
            .collect::<Vec<_>>();
        assert_eq!(edges.len(), 1, "line {line}: {edges:?}");
        assert!(edges[0].to.ends_with(target), "line {line}: {edges:?}");
        assert_eq!(edges[0].confidence, confidence, "line {line}: {edges:?}");
    }
    let cached = analyzer.scan(&root).unwrap();
    assert_eq!(cached.summary.cache_hits, 1);
    assert_eq!(cached.graph.edges.len(), report.graph.edges.len());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn project_declarations_shadow_registered_native_names() {
    let mut file = model();
    file.path = "main.policy".into();
    file.language = "policy_fixture".into();
    file.functions.push(FunctionModel {
        name: "echo".into(),
        fqn: "demo#echo".into(),
        ..Default::default()
    });
    let mut registry = LanguageRegistry::new();
    registry.register(PolicyLanguage(false));
    let graph = analyzer_core::graph::CallGraph::build(&[file], &registry, &Config::default());
    let edge = graph
        .edges
        .iter()
        .find(|edge| edge.callsite.start_line == 14)
        .unwrap();
    assert!(edge.to.ends_with("demo#echo"), "{edge:?}");
    assert_eq!(graph.node(&edge.to).unwrap().kind, NodeKind::Function);
}

#[test]
fn custom_package_resolution_uses_project_ir_without_a_core_language_branch() {
    let mut caller = model();
    caller.path = "main.policy".into();
    caller.language = "policy_fixture".into();
    caller.imports.push(analyzer_core::Import {
        path: "publicWork".into(),
        alias: Some("invoke".into()),
        source: Some("bundle:remote".into()),
        ..Default::default()
    });
    caller.functions.last_mut().unwrap().calls = vec![
        CallSite {
            name: "invoke".into(),
            range: range(Path::new("main.policy"), 20),
            ..Default::default()
        },
        CallSite {
            name: "invoke".into(),
            local_binding: true,
            binding_unknown: true,
            range: range(Path::new("main.policy"), 21),
            ..Default::default()
        },
    ];
    let remote = FileModel {
        path: "remote.policy".into(),
        module: "remote".into(),
        language: "policy_fixture".into(),
        exports: vec![analyzer_core::Export {
            name: "publicWork".into(),
            target: "remote#work".into(),
            source: None,
            type_only: false,
        }],
        functions: vec![FunctionModel {
            name: "work".into(),
            fqn: "remote#work".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut registry = LanguageRegistry::new();
    registry.register(PolicyLanguage(false));
    let graph =
        analyzer_core::graph::CallGraph::build(&[caller, remote], &registry, &Config::default());
    let resolved = graph
        .edges
        .iter()
        .find(|edge| edge.callsite.start_line == 20)
        .unwrap();
    assert_eq!(
        resolved.to.as_ref(),
        "fn:policy_fixture:remote.policy:remote#work"
    );
    assert_eq!(resolved.confidence, Confidence::Resolved);
    let shadowed = graph
        .edges
        .iter()
        .find(|edge| edge.callsite.start_line == 21)
        .unwrap();
    assert_eq!(shadowed.confidence, Confidence::Dynamic);
}

#[test]
fn registered_constructor_inference_policy_keeps_explicit_sites_and_native_controls() {
    for explicit_only in [false, true] {
        let mut file = model();
        file.path = "main.policy".into();
        file.language = "policy_fixture".into();
        file.functions.last_mut().unwrap().calls = vec![
            CallSite {
                name: "Box".into(),
                targets: vec!["demo#Box".into()],
                kind: CallKind::Function,
                range: range(Path::new("main.policy"), 80),
                ..Default::default()
            },
            CallSite {
                name: "Box".into(),
                targets: vec!["demo#Box".into()],
                kind: CallKind::Constructor,
                range: range(Path::new("main.policy"), 81),
                ..Default::default()
            },
            CallSite {
                name: "Host".into(),
                kind: CallKind::Function,
                range: range(Path::new("main.policy"), 82),
                ..Default::default()
            },
        ];
        let mut registry = LanguageRegistry::new();
        registry.register(PolicyLanguage(explicit_only));
        let graph = analyzer_core::graph::CallGraph::build(&[file], &registry, &Config::default());
        for line in [80, 81, 82] {
            let edge = graph
                .edges
                .iter()
                .find(|edge| edge.callsite.start_line == line)
                .unwrap();
            if line == 81 || !explicit_only {
                assert_eq!(
                    edge.confidence,
                    Confidence::Resolved,
                    "{explicit_only} {line}: {edge:?}"
                );
                assert!(
                    edge.to.ends_with(if line == 82 {
                        "Host"
                    } else {
                        "demo#Box#create"
                    }),
                    "{edge:?}"
                );
            } else {
                assert_eq!(
                    edge.confidence,
                    Confidence::Dynamic,
                    "{explicit_only} {line}: {edge:?}"
                );
                assert_eq!(graph.node(&edge.to).unwrap().kind, NodeKind::Unknown);
            }
        }
    }
}
