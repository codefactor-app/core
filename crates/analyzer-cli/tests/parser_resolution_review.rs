use analyzer_core::{
    graph::CallGraph, CallKind, Config, FileModel, LanguageRegistry, LanguageStrategy,
};
use std::path::Path;

fn models(strategy: &dyn LanguageStrategy, sources: &[(&str, &str)]) -> Vec<FileModel> {
    sources
        .iter()
        .map(|(path, source)| {
            let model = strategy.analyze(source, Path::new(path));
            assert!(
                model.parse_errors.is_empty(),
                "{path}: {:?}",
                model.parse_errors
            );
            model
        })
        .collect()
}
fn registry() -> LanguageRegistry {
    let mut registry = LanguageRegistry::new();
    registry.register(analyzer_python::Python);
    registry.register(analyzer_javascript::JavaScript);
    registry.register(analyzer_typescript::TypeScript);
    registry.register(analyzer_php::Php);
    registry.register(analyzer_rust::Rust);
    registry
}
fn linked(graph: &CallGraph, from: &str, to: &str) -> bool {
    graph
        .edges
        .iter()
        .any(|e| e.from.ends_with(from) && e.to.ends_with(to))
}

#[test]
fn mixed_default_named_and_namespace_esm_imports_link_across_files() {
    for (strategy, ext) in [
        (
            Box::new(analyzer_javascript::JavaScript) as Box<dyn LanguageStrategy>,
            "js",
        ),
        (Box::new(analyzer_typescript::TypeScript), "ts"),
    ] {
        let dep = format!("lib/dep.{ext}");
        let main = format!("lib/main.{ext}");
        let files = models(&*strategy, &[
            (&dep, "export default class Client { run() {} } export function helper() {}"),
            (&main, "import Client, {helper as work} from './dep'; import * as api from './dep'; function flow(){ const client=new Client(); client.run(); work(); api.helper(); }"),
        ]);
        let graph = CallGraph::build(&files, &registry(), &Config::default());
        assert!(
            linked(&graph, "main.flow", "dep.Client.run"),
            "{ext}: {graph:?}"
        );
        assert_eq!(
            graph
                .edges
                .iter()
                .filter(|e| e.from.ends_with("main.flow") && e.to.ends_with("dep.helper"))
                .count(),
            2,
            "{ext}: {graph:?}"
        );
    }
}
#[test]
fn rust_use_groups_aliases_and_self_imports_link() {
    let files=models(&analyzer_rust::Rust, &[("lib.rs", "mod dep { pub struct Repo; impl Repo { pub fn new()->Self {Repo} pub fn query(&self){} } pub fn helper(){} } use crate::dep::{self as api, Repo as Store, helper as run}; fn flow(repo:&Store){repo.query();run();api::helper();let created=Store::new();created.query();}")]);
    let graph = CallGraph::build(&files, &registry(), &Config::default());
    assert!(linked(&graph, "::flow", "::dep::Repo::query"), "{graph:?}");
    assert_eq!(
        graph
            .edges
            .iter()
            .filter(|e| e.from.ends_with("::flow") && e.to.ends_with("::dep::helper"))
            .count(),
        2,
        "{graph:?}"
    );
    assert!(linked(&graph, "::flow", "::dep::Repo::new"), "{graph:?}");
}
#[test]
fn python_relative_imports_and_except_types_are_lexical() {
    let files=models(&analyzer_python::Python, &[
        ("pkg/errors.py", "class First(Exception):\n def data(self): pass\nclass Second(Exception):\n def other(self): pass\n"),
        ("pkg/sub/flow.py", "from ..errors import First as Failure, Second\ndef flow():\n try: risky()\n except Failure as error: error.data()\n except Second as error: error.other()\n"),
    ]);
    let graph = CallGraph::build(&files, &registry(), &Config::default());
    assert!(
        linked(&graph, "flow.flow", "errors.First.data"),
        "{graph:?}"
    );
    assert!(
        linked(&graph, "flow.flow", "errors.Second.other"),
        "{graph:?}"
    );
}
#[test]
fn js_and_ts_catch_bindings_do_not_reuse_an_outer_parameter_type() {
    for (strategy, path, source) in [
        (Box::new(analyzer_javascript::JavaScript) as Box<dyn LanguageStrategy>,"scope.js","class Client {run(){}} function flow(){const value=new Client();try{value.run();}catch(value){value.run();}value.run();}"),
        (Box::new(analyzer_typescript::TypeScript),"scope.ts","class Client {run(){}} function flow(value:Client){try{value.run();}catch(value:unknown){value.run();}value.run();}"),
    ] {
        let files=models(&*strategy,&[(path,source)]);
        let graph=CallGraph::build(&files,&registry(),&Config::default());
        let calls=graph.edges.iter().filter(|e|e.from.ends_with(".flow")&&e.to.ends_with(".Client.run")).count();
        assert_eq!(calls,2,"{path}: {graph:?}");
        assert!(graph.edges.iter().any(|e|e.from.ends_with(".flow")&&e.to.starts_with("unknown:")),"{path}");
    }
}
#[test]
fn later_assignments_cannot_retype_earlier_calls_in_any_language() {
    let cases:Vec<(Box<dyn LanguageStrategy>,&str,&str)>=vec![
        (Box::new(analyzer_python::Python),"scope.py","class First:\n def run(self): pass\nclass Second:\n def run(self): pass\ndef flow():\n value=First()\n value.run()\n value=Second()\n value.run()\n"),
        (Box::new(analyzer_javascript::JavaScript),"scope.js","class First{run(){}} class Second{run(){}} function flow(){let value=new First();value.run();value=new Second();value.run();}"),
        (Box::new(analyzer_typescript::TypeScript),"scope.ts","class First{run(){}} class Second{run(){}} function flow(){let value=new First();value.run();value=new Second();value.run();}"),
        (Box::new(analyzer_php::Php),"scope.php","<?php class First{function run(){}} class Second{function run(){}} function flow(){$value=new First();$value->run();$value=new Second();$value->run();}"),
        (Box::new(analyzer_rust::Rust),"scope.rs","struct First;impl First{fn new()->Self{First}fn run(&self){}}struct Second;impl Second{fn new()->Self{Second}fn run(&self){}}fn flow(){let value=First::new();value.run();let value=Second::new();value.run();}"),
    ];
    for (strategy, path, source) in cases {
        let files = models(&*strategy, &[(path, source)]);
        let graph = CallGraph::build(&files, &registry(), &Config::default());
        let separator = if ["rust", "php"].contains(&strategy.language_id()) {
            "::"
        } else {
            "."
        };
        assert!(
            linked(
                &graph,
                &format!(
                    "{}flow",
                    if strategy.language_id() == "php" {
                        ":"
                    } else {
                        separator
                    }
                ),
                &format!("First{separator}run")
            ),
            "{path}: {graph:?}"
        );
        assert!(
            linked(
                &graph,
                &format!(
                    "{}flow",
                    if strategy.language_id() == "php" {
                        ":"
                    } else {
                        separator
                    }
                ),
                &format!("Second{separator}run")
            ),
            "{path}: {graph:?}"
        );
    }
}
#[test]
fn php_group_import_and_catch_fixes_keep_their_resolution() {
    let files=models(&analyzer_php::Php,&[("errors.php","<?php namespace Errors;class First{function data(){}}class Second{function other(){}}"),("flow.php","<?php namespace App;use Errors\\{First as Failure,Second};function flow(){try{risky();}catch(Failure $error){$error->data();}catch(Second $error){$error->other();}}")]);
    let graph = CallGraph::build(&files, &registry(), &Config::default());
    assert!(
        linked(&graph, "App\\flow", "Errors\\First::data"),
        "{graph:?}"
    );
    assert!(
        linked(&graph, "App\\flow", "Errors\\Second::other"),
        "{graph:?}"
    );
}
#[test]
fn uppercase_python_functions_are_not_guessed_as_constructors() {
    let files = models(
        &analyzer_python::Python,
        &[
            ("dep.py", "def Factory(): pass\n"),
            (
                "flow.py",
                "from dep import Factory\ndef flow():\n Factory()\n",
            ),
        ],
    );
    assert_eq!(files[1].functions[0].calls[0].kind, CallKind::Function);
    let graph = CallGraph::build(&files, &registry(), &Config::default());
    assert!(linked(&graph, "flow.flow", "dep.Factory"), "{graph:?}");
}

