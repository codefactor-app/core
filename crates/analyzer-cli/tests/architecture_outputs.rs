use analyzer_core::architecture::{analyze, ArchitectureConfig, Profile};
use analyzer_core::graph::CallGraph;
use analyzer_core::{Analyzer, Config, FileModel, LanguageRegistry, LanguageStrategy};
use serde_json::Value;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
fn registry() -> LanguageRegistry {
    let mut r = LanguageRegistry::new();
    r.register(analyzer_python::Python);
    r.register(analyzer_javascript::JavaScript);
    r.register(analyzer_typescript::TypeScript);
    r.register(analyzer_php::Php);
    r.register(analyzer_rust::Rust);
    r
}
fn check(
    sources: &[(&str, &str)],
    config: ArchitectureConfig,
) -> (
    Vec<FileModel>,
    analyzer_core::architecture::ArchitectureSummary,
    Vec<analyzer_core::rules::Violation>,
) {
    let r = registry();
    let c = Config {
        architecture: config.clone(),
        ..Default::default()
    };
    let files = sources
        .iter()
        .map(|(p, s)| {
            let strategy = r
                .by_extension(Path::new(p).extension().unwrap().to_str().unwrap())
                .unwrap();
            strategy.analyze_with_architecture(s, Path::new(p), true)
        })
        .collect::<Vec<_>>();
    for f in &files {
        assert!(f.parse_errors.is_empty(), "{} {:?}", f.path, f.parse_errors);
    }
    let g = CallGraph::build(&files, &r, &c);
    let (summary, issues) = analyze(&files, &g, &config);
    (files, summary, issues)
}
fn extended() -> ArchitectureConfig {
    ArchitectureConfig {
        profile: Profile::Extended,
        exclude: vec![],
        ..Default::default()
    }
}
fn ids(issues: &[analyzer_core::rules::Violation]) -> Vec<&str> {
    issues.iter().map(|v| v.rule.as_str()).collect()
}
#[test]
fn all_languages_detect_call_cycles_and_ignore_imports_and_unused_types() {
    for sources in [
        vec![("a.php","<?php namespace A; use B\\Right; class Left {function run(Right $b){$b->run($this);}}"),("b.php","<?php namespace B; use A\\Left; class Right {function run(Left $a){$a->run($this);}}")],
        vec![("a.py","from b import ping_b\ndef ping_a(): ping_b()\n"),("b.py","from a import ping_a\ndef ping_b(): ping_a()\n")],
        vec![("a.js","import {ping_b} from './b'; export function ping_a(){ping_b();}"),("b.js","import {ping_a} from './a'; export function ping_b(){ping_a();}")],
        vec![("a.ts","import {ping_b} from './b'; export function ping_a(){ping_b();}"),("b.ts","import {ping_a} from './a'; export function ping_b(){ping_a();}")],
        vec![("lib.rs", "pub mod a; pub mod b;"), ("a.rs","pub fn ping_a(){crate::b::ping_b();}"),("b.rs","pub fn ping_b(){crate::a::ping_a();}")],
    ] {
        let issues=check(&sources,ArchitectureConfig::default()).2;
        let cycle=issues.iter().find(|v|v.rule=="arch_cycle").unwrap_or_else(||panic!("missing call cycle: {}",sources[0].0));
        let d=cycle.architecture.as_ref().unwrap();
        assert!(d.evidence.iter().all(|e|e.dependency.as_ref().unwrap().kind=="call"));
        assert_eq!(d.measurements["call_sites"],2.0);
        assert_eq!(d.cycle.as_ref().unwrap().path.len(),3);
    }
    for sources in [
        vec![("a.php","<?php namespace A; use B\\Right; class Left {function run(Right $b):Right{return $b;}}"),("b.php","<?php namespace B; use A\\Left; class Right {function run(Left $a):Left{return $a;}}")],
        vec![("a.py","from b import B\nclass A:\n def run(self,b:B): return b\n"),("b.py","from a import A\nclass B:\n def run(self,a:A): return a\n")],
        vec![("a.js","import './b'; export class A {}"),("b.js","import './a'; export class B {}")],
        vec![("a.ts","import type {B} from './b'; export class A {run(b:B):B{return b;}}"),("b.ts","import type {A} from './a'; export class B {run(a:A):A{return a;}}")],
        vec![("lib.rs", "pub mod a; pub mod b;"), ("a.rs","use crate::b::B; pub struct A {b:Option<B>}"),("b.rs","use crate::a::A; pub struct B {a:Option<A>}")],
    ] {
        assert!(!ids(&check(&sources,ArchitectureConfig::default()).2).contains(&"arch_cycle"),"{}",sources[0].0);
    }
}
#[test]
fn all_languages_detect_value_access_cycles_without_method_calls() {
    for sources in [
        vec![("a.php","<?php namespace A; use B\\Right; class Left {public int $value=0; function run(Right $b){return $b->value;}}"),("b.php","<?php namespace B; use A\\Left; class Right {public int $value=0; function run(Left $a){$a->value=1;}}")],
        vec![("a.py","from b import B\nclass A:\n value=0\n def run(self,b:B): return b.value\n"),("b.py","from a import A\nclass B:\n value=0\n def run(self,a:A): a.value=1\n")],
        vec![("a.js","import * as b from './b'; export const value=1; export function read(){return b.value;}"),("b.js","import * as a from './a'; export const value=1; export function read(){return a.value;}")],
        vec![("a.ts","import {B} from './b'; export class A {value=0; run(b:B){return b.value;}}"),("b.ts","import {A} from './a'; export class B {value=0; run(a:A){a.value=1;}}")],
        vec![("lib.rs", "pub mod a; pub mod b;"), ("a.rs","use crate::b::B; pub struct A {pub value:i32} pub fn read(b:&B)->i32{b.value}"),("b.rs","use crate::a::A; pub struct B {pub value:i32} pub fn write(a:&mut A){a.value=1;}")],
    ] {
        let issues=check(&sources,ArchitectureConfig::default()).2;
        let d=issues.iter().find(|v|v.rule=="arch_cycle").unwrap_or_else(||panic!("missing value cycle: {}",sources[0].0)).architecture.as_ref().unwrap();
        assert!(d.evidence.iter().all(|e|e.dependency.as_ref().unwrap().kind=="member_access"));
        assert_eq!(d.measurements["call_sites"],0.0);
        assert!(d.measurements["value_access_sites"]>=2.0);
    }
    for sources in [
        vec![("a.py","from b import VALUE_B\nVALUE_A=1\ndef read(): return VALUE_B\n"),("b.py","from a import VALUE_A\nVALUE_B=1\ndef read(): return VALUE_A\n")],
        vec![("a.js","import {VALUE_B} from './b'; export const VALUE_A=1; export function read(){return VALUE_B;}"),("b.js","import {VALUE_A} from './a'; export const VALUE_B=1; export function read(){return VALUE_A;}")],
        vec![("a.ts","import {VALUE_B} from './b'; export const VALUE_A=1; export function read(){return VALUE_B;}"),("b.ts","import {VALUE_A} from './a'; export const VALUE_B=1; export function read(){return VALUE_A;}")],
        vec![("lib.rs", "pub mod a; pub mod b;"), ("a.rs","use crate::b::VALUE_B; pub const VALUE_A:i32=1; pub fn read()->i32{VALUE_B}"),("b.rs","use crate::a::VALUE_A; pub const VALUE_B:i32=1; pub fn read()->i32{VALUE_A}")],
    ] {
        let issues=check(&sources,ArchitectureConfig::default()).2;
        assert!(ids(&issues).contains(&"arch_cycle"),"missing variable cycle: {}",sources[0].0);
    }
}
#[test]
fn inherited_fields_constants_and_lexical_shadows_use_value_witnesses() {
    for sources in [
        vec![("a.php","<?php namespace A; use B\\Right; class Left {const VALUE=1; function read(){return Right::VALUE;}}"),("b.php","<?php namespace B; use A\\Left; class Right {const VALUE=1; function read(){return Left::VALUE;}}")],
        vec![("a.php","<?php namespace A; use B\\Right; class Left extends Right {public int $own=0; function read(){return $this->inherited;}}"),("b.php","<?php namespace B; use A\\Left; class Right {public int $inherited=0; function read(Left $a){return $a->own;}}")],
    ] {
        assert!(ids(&check(&sources,ArchitectureConfig::default()).2).contains(&"arch_cycle"));
    }
    for sources in [
        vec![("a.php","<?php namespace A; use B\\Right; class Left {function read(){return Right::CLASS;}}"),("b.php","<?php namespace B; use A\\Left; class Right {function read(){return Left::ClAsS;}}")],
        vec![("a.py","import b\ndef read(b): return b.value\n"),("b.py","import a\ndef read(): return a.value\n")],
        vec![("a.ts","import {B} from './b'; export class A {value=0; read(b:B){b=null as any;return b.value;}}"),("b.ts","import {A} from './a'; export class B {value=0; read(a:A){return a.value;}}")],
        vec![("a.ts","import {B} from './b'; class Input {b:B;} export class A {value=0; read(input:Input){input=null as any;return input.b.value;}}"),("b.ts","import {A} from './a'; export class B {value=0; read(a:A){return a.value;}}")],
        vec![("a.ts","import type * as b from './b'; export type A=b.B;"),("b.ts","import type * as a from './a'; export type B=a.A;")],
        vec![("lib.rs","pub mod a; pub mod b; pub fn launch(){a::read();}"),("a.rs","pub fn read()->i32{crate::b::VALUE}"),("b.rs","pub const VALUE:i32=1;")],
        vec![("lib.rs","pub mod a; pub mod b;"),("a.rs","use crate::b::VALUE; pub const LEFT:i32=1; pub fn text(){stringify!(VALUE);}"),("b.rs","use crate::a::LEFT; pub const VALUE:i32=1; pub fn text(){stringify!(LEFT);}")],
    ] {
        assert!(!ids(&check(&sources,ArchitectureConfig::default()).2).contains(&"arch_cycle"),"{}",sources[0].0);
    }
}
#[test]
fn php_repository_metadata_and_class_literals_do_not_form_a_cycle() {
    for sources in [
        [("Entity/History.php", "<?php namespace Domain\\Entity; use Domain\\Repository\\HistoryRepository as Repo; #[Meta(repositoryClass: Repo::class)] class History {}"),
         ("Repository/HistoryRepository.php", "<?php namespace Domain\\Repository; use Domain\\Entity\\History; class HistoryRepository { public function run() { register(History::class); } }")],
        [("Entity/History.php", "<?php namespace Domain\\Entity; #[Meta(repositoryClass: \\Domain\\Repository\\HistoryRepository::class)] class History {}"),
         ("Repository/HistoryRepository.php", "<?php namespace Domain\\Repository; class HistoryRepository { public function run() { register(\\Domain\\Entity\\History::class); } }")],
    ] {
        assert!(!ids(&check(&sources, ArchitectureConfig::default()).2).contains(&"arch_cycle"));
    }
}

