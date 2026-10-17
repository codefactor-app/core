use analyzer_core::{
    codemap::DependencyIndex, graph::CallGraph, AnalysisReport, Config, LanguageRegistry,
};
use std::path::Path;

fn index(sources: &[(&str, &str)]) -> DependencyIndex {
    let mut registry = LanguageRegistry::new();
    registry.register(analyzer_php::Php);
    registry.register(analyzer_python::Python);
    registry.register(analyzer_javascript::JavaScript);
    registry.register(analyzer_typescript::TypeScript);
    registry.register(analyzer_rust::Rust);
    let config = Config::default();
    let files = sources
        .iter()
        .map(|(path, source)| {
            let path = Path::new(path);
            let model = registry
                .by_extension(path.extension().unwrap().to_str().unwrap())
                .unwrap()
                .analyze_with_architecture(source, path, true);
            assert!(
                model.parse_errors.is_empty(),
                "{path:?}: {:?}",
                model.parse_errors
            );
            model
        })
        .collect::<Vec<_>>();
    let graph = CallGraph::build(&files, &registry, &config);
    DependencyIndex::build(
        &AnalysisReport {
            version: String::new(),
            schema_version: String::new(),
            root: String::new(),
            config_path: None,
            strategy_capabilities: Default::default(),
            summary: Default::default(),
            aggregates: vec![],
            files,
            metrics: vec![],
            violations: vec![],
            parse_errors: vec![],
            graph,
            deferred_vendor: vec![],
            resolution_diagnostics: vec![],
            architecture: Default::default(),
        },
        &config,
        &registry,
    )
}
fn edges<'a>(
    index: &'a DependencyIndex,
    from: &str,
    to: &str,
    kind: &str,
) -> Vec<&'a analyzer_core::codemap::Witness> {
    index
        .witnesses
        .iter()
        .filter(|w| {
            w.detail_kind == kind
                && index.entities[&w.from].file.as_deref() == Some(from)
                && index.entities[&w.to].file.as_deref() == Some(to)
        })
        .collect()
}

#[test]
fn explicit_js_ts_imports_keep_physical_source_for_all_dependency_kinds() {
    let index = index(&[
        ("model.js", "export class Model { value=1; run(){} }"),
        ("model.ts", "export class Model { value=2; run(){} }"),
        ("main.ts", "import {Model} from './model.js';\nexport class App extends Model { read(model: Model): Model { model.run(); model.value=3; return model; } }"),
        ("main.js", "import {Model} from './model.ts';\nexport class App extends Model { read(){ const model=new Model(); model.run(); model.value=3; return model; } }"),
    ]);
    for (from, to, wrong) in [
        ("main.ts", "model.js", "model.ts"),
        ("main.js", "model.ts", "model.js"),
    ] {
        for kind in ["import", "inheritance", "member_access", "call"] {
            assert!(
                !edges(&index, from, to, kind).is_empty(),
                "missing {from} -> {to} {kind}"
            );
            assert!(
                edges(&index, from, wrong, kind).is_empty(),
                "wrong {from} -> {wrong} {kind}"
            );
        }
        for w in edges(&index, from, to, "member_access") {
            assert_eq!(w.range.file.as_ref(), from);
            assert_eq!(w.range.start_line, 2);
        }
    }
    for kind in ["parameter_type", "return_type"] {
        assert!(
            !edges(&index, "main.ts", "model.js", kind).is_empty(),
            "missing {kind}"
        );
        assert!(
            edges(&index, "main.ts", "model.ts", kind).is_empty(),
            "wrong {kind}"
        );
    }
}

#[test]
fn project_map_retains_language_specific_import_type_data_and_call_witnesses() {
    for sources in [
        vec![("a.php", "<?php namespace A; use B\\Repo; class App extends Repo { function run(Repo $repo):Repo { $repo->work(); $repo->value=2; return $repo; } }"), ("b.php", "<?php namespace B; class Repo { public int $value=0; function work(){} }")],
        vec![("a.py", "from b import Repo\nclass App(Repo):\n def run(self, repo: Repo) -> Repo:\n  repo.work()\n  repo.value=2\n  return repo\n"), ("b.py", "class Repo:\n value=0\n def work(self): pass\n")],
        vec![("a.ts", "import {Repo} from './b'; export class App extends Repo { run(repo:Repo):Repo { repo.work(); repo.value=2; return repo; } }"), ("b.ts", "export class Repo { value=0; work(){} }")],
        vec![("a.js", "import {Repo} from './b'; export class App extends Repo { run(){const repo=new Repo(); repo.work(); repo.value=2; return repo;} }"), ("b.js", "export class Repo { value=0; work(){} }")],
        vec![("lib.rs", "pub mod a; pub mod b;"), ("a.rs", "use crate::b::Repo; pub fn run(repo:&mut Repo)->&mut Repo {repo.work(); repo.value=2; repo}"), ("b.rs", "pub struct Repo {pub value:i32} impl Repo {pub fn work(&self){}}")],
    ] {
        let map = index(&sources);
        let (from, to) = if sources[0].0 == "lib.rs" {(sources[1].0, sources[2].0)} else {(sources[0].0, sources[1].0)};
        for kind in ["import", "member_access", "call"] {
            assert!(!edges(&map, from, to, kind).is_empty(), "missing {from} -> {to}: {kind}");
        }
        if !from.ends_with(".js") {
            for kind in ["parameter_type", "return_type"] {
                assert!(!edges(&map, from, to, kind).is_empty(), "missing {from} -> {to}: {kind}");
            }
        }
    }
}