#[test]
fn relative_esm_paths_exports_and_default_functions_are_not_basename_guesses() {
    for (strategy, ext) in [
        (
            Box::new(analyzer_javascript::JavaScript) as Box<dyn LanguageStrategy>,
            "js",
        ),
        (Box::new(analyzer_typescript::TypeScript), "ts"),
    ] {
        let files=models(&*strategy,&[
            (&format!("dep.{ext}"), "export function helper() {}"),
            (&format!("lib/dep.{ext}"), "function actual() {} export {actual as helper}; export default function factory() {}"),
            (&format!("lib/barrel.{ext}"), "export {helper as work, default as make} from './dep.js';"),
            (&format!("lib/main.{ext}"), "import create, {helper} from './dep.js'; import {work,make} from './barrel'; function flow(){helper();create();work();make();}"),
        ]);
        let graph = CallGraph::build(&files, &registry(), &Config::default());
        assert_eq!(
            graph
                .edges
                .iter()
                .filter(|e| e.from.ends_with("main.flow") && e.to.ends_with("lib/dep.actual"))
                .count(),
            2,
            "{ext}: {graph:?}"
        );
        assert_eq!(
            graph
                .edges
                .iter()
                .filter(|e| e.from.ends_with("main.flow") && e.to.ends_with("lib/dep.factory"))
                .count(),
            2,
            "{ext}: {graph:?}"
        );
        assert!(!graph
            .edges
            .iter()
            .any(|e| e.from.ends_with("main.flow") && e.to.ends_with(":dep.helper")));
    }
}
#[test]
fn imported_python_class_is_identified_from_its_declaration() {
    let files = models(
        &analyzer_python::Python,
        &[
            (
                "pkg/dep.py",
                "class Client:\n def __init__(self): pass\n def run(self): pass\n",
            ),
            (
                "pkg/flow.py",
                "from .dep import Client as Store\ndef flow():\n value=Store()\n value.run()\n",
            ),
        ],
    );
    let graph = CallGraph::build(&files, &registry(), &Config::default());
    assert!(
        linked(&graph, "flow.flow", "dep.Client.__init__"),
        "{graph:?}"
    );
    assert!(linked(&graph, "flow.flow", "dep.Client.run"), "{graph:?}");
}
#[test]
fn shadowed_import_names_do_not_create_project_or_external_edges() {
    let cases: Vec<(Box<dyn LanguageStrategy>, &str, &str)> = vec![
        (
            Box::new(analyzer_python::Python),
            "scope.py",
            "import requests\ndef flow(requests):\n requests.get()\n",
        ),
        (
            Box::new(analyzer_javascript::JavaScript),
            "scope.js",
            "import * as axios from 'axios'; function flow(axios){axios.get();}",
        ),
        (
            Box::new(analyzer_typescript::TypeScript),
            "scope.ts",
            "import * as axios from 'axios'; function flow(axios:unknown){axios.get();}",
        ),
        (
            Box::new(analyzer_php::Php),
            "scope.php",
            "<?php function flow($strlen){$strlen();}",
        ),
        (
            Box::new(analyzer_rust::Rust),
            "scope.rs",
            "use std::process::Command;fn flow(Command:Unknown){Command::new();}",
        ),
    ];
    for (strategy, path, source) in cases {
        let files = models(&*strategy, &[(path, source)]);
        let graph = CallGraph::build(&files, &registry(), &Config::default());
        assert!(!graph.edges.is_empty(), "{path}");
        assert!(
            graph.edges.iter().all(|e| e.to.starts_with("unknown:")),
            "{path}: {graph:?}"
        );
    }
}
#[test]
fn local_imports_and_blocks_do_not_leak_into_other_functions() {
    let python = models(
        &analyzer_python::Python,
        &[
            ("dep.py", "def work(): pass\n"),
            (
                "scope.py",
                "def local():\n from dep import work\n work()\ndef other():\n work()\n",
            ),
        ],
    );
    let graph = CallGraph::build(&python, &registry(), &Config::default());
    assert!(linked(&graph, "scope.local", "dep.work"), "{graph:?}");
    assert!(!linked(&graph, "scope.other", "dep.work"), "{graph:?}");
    for (strategy, path) in [
        (
            Box::new(analyzer_javascript::JavaScript) as Box<dyn LanguageStrategy>,
            "scope.js",
        ),
        (Box::new(analyzer_typescript::TypeScript), "scope.ts"),
    ] {
        let files=models(&*strategy,&[(path,"class Client{run(){}} function flow(){let value=new Client();{let value=unknown();value.run();}value.run();}")]);
        let graph = CallGraph::build(&files, &registry(), &Config::default());
        assert_eq!(
            graph
                .edges
                .iter()
                .filter(|e| e.from.ends_with(".flow") && e.to.ends_with("Client.run"))
                .count(),
            1,
            "{path}: {graph:?}"
        );
    }
}
#[test]
fn reassignment_with_unknown_type_does_not_reuse_a_previous_factory_type() {
    let files=models(&analyzer_typescript::TypeScript,&[("scope.ts","class Client{run(){}} function create():Client{return new Client();} function flow(){let value=create();value.run();value=unknown();value.run();value=42;value.run();}")]);
    let graph = CallGraph::build(&files, &registry(), &Config::default());
    assert_eq!(
        graph
            .edges
            .iter()
            .filter(|e| e.from.ends_with(".flow") && e.to.ends_with("Client.run"))
            .count(),
        1,
        "{graph:?}"
    );
}
#[test]
fn rust_namespace_functions_are_not_class_methods_and_relative_use_has_its_scope() {
    let files=models(&analyzer_rust::Rust,&[("lib.rs","mod dep {pub fn work(){} pub mod inner {use super::work as run;pub fn flow(){run();}}} fn other(){run();}")]);
    let graph = CallGraph::build(&files, &registry(), &Config::default());
    assert!(
        files[0].functions.iter().all(|f| f.parent_type.is_none()),
        "{:?}",
        files[0].functions
    );
    assert!(linked(&graph, "dep::inner::flow", "dep::work"), "{graph:?}");
    assert!(!linked(&graph, "lib::other", "dep::work"), "{graph:?}");
}
#[test]
fn typescript_import_equals_require_preserves_namespace_alias() {
    let files = models(
        &analyzer_typescript::TypeScript,
        &[
            ("lib/dep.ts", "export function work() {}"),
            (
                "lib/main.ts",
                "import api = require('./dep'); function flow(){api.work();}",
            ),
        ],
    );
    let graph = CallGraph::build(&files, &registry(), &Config::default());
    assert!(linked(&graph, "main.flow", "dep.work"), "{graph:?}");
}

