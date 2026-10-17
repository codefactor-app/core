use analyzer_core::{
    cache::Cache, graph::NodeKind, AnalysisReport, Analyzer, Confidence, Config, LanguageRegistry,
    SourceRange,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Deserialize)]
struct Corpus {
    schema_version: usize,
    threshold_percent: usize,
    language_minimum_static_calls: BTreeMap<String, usize>,
    projects: Vec<Project>,
}
#[derive(Deserialize)]
struct Project {
    id: String,
    revision: String,
    language: String,
    files: Vec<PinnedFile>,
    calls: Vec<Annotation>,
}
#[derive(Deserialize)]
struct PinnedFile {
    path: String,
    sha256: String,
}
#[derive(Deserialize)]
struct Annotation {
    id: String,
    path: String,
    line: usize,
    column: usize,
    callee: String,
    source_line: String,
    expected: Option<Target>,
    category: String,
}
#[derive(Deserialize)]
struct Target {
    path: String,
    name: String,
    line: usize,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Correct,
    MissingCall,
    Unresolved,
    Ambiguous,
    WrongTarget,
    DynamicCorrect,
    DynamicMissing,
    DynamicFalsePositive,
}
#[derive(Default, Serialize)]
struct Score {
    total: usize,
    correct: usize,
    missing: usize,
    unresolved: usize,
    ambiguous: usize,
    wrong: usize,
    dynamic_total: usize,
    dynamic_correct: usize,
}
impl Score {
    fn record(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::DynamicCorrect => {
                self.dynamic_total += 1;
                self.dynamic_correct += 1;
            }
            Outcome::DynamicMissing | Outcome::DynamicFalsePositive => self.dynamic_total += 1,
            _ => {
                self.total += 1;
                match outcome {
                    Outcome::Correct => self.correct += 1,
                    Outcome::MissingCall => self.missing += 1,
                    Outcome::Unresolved => self.unresolved += 1,
                    Outcome::Ambiguous => self.ambiguous += 1,
                    Outcome::WrongTarget => self.wrong += 1,
                    _ => unreachable!(),
                }
            }
        }
    }
    fn percent(&self) -> f64 {
        100.0 * self.correct as f64 / self.total.max(1) as f64
    }
    fn passes(&self, threshold: usize) -> bool {
        self.total > 0 && self.correct * 100 >= self.total * threshold
    }
}

fn contains(range: &SourceRange, line: usize, column: usize) -> bool {
    (range.start_line, range.start_column) <= (line, column)
        && (line, column) < (range.end_line, range.end_column)
}

fn evaluate(report: &AnalysisReport, annotation: &Annotation) -> (Outcome, Vec<String>) {
    let sites = report
        .file(&annotation.path)
        .into_iter()
        .flat_map(|file| &file.functions)
        .flat_map(|function| &function.calls)
        .filter(|site| {
            site.name.rsplit(['.', ':']).next().is_some_and(|name| {
                name.trim_start_matches('$') == annotation.callee.trim_start_matches('$')
            }) && contains(&site.range, annotation.line, annotation.column)
        });
    // Inner and outer calls can contain the same token. The independently
    // annotated callee name and narrowest matching span identify this call.
    let site = sites.min_by_key(|site| {
        (
            site.range.end_line - site.range.start_line,
            site.range
                .end_column
                .saturating_sub(site.range.start_column),
        )
    });
    let Some(site) = site else {
        return (
            if annotation.expected.is_none() {
                Outcome::DynamicMissing
            } else {
                Outcome::MissingCall
            },
            vec![],
        );
    };
    let edges = report
        .graph
        .edges
        .iter()
        .filter(|edge| edge.callsite == site.range)
        .collect::<Vec<_>>();
    let actual = edges
        .iter()
        .map(|edge| {
            report.graph.node(&edge.to).map_or_else(
                || edge.to.to_string(),
                |node| {
                    format!(
                        "{}:{}:{}:{:?}",
                        node.file.as_deref().unwrap_or("?"),
                        node.line.unwrap_or(0),
                        node.label,
                        edge.confidence
                    )
                },
            )
        })
        .collect::<Vec<_>>();
    let Some(expected) = &annotation.expected else {
        if edges.is_empty() {
            return (Outcome::DynamicMissing, actual);
        }
        let false_positive = edges.iter().any(|edge| {
            edge.confidence == Confidence::Resolved || report.graph.node(&edge.to).is_none()
        });
        return (
            if false_positive {
                Outcome::DynamicFalsePositive
            } else {
                Outcome::DynamicCorrect
            },
            actual,
        );
    };
    if edges.is_empty() {
        return (Outcome::Unresolved, actual);
    }
    if edges
        .iter()
        .any(|edge| edge.confidence == Confidence::Ambiguous)
    {
        return (Outcome::Ambiguous, actual);
    }
    let targets = edges
        .iter()
        .filter_map(|edge| report.graph.node(&edge.to).map(|node| (edge, node)))
        .collect::<Vec<_>>();
    let correct = !targets.is_empty()
        && targets.len() == edges.len()
        && targets.iter().all(|(edge, node)| {
            edge.confidence == Confidence::Resolved
                && node.kind == NodeKind::Function
                && node.file.as_deref() == Some(expected.path.as_str())
                && node.line == Some(expected.line)
                && node.label.rsplit(['.', ':']).next() == Some(expected.name.as_str())
        });
    let unresolved = targets.iter().all(|(edge, node)| {
        edge.confidence != Confidence::Resolved || node.kind != NodeKind::Function
    });
    (
        if correct {
            Outcome::Correct
        } else if unresolved {
            Outcome::Unresolved
        } else {
            Outcome::WrongTarget
        },
        actual,
    )
}