#[test]
fn esm_module_resolution_retains_cross_language_barrels_directories_and_emitted_paths() {
    for (entry, statement, target) in [
        (
            "barrel.js",
            "import {Model} from './barrel.js';",
            "model.ts",
        ),
        (
            "folder/index.js",
            "import {Model} from './folder';",
            "model.ts",
        ),
        (
            "barrel.ts",
            "import {Model} from './barrel.js';",
            "model.ts",
        ),
    ] {
        let map = index(&[
            ("model.ts", "export class Model { value=1; }"),
            (entry, if entry.starts_with("folder/") {"export {Model} from '../model.ts';"} else {"export {Model} from './model.ts';"}),
            ("main.ts", &format!("{statement}\nexport function run(value:Model):Model{{value.value=2; return value;}}")),
        ]);
        assert!(
            !edges(&map, "main.ts", entry, "import").is_empty(),
            "{entry}: import"
        );
        for kind in ["parameter_type", "return_type", "member_access"] {
            assert!(
                !edges(&map, "main.ts", target, kind).is_empty(),
                "{entry}: {kind}"
            );
        }
    }
}

#[test]
fn ambiguous_esm_resources_do_not_create_same_language_map_edges() {
    let map = index(&[
        ("model.js", "export class Model {}"),
        ("model.ts", "export class Model {}"),
        (
            "main.ts",
            "import {Model} from './model'; export function run(value:Model){return value;}",
        ),
    ]);
    for target in ["model.js", "model.ts"] {
        for kind in ["import", "parameter_type"] {
            assert!(
                edges(&map, "main.ts", target, kind).is_empty(),
                "invented {target} {kind}"
            );
        }
    }
}

#[test]
fn equal_module_names_in_unrelated_languages_do_not_create_dependencies() {
    let map = index(&[
        ("a.py", "import b\ndef run():return b.VALUE\n"),
        ("b.py", "VALUE=1\n"),
        (
            "a.ts",
            "import {VALUE} from './b.ts';export function run(){return VALUE;}",
        ),
        ("b.ts", "export const VALUE=1;"),
        (
            "a.php",
            "<?php namespace A; use B\\Repo; function run(Repo $repo){return $repo->value;}",
        ),
        (
            "b.php",
            "<?php namespace B; class Repo {public int $value=0;}",
        ),
    ]);
    for witness in &map.witnesses {
        let (from, to) = (&map.entities[&witness.from], &map.entities[&witness.to]);
        if to.file.is_some() {
            assert_eq!(
                from.language, to.language,
                "crossed unrelated languages: {witness:?}"
            );
        }
    }
}

#[test]
fn python_container_and_union_annotations_retain_element_dependencies() {
    let map = index(&[
        ("main.py", "from model import Model\ndef read(items: list[Model]) -> dict[str, Model]:\n return {}\ndef optional(value: Model | None):\n pass\n"),
        ("model.py", "class Model: pass\n"),
    ]);
    let references = edges(&map, "main.py", "model.py", "type_reference");
    for line in [2, 4] {
        assert!(
            references.iter().any(|w| w.range.start_line == line),
            "missing annotation type on line {line}: {references:?}"
        );
    }
}

#[test]
fn python_annotation_witnesses_keep_qualified_names_and_ignore_runtime_expressions() {
    let map = index(&[
        ("main.py", "import model as types\nfrom model import Model\ndef read(items: list[types.Model]):\n local: dict[str, types.Model] = {}\n return items\ndef runtime():\n return [Model]\n"),
        ("model.py", "class Model: pass\n"),
    ]);
    let references = edges(&map, "main.py", "model.py", "type_reference");
    for line in [3, 4] {
        assert!(
            references.iter().any(|w| w.range.start_line == line),
            "missing qualified annotation on line {line}: {references:?}"
        );
    }
    assert!(references.iter().all(|w| w.range.start_line != 7));
}