#[test]
fn declarations_in_other_scopes_do_not_supply_a_local_receiver_type() {
    let files=models(&analyzer_rust::Rust,&[("lib.rs","mod first {pub struct Client;impl Client{pub fn run(&self){}} pub fn flow(value:&Client){value.run();}}mod second {pub struct Client;impl Client{pub fn run(&self){}}}")]);
    let graph = CallGraph::build(&files, &registry(), &Config::default());
    assert!(
        linked(&graph, "first::flow", "first::Client::run"),
        "{graph:?}"
    );
    assert!(
        !linked(&graph, "first::flow", "second::Client::run"),
        "{graph:?}"
    );
    let files=models(&analyzer_python::Python,&[("scope.py","class First:\n class Client:\n  def run(self): pass\n def flow(self):\n  value=self.Client()\n  value.run()\nclass Second:\n class Client:\n  def run(self): pass\n")]);
    let graph = CallGraph::build(&files, &registry(), &Config::default());
    // A receiver obtained through a dynamic member must never resolve to another class's Client.
    assert!(
        !linked(&graph, "First.flow", "Second.Client.run"),
        "{graph:?}"
    );
}
#[test]
fn a_later_local_declaration_does_not_use_an_import_before_assignment() {
    let files = models(
        &analyzer_python::Python,
        &[(
            "scope.py",
            "import requests\ndef flow():\n requests.get()\n requests=unknown()\n",
        )],
    );
    let graph = CallGraph::build(&files, &registry(), &Config::default());
    assert!(
        graph.edges.iter().all(|e| e.to.starts_with("unknown:")),
        "{graph:?}"
    );
    for (strategy, path) in [
        (
            Box::new(analyzer_javascript::JavaScript) as Box<dyn LanguageStrategy>,
            "scope.js",
        ),
        (Box::new(analyzer_typescript::TypeScript), "scope.ts"),
    ] {
        let files=models(&*strategy,&[(path,"import * as axios from 'axios';function flow(){{axios.get();let axios=unknown();}axios.get();}")]);
        let graph = CallGraph::build(&files, &registry(), &Config::default());
        assert_eq!(
            graph
                .edges
                .iter()
                .filter(|e| e.to.starts_with("external:"))
                .count(),
            1,
            "{path}: {graph:?}"
        );
    }
}