#[test]
fn cycles_require_operations_in_both_directions_even_with_metadata_or_type_annotations() {
    for (a, b) in [
        ("<?php namespace A; use B\\Right; class Left {}", "<?php namespace B; use A\\Left; class Right {}"),
        ("<?php namespace A; use B\\Right; class Left { public function run(Right $r) {} }", "<?php namespace B; use A\\Left; class Right {}"),
        ("<?php namespace A; use B\\Right; #[Meta(Right::class)] class Left { public function run(Right $r) {} }", "<?php namespace B; use A\\Left; class Right { public function run(Left $l) {} }"),
        ("<?php namespace A; use B\\Right; class Left { public function run(Right $r) {$r->run($this);} }", "<?php namespace B; use A\\Left; class Right { public function run(Left $l) {} }"),
    ] {
        assert!(!ids(&check(&[("a.php",a),("b.php",b)], ArchitectureConfig::default()).2).contains(&"arch_cycle"));
    }
    let sources = [("a.php", "<?php namespace A; use B\\Right; #[Meta(Right::class)] class Left { public function run(Right $r) {$r->run($this);} }"),
        ("b.php", "<?php namespace B; use A\\Left; class Right { public function run(Left $l) {$l->run($this);} }")];
    let issues = check(&sources, ArchitectureConfig::default()).2;
    let d = issues
        .iter()
        .find(|v| v.rule == "arch_cycle")
        .unwrap()
        .architecture
        .as_ref()
        .unwrap();
    assert_eq!(
        d.cycle.as_ref().unwrap().coupling,
        analyzer_core::architecture::CycleCoupling::CodeCycle
    );
    assert!(d
        .evidence
        .iter()
        .all(|e| e.dependency.as_ref().unwrap().kind == "call"));
    assert_eq!(d.measurements["metadata_sites"], 0.0);
}

