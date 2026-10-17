use analyzer_core::{
    algorithmic::{self, ComplexityEstimate, EstimateStatus},
    graph::CallGraph,
    Config, FileModel, LanguageRegistry, LanguageStrategy,
};
use std::path::Path;

fn analyze(source: &str) -> (FileModel, CallGraph) {
    let mut files = vec![analyzer_rust::Rust.analyze(source, Path::new("cost.rs"))];
    assert!(
        files[0].parse_errors.is_empty(),
        "{:?}",
        files[0].parse_errors
    );
    let mut registry = LanguageRegistry::default();
    registry.register(analyzer_rust::Rust);
    let config = Config::default();
    let mut graph = CallGraph::build(&files, &registry, &config);
    algorithmic::analyze(&mut files, &mut graph, &config);
    (files.remove(0), graph)
}
fn estimate<'a>(file: &'a FileModel, name: &str) -> &'a ComplexityEstimate {
    file.functions
        .iter()
        .find(|f| f.name == name)
        .unwrap()
        .algorithmic_complexity
        .as_ref()
        .unwrap()
}
fn expect(file: &FileModel, name: &str, status: EstimateStatus, formula: &str) {
    let e = estimate(file, name);
    assert_eq!(
        e.status,
        status,
        "{name}: {} {:?}",
        e.formula.as_deref().unwrap_or("unknown"),
        e.unknown_reasons
    );
    assert_eq!(e.formula.as_deref(), Some(formula), "{name}");
}
#[test]
fn basic_growth_and_independent_sizes() {
    let (file, graph) = analyze(
        r#"
fn constant(x:usize)->usize{x.wrapping_add(1)}
fn fixed(){for i in 0..100 {let x=i;}}
fn linear(n:usize){for i in 0..n {let x=i;}}
fn wildcard(n:usize){for _ in 0..n {}}
fn sequential(n:usize,m:usize){for i in 0..n {} for j in 0..m {}}
fn nested(n:usize,m:usize){for i in 0..n {for j in 0..m {}}}
fn triangular(n:usize){for i in 0..n {for j in 0..i {}}}
fn slice(items:&[usize]){for item in items.iter(){let x=item;}}
fn logarithmic(n:usize){let mut i=n;while i>0 {i/=2;}}
"#,
    );
    expect(&file, "constant", EstimateStatus::Tight, "Θ(1)");
    expect(&file, "fixed", EstimateStatus::Tight, "Θ(1)");
    expect(&file, "linear", EstimateStatus::Tight, "Θ(n)");
    expect(&file, "wildcard", EstimateStatus::Tight, "Θ(n)");
    expect(&file, "sequential", EstimateStatus::Tight, "Θ(m + n)");
    expect(&file, "nested", EstimateStatus::Tight, "Θ(m·n)");
    expect(&file, "triangular", EstimateStatus::Tight, "Θ(n^2)");
    expect(&file, "slice", EstimateStatus::Tight, "Θ(items.len())");
    expect(&file, "logarithmic", EstimateStatus::Tight, "Θ(log(n))");
    assert!(graph
        .nodes
        .iter()
        .filter(|n| n.kind == analyzer_core::graph::NodeKind::Function)
        .all(|n| n.algorithmic_complexity.is_some()));
    for function in &file.functions {
        let node = graph
            .nodes
            .iter()
            .find(|n| n.label == function.fqn)
            .unwrap();
        assert!(std::sync::Arc::ptr_eq(
            function.algorithmic_complexity.as_ref().unwrap(),
            node.algorithmic_complexity.as_ref().unwrap()
        ));
    }
}
#[test]
fn calls_library_bounds_and_unknowns_are_distinct() {
    let (file, _) = analyze(
        r#"
fn linear(n:usize){for i in 0..n {}}
fn quadratic(n:usize){for i in 0..n {linear(n);}}
fn sort(items:&mut [usize]){items.sort_unstable();}
fn search(items:&[usize],x:usize){items.binary_search(&x);}
fn early(n:usize){for i in 0..n {if i>10 {break;}}}
fn unresolved(n:usize){for i in 0..n {foreign(n);}}
fn recursive(n:usize){recursive(n);}
fn mutual_a(n:usize){mutual_b(n);}
fn mutual_b(n:usize){mutual_a(n);}
fn macro_call(){println!("hello");}
fn opaque<T>(items:&[T]){items.contains(&items[0]);}
"#,
    );
    expect(&file, "quadratic", EstimateStatus::Tight, "Θ(n^2)");
    expect(
        &file,
        "sort",
        EstimateStatus::UpperBound,
        "O(items.len()·log(items.len()))",
    );
    expect(
        &file,
        "search",
        EstimateStatus::UpperBound,
        "O(log(items.len()))",
    );
    expect(&file, "early", EstimateStatus::UpperBound, "O(n)");
    for name in [
        "unresolved",
        "recursive",
        "mutual_a",
        "mutual_b",
        "macro_call",
        "opaque",
    ] {
        assert_eq!(
            estimate(&file, name).status,
            EstimateStatus::Unknown,
            "{name}"
        );
        assert!(estimate(&file, name).formula.is_none());
        assert!(!estimate(&file, name).unknown_reasons.is_empty());
    }
}