#[test]
fn a_project_import_alias_is_not_a_library_operation() {
    let files = models(
        &analyzer_python::Python,
        &[
            ("local.py", "def get(): pass\n"),
            (
                "scope.py",
                "import local as requests\ndef flow():\n requests.get()\n",
            ),
        ],
    );
    let graph = CallGraph::build(&files, &registry(), &Config::default());
    assert!(linked(&graph, "scope.flow", "local.get"), "{graph:?}");
    assert!(!graph.edges.iter().any(|e| e.to.starts_with("external:")));
    for (strategy, path) in [
        (
            Box::new(analyzer_javascript::JavaScript) as Box<dyn LanguageStrategy>,
            "scope.js",
        ),
        (Box::new(analyzer_typescript::TypeScript), "scope.ts"),
    ] {
        let files = models(
            &*strategy,
            &[
                ("local.js", "export function get(){}"),
                (
                    path,
                    "import * as axios from './local';function flow(){axios.get();}",
                ),
            ],
        );
        let graph = CallGraph::build(&files, &registry(), &Config::default());
        assert!(
            linked(&graph, "scope.flow", "local.get"),
            "{path}: {graph:?}"
        );
        assert!(!graph.edges.iter().any(|e| e.to.starts_with("external:")));
    }
}

#[test]
fn inherited_methods_resolve_to_the_base_and_overrides_to_the_child() {
    let cases:Vec<(Box<dyn LanguageStrategy>,&str,&str)>=vec![
        (Box::new(analyzer_python::Python),"scope.py","class Base:\n def run(self): pass\nclass Child(Base):\n def flow(self): self.run()\nclass Override(Base):\n def run(self): pass\n def flow(self): self.run()\n"),
        (Box::new(analyzer_javascript::JavaScript),"scope.js","class Base{run(){}}class Child extends Base{flow(){this.run();}}class Override extends Base{run(){}flow(){this.run();}}"),
        (Box::new(analyzer_typescript::TypeScript),"scope.ts","class Base{run(){}}interface Marker{}class Child extends Base implements Marker{flow(){this.run();}}class Override extends Base{run(){}flow(){this.run();}}"),
        (Box::new(analyzer_php::Php),"scope.php","<?php class Base{function run(){}}class Child extends Base{function flow(){$this->run();}}class Override extends Base{function run(){}function flow(){$this->run();}}"),
    ];
    for (strategy, path, source) in cases {
        let files = models(&*strategy, &[(path, source)]);
        let graph = CallGraph::build(&files, &registry(), &Config::default());
        let sep = if strategy.language_id() == "php" {
            "::"
        } else {
            "."
        };
        assert!(
            linked(&graph, &format!("Child{sep}flow"), &format!("Base{sep}run")),
            "{path}: {graph:?}"
        );
        assert!(
            linked(
                &graph,
                &format!("Override{sep}flow"),
                &format!("Override{sep}run")
            ),
            "{path}: {graph:?}"
        );
        assert!(
            !linked(
                &graph,
                &format!("Override{sep}flow"),
                &format!("Base{sep}run")
            ),
            "{path}: {graph:?}"
        );
    }
}

