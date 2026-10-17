use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct SourceRange {
    pub file: Arc<str>,
    pub start_line: usize,
    pub start_column: usize,
    pub end_line: usize,
    pub end_column: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Loc {
    pub total: usize,
    pub code: usize,
    pub comment: usize,
    pub blank: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AstKind {
    File,
    Module,
    Type,
    Function,
    Call,
    Branch,
    Loop,
    Return,
    Other(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AstNode {
    pub kind: AstKind,
    pub raw_kind: String,
    pub range: SourceRange,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub children: Vec<AstNode>,
}

impl AstNode {
    pub fn visit<'a>(&'a self, f: &mut impl FnMut(&'a AstNode)) {
        f(self);
        for child in &self.children {
            child.visit(f);
        }
    }
    pub fn find<'a>(&'a self, pred: &impl Fn(&AstNode) -> bool, out: &mut Vec<&'a AstNode>) {
        if pred(self) {
            out.push(self);
        }
        for child in &self.children {
            child.find(pred, out);
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RawAst {
    pub root: AstNode,
    #[serde(default)]
    pub errors: Vec<ParseError>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParseError {
    pub message: String,
    pub range: SourceRange,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FileModel {
    pub path: String,
    pub language: String,
    pub module: String,
    #[serde(
        default,
        skip_serializing_if = "crate::types::TypeReferenceSyntax::is_default"
    )]
    pub type_reference_syntax: crate::types::TypeReferenceSyntax,
    /// Graph-only Cargo compilation context; source caches and metrics retain one file.
    #[serde(default, skip_serializing)]
    pub cargo_target: Option<String>,
    #[serde(default, skip_serializing)]
    pub cargo_source_module: Option<String>,
    #[serde(default)]
    pub imports: Vec<Import>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exports: Vec<Export>,
    /// Browser classic-script evidence; modules and unproven runtime globals stay isolated.
    #[serde(default, skip_serializing_if = "is_false")]
    pub browser_script: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub script_globals: Vec<ScriptGlobal>,
    #[serde(default)]
    pub types: Vec<TypeModel>,
    #[serde(default)]
    pub functions: Vec<FunctionModel>,
    #[serde(default)]
    pub loc: Loc,
    #[serde(default)]
    pub parse_errors: Vec<ParseError>,
    /// Actual syntax comments. None lets third-party strategies use the fallback lexer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_ranges: Option<Vec<SourceRange>>,
    #[serde(default)]
    pub vendor: bool,
    #[serde(default)]
    pub source_hash: String,
    #[serde(default, skip_serializing)]
    pub architecture_facts: Option<Arc<crate::architecture::FileFacts>>,
}

/// A proven classic-script callable, or a write that invalidates its global name.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ScriptGlobal {
    pub name: String,
    /// None blocks resolution. A trailing .* blocks all members of an object.
    pub target: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Import {
    pub path: String,
    /// Original ESM module specifier, kept separate from appended export names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// TypeScript's `import name = require(...)` uses require export conditions.
    #[serde(default, skip_serializing_if = "is_false")]
    pub require: bool,
    /// TypeScript imports retained for annotations but erased from runtime bindings.
    #[serde(default, skip_serializing_if = "is_false")]
    pub type_only: bool,
    /// Python's `import pkg.module` binds `pkg`, while retaining the full imported path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<SourceRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(default)]
    pub alias: Option<String>,
    #[serde(default)]
    pub range: SourceRange,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Export {
    pub name: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// TypeScript exports retained for annotations but erased from runtime bindings.
    #[serde(default, skip_serializing_if = "is_false")]
    pub type_only: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TypeKind {
    Class,
    Trait,
    Interface,
    Enum,
    Struct,
    Impl,
    Namespace,
    Other,
}
impl Default for TypeKind {
    fn default() -> Self {
        Self::Other
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TypeModel {
    pub name: String,
    pub fqn: String,
    pub kind: TypeKind,
    pub range: SourceRange,
    #[serde(default)]
    pub fields: Vec<Field>,
    #[serde(default)]
    pub methods: Vec<String>,
    #[serde(default)]
    pub implements: Vec<String>,
    #[serde(default)]
    pub extends: Vec<String>,
    /// PHP trait composition is distinct from class inheritance.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trait_uses: Vec<TraitUse>,
    /// The source uses a dynamic base expression or a custom MRO provider.
    #[serde(default, skip_serializing_if = "is_false")]
    pub inheritance_unknown: bool,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub decorators: Vec<String>,
    /// Rust's `mod name;` declares a module stored in another source file.
    #[serde(default, skip_serializing_if = "is_false")]
    pub external_module: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rust_cfg: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditional_module_paths: Vec<ConditionalModulePath>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TraitUse {
    pub traits: Vec<String>,
    pub range: SourceRange,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub adaptations: Vec<TraitAdaptation>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TraitAdaptation {
    /// Optional for PHP's unqualified `method as alias` form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trait_name: Option<String>,
    pub method: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instead_of: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConditionalModulePath {
    pub condition: String,
    pub path: String,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    #[serde(default)]
    pub type_name: Option<String>,
    pub range: SourceRange,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_static: bool,
    /// A computed/escaped data key that has not been safely determined.
    #[serde(default, skip_serializing_if = "is_false")]
    pub unknown_key: bool,
    /// A direct lexical const/import reference eligible for a proven Symbol key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol_key_reference: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AccessorKind {
    Getter,
    Setter,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FunctionModel {
    pub name: String,
    pub fqn: String,
    pub range: SourceRange,
    pub body_range: SourceRange,
    #[serde(default)]
    pub parent_type: Option<String>,
    #[serde(default)]
    pub parent_function: Option<String>,
    /// Rust implementation contract, distinct from the concrete receiver type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rust_trait: Option<String>,
    #[serde(default)]
    pub parameters: Vec<Parameter>,
    #[serde(default)]
    pub calls: Vec<CallSite>,
    /// JS/TS property operations; only confirmed native accessors become graph calls.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub property_accesses: Vec<PropertyAccess>,
    /// Statically described JS/TS descriptor changes, applied in function source order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub property_mutations: Vec<PropertyMutation>,
    #[serde(default)]
    pub signature: String,
    #[serde(default)]
    pub modifiers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rust_cfg: Vec<String>,
    #[serde(default)]
    pub return_type: Option<String>,
    #[serde(default)]
    pub complexity: usize,
    /// Structural cognitive score supplied by a strategy, independent of calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cognitive_complexity: Option<usize>,
    #[serde(default)]
    pub nesting_depth: usize,
    #[serde(default)]
    pub return_points: usize,
    /// Optional detailed AST; structural scans leave it absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalized_ast: Option<AstNode>,
    #[serde(default)]
    pub accessed_fields: Vec<String>,
    /// Structural accessor shape, not a proof of purity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accessor: Option<AccessorKind>,
    /// Compact language-neutral cost IR, retained in the file cache, not a detailed AST.
    #[serde(default, skip_serializing)]
    pub cost_ir: Option<Arc<crate::algorithmic::CostNode>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub algorithmic_complexity: Option<Arc<crate::algorithmic::ComplexityEstimate>>,
}

impl FunctionModel {
    /// Runtime code at module scope is a graph entry, not a source function declaration.
    pub fn is_module_initializer(&self) -> bool {
        self.modifiers
            .iter()
            .any(|modifier| modifier == "module_initializer")
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Parameter {
    pub name: String,
    #[serde(default)]
    pub type_name: Option<String>,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub variadic: bool,
    #[serde(default)]
    pub implicit_receiver: bool,
    #[serde(default)]
    pub range: SourceRange,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallKind {
    Function,
    Method,
    Static,
    Constructor,
    Dynamic,
    Macro,
}
impl Default for CallKind {
    fn default() -> Self {
        Self::Function
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Resolved,
    Ambiguous,
    Dynamic,
}
impl Default for Confidence {
    fn default() -> Self {
        Self::Dynamic
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CallSite {
    pub name: String,
    pub range: SourceRange,
    pub kind: CallKind,
    #[serde(default)]
    pub receiver: Option<String>,
    #[serde(default)]
    pub receiver_type: Option<String>,
    /// Explicit Rust UFCS contract in `<Type as Trait>::method`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rust_trait: Option<String>,
    /// Finite proven alternatives after a JS/TS conditional binding.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub receiver_types: Vec<String>,
    /// A proven local allocation shared by simple aliases, lost on uncertain merges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_identity: Option<String>,
    /// Type of the lexical root in a property receiver such as `value.field`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_root_type: Option<String>,
    /// A lexical binding shadows file/project names; unknown values must stay unresolved.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    /// A lexical binding exists, but provides no callable/type evidence; block name fallback.
    pub binding_unknown: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    /// A local variable or nested declaration shadows module imports and bare-name lookup.
    pub local_binding: bool,
    #[serde(default)]
    pub targets: Vec<String>,
    /// Unshadowed classic-script lookup, resolved only against proven browser globals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_global: Option<String>,
    #[serde(default)]
    pub confidence: Confidence,
    #[serde(default)]
    pub guard: Arc<[Guard]>,
    #[serde(default)]
    pub in_loop: Arc<[LoopContext]>,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub assignment_to: Option<String>,
    /// The direct initializer of a module const uses an unshadowed Symbol factory.
    #[serde(default, skip_serializing_if = "is_false")]
    pub symbol_constant_assignment: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PropertyAccess {
    pub access: AccessorKind,
    /// Shared source and lexical receiver metadata, without inventing an explicit call.
    #[serde(flatten)]
    pub site: CallSite,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PropertyMutation {
    pub site: CallSite,
    #[serde(default)]
    pub prototype: bool,
    #[serde(default)]
    pub unknown: bool,
    #[serde(default)]
    pub data: bool,
    #[serde(default)]
    pub getter_present: bool,
    #[serde(default)]
    pub setter_present: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub getter: Option<SourceRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setter: Option<SourceRange>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Guard {
    pub kind: String,
    pub raw: String,
    pub normalized: String,
    pub branch: String,
    pub range: SourceRange,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct LoopContext {
    pub header: String,
    pub line: usize,
    pub range: SourceRange,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TypeContext {
    pub variables: BTreeMap<String, String>,
    pub properties: BTreeMap<String, String>,
    pub returns: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExternalOperationRule {
    pub category: String,
    pub pattern: String,
    pub library: String,
    pub operation: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Capabilities {
    pub static_types: bool,
    pub generics: bool,
    pub inheritance: bool,
    pub closures: bool,
    pub guards: bool,
}

impl FileModel {
    /// Restore per-file sharing and release vector growth slack after structural collection.
    pub fn intern_range_paths(&mut self) {
        let mut pool = BTreeMap::<Arc<str>, Arc<str>>::new();
        let mut reasons = BTreeMap::<Arc<str>, Arc<str>>::new();
        let mut intern_reason = |reason: &mut Arc<str>| {
            if let Some(shared) = reasons.get(reason.as_ref()) {
                *reason = shared.clone();
            } else {
                reasons.insert(reason.clone(), reason.clone());
            }
        };
        let mut guards = BTreeMap::<Arc<[Guard]>, Arc<[Guard]>>::new();
        let mut loops = BTreeMap::<Arc<[LoopContext]>, Arc<[LoopContext]>>::new();
        let mut intern = |range: &mut SourceRange| {
            if let Some(shared) = pool.get(range.file.as_ref()) {
                range.file = shared.clone();
            } else {
                pool.insert(range.file.clone(), range.file.clone());
            }
        };
        if let Some(comments) = &mut self.comment_ranges {
            for comment in comments.iter_mut() {
                intern(comment);
            }
            comments.shrink_to_fit();
        }
        if let Some(facts) = &mut self.architecture_facts {
            let facts = Arc::make_mut(facts);
            for f in &mut facts.functions {
                intern(&mut f.range);
                for d in &mut f.decisions {
                    intern(&mut d.range);
                    for c in &mut d.cases {
                        intern(&mut c.range);
                        for r in &mut c.creations {
                            intern(&mut r.range);
                        }
                    }
                }
                for c in &mut f.member_chains {
                    intern(&mut c.range);
                }
            }
            for f in &mut facts.fragments {
                intern(&mut f.range);
            }
            for r in &mut facts.references {
                intern(&mut r.range);
            }
            for access in &mut facts.value_accesses {
                intern(&mut access.range);
            }
        }
        for import in &mut self.imports {
            intern(&mut import.range);
            if let Some(scope) = &mut import.scope {
                intern(scope);
            }
        }
        for kind in &mut self.types {
            intern(&mut kind.range);
            for field in &mut kind.fields {
                intern(&mut field.range);
            }
            kind.fields.shrink_to_fit();
            kind.methods.shrink_to_fit();
            kind.implements.shrink_to_fit();
            kind.extends.shrink_to_fit();
            kind.decorators.shrink_to_fit();
        }
        for function in &mut self.functions {
            intern(&mut function.range);
            intern(&mut function.body_range);
            if let Some(cost) = &mut function.cost_ir {
                Arc::make_mut(cost).intern_ranges(&mut intern, &mut intern_reason);
            }
            for parameter in &mut function.parameters {
                intern(&mut parameter.range);
            }
            for call in function
                .calls
                .iter_mut()
                .chain(
                    function
                        .property_accesses
                        .iter_mut()
                        .map(|access| &mut access.site),
                )
                .chain(
                    function
                        .property_mutations
                        .iter_mut()
                        .map(|mutation| &mut mutation.site),
                )
            {
                intern(&mut call.range);
                call.arguments.shrink_to_fit();
                call.targets.shrink_to_fit();
                call.receiver_types.shrink_to_fit();
                if let Some(shared) = guards.get(call.guard.as_ref()) {
                    call.guard = shared.clone();
                } else {
                    for guard in Arc::make_mut(&mut call.guard) {
                        intern(&mut guard.range);
                    }
                    guards.insert(call.guard.clone(), call.guard.clone());
                }
                if let Some(shared) = loops.get(call.in_loop.as_ref()) {
                    call.in_loop = shared.clone();
                } else {
                    for context in Arc::make_mut(&mut call.in_loop) {
                        intern(&mut context.range);
                    }
                    loops.insert(call.in_loop.clone(), call.in_loop.clone());
                }
            }
            function.parameters.shrink_to_fit();
            function.calls.shrink_to_fit();
            function.property_accesses.shrink_to_fit();
            for mutation in &mut function.property_mutations {
                if let Some(getter) = &mut mutation.getter {
                    intern(getter);
                }
                if let Some(setter) = &mut mutation.setter {
                    intern(setter);
                }
            }
            function.property_mutations.shrink_to_fit();
            function.modifiers.shrink_to_fit();
            function.accessed_fields.shrink_to_fit();
        }
        for error in &mut self.parse_errors {
            intern(&mut error.range);
        }
        // Structural collection is complete: do not retain geometric growth capacity.
        self.imports.shrink_to_fit();
        self.exports.shrink_to_fit();
        self.types.shrink_to_fit();
        self.functions.shrink_to_fit();
        self.parse_errors.shrink_to_fit();
    }
}

#[cfg(test)]
mod context_tests {
    use super::*;

    #[test]
    fn cached_context_arrays_recover_sharing_without_changing_order_or_ranges() {
        let path = "src/example.ts";
        let range = SourceRange {
            file: path.into(),
            start_line: 2,
            end_line: 8,
            ..Default::default()
        };
        let call = CallSite {
            range: range.clone(),
            guard: vec![
                Guard {
                    raw: "outer".into(),
                    normalized: "outer".into(),
                    range: range.clone(),
                    ..Default::default()
                },
                Guard {
                    raw: "inner".into(),
                    normalized: "inner".into(),
                    range: range.clone(),
                    ..Default::default()
                },
            ]
            .into(),
            in_loop: vec![LoopContext {
                header: "while (active)".into(),
                range,
                ..Default::default()
            }]
            .into(),
            ..Default::default()
        };
        let file = FileModel {
            path: path.into(),
            functions: vec![FunctionModel {
                calls: vec![call.clone(), call],
                ..Default::default()
            }],
            ..Default::default()
        };
        let encoded = serde_json::to_string(&file).unwrap();
        let mut decoded: FileModel = serde_json::from_str(&encoded).unwrap();
        decoded.intern_range_paths();
        let calls = &decoded.functions[0].calls;
        assert!(Arc::ptr_eq(&calls[0].guard, &calls[1].guard));
        assert!(Arc::ptr_eq(&calls[0].in_loop, &calls[1].in_loop));
        assert!(Arc::ptr_eq(
            &calls[0].range.file,
            &calls[0].guard[0].range.file
        ));
        assert_eq!(serde_json::to_string(&decoded).unwrap(), encoded);
    }
}
