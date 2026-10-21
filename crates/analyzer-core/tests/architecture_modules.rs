use analyzer_core::architecture::{ArchitectureConfig, FileFacts, Layer};
use analyzer_core::config::{PathLimits, Threshold};
use analyzer_core::graph::CallGraph;
use analyzer_core::ir::*;
use analyzer_core::{Analyzer, Config, ConfigLoader, LanguageRegistry, LanguageStrategy};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

struct Mini;
impl LanguageStrategy for Mini {
    fn language_id(&self) -> &'static str {
        "mini"
    }
    fn file_extensions(&self) -> &'static [&'static str] {
        &["mini"]
    }
    fn parse(&self, _: &str, path: &Path) -> RawAst {
        RawAst {
            root: AstNode {
                kind: AstKind::File,
                raw_kind: "file".into(),
                range: range(path, 1),
                text: None,
                children: vec![],
            },
            errors: vec![],
        }
    }
    fn normalize(&self, source: &str, path: &Path, _: &RawAst) -> FileModel {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        FileModel {
            path: path.display().to_string(),
            language: "mini".into(),
            module: source
                .lines()
                .find_map(|line| line.strip_prefix("module "))
                .unwrap_or("root")
                .into(),
            architecture_facts: Some(FileFacts::default().into()),
            functions: vec![FunctionModel {
                name: name.clone(),
                fqn: name,
                range: range(path, 1),
                body_range: range(path, 1),
                calls: source
                    .lines()
                    .filter_map(|line| line.strip_prefix("call "))
                    .map(|target| CallSite {
                        name: target.into(),
                        targets: vec![target.into()],
                        range: range(path, 3),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }
}
fn range(path: &Path, line: usize) -> SourceRange {
    SourceRange {
        file: path.display().to_string().into(),
        start_line: line,
        end_line: line,
        start_column: 1,
        end_column: 2,
    }
}
struct Project(PathBuf);
impl Project {
    fn new(sources: &[(&str, &str)]) -> Self {
        let root = std::env::temp_dir().join(format!(
            "architecture-modules-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        for (path, source) in sources {
            let target = root.join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, source).unwrap();
        }
        Self(root)
    }
    fn scan(&self, config: Config) -> analyzer_core::AnalysisReport {
        analyzer(config).scan(&self.0).unwrap()
    }
}
impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn analyzer(config: Config) -> Analyzer {
    let mut registry = LanguageRegistry::new();
    registry.register(Mini);
    Analyzer::new(registry, config)
}
fn config() -> Config {
    let mut config = Config::default();
    config.cache.enabled = false;
    config.architecture.exclude.clear();
    config
        .limits
        .global
        .insert("module_cycle".into(), Threshold::new(0.0, 0.0));
    config
}
fn group(name: &str, selectors: &[&str]) -> Layer {
    Layer {
        name: name.into(),
        selectors: selectors.iter().map(|s| (*s).into()).collect(),
    }
}
fn cycle(report: &analyzer_core::AnalysisReport) -> &analyzer_core::rules::Violation {
    let findings = report
        .violations
        .iter()
        .filter(|v| v.rule == "module_cycle")
        .collect::<Vec<_>>();
    assert_eq!(findings.len(), 1);
    findings[0]
}

#[test]
fn numeric_cycle_is_one_component_with_complete_files_and_bounded_evidence() {
    let project = Project::new(&[
        ("a.mini", "module Auth\ncall b"),
        ("b.mini", "module Rank\ncall a"),
        ("spare.mini", "module Auth"),
    ]);
    let mut config = config();
    config.architecture.max_evidence = 1;
    let report = project.scan(config);
    let violation = cycle(&report);
    let diagnostic = violation.module_cycle.as_ref().unwrap();
    assert_eq!(violation.file, "a.mini");
    assert_eq!(violation.value, Some(1.0));
    assert_eq!(violation.limit, Some(0.0));
    assert_eq!(diagnostic.participants, ["mini:Auth", "mini:Rank"]);
    assert_eq!(diagnostic.files, ["a.mini", "b.mini", "spare.mini"]);
    assert_eq!(diagnostic.evidence.len(), 1);
    assert_eq!(diagnostic.evidence_total, 2);
    assert_eq!(diagnostic.cycle.path.first(), diagnostic.cycle.path.last());
    assert_eq!(
        diagnostic
            .affected_ranges()
            .map(|r| r.file.as_ref())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["a.mini", "b.mini", "spare.mini"])
    );
    assert!(report
        .metrics
        .iter()
        .filter(|m| m.id.as_ref() == "module_cycle")
        .all(|m| m.value == 1.0));
    let json = serde_json::to_value(violation).unwrap();
    assert_eq!(json["module_cycle"]["files"].as_array().unwrap().len(), 3);
    assert!(json["module_cycle"].get("affected_locations").is_none());
}

#[test]
fn explicit_groups_use_or_and_preserve_source_modules_and_native_cycles() {
    let project = Project::new(&[
        ("auth/a.mini", "module App\\Auth\\Session\ncall b"),
        ("legacy/b.mini", "module Old\\Session\ncall a"),
        ("c.mini", "module NativeC\ncall d"),
        ("d.mini", "module NativeD\ncall c"),
    ]);
    let mut config = config();
    config.architecture.modules = vec![
        group("auth", &["ns:App\\Auth", "dir:legacy", "glob:auth/**"]),
        group("unused", &["dir:absent"]),
    ];
    let report = project.scan(config);
    let d = cycle(&report).module_cycle.as_ref().unwrap();
    assert_eq!(
        d.participants,
        ["mini:native:NativeC", "mini:native:NativeD"]
    );
    assert_eq!(
        report
            .files
            .iter()
            .find(|f| f.path == "auth/a.mini")
            .unwrap()
            .module,
        "App\\Auth\\Session"
    );
    let arch = report
        .violations
        .iter()
        .find(|v| v.rule == "arch_cycle")
        .unwrap()
        .architecture
        .as_ref()
        .unwrap();
    assert_eq!(d.participants, arch.participants);
    assert_eq!(
        report
            .metrics
            .iter()
            .filter(|m| m.id.as_ref() == "module_cycle" && m.value == 0.0)
            .count(),
        2
    );
}

#[test]
fn overlapping_groups_fail_even_when_advisory_architecture_is_disabled() {
    let project = Project::new(&[("a.mini", "module App\\Auth")]);
    let mut config = config();
    config.architecture.enabled = false;
    config.architecture.modules = vec![
        group("auth", &["ns:App\\Auth"]),
        group("all", &["file:a.mini"]),
    ];
    for reverse in [false, true] {
        if reverse {
            config.architecture.modules.reverse();
        }
        let error = analyzer(config.clone())
            .scan(&project.0)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("a.mini") && error.contains("all") && error.contains("auth"),
            "{error}"
        );
    }
}

#[test]
fn thresholds_legacy_baseline_and_suppressions_apply_before_grouping() {
    let project = Project::new(&[
        ("a.mini", "module A\ncall b"),
        ("b.mini", "module B\ncall a"),
    ]);
    let mut config = config();
    config.limits.by_path.push(PathLimits {
        glob: "a.mini".into(),
        metrics: BTreeMap::from([(
            "module_cycle".into(),
            Threshold {
                warn: Some(0.0),
                error: Some(2.0),
            },
        )]),
    });
    let report = project.scan(config.clone());
    assert_eq!(cycle(&report).file, "b.mini");
    std::fs::write(
        project.0.join(".analyzer-baseline.json"),
        "[\"b.mini:B:module_cycle:error\"]",
    )
    .unwrap();
    let report = project.scan(config.clone());
    assert_eq!(cycle(&report).file, "a.mini");
    assert_eq!(
        cycle(&report).severity,
        analyzer_core::rules::Severity::Warn
    );
    std::fs::write(
        project.0.join("a.mini"),
        "# analyzer-ignore: module_cycle\nmodule A\ncall b",
    )
    .unwrap();
    assert!(!project
        .scan(config)
        .violations
        .iter()
        .any(|v| v.rule == "module_cycle"));
}

#[test]
fn grouped_baseline_ignores_representative_and_file_order() {
    let project = Project::new(&[
        ("a.mini", "module A\ncall b"),
        ("b.mini", "module B\ncall a"),
    ]);
    let config = config();
    let first = project.scan(config.clone());
    let fingerprint = cycle(&first).fingerprint();
    assert!(fingerprint.starts_with("module_cycle:"));
    std::fs::write(
        project.0.join(".analyzer-baseline.json"),
        serde_json::to_string(&vec![fingerprint.clone()]).unwrap(),
    )
    .unwrap();
    std::fs::write(
        project.0.join("a.mini"),
        "# analyzer-ignore: module_cycle\nmodule A\ncall b",
    )
    .unwrap();
    assert!(!project
        .scan(config)
        .violations
        .iter()
        .any(|v| v.rule == "module_cycle"));
    let mut modified = cycle(&first).clone();
    modified
        .module_cycle
        .as_mut()
        .unwrap()
        .participants
        .reverse();
    modified.file = "z.mini".into();
    assert_eq!(fingerprint, modified.fingerprint());
    modified.severity = analyzer_core::rules::Severity::Warn;
    assert_ne!(fingerprint, modified.fingerprint());
}

#[test]
fn separate_components_and_numeric_rule_survive_disabled_advisories() {
    let project = Project::new(&[
        ("a.mini", "module A\ncall b"),
        ("b.mini", "module B\ncall a"),
        ("c.mini", "module C\ncall d"),
        ("d.mini", "module D\ncall c"),
    ]);
    let mut config = config();
    config.architecture.enabled = false;
    let report = project.scan(config);
    assert_eq!(
        report
            .violations
            .iter()
            .filter(|v| v.rule == "module_cycle")
            .count(),
        2
    );
    assert_eq!(report.summary.errors, 2);
    assert!(!report.violations.iter().any(|v| v.architecture.is_some()));
}

#[test]
fn module_configuration_is_strictly_validated() {
    for yaml in [
        "modules: [{name: one, selectors: ['class:Foo']}]",
        "modules: [{name: one, selectors: ['glob:[']}]",
        "modules: [{name: '', selectors: ['dir:src']}]",
        "modules: [{name: '  ', selectors: ['dir:src']}]",
        "modules: [{name: one, selectors: []}]",
        "modules: [{name: one, selectors: ['dir:src']}, {name: one, selectors: ['dir:lib']}]",
    ] {
        let project = Project::new(&[("analyzer.yaml", &format!("architecture:\n  {yaml}\n"))]);
        let loaded = ConfigLoader::new()
            .load(&project.0.join("analyzer.yaml"), true, false)
            .unwrap();
        assert!(!loaded.diagnostics.is_empty(), "{yaml}");
        assert!(
            loaded.config.architecture.modules.is_empty()
                || yaml.contains("{name: one, selectors: ['dir:src']},"),
            "{yaml}"
        );
    }
    let project = Project::new(&[(
        "analyzer.yaml",
        "architecture:\n  modules: [{name: one, selectors: ['dir:src', 'ns:App\\Auth']}]\n",
    )]);
    let loaded = ConfigLoader::new()
        .load(&project.0.join("analyzer.yaml"), true, false)
        .unwrap();
    assert_eq!(loaded.config.architecture.modules.len(), 1);
}

#[test]
fn lcom_is_inapplicable_to_interfaces_and_enums_in_every_language() {
    for language in ["php", "python", "javascript", "typescript", "rust"] {
        for kind in [
            TypeKind::Class,
            TypeKind::Trait,
            TypeKind::Struct,
            TypeKind::Interface,
            TypeKind::Enum,
        ] {
            let applies = !matches!(kind, TypeKind::Interface | TypeKind::Enum);
            let file = FileModel {
                path: "type".into(),
                language: language.into(),
                types: vec![TypeModel {
                    fqn: "Shape".into(),
                    kind,
                    ..Default::default()
                }],
                functions: ["one", "two"]
                    .into_iter()
                    .map(|name| FunctionModel {
                        name: name.into(),
                        fqn: format!("Shape::{name}"),
                        parent_type: Some("Shape".into()),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            };
            let mut config = config();
            config
                .limits
                .global
                .insert("lcom4".into(), Threshold::new(1.0, 1.0));
            let metrics = analyzer_core::metrics::advanced_metrics(
                std::slice::from_ref(&file),
                &CallGraph::default(),
                &BTreeMap::new(),
                &ArchitectureConfig::default(),
            );
            assert_eq!(
                metrics.iter().any(|m| m.id.as_ref() == "lcom4"),
                applies,
                "{language}"
            );
            let violations = analyzer_core::rules::evaluate(
                &metrics,
                &[file],
                &[],
                &config,
                &BTreeSet::new(),
                &BTreeMap::new(),
            );
            assert_eq!(
                violations.iter().any(|v| v.rule == "lcom4"),
                applies,
                "{language}"
            );
        }
    }
}

#[test]
fn stability_uses_the_same_explicit_component_assignments() {
    let mut sources = vec![
        (
            "core/a.mini".to_string(),
            "module A\\Deep\ncall b".to_string(),
        ),
        (
            "core/spare.mini".to_string(),
            "module Other\\Namespace".to_string(),
        ),
        (
            "out/b.mini".to_string(),
            "module B\ncall q1\ncall q2\ncall q3\ncall q4\ncall q5".to_string(),
        ),
    ];
    for index in 1..=5 {
        sources.push((format!("p{index}.mini"), format!("module P{index}\ncall a")));
        sources.push((format!("q{index}.mini"), format!("module Q{index}")));
    }
    let project = Project::new(
        &sources
            .iter()
            .map(|(path, source)| (path.as_str(), source.as_str()))
            .collect::<Vec<_>>(),
    );
    let mut config = config();
    config.architecture.profile = analyzer_core::architecture::Profile::Extended;
    config.architecture.modules = vec![
        group("stable", &["dir:core"]),
        group("unstable", &["dir:out"]),
    ];
    let report = project.scan(config);
    let finding = report
        .violations
        .iter()
        .find(|v| v.rule == "arch_stability")
        .unwrap();
    assert_eq!(
        finding.architecture.as_ref().unwrap().participants,
        ["mini:group:stable", "mini:group:unstable"]
    );
}
