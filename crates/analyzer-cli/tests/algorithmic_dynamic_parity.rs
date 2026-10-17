use analyzer_core::{
    algorithmic::{self, ComplexityEstimate, EstimateStatus},
    graph::CallGraph,
    Config, FileModel, LanguageRegistry,
};
use std::path::Path;

fn analyze(path: &str, source: &str, config: &Config) -> FileModel {
    let mut registry = LanguageRegistry::default();
    registry.register(analyzer_python::Python);
    registry.register(analyzer_javascript::JavaScript);
    registry.register(analyzer_typescript::TypeScript);
    let strategy = registry
        .by_extension(Path::new(path).extension().unwrap().to_str().unwrap())
        .unwrap();
    let mut files = vec![strategy.analyze(source, Path::new(path))];
    assert!(
        files[0].parse_errors.is_empty(),
        "{path}: {:?}",
        files[0].parse_errors
    );
    let mut graph = CallGraph::build(&files, &registry, config);
    algorithmic::analyze(&mut files, &mut graph, config);
    files.remove(0)
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
fn expect(file: &FileModel, name: &str, status: EstimateStatus, formula: Option<&str>) {
    let e = estimate(file, name);
    assert_eq!(e.status, status, "{name}: {e:?}");
    assert_eq!(e.formula.as_deref(), formula, "{name}: {e:?}");
}

#[test]
fn receiver_values_and_unused_custom_annotations_do_not_hide_body_costs() {
    let python = analyze(
        "receivers.py",
        r#"
class Value:
    def receiver(self):
        return self
    def renamed(current):
        return current

def unused(request: Custom):
    return True

def linear(request: Custom, n: int):
    for i in range(n):
        pass

def operation(request: Custom):
    return request + 1
"#,
        &Config::default(),
    );
    for name in ["receiver", "renamed"] {
        expect(&python, name, EstimateStatus::Tight, Some("Θ(1)"));
    }
    expect(&python, "unused", EstimateStatus::Heuristic, Some("O(1)"));
    assert!(estimate(&python, "unused")
        .assumptions
        .iter()
        .any(|a| a.contains("finaliz")));
    expect(&python, "linear", EstimateStatus::Heuristic, Some("O(n)"));
    expect(&python, "operation", EstimateStatus::Unknown, None);
    for (path, source) in [
        ("receivers.js", "class Value { receiver(){return this;} static staticReceiver(){return this;} } function outside(){return this;}"),
        ("receivers.ts", "class Value { receiver(){return this;} static staticReceiver(){return this;} } function outside(){return this;} function unused(request:Custom){return true;} function operation(request:Custom){return request+1;}"),
    ] {
        let file = analyze(path, source, &Config::default());
        for name in ["receiver", "staticReceiver", "outside"] {
            expect(&file, name, EstimateStatus::Tight, Some("Θ(1)"));
        }
        if path.ends_with("ts") {
            expect(&file, "unused", EstimateStatus::Tight, Some("Θ(1)"));
            expect(&file, "operation", EstimateStatus::Unknown, None);
        }
    }
}

#[test]
fn native_literal_containers_and_unary_forms_are_modeled() {
    for (path, source) in [
        ("literals.py", "def literal():\n return {'name': 1, 'items': [2, 3]}\ndef unary(n:int):\n return -n\ndef fixed():\n for item in [1,2,3]:\n  copy=item\n"),
        ("literals.js", "function literal(){return {name:1,items:[2,3]};} function unary(n){return -n;} function fixed(){for(const item of [1,2,3]){const copy=item;}} function shorthand(n){return {n};}"),
        ("literals.ts", "function literal(){return {name:1,items:[2,3]};} function unary(n:number){return -n;} function fixed(){for(const item of [1,2,3]){const copy=item;}} function shorthand(n:number){return {n};}"),
    ] {
        let file = analyze(path, source, &Config::default());
        expect(&file, "literal", EstimateStatus::Tight, Some("Θ(1)"));
        expect(&file, "fixed", EstimateStatus::Heuristic, Some("O(1)"));
        if path.ends_with("ts") {
            expect(&file, "unary", EstimateStatus::Tight, Some("Θ(1)"));
        } else {
            expect(&file, "unary", EstimateStatus::Heuristic, Some("O(1)"));
        }
        if !path.ends_with("py") {
            expect(&file, "shorthand", EstimateStatus::Tight, Some("Θ(1)"));
        }
    }
}

#[test]
fn own_declared_storage_uses_explicit_dynamic_semantics_assumptions() {
    for (path, source) in [
        ("storage.py", "class Value:\n title: str = ''\n enabled: bool = False\n items: list[int] = []\n def getTitle(self):\n  return self.title\n def setEnabled(self, enabled:bool):\n  self.enabled=enabled\n  return self\n def setItems(self, items:list[int]):\n  self.items=items\n def missing(self):\n  return self.missing\n def rebound(self, other:Custom):\n  self=other\n  return self.title\n def fieldLoop(self):\n  for item in self.items:\n   pass\n"),
        ("storage.js", "class Value { title=''; enabled=false; items=[]; getTitle(){return this.title;} setEnabled(enabled){this.enabled=enabled;return this;} setItems(items){this.items=items;} missing(){return this.missing;} fieldLoop(){for(const item of this.items){}} }"),
        ("storage.ts", "class Value { title:string=''; enabled:boolean=false; items:number[]=[]; getTitle(){return this.title;} setEnabled(enabled:boolean){this.enabled=enabled;return this;} setItems(items:number[]){this.items=items;} missing(){return this.missing;} fieldLoop(){for(const item of this.items){}} }"),
    ] {
        let file = analyze(path, source, &Config::default());
        for name in ["getTitle", "setEnabled"] {
            expect(&file, name, EstimateStatus::Heuristic, Some("O(1)"));
            assert!(estimate(&file, name).assumptions.iter().any(|a| a.contains("storage")), "{path} {name}");
        }
        for name in ["setItems", "missing", "fieldLoop"] {
            expect(&file, name, EstimateStatus::Unknown, None);
        }
        if path.ends_with("py") {
            expect(&file, "rebound", EstimateStatus::Unknown, None);
        }
        let mut strict = Config::default();
        strict.algorithmic_complexity.heuristics = false;
        let file = analyze(path, source, &strict);
        for name in ["getTitle", "setEnabled"] {
            expect(&file, name, EstimateStatus::Unknown, None);
        }
    }
}

#[test]
fn arbitrary_getters_hooks_computed_keys_and_unpacking_stay_unknown() {
    for (path, source) in [
        ("unsafe.py", "class Value:\n @property\n def title(self):\n  return foreign()\n def getter(self):\n  return self.title\nclass Hooked:\n title:str=''\n def __getattribute__(self,name):\n  return foreign()\n def hooked(self):\n  return self.title\nclass Base:\n title:str=''\nclass Derived(Base):\n def inherited(self):\n  return self.title\ndef computed(key:Custom):\n return {key:1}\ndef unpacked(items:list[int]):\n return [*items]\ndef spread(items:Custom):\n return {**items}\n"),
        ("unsafe.js", "class Value { get title(){return foreign();} getter(){return this.title;} } class Base {title='';} class Derived extends Base { inherited(){return this.title;} } function computed(key){return {[key]:1};} function unpacked(items){return [...items];} function spread(items){return {...items};} function custom(object){return object.value;}"),
        ("unsafe.ts", "class Value { get title(){return foreign();} getter(){return this.title;} } class Base {title='';} class Derived extends Base { inherited(){return this.title;} } function computed(key:Custom){return {[key]:1};} function unpacked(items:number[]){return [...items];} function spread(items:Custom){return {...items};} function custom(object:Custom){return object.value;}"),
    ] {
        let file = analyze(path, source, &Config::default());
        for name in ["getter", "inherited", "computed", "unpacked", "spread"] {
            expect(&file, name, EstimateStatus::Unknown, None);
        }
        expect(&file, if path.ends_with("py") {"hooked"} else {"custom"}, EstimateStatus::Unknown, None);
    }
}

#[test]
fn python_constructor_fields_use_the_declared_receiver_and_reject_rebinding() {
    let file = analyze(
        "constructor.py",
        r#"
class Value:
    def __init__(instance, enabled: bool):
        instance.value = 0
        instance.enabled = enabled
    def getValue(current):
        return current.value
    def setValue(current, value: int):
        current.value = value
    def getEnabled(current):
        return current.enabled
    def branchRebound(current, other, flag):
        if flag:
            current = other
        return current.value
    def loopRebound(current, items):
        for current in items:
            pass
        return current.value
    def deleted(current):
        del current.value
        return current.value

class Replaced:
    value: int = 0
    value = property(foreign)
    def replacement(self):
        return self.value

class Conditional:
    value: int = 0
    if foreign():
        value = property(foreign)
    def conditional(self):
        return self.value

class Nested:
    def __init__(self):
        def initialize(self):
            self.value = 0
    def nested(self):
        return self.value
"#,
        &Config::default(),
    );
    for name in ["getValue", "setValue", "getEnabled"] {
        expect(&file, name, EstimateStatus::Heuristic, Some("O(1)"));
    }
    for name in [
        "branchRebound",
        "loopRebound",
        "deleted",
        "replacement",
        "conditional",
        "nested",
    ] {
        expect(&file, name, EstimateStatus::Unknown, None);
    }
}

#[test]
fn object_literal_prototype_initialization_is_not_certified_constant() {
    for path in ["prototype.js", "prototype.ts"] {
        let source = if path.ends_with("ts") {
            "function prototype(proto:Custom){return {__proto__:proto};}"
        } else {
            "function prototype(proto){return {__proto__:proto};}"
        };
        let file = analyze(path, source, &Config::default());
        expect(&file, "prototype", EstimateStatus::Unknown, None);
    }
}

#[test]
fn receiver_field_increments_require_storage_assumptions_and_length_writes_stay_unknown() {
    for path in ["updates.js", "updates.ts"] {
        let source = if path.ends_with("ts") {
            "class Value {count:number=0; increment(){this.count++;}} function resize(items:number[]){items.length++;}"
        } else {
            "class Value {count=0; increment(){this.count++;}} function resize(items){items.length++;}"
        };
        let file = analyze(path, source, &Config::default());
        expect(&file, "increment", EstimateStatus::Heuristic, Some("O(1)"));
        expect(&file, "resize", EstimateStatus::Unknown, None);
        let mut strict = Config::default();
        strict.algorithmic_complexity.heuristics = false;
        let file = analyze(path, source, &strict);
        expect(&file, "increment", EstimateStatus::Unknown, None);
    }
}

#[test]
fn python_attribute_hook_aliases_and_conditional_bindings_prevent_storage_inference() {
    for binding in [
        "__getattribute__ = hook",
        "__getattr__ = hook",
        "__setattr__ = hook",
        "if True:\n        __getattribute__ = hook",
        "from hooks import operation as __getattr__",
        "for __setattr__ in [hook]:\n        pass",
    ] {
        let source = format!(
            "def hook(self, name):\n    return foreign()\nclass Value:\n    value: int = 0\n    {binding}\n    def read(self):\n        return self.value\n    def write(self):\n        self.value = 1\n"
        );
        let file = analyze("hook_alias.py", &source, &Config::default());
        for name in ["read", "write"] {
            expect(&file, name, EstimateStatus::Unknown, None);
        }
    }
}