#[test]
fn python_dotted_imports_bind_the_package_and_relative_module_imports_keep_their_level() {
    let files=models(&analyzer_python::Python,&[("pkg/dep.py","def work(): pass\n"),("pkg/main.py","import pkg.dep\nimport pkg.dep as api\nfrom . import dep\ndef flow():\n pkg.dep.work()\n api.work()\n dep.work()\n")]);
    let graph = CallGraph::build(&files, &registry(), &Config::default());
    assert_eq!(
        graph
            .edges
            .iter()
            .filter(|e| e.from.ends_with("main.flow") && e.to.ends_with("pkg.dep.work"))
            .count(),
        3,
        "{graph:?}"
    );
}
#[test]
fn a_python_local_import_shadows_a_global_import_for_the_whole_function() {
    let files = models(
        &analyzer_python::Python,
        &[(
            "scope.py",
            "import requests\ndef flow():\n requests.get()\n import requests\n requests.get()\n",
        )],
    );
    let graph = CallGraph::build(&files, &registry(), &Config::default());
    assert_eq!(
        graph
            .edges
            .iter()
            .filter(|e| e.to.starts_with("external:"))
            .count(),
        1,
        "{graph:?}"
    );
}

#[test]
fn inherited_methods_follow_import_aliases_and_explicit_super_calls() {
    let cases:Vec<(Box<dyn LanguageStrategy>,&str,&str,&str,&str)>=vec![
        (Box::new(analyzer_python::Python),"base.py","class Base:\n def run(self): pass\n","main.py","from base import Base as Parent\nclass Child(Parent):\n def flow(self):\n  self.run()\n  super().run()\n"),
        (Box::new(analyzer_javascript::JavaScript),"base.js","export default class Base{run(){}}","main.js","import Parent from './base';class Child extends Parent{flow(){this.run();super.run();}}"),
        (Box::new(analyzer_typescript::TypeScript),"base.ts","export default class Base{run(){}}","main.ts","import Parent from './base';class Child extends Parent{flow(){this.run();super.run();}}"),
        (Box::new(analyzer_php::Php),"base.php","<?php namespace Dep;class Base{function run(){}}","main.php","<?php namespace App;use Dep\\{Base as Parent};class Child extends Parent{function flow(){$this->run();parent::run();}}"),
    ];
    for (strategy, base, base_source, main, main_source) in cases {
        let files = models(&*strategy, &[(base, base_source), (main, main_source)]);
        let graph = CallGraph::build(&files, &registry(), &Config::default());
        let sep = if strategy.language_id() == "php" {
            "::"
        } else {
            "."
        };
        assert_eq!(
            graph
                .edges
                .iter()
                .filter(|e| e.from.ends_with(&format!("Child{sep}flow"))
                    && e.to.ends_with(&format!("Base{sep}run")))
                .count(),
            2,
            "{main}: {graph:?}"
        );
    }
}

