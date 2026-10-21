use analyzer_core::{
    graph::CallGraph,
    metrics::{advanced_metrics, MetricValue},
    Config, FileModel, LanguageRegistry,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

fn registry() -> LanguageRegistry {
    let mut registry = LanguageRegistry::new();
    registry.register(analyzer_php::Php);
    registry.register(analyzer_python::Python);
    registry.register(analyzer_javascript::JavaScript);
    registry.register(analyzer_typescript::TypeScript);
    registry.register(analyzer_rust::Rust);
    registry
}

fn analyze(sources: &[(&str, &str)]) -> (Vec<FileModel>, Vec<MetricValue>) {
    let (files, metrics) = analyze_recovering(sources);
    for file in &files {
        assert!(file.parse_errors.is_empty(), "{:?}", file.parse_errors);
    }
    (files, metrics)
}

/// Like `analyze`, for valid code that the grammar only parses with error recovery.
fn analyze_recovering(sources: &[(&str, &str)]) -> (Vec<FileModel>, Vec<MetricValue>) {
    let registry = registry();
    let files = sources
        .iter()
        .map(|(path, source)| {
            let path = Path::new(path);
            let strategy = registry
                .by_extension(path.extension().unwrap().to_str().unwrap())
                .unwrap();
            strategy.analyze(source, path)
        })
        .collect::<Vec<_>>();
    let graph = CallGraph::build(&files, &registry, &Config::default());
    let sources = sources
        .iter()
        .map(|(path, source)| (path.to_string(), source.to_string()))
        .collect::<BTreeMap<_, _>>();
    let metrics = advanced_metrics(&files, &graph, &sources, &Default::default());
    (files, metrics)
}

fn metric(metrics: &[MetricValue], path: &str, id: &str) -> f64 {
    metrics
        .iter()
        .find(|metric| metric.path.as_ref() == path && metric.id.as_ref() == id)
        .unwrap()
        .value
}

#[test]
fn source_operators_private_members_and_lifetimes_do_not_hide_work_from_scores() {
    for (path, small, large) in [
        (
            "work.py",
            "def work(x):\n return x // 2\n",
            "def work(x):\n return x // 2 + x * 3 + x * 4\n",
        ),
        (
            "work.js",
            "class Value { #field=0; work(x){return this.#field + x;} }",
            "class Value { #field=0; work(x){return this.#field + x * 3 + x * 4;} }",
        ),
        (
            "work.ts",
            "class Value { #field=0; work(x:number){return this.#field + x;} }",
            "class Value { #field=0; work(x:number){return this.#field + x * 3 + x * 4;} }",
        ),
        (
            "work.rs",
            "fn work(x: &'static i32) -> i32 { *x }",
            "fn work(x: &'static i32) -> i32 { *x + *x * 3 + *x * 4 }",
        ),
    ] {
        let (_, small) = analyze(&[(path, small)]);
        let (_, large) = analyze(&[(path, large)]);
        assert!(
            metric(&large, path, "halstead_volume") > metric(&small, path, "halstead_volume"),
            "{path}: executable work must increase token volume"
        );
        assert!(
            metric(&large, path, "maintainability_index")
                < metric(&small, path, "maintainability_index"),
            "{path}: maintainability must reflect the additional work"
        );
    }
}

#[test]
fn duplicate_detection_keeps_python_floor_division_operands() {
    let (_, metrics) = analyze(&[(
        "work.py",
        "def first(x):\n a=x+1; b=a+2; c=b+3; d=c+4; e=d+5; return e // 2\n\
         def second(x):\n a=x+1; b=a+2; c=b+3; d=c+4; e=d+5; return e // 7\n",
    )]);
    assert!(
        !metrics
            .iter()
            .any(|metric| metric.id.as_ref() == "duplicate_functions"),
        "different divisors change executable behavior"
    );
}

#[test]
fn module_cycle_scores_do_not_join_independent_language_graphs() {
    let (_, metrics) = analyze(&[
        ("a.py", "from b import work\ndef run():\n work()\n"),
        ("b.py", "def work():\n pass\n"),
        ("a.js", "export function work() {}"),
        (
            "b.js",
            "import {work} from './a.js'; export function run() {work();}",
        ),
    ]);
    for path in ["a.py", "b.py", "a.js", "b.js"] {
        assert_eq!(metric(&metrics, path, "module_cycle"), 0.0, "{path}");
    }
}

#[test]
fn real_javascript_typescript_interop_cycles_keep_distinct_module_identities() {
    for sources in [
        vec![
            (
                "a.js",
                "import {right} from './b.ts'; export function left(){right();}",
            ),
            (
                "b.ts",
                "import {left} from './a.js'; export function right(){left();}",
            ),
        ],
        // Equal stems are still two physical modules linked by actual imports.
        vec![
            (
                "a.js",
                "import {right} from './a.ts'; export function left(){right();}",
            ),
            (
                "a.ts",
                "import {left} from './a.js'; export function right(){left();}",
            ),
        ],
    ] {
        let (_, metrics) = analyze(&sources);
        for (path, _) in &sources {
            assert_eq!(metric(&metrics, path, "module_cycle"), 1.0, "{path}");
        }
    }
}

#[test]
fn real_module_cycle_scores_are_preserved_in_every_language() {
    for sources in [
        vec![
            ("a.php", "<?php namespace A; function left() { \\B\\right(); }"),
            ("b.php", "<?php namespace B; function right() { \\A\\left(); }"),
        ],
        vec![
            (
                "a.php",
                "<?php namespace A; use B\\Right; class Left {function run(Right $b){$b->run($this);}}",
            ),
            (
                "b.php",
                "<?php namespace B; use A\\Left; class Right {function run(Left $a){$a->run($this);}}",
            ),
        ],
        vec![
            ("a.py", "from b import right\ndef left():\n right()\n"),
            ("b.py", "from a import left\ndef right():\n left()\n"),
        ],
        vec![
            (
                "a.js",
                "import {right} from './b'; export function left(){right();}",
            ),
            (
                "b.js",
                "import {left} from './a'; export function right(){left();}",
            ),
        ],
        vec![
            (
                "a.ts",
                "import {right} from './b'; export function left(){right();}",
            ),
            (
                "b.ts",
                "import {left} from './a'; export function right(){left();}",
            ),
        ],
        vec![
            ("lib.rs", "pub mod a; pub mod b;"),
            ("a.rs", "pub fn left(){crate::b::right();}"),
            ("b.rs", "pub fn right(){crate::a::left();}"),
        ],
    ] {
        let (_, metrics) = analyze(&sources);
        for (path, _) in sources.iter().filter(|(path, _)| *path != "lib.rs") {
            assert_eq!(metric(&metrics, path, "module_cycle"), 1.0, "{path}");
        }
    }
}
/// `module_cycle` reuses the `arch_cycle` graph: a property, constant or variable
/// read closes a cycle like a call does, while an unresolved interface dispatch
/// does not, so both report exactly the same modules.
#[test]
fn module_cycle_matches_arch_cycle_participants_in_every_language() {
    let registry = registry();
    for (expected, sources) in [
        (3, vec![
            ("app/a.py", "from app.b import step_b\nLIMIT = 3\ndef step_a(n):\n return step_b(n)\n"),
            ("app/b.py", "from app.c import step_c\ndef step_b(n):\n return step_c(n)\n"),
            ("app/c.py", "from app import a\ndef step_c(n):\n return n + a.LIMIT\n"),
        ]),
        (2, vec![
            ("app/a.py", "from app.b import step_b\nLIMIT = 3\ndef step_a(n):\n return step_b(n)\n"),
            ("app/b.py", "from app import a\ndef step_b(n):\n return min(n, a.LIMIT)\n"),
        ]),
        (3, vec![
            ("src/a.ts", "import {stepB} from './b'; export const LIMIT = 3; export function stepA(n: number) { return stepB(n); }"),
            ("src/b.ts", "import {stepC} from './c'; export function stepB(n: number) { return stepC(n); }"),
            ("src/c.ts", "import {LIMIT} from './a'; export function stepC(n: number) { return Math.min(n, LIMIT); }"),
        ]),
        (3, vec![
            ("src/a.js", "import {stepB} from './b.js'; export const LIMIT = 3; export function stepA(n) { return stepB(n); }"),
            ("src/b.js", "import {stepC} from './c.js'; export function stepB(n) { return stepC(n); }"),
            ("src/c.js", "import {LIMIT} from './a.js'; export function stepC(n) { return Math.min(n, LIMIT); }"),
        ]),
        (3, vec![
            ("src/A.php", "<?php namespace App\\A; use App\\B\\B; class A { public const LIMIT = 3; public function step(B $b, int $n): int { return $b->step($n); } }"),
            ("src/B.php", "<?php namespace App\\B; use App\\C\\C; class B { public function __construct(private C $c) {} public function step(int $n): int { return $this->c->step($n); } }"),
            ("src/C.php", "<?php namespace App\\C; use App\\A\\A; class C { public function step(int $n): int { return min($n, A::LIMIT); } }"),
        ]),
        // A stub in an `architecture.exclude` path must not make `A::LIMIT`
        // ambiguous for the metric only: both resolve over the same files.
        (3, vec![
            ("src/A.php", "<?php namespace App\\A; use App\\B\\B; class A { public const LIMIT = 3; public function step(B $b, int $n): int { return $b->step($n); } }"),
            ("src/B.php", "<?php namespace App\\B; use App\\C\\C; class B { public function __construct(private C $c) {} public function step(int $n): int { return $this->c->step($n); } }"),
            ("src/C.php", "<?php namespace App\\C; use App\\A\\A; class C { public function step(int $n): int { return min($n, A::LIMIT); } }"),
            ("tests/fixtures/A.php", "<?php namespace App\\A; class A { public const LIMIT = 1; }"),
        ]),
        (3, vec![
            ("src/lib.rs", "pub mod a; pub mod b; pub mod c;"),
            ("src/a.rs", "pub const LIMIT: u32 = 3; pub fn step_a(n: u32) -> u32 { crate::b::step_b(n) }"),
            ("src/b.rs", "pub fn step_b(n: u32) -> u32 { crate::c::step_c(n) }"),
            ("src/c.rs", "use crate::a::LIMIT; pub fn step_c(n: u32) -> u32 { n.min(LIMIT) }"),
        ]),
        (0, vec![
            ("src/A.php", "<?php namespace App\\A; use App\\B\\Port; class A { public function run(Port $p): int { return $p->go(); } public function back(): int { return 1; } }"),
            ("src/B.php", "<?php namespace App\\B; use App\\A\\A; interface Port { public function go(): int; } class Impl implements Port { public function __construct(private A $a) {} public function go(): int { return $this->a->back(); } }"),
        ]),
    ] {
        let files = sources
            .iter()
            .map(|(path, source)| {
                let path = Path::new(path);
                registry
                    .by_extension(path.extension().unwrap().to_str().unwrap())
                    .unwrap()
                    .analyze_with_architecture(source, path, true)
            })
            .collect::<Vec<_>>();
        let graph = CallGraph::build(&files, &registry, &Config::default());
        let texts = sources
            .iter()
            .map(|(path, source)| (path.to_string(), source.to_string()))
            .collect::<BTreeMap<_, _>>();
        let architecture = analyzer_core::architecture::ArchitectureConfig::default();
        let metrics = advanced_metrics(&files, &graph, &texts, &architecture);
        let (_, issues) = analyzer_core::architecture::analyze(&files, &graph, &architecture);
        let arch = issues
            .iter()
            .filter(|v| v.rule == "arch_cycle")
            .flat_map(|v| v.architecture.as_ref().unwrap().participants.clone())
            .collect::<BTreeSet<_>>();
        let flagged = files
            .iter()
            .filter(|f| metric(&metrics, &f.path, "module_cycle") == 1.0)
            .map(|f| format!("{}:{}", f.language, f.module))
            .collect::<BTreeSet<_>>();
        assert_eq!(arch.len(), expected, "{}: {arch:?}", sources[0].0);
        assert_eq!(flagged, arch, "{}", sources[0].0);
        for (path, _) in sources.iter().filter(|(path, _)| path.starts_with("tests/")) {
            assert_eq!(metric(&metrics, path, "module_cycle"), 0.0, "{path}");
        }
    }
}

#[test]
fn php_absolute_function_names_resolve_without_namespace_or_variable_shadowing() {
    let (files, _) = analyze(&[
        ("library.php", "<?php namespace Library; function work() {}"),
        ("global.php", "<?php function work() {}"),
        (
            "caller.php",
            r#"<?php
namespace App;
function work() {}
function invoke($work) {
    \Library\work();
    \work();
    \Missing\work();
    $work();
}
function bare() { work(); }
"#,
        ),
    ]);
    let graph = CallGraph::build(&files, &registry(), &Config::default());
    for (line, target) in [(5, "Library\\work"), (6, "work"), (10, "App\\work")] {
        let edges = graph
            .edges
            .iter()
            .filter(|edge| {
                edge.callsite.file.as_ref() == "caller.php" && edge.callsite.start_line == line
            })
            .collect::<Vec<_>>();
        assert_eq!(edges.len(), 1, "line {line}: {edges:?}");
        let edge = edges[0];
        assert_eq!(
            edge.confidence,
            analyzer_core::Confidence::Resolved,
            "line {line}: {edge:?}"
        );
        assert_eq!(graph.node(&edge.to).unwrap().label, target);
    }
    for line in [7, 8] {
        let edge = graph
            .edges
            .iter()
            .find(|edge| {
                edge.callsite.file.as_ref() == "caller.php" && edge.callsite.start_line == line
            })
            .unwrap();
        assert_eq!(
            edge.confidence,
            analyzer_core::Confidence::Dynamic,
            "line {line}: {edge:?}"
        );
        assert_eq!(
            graph.node(&edge.to).unwrap().kind,
            analyzer_core::graph::NodeKind::Unknown
        );
    }
}

#[test]
fn python_field_and_cohesion_scores_use_the_actual_implicit_receiver() {
    let (files, metrics) = analyze(&[(
        "receivers.py",
        r#"class Conventional:
 def __init__(self, value: int):
  self.value = value
 def first(self): return self.value
 def second(self): return self.value
class Custom:
 def __init__(me, value: int):
  me.value = value
  def local(me): me.hidden = 0
  me.nested.member = 1
 def first(me): return me.value
 def second(me): return me.value
class Static:
 @staticmethod
 def first(self): return self.value
 @staticmethod
 def second(self): return self.value
"#,
    )]);
    for class in ["Conventional", "Custom"] {
        let ty = files[0].types.iter().find(|ty| ty.name == class).unwrap();
        assert_eq!(ty.fields.len(), 1, "{class}: {:?}", ty.fields);
        assert_eq!(ty.fields[0].name, "value");
        assert_eq!(ty.fields[0].type_name.as_deref(), Some("int"));
        assert_eq!(
            metrics
                .iter()
                .find(|metric| metric.entity.as_ref() == ty.fqn && metric.id.as_ref() == "lcom4")
                .unwrap()
                .value,
            1.0,
            "{class}"
        );
    }
    let static_lcom = metrics
        .iter()
        .find(|metric| {
            metric.entity.as_ref() == "receivers.Static" && metric.id.as_ref() == "lcom4"
        })
        .unwrap();
    assert_eq!(
        static_lcom.value, 2.0,
        "a parameter named self is not an implicit receiver in a static method"
    );
}

#[test]
fn constructor_promoted_properties_join_cohesion_like_assigned_fields() {
    let (files, metrics) = analyze(&[
        (
            "promoted.php",
            r#"<?php
class Promoted {
    public function __construct(private readonly Repo $repo, private Logger $log,) {}
    public function a() { return $this->repo; }
    public function b() { return $this->repo?->x(); }
}
class Classic {
    private Repo $repo;
    private Logger $log;
    public function __construct(Repo $repo, Logger $log) { $this->repo = $repo; $this->log = $log; }
    public function a() { return $this->repo; }
    public function b() { return $this->repo?->x(); }
}
class MixedBody {
    public function __construct(private Repo $repo, private Logger $log) { $this->repo->warm(); }
    public function a() { return $this->repo; }
    public function b() { return $this->repo; }
}
class Plain {
    public function __construct(Repo $repo, Logger $log) {}
    public function a() { return $this->repo; }
    public function b() { return $this->repo; }
}
"#,
        ),
        (
            "promoted.ts",
            r#"declare function Inject(token: string): ParameterDecorator;
class Repo { find() { return 1; } }
export class Promoted {
  constructor(private readonly repo: Repo, public log?: string) {}
  a() { return this.repo; }
  b() { return this.repo.find(); }
}
export class Decorated {
  constructor(@Inject('R') private readonly repo: Repo, @Inject('L') protected log: string) {}
  a() { return this.repo; }
  b() { return this.repo.find(); }
}
class Base { constructor(readonly repo: Repo, readonly log: string) {} }
export class Child extends Base {
  constructor(override readonly repo: Repo, override readonly log: string) { super(repo, log); }
  a() { return this.repo; }
  b() { return this.repo.find(); }
}
export class Classic {
  private repo: Repo;
  private log: string;
  constructor(repo: Repo, log: string) { this.repo = repo; this.log = log; }
  a() { return this.repo; }
  b() { return this.repo.find(); }
}
export class Plain {
  constructor(repo: Repo, log: string) {}
  a() { return (this as any).repo; }
  b() { return (this as any).repo; }
}
"#,
        ),
        (
            "promoted.js",
            "class Plain { constructor(repo) {} a() { return this.repo } b() { return this.repo } }",
        ),
    ]);
    let lcom4 = |fqn: &str| {
        metrics
            .iter()
            .find(|metric| metric.entity.as_ref() == fqn && metric.id.as_ref() == "lcom4")
            .unwrap_or_else(|| panic!("{fqn}"))
            .value
    };
    for file in &files[..2] {
        let constructor = if file.path.ends_with(".php") {
            "__construct"
        } else {
            "constructor"
        };
        for ty in &file.types {
            if !["Promoted", "Decorated", "Child", "Classic", "MixedBody"]
                .contains(&ty.name.as_str())
            {
                continue;
            }
            assert_eq!(lcom4(&ty.fqn), 1.0, "{}", ty.fqn);
            let mut names = ty
                .fields
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>();
            names.sort();
            assert_eq!(names, ["log", "repo"], "{}", ty.fqn);
            let init = file
                .functions
                .iter()
                .find(|f| {
                    f.parent_type.as_deref() == Some(ty.fqn.as_str()) && f.name == constructor
                })
                .unwrap();
            assert_eq!(init.accessed_fields, ["log", "repo"], "{}", ty.fqn);
        }
    }
    let php = &files[0];
    let promoted = php.types.iter().find(|ty| ty.name == "Promoted").unwrap();
    assert!(
        promoted
            .fields
            .iter()
            .all(|f| f.visibility.as_deref() == Some("private")),
        "{:?}",
        promoted.fields
    );
    for file in &files {
        let plain = file.types.iter().find(|ty| ty.name == "Plain").unwrap();
        assert!(plain.fields.is_empty(), "{:?}", plain.fields);
        assert_eq!(lcom4(&plain.fqn), 2.0, "{}", plain.fqn);
    }
}

#[test]
fn php_readonly_promotion_without_visibility_is_a_public_field() {
    // Valid since PHP 8.1, but tree-sitter-php has no node for `readonly Foo $x`
    // without a visibility and recovers it as a plain parameter with ERROR nodes.
    let (files, metrics) = analyze_recovering(&[(
        "readonly.php",
        r#"<?php
class ReadonlyOnly {
    public function __construct(
        readonly Repo $repo,
        #[\SensitiveParameter] readonly ?Logger $log,
        READONLY \App\Clock &$clock,
    ) {}
    public function a() { return $this->repo; }
    public function b() { return $this->repo?->x(); }
}
"#,
    )]);
    let ty = &files[0].types[0];
    let fields = ty
        .fields
        .iter()
        .map(|f| {
            (
                f.name.as_str(),
                f.type_name.as_deref(),
                f.visibility.as_deref(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        fields,
        [
            ("repo", Some("Repo"), Some("public")),
            ("log", Some("Logger"), Some("public")),
            ("clock", Some("App\\Clock"), Some("public")),
        ]
    );
    let init = files[0]
        .functions
        .iter()
        .find(|f| f.name == "__construct")
        .unwrap();
    assert_eq!(init.accessed_fields, ["clock", "log", "repo"]);
    let lcom4 = metrics
        .iter()
        .find(|metric| metric.entity.as_ref() == ty.fqn && metric.id.as_ref() == "lcom4")
        .unwrap();
    assert_eq!(lcom4.value, 1.0);
}