#[test]
fn counters_aliases_slices_and_side_effects_keep_their_guarantees() {
    let (file, _) = analyze(
        r#"
fn linear(n:usize){for i in 0..n {}}
fn slice_scan(items:&[usize]){for item in items.iter(){}}
fn fixed_array(items:&[usize;10]){for item in items.iter(){}}
fn offset(n:usize){for i in 3..n {}}
fn stepped(n:usize){for i in (0..n).step_by(2) {}}
fn bound_alias(items:&[usize]){let n=items.len();for i in 0..n {}}
fn full_slice(items:&[usize]){slice_scan(&items[..]);}
fn partial_slice(items:&[usize],n:usize){slice_scan(&items[..n]);}
fn derived_partial(items:&[usize],n:usize){let len=items[..n].len()+1;let mut i=0;while i<len {i+=1;}}
fn inclusive_slice(items:&[usize]){slice_scan(&items[..=0]);}
fn counter_calls(n:usize){let mut i=0;while i<n {linear(i);i+=1;}}
fn shrinking_calls(n:usize){let mut i=n;while i>0 {linear(i);i/=2;}}
fn geometric(n:usize){let mut i=1;while i<n {i*=2;}}
fn changing_geometric(mut n:usize){let mut i=1;while i<n {n=0;i*=2;}}
fn excessive_shift(n:u8){let mut i=n;while i>0 {i>>=8;}}
fn mutation(mut n:usize){let mut i=0;while i<n {n=0;i+=1;}}
fn conditional_step(n:usize){let mut i=0;while i<n {if i>1 {i+=1;}}}
fn zero_loop(){for i in 0..0 {unknown();}}
fn set_zero(n:&mut usize){*n=0;}
fn borrowed_counter(mut n:usize){let mut i=0;while i<n {set_zero(&mut n);i+=1;}}
fn two(a:usize,b:usize){linear(b);}
fn argument_order(mut n:usize){two({n=0;1},n);}
"#,
    );
    expect(&file, "fixed_array", EstimateStatus::Tight, "Θ(1)");
    expect(&file, "offset", EstimateStatus::Tight, "Θ(n)");
    expect(&file, "stepped", EstimateStatus::Tight, "Θ(n)");
    expect(
        &file,
        "bound_alias",
        EstimateStatus::Tight,
        "Θ(items.len())",
    );
    expect(&file, "full_slice", EstimateStatus::Tight, "Θ(items.len())");
    expect(&file, "partial_slice", EstimateStatus::UpperBound, "O(n)");
    expect(&file, "derived_partial", EstimateStatus::UpperBound, "O(n)");
    expect(&file, "counter_calls", EstimateStatus::Tight, "Θ(n^2)");
    expect(
        &file,
        "shrinking_calls",
        EstimateStatus::UpperBound,
        "O(n·log(n))",
    );
    expect(&file, "geometric", EstimateStatus::Heuristic, "O(log(n))");
    expect(&file, "zero_loop", EstimateStatus::Tight, "Θ(1)");
    for name in [
        "mutation",
        "inclusive_slice", // Bundled grammar parses this prefix range as an assignment.
        "changing_geometric",
        "excessive_shift",
        "conditional_step",
        "borrowed_counter",
        "argument_order",
    ] {
        assert_eq!(
            estimate(&file, name).status,
            EstimateStatus::Unknown,
            "{name}"
        );
    }
    let mut files = vec![file];
    let mut config = Config::default();
    config.algorithmic_complexity.heuristics = false;
    let mut registry = LanguageRegistry::default();
    registry.register(analyzer_rust::Rust);
    let mut graph = CallGraph::build(&files, &registry, &config);
    algorithmic::analyze(&mut files, &mut graph, &config);
    assert_eq!(
        estimate(&files[0], "geometric").status,
        EstimateStatus::Unknown
    );
}