#[test]
fn assigned_local_closures_and_aliases_resolve_in_all_languages() {
    let cases:Vec<(Box<dyn LanguageStrategy>,&str,&str)>=vec![
        (Box::new(analyzer_python::Python),"scope.py","def leaf(): pass\ndef flow():\n work=lambda:leaf()\n again=work\n work()\n again()\n"),
        (Box::new(analyzer_javascript::JavaScript),"scope.js","function leaf(){}function flow(){const work=()=>leaf();const again=work;work();again();}"),
        (Box::new(analyzer_typescript::TypeScript),"scope.ts","function leaf(){}function flow(){const work=()=>leaf();const again=work;work();again();}"),
        (Box::new(analyzer_php::Php),"scope.php","<?php function leaf(){}function flow(){$work=function(){leaf();};$again=$work;$work();$again();}"),
        (Box::new(analyzer_rust::Rust),"scope.rs","fn leaf(){}fn flow(){let work=||leaf();let again=work;work();again();}"),
    ];
    for (strategy, path, source) in cases {
        let files = models(&*strategy, &[(path, source)]);
        let graph = CallGraph::build(&files, &registry(), &Config::default());
        let local = files[0]
            .functions
            .iter()
            .find(|f| f.name == "work")
            .expect(path);
        let id = format!("fn:{}:{path}:{}", strategy.language_id(), local.fqn);
        assert_eq!(
            graph
                .edges
                .iter()
                .filter(|e| e.from.ends_with("flow") && e.to.as_ref() == id)
                .count(),
            2,
            "{path}: {graph:?}"
        );
    }
}
#[test]
fn bare_calls_and_import_aliases_stay_in_their_namespace() {
    let files = models(
        &analyzer_rust::Rust,
        &[(
            "lib.rs",
            "mod first{fn work(){}fn flow(){work();}}mod second{fn work(){}fn flow(){work();}}",
        )],
    );
    let graph = CallGraph::build(&files, &registry(), &Config::default());
    for name in ["first", "second"] {
        assert!(
            linked(&graph, &format!("{name}::flow"), &format!("{name}::work")),
            "{graph:?}"
        );
    }
    let files=models(&analyzer_php::Php,&[("dep.php","<?php namespace Dep;class First{function run(){}}class Second{function run(){}}"),("main.php","<?php namespace First;use Dep\\First as Client;function work(){}function flow(Client $v){work();$v->run();}namespace Second;use Dep\\Second as Client;function work(){}function flow(Client $v){work();$v->run();}")]);
    let graph = CallGraph::build(&files, &registry(), &Config::default());
    for name in ["First", "Second"] {
        assert!(
            linked(&graph, &format!("{name}\\flow"), &format!("{name}\\work")),
            "{graph:?}"
        );
        assert!(
            linked(
                &graph,
                &format!("{name}\\flow"),
                &format!("Dep\\{name}::run")
            ),
            "{graph:?}"
        );
    }
    assert!(!linked(&graph, "First\\flow", "Dep\\Second::run"));
}

