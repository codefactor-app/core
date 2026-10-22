//! Immutable project indexes and transient per-call state.
use super::super::{inheritance, model::*, properties, symbols::*};
use crate::{config::*, ir::*, language::LanguageRegistry};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::Arc,
};
pub(super) type Functions<'a> = BTreeMap<&'a str, BTreeMap<String, Vec<(String, String)>>>;
pub(super) type Parents<'a> = BTreeMap<&'a str, BTreeMap<&'a str, Vec<String>>>;
pub(super) type Fields<'a> =
    BTreeMap<&'a str, BTreeMap<&'a str, BTreeMap<&'a str, Option<String>>>>;
pub(super) struct ProjectIndex<'a> {
    pub(super) files: &'a [FileModel],
    pub(super) registry: &'a LanguageRegistry,
    pub(super) config: &'a Config,
    pub(super) context: &'a crate::resolution::ResolutionContext,
    pub(super) browser: BrowserBindings<'a>,
    pub(super) uncertain_functions: HashSet<String>,
    pub(super) project_modules: BTreeSet<(&'a str, &'a str)>,
    pub(super) source_modules: BTreeSet<&'a str>,
    pub(super) modules: ModuleIndex<'a>,
    pub(super) exports: ExportBindings<'a>,
    pub(super) runtime_exports: ExportBindings<'a>,
    pub(super) types: KnownTypes,
    pub(super) parents: Parents<'a>,
    pub(super) external_bases: ExternalBases<'a>,
    pub(super) inheritance: inheritance::InheritanceIndex,
    pub(super) fields: Fields<'a>,
    pub(super) constant_types: BTreeMap<(&'a str, String), String>,
    pub(super) returns_by_id: BTreeMap<String, String>,
    pub(super) property_index: properties::PropertyIndex,
    pub(super) implementations: BTreeMap<&'a str, BTreeMap<String, Vec<String>>>,
    pub(super) rust_methods: HashMap<String, (String, Option<String>, bool)>,
    pub(super) rust_contracts: HashSet<String>,
    pub(super) rust_defaults: HashSet<String>,
    pub(super) functions: Functions<'a>,
}
pub(super) struct BrowserBindings<'a> {
    pub(super) files: BTreeSet<&'a str>,
    pub(super) symbols: BTreeMap<String, BTreeSet<String>>,
    pub(super) blocked: BTreeSet<String>,
    pub(super) blocked_targets: BTreeSet<String>,
}
impl<'a> BrowserBindings<'a> {
    pub(super) fn new(
        files: &'a [FileModel],
        context: &crate::resolution::ResolutionContext,
    ) -> Self {
        let browser_files = files
            .iter()
            .filter(|file| file.browser_script && !context.javascript_package_module(&file.path))
            .map(|file| file.path.as_str())
            .collect::<BTreeSet<_>>();
        let mut script_symbols = BTreeMap::<String, BTreeSet<String>>::new();
        let mut script_blocked = BTreeSet::<String>::new();
        for file in files
            .iter()
            .filter(|file| browser_files.contains(file.path.as_str()))
        {
            for binding in &file.script_globals {
                if let Some(target) = &binding.target {
                    script_symbols
                        .entry(binding.name.clone())
                        .or_default()
                        .insert(target.clone());
                } else {
                    script_blocked.insert(binding.name.clone());
                }
            }
        }
        let mut result = Self {
            files: browser_files,
            symbols: script_symbols,
            blocked: script_blocked,
            blocked_targets: BTreeSet::new(),
        };
        result.blocked_targets = result
            .symbols
            .iter()
            .filter(|(name, _)| result.blocked_name(name))
            .flat_map(|(_, targets)| targets.iter().cloned())
            .collect();
        result
    }
    pub(super) fn blocked_name(&self, name: &str) -> bool {
        let script_blocked = &self.blocked;

        if script_blocked.contains("*") {
            return true;
        }
        let mut prefix = Some(name);
        while let Some(scope) = prefix {
            if script_blocked.contains(scope) || script_blocked.contains(&format!("{scope}.*")) {
                return true;
            }
            prefix = scope.rsplit_once('.').map(|(parent, _)| parent);
        }
        false
    }
    pub(super) fn target(&self, name: &str) -> Option<String> {
        let script_symbols = &self.symbols;
        let blocked_script_targets = &self.blocked_targets;
        let script_blocked_name = |name: &str| self.blocked_name(name);

        if script_blocked_name(name) {
            return None;
        }
        let targets = script_symbols.get(name)?;
        (targets.len() == 1
            && !targets
                .iter()
                .any(|target| blocked_script_targets.contains(target)))
        .then(|| targets.iter().next().unwrap().clone())
    }
}
#[derive(Default)]
pub(super) struct CallState {
    pub(super) candidates: Vec<String>,
    pub(super) imported_function: bool,
    pub(super) qualified: Vec<String>,
    pub(super) receiver_type: Option<String>,
    pub(super) python_super_anchor: Option<String>,
    pub(super) qualified_receiver: bool,
    pub(super) imported_constant: Option<String>,
    pub(super) interface_dispatch: bool,
    pub(super) constructor_type: Option<String>,
    pub(super) external_candidates: Vec<String>,
    pub(super) has_definition: bool,
    pub(super) standard_receiver: Option<String>,
    pub(super) standard: bool,
    pub(super) targets: Vec<String>,
    pub(super) inherited_standard_receiver: Option<String>,
    pub(super) inheritance_uncertain: bool,
    pub(super) ordered_lookup: bool,
    pub(super) blocked_property_descriptor: bool,
}
pub(super) struct CallContext<'a, 'imports, 'locals> {
    pub(super) file: &'a FileModel,
    pub(super) f: &'a FunctionModel,
    pub(super) call: &'a CallSite,
    pub(super) context: &'a TypeContext,
    pub(super) from: &'a Arc<str>,
    pub(super) strategy: &'a Arc<dyn crate::LanguageStrategy>,
    pub(super) rules: &'a [(ExternalOperationRule, Option<globset::GlobMatcher>)],
    pub(super) imports_index: &'a crate::scoped_imports::ScopedImports<'imports>,
    pub(super) local_types: &'a mut Cow<'locals, BTreeMap<String, String>>,
    pub(super) normalized_symbols: &'a mut HashMap<String, String>,
}
pub(super) struct Emission<'a> {
    pub(super) graph: &'a mut CallGraph,
    pub(super) generated: &'a mut BTreeMap<String, GraphNode>,
    pub(super) edge_ids: &'a mut BTreeMap<String, Arc<str>>,
}

pub(super) struct FileCalls<'a, 'imports> {
    pub file: &'a FileModel,
    pub strategy: &'a Arc<dyn crate::LanguageStrategy>,
    pub rules: &'a [(ExternalOperationRule, Option<globset::GlobMatcher>)],
    pub imports_index: &'a crate::scoped_imports::ScopedImports<'imports>,
    pub normalized_symbols: &'a mut HashMap<String, String>,
}