#[test]
fn shadowed_types_and_custom_comparators_are_never_certified() {
    for source in [
        "struct Vec<T>(T);fn f(items:Vec<usize>){for i in items.iter(){}}",
        "type usize=Custom;fn f(n:usize){for i in 0..n {}}",
        "use custom::String;fn f(s:String){for i in 0..s.len() {}}",
        "fn f(items:&mut [Custom]){items.sort_unstable();}",
        "fn f(items:Vec<usize>){}",
        "fn f(text:String){}",
    ] {
        let (file, _) = analyze(source);
        assert_eq!(
            estimate(&file, "f").status,
            EstimateStatus::Unknown,
            "{source}"
        );
    }
}

#[test]
fn disabled_callees_and_unsupported_languages_do_not_pass_as_constant_cost() {
    use analyzer_core::algorithmic::{AlgorithmicOverride, UnknownPolicy};
    let (file, _) = analyze("fn callee(n:usize){for i in 0..n {}} fn caller(n:usize){callee(n);}");
    let mut files = vec![file];
    let mut registry = LanguageRegistry::default();
    registry.register(analyzer_rust::Rust);
    let mut config = Config::default();
    config.algorithmic_complexity.unknown = UnknownPolicy::Error;
    config.algorithmic_complexity.by_entity.insert(
        "cost::callee".into(),
        AlgorithmicOverride {
            enabled: Some(false),
            ..AlgorithmicOverride::default()
        },
    );
    let mut graph = CallGraph::build(&files, &registry, &config);
    algorithmic::analyze(&mut files, &mut graph, &config);
    assert_eq!(
        estimate(&files[0], "callee").status,
        EstimateStatus::Disabled
    );
    assert_eq!(
        estimate(&files[0], "caller").status,
        EstimateStatus::Unknown
    );
    let diagnostics = algorithmic::violations(&files, &config);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].rule, "algorithmic_time_unknown");
    config.algorithmic_complexity.enabled = false;
    algorithmic::analyze(&mut files, &mut graph, &config);
    assert!(files[0]
        .functions
        .iter()
        .all(|f| f.algorithmic_complexity.as_ref().unwrap().status == EstimateStatus::Disabled));
    assert!(algorithmic::violations(&files, &config).is_empty());
    config.algorithmic_complexity.enabled = true;
    config.algorithmic_complexity.by_entity.clear();
    files[0].language = "unrecognized_language".into();
    let mut graph = CallGraph::build(&files, &registry, &config);
    algorithmic::analyze(&mut files, &mut graph, &config);
    assert!(files[0]
        .functions
        .iter()
        .all(|f| f.algorithmic_complexity.as_ref().unwrap().status == EstimateStatus::Unsupported));
    assert!(algorithmic::violations(&files, &config).is_empty());
}

#[test]
fn symbolic_rules_only_error_on_tight_classes_and_honor_suppression() {
    use analyzer_core::{
        algorithmic::{ComplexityLimits, UnknownPolicy},
        rules::{self, Severity},
    };
    use std::collections::{BTreeMap, BTreeSet};
    let source = r#"
fn exact(n:usize){for i in 0..n {for j in 0..n {}}}
fn bound(items:&[usize]){for item in items.iter(){items.contains(item);}}
fn guessed(n:usize){let mut i=1;while i<n {i*=2;for j in 0..n {for k in 0..n {}}}}
fn equal(n:usize){for i in 0..n {}}
fn unknown(){foreign();}
// analyzer-ignore: algorithmic_time
fn ignored(n:usize){for i in 0..n {for j in 0..n {}}}
"#;
    let (file, _) = analyze(source);
    let mut config = Config::default();
    config.algorithmic_complexity.limits = ComplexityLimits {
        warn: Some("N".into()),
        error: Some("N*log(N)".into()),
    };
    let sources = BTreeMap::from([("cost.rs".into(), source.into())]);
    let files = vec![file];
    let values = rules::evaluate(&[], &files, &[], &config, &BTreeSet::new(), &sources);
    let find = |name: &str| {
        values
            .iter()
            .find(|v| v.entity.ends_with(&format!("::{name}")))
            .unwrap()
    };
    assert_eq!(find("exact").severity, Severity::Error);
    assert_eq!(find("bound").severity, Severity::Warn);
    assert_eq!(find("guessed").severity, Severity::Warn);
    assert!(values
        .iter()
        .all(|v| v.value.is_none() && v.limit.is_none()));
    assert!(!values.iter().any(|v| v.entity.ends_with("::equal")
        || v.entity.ends_with("::unknown")
        || v.entity.ends_with("::ignored")));
    let baseline = values.iter().map(|v| v.fingerprint()).collect();
    assert!(rules::evaluate(&[], &files, &[], &config, &baseline, &sources).is_empty());
    config.algorithmic_complexity.unknown = UnknownPolicy::Error;
    assert!(
        rules::evaluate(&[], &files, &[], &config, &baseline, &sources)
            .iter()
            .any(|v| v.rule == "algorithmic_time_unknown" && v.severity == Severity::Error)
    );
}