#[test]
fn an_ambiguous_factory_return_type_is_not_selected_arbitrarily() {
    let files=models(&analyzer_typescript::TypeScript,&[("scope.ts","class First{run(){}}class Second{run(){}}interface Creator{make():First|Second;}class A implements Creator{make():First{return new First();}}class B implements Creator{make():Second{return new Second();}}function flow(c:Creator){const value=c.make();value.run();}")]);
    let graph = CallGraph::build(&files, &registry(), &Config::default());
    assert!(linked(&graph, "scope.flow", "scope.A.make"), "{graph:?}");
    assert!(linked(&graph, "scope.flow", "scope.B.make"), "{graph:?}");
    assert!(
        !linked(&graph, "scope.flow", "scope.First.run"),
        "{graph:?}"
    );
    assert!(
        !linked(&graph, "scope.flow", "scope.Second.run"),
        "{graph:?}"
    );
}

#[test]
fn iteration_bindings_do_not_reuse_an_outer_receiver_type() {
    let cases:Vec<(Box<dyn LanguageStrategy>,&str,&str)>=vec![
        (Box::new(analyzer_python::Python),"scope.py","class Client:\n def run(self): pass\ndef flow():\n value=Client()\n for value in items:\n  value.run()\n"),
        (Box::new(analyzer_javascript::JavaScript),"scope.js","class Client{run(){}}function flow(){const value=new Client();for(let value of items){value.run();}value.run();}"),
        (Box::new(analyzer_typescript::TypeScript),"scope.ts","class Client{run(){}}function flow(value:Client){for(let value of items){value.run();}value.run();}"),
        (Box::new(analyzer_php::Php),"scope.php","<?php class Client{function run(){}}function flow(){$value=new Client();foreach($items as $value){$value->run();}}"),
        (Box::new(analyzer_rust::Rust),"scope.rs","struct Client;impl Client{fn run(&self){}}fn flow(value:Client){for value in items{value.run();}value.run();}"),
    ];
    for (strategy, path, source) in cases {
        let files = models(&*strategy, &[(path, source)]);
        let graph = CallGraph::build(&files, &registry(), &Config::default());
        let expected = if ["python", "php"].contains(&strategy.language_id()) {
            0
        } else {
            1
        };
        assert_eq!(
            graph
                .edges
                .iter()
                .filter(|e| e.from.ends_with("flow")
                    && (e.to.ends_with("Client.run") || e.to.ends_with("Client::run")))
                .count(),
            expected,
            "{path}: {graph:?}"
        );
    }
}
#[test]
fn destructured_parameters_and_assignments_shadow_imported_names() {
    for (strategy, path) in [
        (
            Box::new(analyzer_javascript::JavaScript) as Box<dyn LanguageStrategy>,
            "scope.js",
        ),
        (Box::new(analyzer_typescript::TypeScript), "scope.ts"),
    ] {
        let files=models(&*strategy,&[(path,"import * as axios from 'axios';function flow({axios}){axios.get();}function other(){const {axios}=unknown();axios.get();}")]);
        let graph = CallGraph::build(&files, &registry(), &Config::default());
        assert!(
            graph.edges.iter().all(|e| e.to.starts_with("unknown:")),
            "{path}: {graph:?}"
        );
    }
}