#[test]
fn console_architecture_cards_group_the_cycle_assessment_and_actions() {
    let f = Fixture::new();
    std::fs::write(f.0.join("a.py"), "class A: pass\n").unwrap();
    std::fs::write(f.0.join("b.py"), "class B: pass\n").unwrap();
    std::fs::write(f.0.join("History.php"), "<?php namespace Domain\\Entity; use Domain\\Repository\\HistoryRepository; #[Meta(repositoryClass: HistoryRepository::class)] class History {public function run(HistoryRepository $repo){$repo->run($this);}}").unwrap();
    std::fs::write(f.0.join("HistoryRepository.php"), "<?php namespace Domain\\Repository; use Domain\\Entity\\History; class HistoryRepository { public function run(History $history) {$history->run($this);} }").unwrap();
    let output = f.scan(&["--format", "console"]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    let start = text.find("ПРЕДУПРЕЖДЕНИЕ #1").unwrap();
    let card = &text[start..];
    let sections = [
        "Факт:",
        "Оценка:",
        "Замкнутый путь:",
        "Основания · места в коде:",
        "Что проверить или изменить:",
        "Границы оценки:",
    ];
    let positions = sections.map(|label| card.find(label).unwrap());
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(card.contains("Вызов"));
    assert!(card.contains("обращения к свойствам и переменным"));
    assert!(!card.contains("Гипотеза:"));
    assert!(!card.contains("Dependency inversion"));
    // Explanatory prose stays readable without terminal-dependent wrapping.
    for line in card.lines().filter(|line| {
        line.starts_with("      ") && !line.contains("php:") && !line.contains(".php")
    }) {
        assert!(line.chars().count() <= 92, "{line}");
    }
}
#[test]
fn second_review_qualified_imports_preserve_type_dependencies_and_alias_scopes() {
    for (sources, source_class, target_class) in [
        (vec![("a.py", "import b as models\nclass Service:\n    def run(self, repo: models.Repo): return repo\n"), ("b.py", "class Repo: pass\n")], "a.Service", "b.Repo"),
        (vec![("a.ts", "import * as models from './b'; export class Service { run(repo: models.Repo) { return repo; } }"), ("b.ts", "export class Repo {}")], "a.Service", "b.Repo"),
        (vec![("a.php", "<?php namespace A; use B as Models; class Service { public function run(Models\\Repo $repo): Models\\Repo { return $repo; } }"), ("b.php", "<?php namespace B; class Repo {}")], "A\\Service", "B\\Repo"),
    ] {
        let policy: ArchitectureConfig = serde_yaml::from_str(&format!("layers:\n  - {{name: app, selectors: ['class:{source_class}']}}\n  - {{name: data, selectors: ['class:{target_class}']}}\nforbidden_dependencies:\n  - {{from: app, to: data}}\n")).unwrap();
        let (_, _, issues) = check(&sources, policy);
        assert!(ids(&issues).contains(&"arch_boundary"), "{}", sources[0].0);
    }
    let policy: ArchitectureConfig = serde_yaml::from_str("layers:\n  - {name: app, selectors: ['file:main.py']}\n  - {name: first, selectors: ['class:b.Repo']}\n  - {name: second, selectors: ['class:c.Repo']}\nforbidden_dependencies:\n  - {from: app, to: first}\n  - {from: app, to: second}\n").unwrap();
    let sources = [("main.py", "def first():\n    import b as models\n    def send(repo: models.Repo): return repo\n    return send\ndef second():\n    import c as models\n    def send(repo: models.Repo): return repo\n    return send\n"), ("b.py", "class Repo: pass\n"), ("c.py", "class Repo: pass\n")];
    let issues = check(&sources, policy).2;
    assert_eq!(
        issues.iter().filter(|v| v.rule == "arch_boundary").count(),
        2
    );
}
#[test]
fn second_review_local_functions_are_not_class_contract_methods() {
    for source in [
        "class Base:\n    def operation(self): return 42\nclass Child(Base):\n    def helper(self):\n        def operation(): raise NotImplementedError()\n        return operation\n",
        "class Base:\n    def helper(self):\n        def operation(): return 42\n        return operation\nclass Child(Base):\n    def operation(self): raise NotImplementedError()\n",
    ] {
        assert!(!ids(&check(&[("members.py", source)], extended()).2).contains(&"arch_lsp"));
    }
    let source = "abstract class Contract { a() { function helper1(){} function helper2(){} function helper3(){} function helper4(){} } b(){} c(){} d(){} }\nclass A1 { run(api: Contract) { api.a(); api.b(); } }\nclass A2 { run(api: Contract) { api.a(); api.b(); } }\nclass B1 { run(api: Contract) { api.c(); api.d(); } }\nclass B2 { run(api: Contract) { api.c(); api.d(); } }";
    assert!(!ids(&check(&[("members.ts", source)], extended()).2).contains(&"arch_isp"));
}
#[test]
fn second_review_opaque_rust_tokens_and_python_introspection_preserve_names() {
    let rust_body = |name: &str| {
        (0..15)
            .map(|i| format!("{name} = ({name} + {i}) * 2;\n"))
            .collect::<String>()
    };
    let source = format!("fn first(mut value:i64)->&'static str {{ {} stringify!(value) }}\nfn second(mut item:i64)->&'static str {{ {} stringify!(item) }}", rust_body("value"), rust_body("item"));
    assert!(
        !ids(&check(&[("macros.rs", &source)], ArchitectureConfig::default()).2)
            .contains(&"arch_dry")
    );
    let source = format!("fn demo(value:i64)->(i64,i64) {{ macro_rules! keep {{ ($v:ident) => {{ $v }}; }} let first = |mut value:i64|->i64 {{ {} keep!(value) }}; let second = |mut item:i64|->i64 {{ {} keep!(value) }}; (first(1), second(1)) }}", rust_body("value"), rust_body("item"));
    assert!(!ids(&check(
        &[("macro_bindings.rs", &source)],
        ArchitectureConfig::default()
    )
    .2)
    .contains(&"arch_dry"));
    let source = format!("fn first(mut any:i64)->i64 {{ {} #[cfg(any())] {{ return any; }} return 0; }}\nfn second(mut all:i64)->i64 {{ {} #[cfg(all())] {{ return all; }} return 0; }}", rust_body("any"), rust_body("all"));
    assert!(
        !ids(&check(&[("attributes.rs", &source)], ArchitectureConfig::default()).2)
            .contains(&"arch_dry")
    );
    for operation in ["vars()", "dir()"] {
        let body = |name: &str| {
            (0..15)
                .map(|i| format!("    {name} = ({name} + {i}) * 2\n"))
                .collect::<String>()
        };
        let source = format!("def first(value):\n{}    return {operation}\ndef second(item):\n{}    return {operation}\n", body("value"), body("item"));
        assert!(
            !ids(&check(
                &[("introspection.py", &source)],
                ArchitectureConfig::default()
            )
            .2)
            .contains(&"arch_dry"),
            "{operation}"
        );
        let body = format!("    total = 0\n{}    return {operation}\n", body("total"));
        let source = format!("def first(value):\n{body}def second(item):\n{body}");
        assert!(
            !ids(&check(
                &[("unused_parameters.py", &source)],
                ArchitectureConfig::default()
            )
            .2)
            .contains(&"arch_dry"),
            "unused parameter {operation}"
        );
    }
}
#[test]
fn second_review_decodes_javascript_import_and_reexport_literals() {
    for extension in ["js", "ts"] {
        let a = format!("a.{extension}");
        let b = format!("b.{extension}");
        for literal in ["./\\x62", "./\\u0062", "./\\u{62}"] {
            for statement in [
                format!("import '{literal}';"),
                format!("export * from '{literal}';"),
            ] {
                let (_, _, issues) = check(
                    &[(&a, &statement), (&b, "import './a';")],
                    ArchitectureConfig::default(),
                );
                assert!(
                    !ids(&issues).contains(&"arch_cycle"),
                    "{extension} {statement}"
                );
            }
        }
    }
    let policy: ArchitectureConfig = serde_yaml::from_str("layers:\n  - {name: app, selectors: ['class:a.Service']}\n  - {name: data, selectors: ['class:b.Repo']}\nforbidden_dependencies:\n  - {from: app, to: data}\n").unwrap();
    let sources = [("a.ts", "import {Repo} from './\\x62'; export class Service { run(repo: Repo) { return repo; } }"), ("b.ts", "export class Repo {}")];
    assert!(ids(&check(&sources, policy).2).contains(&"arch_boundary"));
    let (_, summary, issues) = check(
        &[("unknown.ts", "export * from './\\uD800';")],
        ArchitectureConfig::default(),
    );
    assert!(summary.unresolved_dependencies > 0);
    assert!(issues.is_empty());
    assert_eq!(
        summary
            .rules
            .iter()
            .find(|r| r.rule == "arch_cycle")
            .unwrap()
            .status,
        "partial"
    );
}
#[test]
fn second_review_python_docstrings_do_not_change_refusal_facts() {
    let source = "class Base:\n    def run(self):\n        \"\"\"Documented refusal.\"\"\"\n        raise NotImplementedError()\nclass Child(Base):\n    def run(self): raise NotImplementedError()\n";
    assert!(!ids(&check(&[("stubs.py", source)], extended()).2).contains(&"arch_lsp"));
    let source = "class Base:\n    def run(self): return 42\nclass Child(Base):\n    def run(self):\n        \"\"\"Documented refusal.\"\"\"\n        raise NotImplementedError()\n";
    assert!(ids(&check(&[("stubs.py", source)], extended()).2).contains(&"arch_lsp"));
}
#[test]
fn second_review_qualified_rust_refusals_require_unshadowed_intrinsics() {
    let base = "trait Base { fn run(&self)->i32 { 42 } } struct Child;";
    for macro_name in [
        "unimplemented",
        "core::unimplemented",
        "std::unimplemented",
        "::core::unimplemented",
        "::std::unimplemented",
    ] {
        let source =
            format!("{base} impl Base for Child {{ fn run(&self)->i32 {{ {macro_name}!() }} }}");
        assert!(
            ids(&check(&[("stubs.rs", &source)], extended()).2).contains(&"arch_lsp"),
            "{macro_name}"
        );
    }
    let source = format!("macro_rules! unimplemented {{ () => {{ 42 }}; }} {base} impl Base for Child {{ fn run(&self)->i32 {{ unimplemented!() }} }}");
    assert!(!ids(&check(&[("stubs.rs", &source)], extended()).2).contains(&"arch_lsp"));
    let source = format!("macro_rules! unimplemented {{ () => {{ 42 }}; }} {base} impl Base for Child {{ fn run(&self)->i32 {{ ::core::unimplemented!() }} }}");
    assert!(ids(&check(&[("stubs.rs", &source)], extended()).2).contains(&"arch_lsp"));
    let source = format!("mod custom {{ macro_rules! unimplemented {{ () => {{ 42 }}; }} pub(crate) use unimplemented; }} use custom as core; {base} impl Base for Child {{ fn run(&self)->i32 {{ core::unimplemented!() }} }}");
    assert!(!ids(&check(&[("stubs.rs", &source)], extended()).2).contains(&"arch_lsp"));
    for namespace in ["core", "std"] {
        let root = format!("pub mod {namespace}; pub mod feature; #[macro_export] macro_rules! custom {{ () => {{ 42 }}; }}");
        let module = format!("{namespace}.rs");
        let feature = format!("{base} impl Base for Child {{ fn run(&self)->i32 {{ ::{namespace}::unimplemented!() }} }}");
        assert!(
            !ids(&check(
                &[
                    ("lib.rs", &root),
                    (&module, "pub use custom as unimplemented;"),
                    ("feature.rs", &feature),
                ],
                extended()
            )
            .2)
            .contains(&"arch_lsp"),
            "cross-file namespace {namespace}"
        );
        let root = format!("pub mod custom; pub mod feature; pub use custom as {namespace}; #[macro_export] macro_rules! custom_macro {{ () => {{ 42 }}; }}");
        assert!(
            !ids(&check(
                &[
                    ("lib.rs", &root),
                    ("custom.rs", "pub use custom_macro as unimplemented;"),
                    ("feature.rs", &feature),
                ],
                extended()
            )
            .2)
            .contains(&"arch_lsp"),
            "cross-file namespace alias {namespace}"
        );
        let root = format!("extern crate custom as {namespace}; pub mod feature;");
        assert!(
            !ids(&check(&[("lib.rs", &root), ("feature.rs", &feature)], extended()).2)
                .contains(&"arch_lsp"),
            "cross-file extern crate alias {namespace}"
        );
    }
}
#[test]
fn review_dry_preserves_block_bindings_and_automatic_semicolon_boundaries() {
    let arithmetic = (0..15)
        .map(|i| format!("input = (input + {i}) * 2;\n"))
        .collect::<String>();
    for path in ["scope.js", "scope.ts"] {
        for declaration in ["let", "const"] {
            let source = format!(
                "const value = 10; const item = 20;\nfunction first(input) {{ {arithmetic} if(input) {{ {declaration} value = 0; }} return value; }}\nfunction second(input) {{ {arithmetic} if(input) {{ {declaration} item = 0; }} return item; }}"
            );
            assert!(
                !ids(&check(&[(path, &source)], ArchitectureConfig::default()).2)
                    .contains(&"arch_dry"),
                "{path} {declaration}"
            );
        }
        let source = format!(
            "function first(input) {{ {arithmetic} return input\n}}\nfunction second(input) {{ {arithmetic} return\ninput\n}}"
        );
        assert!(
            !ids(&check(&[(path, &source)], ArchitectureConfig::default()).2).contains(&"arch_dry"),
            "{path}: ASI changes the return operation"
        );
    }
}
#[test]
fn review_dry_preserves_ordered_callees_and_external_import_bindings() {
    let arithmetic = (0..15)
        .map(|i| format!("    x = (x + {i}) * 2\n"))
        .collect::<String>();
    let left = format!("from ops import plus as first\nfrom ops import multiply as second\ndef work(x):\n{arithmetic}    return second(first(x))\n");
    let right = format!("from ops import multiply as first\nfrom ops import plus as second\ndef work(x):\n{arithmetic}    return second(first(x))\n");
    let ops = "def plus(x): return x + 1\ndef multiply(x): return x * 2\n";
    assert!(!ids(&check(
        &[("left.py", &left), ("right.py", &right), ("ops.py", ops)],
        ArchitectureConfig::default()
    )
    .2)
    .contains(&"arch_dry"));
    assert!(ids(&check(
        &[("left.py", &left), ("right.py", &left), ("ops.py", ops)],
        ArchitectureConfig::default()
    )
    .2)
    .contains(&"arch_dry"));

    let body = (0..15)
        .map(|i| format!("        value = Number(value) + {i}\n"))
        .collect::<String>();
    let function = |name: &str, package: &str| {
        format!("def {name}(value):\n    from {package} import Number\n    if value:\n{body}    return value\n")
    };
    let different = function("one", "package_one") + &function("two", "package_two");
    assert!(!ids(&check(
        &[("external.py", &different)],
        ArchitectureConfig::default()
    )
    .2)
    .contains(&"arch_dry"));
    let same = function("one", "package_one") + &function("two", "package_one");
    assert!(
        ids(&check(&[("external.py", &same)], ArchitectureConfig::default()).2)
            .contains(&"arch_dry")
    );
}
#[test]
fn review_dry_fingerprints_keep_distinct_binding_groups_out_of_each_others_baselines() {
    let block = "        value = convert(value) + 1\n".repeat(15);
    let function = |name: &str, result: usize| {
        format!("def {name}(value):\n    from a import convert\n    if value:\n{block}    from b import convert\n    if value:\n{block}    return {result}\n")
    };
    let source = function("one", 1) + &function("two", 2);
    let f = Fixture::new();
    std::fs::write(f.0.join("a.py"), "def convert(value): return value + 1\n").unwrap();
    std::fs::write(f.0.join("b.py"), "def convert(value): return value * 2\n").unwrap();
    std::fs::write(f.0.join("main.py"), &source).unwrap();
    let analyzer = Analyzer::new(registry(), Config::default());
    let report = analyzer.scan(&f.0).unwrap();
    let copies = report
        .violations
        .iter()
        .filter(|v| v.rule == "arch_dry")
        .collect::<Vec<_>>();
    assert_eq!(copies.len(), 2);
    assert_ne!(copies[0].fingerprint(), copies[1].fingerprint());
    analyzer_core::rules::write_baseline(
        &f.0.join(".analyzer-baseline.json"),
        &[copies[0].clone()],
    )
    .unwrap();
    let filtered = analyzer.scan(&f.0).unwrap();
    let remaining = filtered
        .violations
        .iter()
        .filter(|v| v.rule == "arch_dry")
        .collect::<Vec<_>>();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].fingerprint(), copies[1].fingerprint());
    std::fs::write(f.0.join("main.py"), format!("\n\n{source}")).unwrap();
    let moved = analyzer.scan(&f.0).unwrap();
    let moved = moved
        .violations
        .iter()
        .filter(|v| v.rule == "arch_dry")
        .collect::<Vec<_>>();
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].fingerprint(), copies[1].fingerprint());
}
#[test]
fn review_direct_imports_reexports_and_abstract_contract_boundaries() {
    let policy: ArchitectureConfig = serde_yaml::from_str("layers:\n  - {name: application, selectors: ['class:boundary.Service']}\n  - {name: contracts, selectors: ['class:boundary.Repository']}\nforbidden_dependencies:\n  - {from: application, to: contracts, principle: DIP}\n").unwrap();
    for (modifier, pattern) in [
        ("abstract ", "Layer boundary"),
        ("", "Dependency inversion"),
    ] {
        let source = format!("export {modifier}class Repository {{ save():void {{}} }}\nexport class Service extends Repository {{}}\n");
        let (_, _, issues) = check(&[("boundary.ts", &source)], policy.clone());
        let issue = issues.iter().find(|v| v.rule == "arch_boundary").unwrap();
        assert_eq!(
            issue.architecture.as_ref().unwrap().recommendations[0].pattern,
            pattern
        );
    }
    let sources = [
        ("a.ts", "export {X} from './c';\n"),
        ("b.ts", "import {X} from './a'; export class Y {}\n"),
        ("c.ts", "import {Y} from './b'; export class X {}\n"),
    ];
    let (_, _, issues) = check(&sources, ArchitectureConfig::default());
    assert!(!ids(&issues).contains(&"arch_cycle"));
    let module_policy:ArchitectureConfig = serde_yaml::from_str("layers:\n  - {name: consumer, selectors: ['file:b.ts']}\n  - {name: barrel, selectors: ['file:a.ts']}\nforbidden_dependencies:\n  - {from: consumer, to: barrel}\n").unwrap();
    assert!(ids(&check(&sources, module_policy).2).contains(&"arch_boundary"));

    for (suffix, language) in [("js", "javascript"), ("ts", "typescript")] {
        let a = format!("a.{suffix}");
        let b = format!("b.{suffix}");
        for (left, right) in [
            ("import './b';", "import './a';"),
            ("export * from './b';", "export * from './a';"),
        ] {
            let (_, summary, issues) =
                check(&[(&a, left), (&b, right)], ArchitectureConfig::default());
            assert!(
                !ids(&issues).contains(&"arch_cycle"),
                "{language}: {left}, {right}; {summary:?}"
            );
        }
        let main = format!("pkg/main.{suffix}");
        let index = format!("pkg/folder/index.{suffix}");
        let (_, _, issues) = check(
            &[(&main, "import './folder';"), (&index, "import '../main';")],
            ArchitectureConfig::default(),
        );
        assert!(
            !ids(&issues).contains(&"arch_cycle"),
            "{language}: relative directory imports"
        );
    }
}
#[test]
fn review_shadowed_builtins_and_rebound_receivers_do_not_prove_solid_or_demeter() {
    let classes = "class Base:\n    def run(self): return 1\nclass Child(Base):\n    def run(self): raise NotImplementedError()\n";
    for binding in [
        "NotImplementedError, other = ValueError, None\n",
        "for NotImplementedError in [ValueError]:\n    pass\n",
        "(NotImplementedError := ValueError)\n",
    ] {
        let source = format!("{binding}{classes}");
        assert!(
            !ids(&check(&[("shadow.py", &source)], extended()).2).contains(&"arch_lsp"),
            "{binding}"
        );
    }
    let types =
        "class Shape: pass\nclass A(Shape): pass\nclass B(Shape): pass\nclass C(Shape): pass\n";
    let dispatch = ["one", "two", "three"].iter().map(|name| format!("def {name}(value:Shape):\n    if isinstance(value,A): return 1\n    elif isinstance(value,B): return 2\n    elif isinstance(value,C): return 3\n")).collect::<String>();
    for binding in [
        "def isinstance(x, expected): return True\n",
        "from external import isinstance\n",
        "isinstance = lambda x, expected: True\n",
    ] {
        let source = format!("{binding}{types}{dispatch}");
        assert!(
            !ids(&check(&[("dispatch.py", &source)], extended()).2).contains(&"arch_ocp"),
            "{binding}"
        );
    }
    let types = "class Leaf { run(){return 1;} } class Middle { leaf:Leaf; ping(){return 1;} } class Root { middle:Middle; ping(){return 1;} } class DtoMiddle { leaf:Leaf; } class DtoRoot { middle:DtoMiddle; }";
    for body in [
        "{const root=other;root.middle.leaf.run();root.middle.leaf.run();}",
        "root=other;root.middle.leaf.run();root.middle.leaf.run();",
    ] {
        let source = format!("{types} function work(root:Root,other:DtoRoot) {{ {body} }}");
        assert!(
            !ids(&check(&[("navigation.ts", &source)], extended()).2).contains(&"arch_demeter"),
            "{body}"
        );
    }
}
#[test]
fn review_cohesion_does_not_invent_dynamic_fields_or_merge_duplicate_classes() {
    let methods = (0..4).map(|i| format!("public function left{i}($field='shared') {{ return $this->$field; }}\npublic function right{i}($property='shared') {{ return $this->$property; }}\n")).collect::<String>();
    let dynamic = format!("<?php class Example {{ public $shared = 1; {methods} }}");
    assert!(!ids(&check(&[("dynamic.php", &dynamic)], extended()).2).contains(&"arch_srp"));
    let class = |field: &str| {
        let methods = (0..4)
            .map(|i| format!("public function method{i}() {{ return $this->{field}++; }}\n"))
            .collect::<String>();
        format!("<?php namespace App; class Service {{ private int ${field}=0; {methods} }}")
    };
    assert!(!ids(&check(
        &[("a.php", &class("first")), ("b.php", &class("second"))],
        extended()
    )
    .2)
    .contains(&"arch_srp"));
}
#[test]
fn renamed_clones_in_every_language_preserve_operations_literals_and_binding_reuse() {
    for (path, a, b) in [
        ("copies.py", "value", "item"),
        ("copies.js", "value", "item"),
        ("copies.ts", "value", "item"),
        ("copies.php", "value", "item"),
        ("copies.rs", "value", "item"),
    ] {
        let function = |name: &str, param: &str, last: &str| {
            let mut body = String::new();
            for i in 0..15 {
                match path {
                    "copies.py" => body.push_str(&format!("    {param} = ({param} + {i}) * 2\n")),
                    "copies.php" => body.push_str(&format!("${param} = (${param} + {i}) * 2;\n")),
                    _ => body.push_str(&format!("{param} = ({param} + {i}) * 2;\n")),
                }
            }
            match path {
                "copies.py"=>format!("def {name}({param}):\n{body}    return {param} + {last}\n"),
                "copies.php"=>format!("function {name}(${param}) {{ {body}return ${param} + {last}; }}\n"),
                "copies.rs"=>format!("fn {name}(mut {param}: i64) -> i64 {{ {body}return {param} + {last}; }}\n"),
                "copies.ts"=>format!("function {name}({param}: number): number {{ {body}return {param} + {last}; }}\n"),
                _=>format!("function {name}({param}) {{ {body}return {param} + {last}; }}\n"),
            }
        };
        let mut source = if path.ends_with("php") {
            "<?php\n".into()
        } else {
            String::new()
        };
        source += &function("first", a, "99");
        source += &function("second", b, "99");
        source += &function("different", b, "98");
        let (_, _, issues) = check(&[(path, &source)], ArchitectureConfig::default());
        let clones = issues
            .iter()
            .filter(|v| v.rule == "arch_dry")
            .collect::<Vec<_>>();
        assert_eq!(clones.len(), 1, "{path}: {:?}", ids(&issues));
        assert_eq!(clones[0].architecture.as_ref().unwrap().evidence_total, 2);
        assert!(!clones[0]
            .architecture
            .as_ref()
            .unwrap()
            .participants
            .iter()
            .any(|s| s.contains("different")));
    }
    let source="function a(x, y) { x=x+x; x=x+x; x=x+x; x=x+x; x=x+x; x=x+x; x=x+x; x=x+x; x=x+x; x=x+x; x=x+x; return x; } function b(v,w) {v=v+w;v=v+w;v=v+w;v=v+w;v=v+w;v=v+w;v=v+w;v=v+w;v=v+w;v=v+w;v=v+w;return v;}";
    assert!(
        !ids(&check(&[("reuse.js", source)], ArchitectureConfig::default()).2)
            .contains(&"arch_dry")
    );
}
#[test]
fn renamed_clones_preserve_keyword_argument_contracts_and_local_declarations() {
    let python = |name: &str, parameter: &str, keyword: &str| {
        let body = (0..15)
            .map(|i| format!("    {parameter} = emit({keyword}={parameter}, scale={i})\n"))
            .collect::<String>();
        format!("def {name}({parameter}):\n{body}    return {parameter}\n")
    };
    let different_keywords = python("first", "value", "value") + &python("second", "item", "item");
    assert!(!ids(&check(
        &[("keywords.py", &different_keywords)],
        ArchitectureConfig::default()
    )
    .2)
    .contains(&"arch_dry"));

    let same_keywords = python("first", "value", "amount") + &python("second", "item", "amount");
    assert!(ids(&check(
        &[("keywords.py", &same_keywords)],
        ArchitectureConfig::default()
    )
    .2)
    .contains(&"arch_dry"));

    for path in ["locals.js", "locals.ts"] {
        let function = |name: &str, local: &str| {
            let body = (0..15)
                .map(|i| format!("{local} = ({local} + {i}) * 2;\n"))
                .collect::<String>();
            format!("function {name}(input) {{ let {local} = input; {body} return {local}; }}")
        };
        let source = function("first", "value") + &function("second", "item");
        assert!(
            ids(&check(&[(path, &source)], ArchitectureConfig::default()).2).contains(&"arch_dry")
        );
    }
}
#[test]
fn repeated_dispatch_and_product_families_have_structural_recommendations() {
    let mut source="abstract class Shape {} class A extends Shape {} class B extends Shape {} class C extends Shape {}\n".to_string();
    for name in ["one", "two", "three"] {
        source+=&format!("function {name}(value: Shape) {{ if (value instanceof A) {{ return 1; }} else if (value instanceof B) {{ return 2; }} else if (value instanceof C) {{ return 3; }} }}\n");
    }
    let (_, _, issues) = check(&[("dispatch.ts", &source)], extended());
    let issue = issues
        .iter()
        .find(|v| v.rule == "arch_ocp")
        .expect("repeated type dispatch");
    assert_eq!(
        issue.architecture.as_ref().unwrap().recommendations[0].pattern,
        "Strategy"
    );
    for (path, header, site) in [
        ("dispatch.php", "<?php namespace App; abstract class Shape {} class A extends Shape {} class B extends Shape {} class C extends Shape {}", "function NAME(Shape $value) {if($value instanceof A){return 1;}elseif($value instanceof B){return 2;}elseif($value instanceof C){return 3;}}"),
        ("dispatch.py", "class Shape: pass\nclass A(Shape): pass\nclass B(Shape): pass\nclass C(Shape): pass\n", "def NAME(value:Shape):\n    if isinstance(value,A): return 1\n    elif isinstance(value,B): return 2\n    elif isinstance(value,C): return 3\n"),
        ("dispatch.js", "class Shape {} class A extends Shape {} class B extends Shape {} class C extends Shape {}", "function NAME(value) {if(value instanceof A){return 1;}else if(value instanceof B){return 2;}else if(value instanceof C){return 3;}}"),
    ] {
        let mut source=header.to_owned();
        for name in ["one","two","three"] {source+=&site.replace("NAME",name);}
        let (files, _, issues)=check(&[(path,&source)],extended());
        assert!(ids(&issues).contains(&"arch_ocp"),"{path}: {:?} {:?}",files[0].types,files[0].architecture_facts.as_ref().unwrap().functions);
    }
    let mut source="abstract class Button {} abstract class Menu {} class AButton extends Button {} class BButton extends Button {} class CButton extends Button {} class AMenu extends Menu {} class BMenu extends Menu {} class CMenu extends Menu {}\n".to_string();
    for name in ["first", "second"] {
        source+=&format!("function {name}(kind: string) {{ if (kind === 'a') {{ return [new AButton(), new AMenu()]; }} else if (kind === 'b') {{ return [new BButton(), new BMenu()]; }} else if (kind === 'c') {{ return [new CButton(), new CMenu()]; }} }}\n");
    }
    let (_, _, issues) = check(&[("factory.ts", &source)], extended());
    let issue = issues
        .iter()
        .find(|v| v.rule == "arch_factory")
        .expect("repeated creation");
    assert_eq!(
        issue.architecture.as_ref().unwrap().recommendations[0].pattern,
        "Abstract Factory"
    );
    assert!(
        !ids(&check(&[("factory.ts", &source)], ArchitectureConfig::default()).2)
            .contains(&"arch_factory")
    );
    let (_,_,issues)=check(&[("closed.rs","enum Mode { A, B, C } fn a(m:Mode)->u8{match m {Mode::A=>1,Mode::B=>2,Mode::C=>3}} fn b(m:Mode)->u8{match m {Mode::A=>1,Mode::B=>2,Mode::C=>3}} fn c(m:Mode)->u8{match m {Mode::A=>1,Mode::B=>2,Mode::C=>3}}")],extended());
    assert!(!ids(&issues).contains(&"arch_ocp"));
}
#[test]
fn srp_does_not_connect_foreign_methods_with_matching_names() {
    let mut source =
        "class Mixed:\n    def __init__(self):\n        self.a = 1\n        self.b = 2\n"
            .to_string();
    for i in 0..4 {
        source+=&format!("    def left{i}(self, other):\n        other.right0()\n        self.a += {i}\n        return self.a + {i}\n");
        source += &format!(
            "    def right{i}(self):\n        self.b += {i}\n        return self.b + {i}\n"
        );
    }
    let (_, _, issues) = check(&[("mixed.py", &source)], extended());
    assert!(ids(&issues).contains(&"arch_srp"));
    source = source.replace("other.right0()", "self.right0()");
    let (_, _, issues) = check(&[("mixed.py", &source)], extended());
    assert!(!ids(&issues).contains(&"arch_srp"));
    let utility="class Utility:\n    def a(self): return 1\n    def b(self): return 2\n    def c(self): return 3\n    def d(self): return 4\n    def e(self): return 5\n    def f(self): return 6\n    def g(self): return 7\n    def h(self): return 8\n";
    assert!(!ids(&check(&[("utility.py", utility)], extended()).2).contains(&"arch_srp"));
}
#[test]
fn substitution_detects_only_a_concrete_override_refusing_an_operation() {
    let source="class Base:\n    def run(self):\n        return 1\nclass Child(Base):\n    def run(self):\n        raise NotImplementedError()\n";
    assert!(ids(&check(&[("lsp.py", source)], extended()).2).contains(&"arch_lsp"));
    let abstract_source = source.replace("return 1", "raise NotImplementedError()");
    assert!(!ids(&check(&[("lsp.py", &abstract_source)], extended()).2).contains(&"arch_lsp"));
    let shadowed = format!("class NotImplementedError(Exception):\n    pass\n{source}");
    assert!(!ids(&check(&[("shadow.py", &shadowed)], extended()).2).contains(&"arch_lsp"));
    let rebound = format!("NotImplementedError = ValueError\n{source}");
    assert!(!ids(&check(&[("rebound.py", &rebound)], extended()).2).contains(&"arch_lsp"));
    let two_operations = "class Base:\n    def run(self): return 1\n    def stop(self): return 2\nclass Child(Base):\n    def run(self): raise NotImplementedError()\n    def stop(self): raise NotImplementedError()\n";
    let (_, _, issues) = check(&[("operations.py", two_operations)], extended());
    let fingerprints = issues
        .iter()
        .filter(|v| v.rule == "arch_lsp")
        .map(|v| v.fingerprint())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(fingerprints.len(), 2);
}