#[test]
fn cached_callers_recompute_when_callees_change_and_vendor_is_expanded() {
    use analyzer_core::{algorithmic::ComplexityLimits, Analyzer};
    let root = std::env::temp_dir().join(format!(
        "codefactor-cost-scan-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("lib.rs"), "mod caller; mod callee;\n").unwrap();
    std::fs::write(
        root.join("caller.rs"),
        "use crate::callee::work;fn caller(n:usize){for i in 0..n {work(n);}}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("callee.rs"),
        "pub fn work(n:usize){for i in 0..n {}}\n",
    )
    .unwrap();
    let mut registry = LanguageRegistry::default();
    registry.register(analyzer_rust::Rust);
    let mut config = Config::default();
    config.algorithmic_complexity.limits = ComplexityLimits {
        warn: Some("N".into()),
        error: Some("N*log(N)".into()),
    };
    let analyzer = Analyzer::new(registry, config);
    let first = analyzer.scan(&root).unwrap();
    assert_eq!(first.schema_version, "3");
    let caller = first.file("caller.rs").unwrap();
    expect(caller, "caller", EstimateStatus::Tight, "Θ(n^2)");
    let warm = analyzer.scan(&root).unwrap();
    assert_eq!(warm.summary.cache_hits, 3);
    expect(
        warm.file("caller.rs").unwrap(),
        "caller",
        EstimateStatus::Tight,
        "Θ(n^2)",
    );
    std::fs::write(root.join("callee.rs"), "pub fn work(n:usize){}\n").unwrap();
    let changed = analyzer.scan(&root).unwrap();
    assert_eq!(changed.summary.cache_hits, 2);
    expect(
        changed.file("caller.rs").unwrap(),
        "caller",
        EstimateStatus::Tight,
        "Θ(n)",
    );
    assert!(!changed
        .violations
        .iter()
        .any(|v| v.rule == "algorithmic_time"));
    std::fs::remove_file(root.join("callee.rs")).unwrap();
    std::fs::write(
        root.join("caller.rs"),
        "use crate::vendor::acme::work;fn caller(n:usize){for i in 0..n {work(n);}}\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("vendor")).unwrap();
    std::fs::write(
        root.join("lib.rs"),
        "mod caller; mod vendor {pub mod acme;}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("vendor/acme.rs"),
        "pub fn work(n:usize){for i in 0..n {}}\n",
    )
    .unwrap();
    let mut report = analyzer.scan(&root).unwrap();
    assert_eq!(
        estimate(report.file("caller.rs").unwrap(), "caller").status,
        EstimateStatus::Unknown
    );
    let package = report.deferred_vendor.first().unwrap().package.clone();
    analyzer
        .expand_vendor(&root, &mut report, &package)
        .unwrap();
    expect(
        report.file("caller.rs").unwrap(),
        "caller",
        EstimateStatus::Tight,
        "Θ(n^2)",
    );
    assert!(report.violations.iter().any(
        |v| v.rule == "algorithmic_time" && v.severity == analyzer_core::rules::Severity::Error
    ));
    assert_eq!(
        report.summary.errors,
        report
            .violations
            .iter()
            .filter(|v| v.severity == analyzer_core::rules::Severity::Error)
            .count()
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn shared_size_bindings_keep_sibling_scopes_and_mutations_isolated() {
    let (file, _) = analyze(
        r#"
fn sibling(n:usize){{let m=n;for i in 0..m {}}for i in 0..n {}}
fn shadow(n:usize){{let n=2;for i in 0..n {}}for i in 0..n {}}
fn countdown(n:usize){let mut i=n;while i>0 {i-=1;}for j in 0..n {}}
fn changed(mut n:usize){{n=3;}for i in 0..n {}}
fn conditional(mut n:usize){if n>0 {n=3;}for i in 0..n {}}
fn alias(mut n:usize){let alias=&mut n;for i in 0..n {}}
"#,
    );
    for name in ["sibling", "shadow", "countdown"] {
        expect(&file, name, EstimateStatus::Tight, "Θ(n)");
    }
    for name in ["changed", "conditional", "alias"] {
        assert_eq!(
            estimate(&file, name).status,
            EstimateStatus::Unknown,
            "{name}"
        );
    }
}