#[test]
fn loop_initializers_and_destructured_catch_bindings_keep_their_scope() {
    for (strategy, path) in [
        (
            Box::new(analyzer_javascript::JavaScript) as Box<dyn LanguageStrategy>,
            "scope.js",
        ),
        (Box::new(analyzer_typescript::TypeScript), "scope.ts"),
    ] {
        let files=models(&*strategy,&[(path,"import * as axios from 'axios';class First{run(){}}class Second{run(){}}function flow(){const value=new First();for(let value=new Second();condition;update()){value.run();}value.run();try{unknown();}catch({axios}){axios.get();}}")]);
        let graph = CallGraph::build(&files, &registry(), &Config::default());
        assert!(
            linked(&graph, "scope.flow", "scope.First.run"),
            "{path}: {graph:?}"
        );
        assert!(
            linked(&graph, "scope.flow", "scope.Second.run"),
            "{path}: {graph:?}"
        );
        assert!(
            !graph.edges.iter().any(|e| e.to.starts_with("external:")),
            "{path}: {graph:?}"
        );
    }
}

#[test]
fn property_receivers_use_the_current_root_type_after_reassignment() {
    let files=models(&analyzer_typescript::TypeScript,&[("scope.ts","class A{run(){}}class B{run(){}}class First{field:A;}class Second{field:B;}function flow(value:First){value.field.run();value=new Second();value.field.run();}")]);
    let graph = CallGraph::build(&files, &registry(), &Config::default());
    assert!(linked(&graph, "scope.flow", "scope.A.run"), "{graph:?}");
    assert!(linked(&graph, "scope.flow", "scope.B.run"), "{graph:?}");
    assert_eq!(
        graph
            .edges
            .iter()
            .filter(|e| e.from.ends_with("scope.flow") && e.to.ends_with("scope.A.run"))
            .count(),
        1,
        "{graph:?}"
    );
}

#[test]
fn nested_block_restoration_keeps_assignments_and_branch_uncertainty() {
    for (strategy, path, source) in [
        (Box::new(analyzer_javascript::JavaScript) as Box<dyn LanguageStrategy>, "scope.js", "class First{}class Second{}function flow(){let value=new First();let other=new Second();value.run();{let value=new Second();value.run();{let value=new First();value.run();}}value.run();{value=other;value.run();}value.run();if(condition){value=new First();value.run();}value.run();while(condition){value=new First();value.run();}value.run();}"),
        (Box::new(analyzer_typescript::TypeScript), "scope.ts", "class First{}class Second{}function flow(value:First,other:Second){value.run();{let value:Second=other;value.run();{let value:First;value.run();}}value.run();{value=other;value.run();}value.run();if(condition){value=new First();value.run();}value.run();while(condition){value=new First();value.run();}value.run();}"),
        (Box::new(analyzer_rust::Rust), "scope.rs", "struct First;struct Second;fn flow(mut value:First,other:Second){value.run();{let value:Second=other;value.run();{let value:First;value.run();}}value.run();{value=other;value.run();}value.run();}"),
    ] {
        let files = models(&*strategy, &[(path, source)]);
        let function = files[0].functions.iter().find(|f| f.name == "flow").unwrap();
        let calls: Vec<_> = function.calls.iter().filter(|c| c.name == "run").collect();
        let types: Vec<_> = calls.iter().map(|c| c.receiver_type.as_deref().map(|ty| ty.rsplit(['.', ':']).next().unwrap())).collect();
        let expected = vec![Some("First"), Some("Second"), Some("First"), Some("First"), Some("Second"), Some("Second"), Some("First"), None, Some("First"), None];
        if strategy.language_id() == "rust" {
            assert_eq!(types, expected[..6], "{path}: {calls:?}");
        } else {
            assert_eq!(types, expected, "{path}: {calls:?}");
            let alternatives = calls[7].receiver_types.iter().map(|ty| ty.rsplit(['.', ':']).next().unwrap()).collect::<std::collections::BTreeSet<_>>();
            assert_eq!(alternatives, std::collections::BTreeSet::from(["First", "Second"]), "{path}");
            assert!(!calls[7].binding_unknown && calls[7].confidence == analyzer_core::Confidence::Ambiguous, "{path}");
            assert!(calls[9].binding_unknown, "{path}");
        }
    }
}