#[test]
fn duplicate_matching_keeps_python_block_boundaries() {
    let source = "def first(value):\n    if value > 0:\n        value += 1\n        if value > 1:\n            value += 2\n        value += 3\n    value += 4\n    return value\ndef second(value):\n    if value > 0:\n        value += 1\n    if value > 1:\n        value += 2\n        value += 3\n    value += 4\n    return value\n";
    let mut config = ArchitectureConfig::default();
    config.rules.insert(
        "arch_dry".into(),
        analyzer_core::architecture::RuleConfig {
            min_tokens: Some(30),
            min_statements: Some(2),
            ..Default::default()
        },
    );
    assert!(!ids(&check(&[("indentation.py", source)], config).2).contains(&"arch_dry"));
}
#[test]
fn observed_interface_consumers_form_disjoint_groups() {
    let mut source="interface Service { a():void; b():void; c():void; d():void; e():void; f():void; g():void; h():void; }\n".to_string();
    for (name, methods) in [
        ("Left1", ["a", "b"]),
        ("Left2", ["a", "b"]),
        ("Right1", ["c", "d"]),
        ("Right2", ["c", "d"]),
    ] {
        source += &format!(
            "class {name} {{ run(s:Service){{s.{}();s.{}();}} }}\n",
            methods[0], methods[1]
        );
    }
    assert!(ids(&check(&[("isp.ts", &source)], extended()).2).contains(&"arch_isp"));
    let overlap = source.replace("s.c()", "s.a()");
    assert!(!ids(&check(&[("overlap.ts", &overlap)], extended()).2).contains(&"arch_isp"));
}
#[test]
fn import_only_cycles_are_ignored_while_explicit_boundaries_keep_imports() {
    let sources = [
        ("a.py", "from b import B\nclass A:\n    pass\n"),
        ("b.py", "from a import A\nclass B:\n    pass\n"),
    ];
    let (_, _, issues) = check(&sources, ArchitectureConfig::default());
    assert!(!ids(&issues).contains(&"arch_cycle"));
    let config:ArchitectureConfig=serde_yaml::from_str("layers:\n  - {name: domain, selectors: ['dir:src/domain']}\n  - {name: infra, selectors: ['dir:src/infra']}\nforbidden_dependencies:\n  - {from: domain, to: infra, principle: DIP}\n").unwrap();
    let sources = [
        (
            "src/domain/service.ts",
            "import { Store } from '../infra/store'; export function use(s:Store){s.save();}",
        ),
        (
            "src/infra/store.ts",
            "export class Store {save(){return 1;}}",
        ),
    ];
    let (_, _, issues) = check(&sources, config.clone());
    assert!(ids(&issues).contains(&"arch_boundary"));
    let mut config = config;
    config.composition_roots = vec!["dir:src/domain".into()];
    assert!(!ids(&check(&sources, config).2).contains(&"arch_boundary"));
    let sources = [("src/domainExtra/service.ts", sources[0].1), sources[1]];
    assert!(!ids(&check(&sources, ArchitectureConfig::default()).2).contains(&"arch_boundary"));
}
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "codefactor-architecture-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("a.py"),
            "from b import ping_b\ndef ping_a(): ping_b()\n",
        )
        .unwrap();
        std::fs::write(
            root.join("b.py"),
            "from a import ping_a\ndef ping_b(): ping_a()\n",
        )
        .unwrap();
        Self(root)
    }
    fn scan(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_codefactor"))
            .args(["scan", self.0.to_str().unwrap()])
            .args(args)
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn disabling_advisory_architecture_preserves_numeric_value_access_cycles() {
    for sources in [
        [
            ("a.php", "<?php namespace A; use B\\Right; class Left {const VALUE=1; function read(){return Right::VALUE;}}"),
            ("b.php", "<?php namespace B; use A\\Left; class Right {const VALUE=1; function read(){return Left::VALUE;}}"),
        ],
        [
            ("a.ts", "import {B} from './b'; export class A {value=0; run(b:B){return b.value;}}"),
            ("b.ts", "import {A} from './a'; export class B {value=0; run(a:A){a.value=1;}}"),
        ],
    ] {
        let fixture = Fixture::new();
        for path in ["a.py", "b.py"] { std::fs::remove_file(fixture.0.join(path)).unwrap(); }
        for (path, source) in sources { std::fs::write(fixture.0.join(path), source).unwrap(); }
        std::fs::write(fixture.0.join("analyzer.yaml"), "limits:\n  module_cycle: {error: 0}\n").unwrap();
        let enabled = fixture.scan(&["--format", "json"]);
        assert_eq!(enabled.status.code(), Some(2));
        let enabled: Value = serde_json::from_slice(&enabled.stdout).unwrap();
        assert_eq!(enabled["violations"].as_array().unwrap().iter().filter(|v| v["rule"] == "module_cycle").count(), 1);
        for cli_disabled in [true, false] {
            let args = if cli_disabled {
                vec!["--no-architecture", "--format", "json"]
            } else {
                std::fs::write(fixture.0.join("analyzer.yaml"), "architecture: {enabled: false}\nlimits:\n  module_cycle: {error: 0}\n").unwrap();
                vec!["--format", "json"]
            };
            let output = fixture.scan(&args);
            assert_eq!(output.status.code(), Some(2), "{}: {}", sources[0].0, String::from_utf8_lossy(&output.stderr));
            let report: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["summary"]["cache_hits"], 2);
            let violations = report["violations"].as_array().unwrap();
            assert!(!violations.iter().any(|v| v.get("architecture").is_some()));
            let cycles = violations.iter().filter(|v| v["rule"] == "module_cycle").collect::<Vec<_>>();
            assert_eq!(cycles.len(), 1, "{}", sources[0].0);
            assert_eq!(cycles[0]["module_cycle"]["files"], serde_json::json!([sources[0].0, sources[1].0]));
            assert!(cycles[0]["module_cycle"]["evidence"].as_array().unwrap().iter().all(|e| e["dependency"]["kind"] == "member_access"));
            let metrics = report["metrics"].as_array().unwrap().iter().filter(|m| m["id"] == "module_cycle").collect::<Vec<_>>();
            assert_eq!(metrics.len(), 2);
            assert!(metrics.iter().all(|metric| metric["value"] == 1.0));
        }
    }
}
#[test]
fn numeric_module_cycles_export_one_blocking_component_with_complete_membership() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.join("analyzer.yaml"), "cache: {enabled: false}\narchitecture: {enabled: false, max_evidence: 1}\nlimits:\n  module_cycle: {error: 0}\n").unwrap();
    std::fs::write(
        fixture.0.join("a.py"),
        "from b import ping_b\ndef ping_a(): ping_b()\n",
    )
    .unwrap();
    std::fs::write(
        fixture.0.join("b.py"),
        "from c import ping_c\ndef ping_b(): ping_c()\n",
    )
    .unwrap();
    std::fs::write(
        fixture.0.join("c.py"),
        "from a import ping_a\ndef ping_c(): ping_a()\n",
    )
    .unwrap();
    let output = fixture.scan(&["--format", "json"]);
    assert_eq!(output.status.code(), Some(2));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let cycles = report["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["rule"] == "module_cycle")
        .collect::<Vec<_>>();
    assert_eq!(cycles.len(), 1);
    let detail = &cycles[0]["module_cycle"];
    assert_eq!(detail["files"], serde_json::json!(["a.py", "b.py", "c.py"]));
    assert_eq!(detail["participants"].as_array().unwrap().len(), 3);
    assert_eq!(detail["evidence"].as_array().unwrap().len(), 1);
    assert!(detail["evidence_total"].as_u64().unwrap() >= 3);
    assert!(cycles[0].get("architecture").is_none());
    for (format, expected) in [
        ("console", "Файлы компоненты:"),
        ("md", "Files in component:"),
    ] {
        let output = fixture.scan(&["--format", format]);
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains(expected), "{text}");
        assert!(text.contains("c.py"));
    }
    let output = fixture.scan(&["--format", "sarif"]);
    let sarif: Value = serde_json::from_slice(&output.stdout).unwrap();
    let results = sarif["runs"][0]["results"].as_array().unwrap();
    let cycle = results
        .iter()
        .find(|result| result["ruleId"] == "module_cycle")
        .unwrap();
    assert_eq!(cycle["level"], "error");
    assert_eq!(cycle["relatedLocations"].as_array().unwrap().len(), 3);
    assert!(cycle["properties"].get("non_blocking").is_none());
    let (_server, port) = architecture_test_server(&fixture);
    for file in ["a.py", "b.py", "c.py"] {
        let result = web_get(port, &format!("/api/violations?file={file}&kind=metrics")).unwrap();
        assert_eq!(result["total"], 1, "{file}: {result}");
        let source = web_get(port, &format!("/api/file?id={file}")).unwrap();
        assert_eq!(source["violation_count"], 1);
        assert!(!source["violation_lines"].as_array().unwrap().is_empty());
        let found = web_get(port, &format!("/api/violations?q={file}&kind=metrics")).unwrap();
        assert_eq!(found["total"], 1);
    }
    let tree = web_get(port, "/api/tree").unwrap();
    for file in ["a.py", "b.py", "c.py"] {
        let node = tree["children"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["path"] == file)
            .unwrap();
        assert_eq!(node["violations"], 1);
    }
}
#[test]
fn overlapping_module_watch_update_retains_the_report_and_exposes_the_error() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.join("analyzer.yaml"), "cache: {enabled: false}\narchitecture:\n  modules:\n    - {name: first, selectors: ['glob:a*.py']}\n    - {name: second, selectors: ['glob:*b.py']}\n").unwrap();
    let (_server, port) = architecture_path_server(&fixture.0, true);
    let started = Instant::now();
    let revision = loop {
        let status = web_get(port, "/api/status").unwrap();
        if status["revision"].as_u64().unwrap() > 0 {
            break status["revision"].as_u64().unwrap();
        }
        assert!(started.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(50));
    };
    std::fs::write(fixture.0.join("ab.py"), "def overlap(): pass\n").unwrap();
    let started = Instant::now();
    loop {
        let status = web_get(port, "/api/status").unwrap();
        if let Some(error) = status["watch_error"].as_str() {
            assert!(error.contains("ab.py"), "{error}");
            assert_eq!(status["revision"], revision);
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(web_get(port, "/api/tree").unwrap()["summary"]["files"], 2);
    std::fs::remove_file(fixture.0.join("ab.py")).unwrap();
    let started = Instant::now();
    loop {
        let status = web_get(port, "/api/status").unwrap();
        if status["watch_error"].is_null() {
            assert_eq!(status["revision"], revision);
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(50));
    }
}
#[test]
fn architecture_is_nonblocking_exported_and_can_be_disabled() {
    let f = Fixture::new();
    let output = f.scan(&["--format", "json", "--fail-on-warn"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], "3");
    assert_eq!(report["summary"]["architecture_warnings"], 1);
    assert!(report["violations"]
        .as_array()
        .unwrap()
        .iter()
        .all(|v| v["severity"] == "warn"));
    assert!(report["files"]
        .as_array()
        .unwrap()
        .iter()
        .all(|v| v.get("architecture_facts").is_none()));
    let fingerprints = |r: &Value| r["violations"].clone();
    let warm = f.scan(&["--format", "json"]);
    let warm: Value = serde_json::from_slice(&warm.stdout).unwrap();
    assert_eq!(fingerprints(&report), fingerprints(&warm));
    assert_eq!(warm["summary"]["cache_hits"], 2);
    for format in ["console", "md", "sarif"] {
        let output = f.scan(&["--format", format]);
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains(if format == "console" {
            "Факт:"
        } else if format == "md" {
            "## Architecture"
        } else {
            "relatedLocations"
        }));
    }
    let output = f.scan(&["--format", "json", "--no-architecture", "--fail-on-warn"]);
    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["architecture"]["enabled"], false);
    assert_eq!(report["summary"]["architecture_warnings"], 0);
    std::fs::write(
        f.0.join("analyzer.yaml"),
        "limits:\n  file_lines: {warn: 1, error: 9999}\n",
    )
    .unwrap();
    assert_eq!(f.scan(&["--fail-on-warn"]).status.code(), Some(1));
    std::fs::write(
        f.0.join("analyzer.yaml"),
        "limits:\n  file_lines: {warn: 1, error: 1}\n",
    )
    .unwrap();
    assert_eq!(f.scan(&["--fail-on-warn"]).status.code(), Some(2));
}
#[test]
fn baseline_suppression_exclusions_and_configuration_invalidation() {
    let f = Fixture::new();
    let analyzer = Analyzer::new(registry(), Config::default());
    let first = analyzer.scan(&f.0).unwrap();
    let issue = first
        .violations
        .iter()
        .find(|v| v.architecture.is_some())
        .unwrap();
    analyzer_core::rules::write_baseline(&f.0.join(".analyzer-baseline.json"), &first.violations)
        .unwrap();
    assert!(analyzer.scan(&f.0).unwrap().violations.is_empty());
    std::fs::remove_file(f.0.join(".analyzer-baseline.json")).unwrap();
    let primary = f.0.join(&issue.file);
    let source = std::fs::read_to_string(&primary).unwrap();
    let source = source
        .lines()
        .enumerate()
        .map(|(line, text)| {
            if line + 1 == issue.line {
                format!("{text} # analyzer-ignore: arch_cycle")
            } else {
                text.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&primary, source).unwrap();
    assert!(analyzer.scan(&f.0).unwrap().violations.is_empty());
    let mut c = Config::default();
    c.architecture.exclude = vec!["a.py".into()];
    let filtered = Analyzer::new(registry(), c).scan(&f.0).unwrap();
    assert!(filtered.violations.is_empty());
    assert_eq!(filtered.architecture.analyzed_files, 1);
    let bad="architecture:\n  enabled: true\n  rules:\n    arch_dry: {unknown_tokens: 0}\nlanguages: {enabled: [python]}\n";
    std::fs::write(f.0.join("analyzer.yaml"), bad).unwrap();
    let checked = Command::new(env!("CARGO_BIN_EXE_codefactor"))
        .args([
            "scan",
            f.0.to_str().unwrap(),
            "--strict-config",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(!checked.status.success());
}

#[test]
fn deep_navigation_requires_known_behavioural_types_and_repeated_sites() {
    let source="class Leaf { run(){return 1;} } class Middle { leaf:Leaf; ping(){return 1;} } class Root { middle:Middle; ping(){return 1;} } function work(root:Root){root.middle.leaf.run();root.middle.leaf.run();}";
    assert!(ids(&check(&[("navigation.ts", source)], extended()).2).contains(&"arch_demeter"));
    let dto = source.replace(
        "class Middle { leaf:Leaf; ping(){return 1;} }",
        "class Middle { leaf:Leaf; }",
    );
    assert!(!ids(&check(&[("dto.ts", &dto)], extended()).2).contains(&"arch_demeter"));
    let fluent="class Builder { next():Builder{return this;} run(){return 1;} } function work(b:Builder){b.next().next().run();b.next().next().run();}";
    assert!(!ids(&check(&[("fluent.ts", fluent)], extended()).2).contains(&"arch_demeter"));
}
#[test]
fn duplicate_spelling_does_not_equate_different_imported_callees_or_shadowed_bindings() {
    let body = (0..15)
        .map(|i| format!("    x = (x + {i}) * 2\n"))
        .collect::<String>();
    let a = format!("from one import finish\ndef work(x):\n{body}    return finish(x)\n");
    let b = format!("from two import finish\ndef work(x):\n{body}    return finish(x)\n");
    let sources = [
        ("a.py", a.as_str()),
        ("b.py", b.as_str()),
        ("one.py", "def finish(x):\n    return 1\n"),
        ("two.py", "def finish(x):\n    return 2\n"),
    ];
    assert!(!ids(&check(&sources, ArchitectureConfig::default()).2).contains(&"arch_dry"));
    let a="function one(x){let y=x; {let x=2; y=y+x;y=y+x;y=y+x;y=y+x;y=y+x;y=y+x;y=y+x;y=y+x;y=y+x;y=y+x;y=y+x;y=y+x;}return y;}";
    let b="function two(v){let y=v; {let v=2; y=y+v;y=y+v;y=y+v;y=y+v;y=y+v;y=y+v;y=y+v;y=y+v;y=y+v;y=y+v;y=y+v;y=y+v;}return y;}";
    assert!(!ids(&check(
        &[("shadow.js", &format!("{a}\n{b}"))],
        ArchitectureConfig::default()
    )
    .2)
    .contains(&"arch_dry"));
}

#[test]
fn clone_signatures_and_layer_policies_resolve_types_and_scoped_aliases() {
    let body = (0..15)
        .map(|i| format!("value.count = value.count + {i};"))
        .collect::<String>();
    let first = format!("import {{Record}} from './left'; function first(value:Record){{{body}return value.count;}}");
    let second = format!("import {{Record}} from './right'; function second(value:Record){{{body}return value.count;}}");
    let third = format!("import {{Record as SameRecord}} from './left'; function third(value:SameRecord){{{body}return value.count;}}");
    let sources = [
        ("left.ts", "export class Record { count:number; }"),
        ("right.ts", "export class Record { count:number; }"),
        ("first.ts", first.as_str()),
        ("second.ts", second.as_str()),
        ("third.ts", third.as_str()),
    ];
    let (_, _, issues) = check(&sources, ArchitectureConfig::default());
    let clones = issues
        .iter()
        .filter(|v| v.rule == "arch_dry")
        .collect::<Vec<_>>();
    assert_eq!(clones.len(), 1);
    let participants = &clones[0].architecture.as_ref().unwrap().participants;
    assert_eq!(participants.len(), 2);
    assert!(participants.iter().all(|p| !p.contains("second.ts")));
    let body = (0..15)
        .map(|i| format!("$value->count = $value->count + {i};"))
        .collect::<String>();
    let php=format!("<?php namespace One {{ use Left\\Record; function first(Record $value){{{body}return $value->count;}} }} namespace Two {{ use Right\\Record; function second(Record $value){{{body}return $value->count;}} }}");
    let sources = [
        (
            "left.php",
            "<?php namespace Left; class Record {public int $count;}",
        ),
        (
            "right.php",
            "<?php namespace Right; class Record {public int $count;}",
        ),
        ("scopes.php", php.as_str()),
    ];
    assert!(!ids(&check(&sources, ArchitectureConfig::default()).2).contains(&"arch_dry"));
    let policy:ArchitectureConfig=serde_yaml::from_str("layers:\n  - {name: domain, selectors: ['class:domain.Service']}\n  - {name: infra, selectors: ['class:infra.Repo']}\nforbidden_dependencies:\n  - {from: domain, to: infra, principle: DIP}\n").unwrap();
    let sources = [
        (
            "domain.ts",
            "import {Repo} from './infra'; class Service { run(repo:Repo){repo.work();}}",
        ),
        ("infra.ts", "export class Repo {work(){return 1;}}"),
    ];
    assert!(ids(&check(&sources, policy).2).contains(&"arch_boundary"));
}
#[test]
fn ambiguous_layer_assignments_and_duplicate_symbols_are_not_guessed() {
    let sources = [
        (
            "a.ts",
            "import {Store} from './b';function use(s:Store){s.save();}",
        ),
        ("b.ts", "export class Store {save(){return 1;}}"),
    ];
    let config:ArchitectureConfig=serde_yaml::from_str("layers:\n - {name: one, selectors: ['file:a.ts']}\n - {name: also_one, selectors: ['file:a.ts']}\n - {name: two, selectors: ['file:b.ts']}\nforbidden_dependencies:\n - {from: one, to: two}\n").unwrap();
    let (_, summary, issues) = check(&sources, config);
    assert!(!ids(&issues).contains(&"arch_boundary"));
    assert!(!summary.diagnostics.is_empty());
    let config = extended();
    let (_, summary, _) = check(&[("bad.js", "function empty(){}")], config);
    assert!(summary.rules.iter().any(|r| r.rule == "arch_stability"));
}
#[test]
fn profiles_thresholds_and_cache_modes_are_independent() {
    let f = Fixture::new();
    let mut c = Config::default();
    c.architecture.enabled = false;
    let disabled = Analyzer::new(registry(), c.clone()).scan(&f.0).unwrap();
    assert!(disabled
        .files
        .iter()
        .all(|f| f.architecture_facts.is_some()));
    c.architecture.enabled = true;
    let cold = Analyzer::new(registry(), c.clone()).scan(&f.0).unwrap();
    assert_eq!(cold.summary.cache_hits, 2);
    assert!(cold.files.iter().all(|f| f.architecture_facts.is_some()));
    c.architecture.profile = Profile::Extended;
    let warm = Analyzer::new(registry(), c).scan(&f.0).unwrap();
    assert_eq!(warm.summary.cache_hits, 2);
    let unsupported = analyzer_typescript::TypeScript.analyze("function f(){}", Path::new("no.ts"));
    assert!(unsupported.architecture_facts.is_none());
    let mut r = registry();
    let graph = CallGraph::build(std::slice::from_ref(&unsupported), &r, &Config::default());
    let (summary, _) = analyze(&[unsupported], &graph, &ArchitectureConfig::default());
    assert_eq!(
        summary
            .rules
            .iter()
            .find(|r| r.rule == "arch_dry")
            .unwrap()
            .status,
        "unsupported"
    );
    let _ = &mut r;
}

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn web_get(port: u16, path: &str) -> Option<Value> {
    let mut socket = TcpStream::connect(("127.0.0.1", port)).ok()?;
    socket.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    write!(
        socket,
        "GET {path} HTTP/1.0\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut response = String::new();
    socket.read_to_string(&mut response).ok()?;
    if !response.starts_with("HTTP/1.1 200") && !response.starts_with("HTTP/1.0 200") {
        return None;
    }
    serde_json::from_str(response.split_once("\r\n\r\n")?.1).ok()
}
fn architecture_test_server(fixture: &Fixture) -> (Server, u16) {
    architecture_path_server(&fixture.0, false)
}
fn architecture_path_server(root: &Path, watch: bool) -> (Server, u16) {
    let port = TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut command = Command::new(env!("CARGO_BIN_EXE_codefactor"));
    command.args(["serve", root.to_str().unwrap(), "--port", &port.to_string()]);
    if watch {
        command.arg("--watch");
    }
    let server = Server(
        command
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let started = Instant::now();
    while web_get(port, "/api/status").is_none() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "server did not start"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    (server, port)
}
fn duplicate_python_source() -> String {
    let body = (0..15)
        .map(|i| format!("    value = (value + {i}) * 2\n"))
        .collect::<String>();
    format!("def one(value):\n{body}    return value\ndef two(value):\n{body}    return value\n")
}
#[test]
fn second_review_single_file_source_ast_watch_and_baseline_share_the_scan_root() {
    let fixture = Fixture::new();
    let selected = fixture.0.join("copy.py");
    std::fs::write(&selected, duplicate_python_source()).unwrap();
    let (_server, port) = architecture_path_server(&selected, true);
    assert_eq!(web_get(port, "/api/tree").unwrap()["summary"]["files"], 1);
    assert_eq!(web_get(port, "/api/architecture").unwrap()["warnings"], 1);
    assert!(web_get(port, "/api/file?id=copy.py").is_some());
    assert!(web_get(port, "/api/ast?path=copy.py").is_some());
    assert!(
        web_get(port, "/api/ast?path=a.py").is_none(),
        "single-file mode must not expose sibling ASTs"
    );
    let revision = web_get(port, "/api/status").unwrap()["revision"]
        .as_u64()
        .unwrap();
    std::fs::write(&selected, "def unique(value): return value\n").unwrap();
    let started = Instant::now();
    while web_get(port, "/api/status").unwrap()["revision"]
        .as_u64()
        .unwrap()
        <= revision
        || web_get(port, "/api/architecture").unwrap()["warnings"] != 0
    {
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "single-file watch did not refresh"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(web_get(port, "/api/architecture").unwrap()["warnings"], 0);
    assert_eq!(web_get(port, "/api/tree").unwrap()["summary"]["files"], 1);
    std::fs::write(&selected, duplicate_python_source()).unwrap();
    let baseline = Command::new(env!("CARGO_BIN_EXE_codefactor"))
        .args(["baseline", "create", "--root", selected.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        baseline.status.success(),
        "{}",
        String::from_utf8_lossy(&baseline.stderr)
    );
    assert!(fixture.0.join(".analyzer-baseline.json").is_file());
    let prefix = fixture.0.join("filtered");
    let scan = Command::new(env!("CARGO_BIN_EXE_codefactor"))
        .args([
            "scan",
            selected.to_str().unwrap(),
            "--format",
            "json",
            "--out",
            prefix.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        scan.status.success(),
        "{}",
        String::from_utf8_lossy(&scan.stderr)
    );
    let report: Value =
        serde_json::from_slice(&std::fs::read(prefix.with_extension("json")).unwrap()).unwrap();
    assert_eq!(report["summary"]["files"], 1);
    assert_eq!(report["summary"]["architecture_warnings"], 0);
}
#[test]
fn second_review_sarif_encodes_reserved_and_unicode_path_bytes() {
    let fixture = Fixture::new();
    let selected = fixture.0.join("copy#v2?% café.py");
    std::fs::write(&selected, duplicate_python_source()).unwrap();
    let prefix = fixture.0.join("uris");
    let scan = Command::new(env!("CARGO_BIN_EXE_codefactor"))
        .args([
            "scan",
            selected.to_str().unwrap(),
            "--format",
            "sarif",
            "--out",
            prefix.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        scan.status.success(),
        "{}",
        String::from_utf8_lossy(&scan.stderr)
    );
    let report: Value =
        serde_json::from_slice(&std::fs::read(prefix.with_extension("sarif")).unwrap()).unwrap();
    let result = report["runs"][0]["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["ruleId"] == "arch_dry")
        .unwrap();
    let expected = "copy%23v2%3F%25%20caf%C3%A9.py";
    assert_eq!(
        result["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
        expected
    );
    for related in result["relatedLocations"].as_array().unwrap() {
        assert_eq!(
            related["physicalLocation"]["artifactLocation"]["uri"],
            expected
        );
    }
}
#[test]
fn review_related_files_include_every_scc_member_despite_limited_displayed_evidence() {
    let f = Fixture::new();
    std::fs::write(
        f.0.join("a.py"),
        "from b import ping_b\nfrom c import ping_c\ndef ping_a(): ping_b(); ping_c()\n",
    )
    .unwrap();
    std::fs::write(
        f.0.join("b.py"),
        "from a import ping_a\ndef ping_b(): ping_a()\n",
    )
    .unwrap();
    std::fs::write(
        f.0.join("c.py"),
        "from a import ping_a\ndef ping_c(): ping_a()\n",
    )
    .unwrap();
    std::fs::write(
        f.0.join("analyzer.yaml"),
        "architecture: {max_evidence: 1}\n",
    )
    .unwrap();
    let (_server, port) = architecture_test_server(&f);
    let all = web_get(port, "/api/violations?kind=architecture").unwrap();
    assert_eq!(all["total"], 1);
    let diagnostic = &all["violations"][0]["architecture"];
    assert_eq!(diagnostic["evidence"].as_array().unwrap().len(), 1);
    assert_eq!(diagnostic["measurements"]["modules"], 3.0);
    assert!(diagnostic.get("affected_locations").is_none());
    for file in ["a.py", "b.py", "c.py"] {
        let selected = web_get(
            port,
            &format!("/api/violations?kind=architecture&file={file}"),
        )
        .unwrap();
        assert_eq!(selected["total"], 1, "{file}");
        let selected =
            web_get(port, &format!("/api/violations?kind=architecture&q={file}")).unwrap();
        assert_eq!(selected["total"], 1, "search {file}");
        let source = web_get(port, &format!("/api/file?id={file}")).unwrap();
        assert_eq!(source["violation_count"], 1, "{file}");
        assert_eq!(
            source["violation_lines"][0],
            if file == "a.py" { 3 } else { 2 },
            "{file}"
        );
    }
}
#[test]
fn review_related_files_include_clone_group_members_beyond_the_default_evidence_cap() {
    let f = Fixture::new();
    std::fs::remove_file(f.0.join("a.py")).unwrap();
    std::fs::remove_file(f.0.join("b.py")).unwrap();
    let body = (0..15)
        .map(|i| format!("    value = (value + {i}) * 2\n"))
        .collect::<String>();
    let source = format!("def work(value):\n{body}    return value\n");
    for i in 0..61 {
        std::fs::write(f.0.join(format!("copy{i:02}.py")), &source).unwrap();
    }
    let (_server, port) = architecture_test_server(&f);
    let all = web_get(port, "/api/violations?kind=architecture&rule=arch_dry").unwrap();
    assert_eq!(all["total"], 1);
    let diagnostic = &all["violations"][0]["architecture"];
    assert_eq!(diagnostic["evidence_total"], 61);
    assert_eq!(diagnostic["evidence"].as_array().unwrap().len(), 50);
    assert!(!diagnostic["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["range"]["file"] == "copy60.py"));
    assert!(diagnostic.get("affected_locations").is_none());
    let selected = web_get(port, "/api/violations?kind=architecture&file=copy60.py").unwrap();
    assert_eq!(selected["total"], 1);
    let selected = web_get(port, "/api/violations?kind=architecture&q=copy60.py").unwrap();
    assert_eq!(selected["total"], 1);
    let source = web_get(port, "/api/file?id=copy60.py").unwrap();
    assert_eq!(source["violation_count"], 1);
    assert!(!source["violation_lines"].as_array().unwrap().is_empty());
}
#[test]
fn web_filters_related_files_and_watch_refresh_share_architecture_results() {
    let f = Fixture::new();
    let port = TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let _server = Server(
        Command::new(env!("CARGO_BIN_EXE_codefactor"))
            .args([
                "serve",
                f.0.to_str().unwrap(),
                "--port",
                &port.to_string(),
                "--watch",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let started = Instant::now();
    while web_get(port, "/api/status").is_none() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "server did not start"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(web_get(port, "/api/architecture").unwrap()["warnings"], 1);
    assert_eq!(
        web_get(port, "/api/violations?kind=metrics").unwrap()["total"],
        0
    );
    assert_eq!(
        web_get(port, "/api/violations?kind=metrics").unwrap()["summary"]["warnings"],
        0
    );
    let issues = web_get(
        port,
        "/api/violations?kind=architecture&principle=ADP&file=b.py&per_page=1",
    )
    .unwrap();
    assert_eq!(issues["total"], 1);
    assert_eq!(issues["violations"][0]["architecture"]["evidence_total"], 2);
    assert_eq!(
        web_get(port, "/api/violations?kind=architecture&q=ADP").unwrap()["total"],
        1
    );
    assert_eq!(
        web_get(port, "/api/violations?kind=architecture&principle=DRY").unwrap()["total"],
        0
    );
    let file = web_get(port, "/api/file?id=b.py").unwrap();
    assert_eq!(file["violation_count"], 1);
    assert_eq!(file["violation_lines"][0], 2);
    let revision = web_get(port, "/api/status").unwrap()["revision"]
        .as_u64()
        .unwrap();
    std::fs::write(f.0.join("b.py"), "class B:\n    pass\n").unwrap();
    let started = Instant::now();
    loop {
        let status = web_get(port, "/api/status").unwrap();
        if status["revision"].as_u64().unwrap() > revision
            && web_get(port, "/api/architecture").unwrap()["warnings"] == 0
        {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "watch did not refresh"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(web_get(port, "/api/architecture").unwrap()["warnings"], 0);
    assert_eq!(
        web_get(port, "/api/tree").unwrap()["summary"]["architecture_warnings"],
        0
    );
}