#[test]
fn pinned_real_source_call_resolution_reaches_ninety_percent() {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/call_resolution_corpus");
    let corpus: Corpus =
        serde_json::from_str(&std::fs::read_to_string(base.join("manifest.json")).unwrap())
            .unwrap();
    assert_eq!(corpus.schema_version, 1);
    assert_eq!(corpus.threshold_percent, 90);
    let languages = ["javascript", "php", "python", "rust", "typescript"];
    assert_eq!(
        corpus
            .language_minimum_static_calls
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        languages
    );
    assert!(corpus
        .language_minimum_static_calls
        .values()
        .all(|minimum| *minimum >= 30));
    assert_eq!(
        corpus
            .projects
            .iter()
            .filter(|project| ["redux", "immer", "getopts", "glob"].contains(&project.id.as_str()))
            .flat_map(|project| &project.calls)
            .filter(|call| call.expected.is_some())
            .count(),
        303,
        "the original static corpus must be preserved"
    );
    let mut overall = Score::default();
    let mut by_language = BTreeMap::<String, Score>::new();
    let mut by_category = BTreeMap::<String, Score>::new();
    let mut project_results = Vec::new();
    let mut failures = Vec::new();
    let fixed_regressions = [
        "immer:src/core/proxy.ts:216:1:each",
        "getopts:src/lib.rs:441:57:long_to_short",
        "glob:src/lib.rs:460:39:matches_with",
        "glob:src/lib.rs:956:40:matches_with",
        // Dynamic mutations of an unknown object must not erase dispatch
        // proofs for unrelated, known class receivers across the project.
        "immer:src/core/immerClass.ts:45:4:setAutoFreeze",
        "immer:src/core/immerClass.ts:47:4:setUseStrictShallowCopy",
        "immer:src/core/immerClass.ts:125:5:produceWithPatches",
        "immer:src/core/immerClass.ts:129:18:produce",
        "immer:src/core/immerClass.ts:198:10:produce",
        "immer:src/plugins/mapset.ts:63:9:has",
        "immer:src/plugins/mapset.ts:97:22:get",
        "immer:src/plugins/mapset.ts:130:20:get",
        "immer:src/plugins/mapset.ts:147:20:get",
        "immer:src/plugins/mapset.ts:212:9:has",
        "immer:src/plugins/mapset.ts:221:9:has",
    ];
    let mut regression_outcomes = BTreeMap::new();
    let mut syntax_failures = Vec::new();
    for project in &corpus.projects {
        assert_eq!(project.revision.len(), 40);
        let root = base.join("projects").join(&project.id);
        for file in &project.files {
            let source = std::fs::read_to_string(root.join(&file.path)).unwrap();
            assert_eq!(
                Cache::content_hash(&source, "", ""),
                file.sha256,
                "upstream source changed: {}/{}",
                project.id,
                file.path
            );
        }
        let config: Config =
            serde_yaml::from_str(&std::fs::read_to_string(root.join("analyzer.yaml")).unwrap())
                .unwrap();
        let mut registry = LanguageRegistry::new();
        registry.register(analyzer_typescript::TypeScript);
        registry.register(analyzer_rust::Rust);
        registry.register(analyzer_python::Python);
        registry.register(analyzer_php::Php);
        registry.register(analyzer_javascript::JavaScript);
        let report = Analyzer::new(registry, config).scan(&root).unwrap();
        if !report.parse_errors.is_empty() {
            syntax_failures.push(format!("{}: {:?}", project.id, report.parse_errors));
        }
        let mut score = Score::default();
        let mut outcomes = Vec::new();
        for annotation in &project.calls {
            let source = std::fs::read_to_string(root.join(&annotation.path)).unwrap();
            assert_eq!(
                source.lines().nth(annotation.line - 1),
                Some(annotation.source_line.as_str()),
                "annotation source drift: {}",
                annotation.id
            );
            if let Some(target) = &annotation.expected {
                let target_source = std::fs::read_to_string(root.join(&target.path)).unwrap();
                assert!(target_source
                    .lines()
                    .nth(target.line - 1)
                    .unwrap()
                    .contains(&target.name));
            }
            let (outcome, actual) = evaluate(&report, annotation);
            if fixed_regressions.contains(&annotation.id.as_str()) {
                regression_outcomes.insert(annotation.id.as_str(), outcome);
            }
            overall.record(outcome);
            score.record(outcome);
            by_language
                .entry(project.language.clone())
                .or_default()
                .record(outcome);
            by_category
                .entry(annotation.category.clone())
                .or_default()
                .record(outcome);
            if !matches!(outcome, Outcome::Correct | Outcome::DynamicCorrect) {
                failures.push(format!("{}: {outcome:?} -> {actual:?}", annotation.id));
            }
            outcomes
                .push(serde_json::json!({"id":annotation.id,"outcome":outcome,"actual":actual}));
        }
        println!(
            "{}: {}/{} ({:.2}%), dynamic {}/{}",
            project.id,
            score.correct,
            score.total,
            score.percent(),
            score.dynamic_correct,
            score.dynamic_total
        );
        project_results.push(serde_json::json!({"project":project.id,"revision":project.revision,"score":score,"accuracy_percent":score.percent(),"parse_errors":report.parse_errors,"outcomes":outcomes}));
    }
    let result = serde_json::json!({
        "schema_version":1,"metric":"exact_correct_static_targets / all_annotated_static_calls",
        "threshold_percent":corpus.threshold_percent,"overall":overall,"accuracy_percent":overall.percent(),
        "by_language":by_language,"by_category":by_category,"projects":project_results
    });
    if let Some(path) = std::env::var_os("CODEFACTOR_CORPUS_REPORT") {
        std::fs::write(path, serde_json::to_string_pretty(&result).unwrap() + "\n").unwrap();
    }
    println!(
        "overall: {}/{} ({:.2}%)",
        overall.correct,
        overall.total,
        overall.percent()
    );
    assert!(
        overall.total >= corpus.language_minimum_static_calls.values().sum(),
        "corpus denominator unexpectedly shrank"
    );
    assert!(
        overall.passes(corpus.threshold_percent),
        "{result}\n{}",
        failures.join("\n")
    );
    assert_eq!(
        by_language.keys().map(String::as_str).collect::<Vec<_>>(),
        languages,
        "all five languages must remain represented"
    );
    for (language, score) in &by_language {
        assert!(
            score.total >= corpus.language_minimum_static_calls[language],
            "{language} static denominator unexpectedly shrank"
        );
        assert!(
            score.passes(corpus.threshold_percent),
            "{language}: {result}\n{}",
            failures.join("\n")
        );
    }
    assert_eq!(
        overall.dynamic_correct,
        overall.dynamic_total,
        "false confident dynamic target: {}",
        failures.join("\n")
    );
    assert!(
        syntax_failures.is_empty(),
        "valid pinned sources produced syntax diagnostics: {}",
        syntax_failures.join("\n")
    );
    for id in fixed_regressions {
        assert_eq!(
            regression_outcomes.get(id),
            Some(&Outcome::Correct),
            "fixed concrete regression returned: {id}\n{}",
            failures.join("\n")
        );
    }
}

#[test]
fn corpus_denominator_includes_missing_unresolved_and_wrong_calls() {
    let mut score = Score::default();
    for outcome in [
        Outcome::Correct,
        Outcome::MissingCall,
        Outcome::Unresolved,
        Outcome::Ambiguous,
        Outcome::WrongTarget,
    ] {
        score.record(outcome);
    }
    assert_eq!(score.total, 5);
    assert_eq!(score.correct, 1);
    assert_eq!(score.percent(), 20.0);
    assert!(!score.passes(90));
    score.record(Outcome::DynamicCorrect);
    score.record(Outcome::DynamicFalsePositive);
    assert_eq!(score.total, 5);
    assert_eq!(score.dynamic_total, 2);
    assert_eq!(score.dynamic_correct, 1);
}
