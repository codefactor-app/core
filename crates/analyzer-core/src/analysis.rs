use crate::config::Config;
use crate::ir::*;
use crate::report::AnalysisReport;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

pub trait LanguageStrategy: Send + Sync {
    fn language_id(&self) -> &'static str;
    fn file_extensions(&self) -> &'static [&'static str];
    fn parse(&self, source: &str, path: &Path) -> RawAst;
    fn normalize(&self, source: &str, path: &Path, ast: &RawAst) -> FileModel;
    /// Build structural IR for scans. Strategies may avoid allocating a detailed AST.
    /// The default preserves compatibility with strategies that normalize RawAst.
    fn analyze(&self, source: &str, path: &Path) -> FileModel {
        let ast = self.parse(source, path);
        let mut model = self.normalize(source, path, &ast);
        model.type_reference_syntax = self.type_reference_syntax();
        for error in ast.errors {
            if !model
                .parse_errors
                .iter()
                .any(|existing| existing.message == error.message && existing.range == error.range)
            {
                model.parse_errors.push(error);
            }
        }
        model
    }
    /// Collect optional advisory facts without allocating a public detailed AST.
    fn analyze_with_architecture(&self, source: &str, path: &Path, _enabled: bool) -> FileModel {
        self.analyze(source, path)
    }
    fn infer_types(&self, _file: &FileModel, function: &FunctionModel) -> TypeContext {
        let mut ctx = TypeContext::default();
        for p in &function.parameters {
            if let Some(t) = &p.type_name {
                ctx.variables.insert(p.name.clone(), t.clone());
            }
        }
        ctx
    }
    fn resolve_call(
        &self,
        call: &CallSite,
        _context: &TypeContext,
        _file: &FileModel,
    ) -> Vec<String> {
        call.targets.clone()
    }
    /// Resolve language-specific imports or dispatch against the normalized
    /// project. Return qualified declaration names; core verifies target identity
    /// and preserves ambiguous alternatives. Existing adapters use the file hook.
    fn resolve_project_call(
        &self,
        call: &CallSite,
        context: &TypeContext,
        file: &FileModel,
        _project: &[FileModel],
    ) -> Vec<String> {
        self.resolve_call(call, context, file)
    }
    fn external_operations(&self) -> Vec<ExternalOperationRule> {
        Vec::new()
    }
    /// Concrete language semantics used after project/lexical declarations.
    /// Neutral defaults let existing external strategies keep working.
    fn is_native_function(&self, _symbol: &str) -> bool {
        false
    }
    fn is_native_receiver(&self, _symbol: &str) -> bool {
        false
    }
    fn is_native_global_receiver(&self, _symbol: &str) -> bool {
        false
    }
    fn is_native_method(&self, _owner: &str, _name: &str) -> bool {
        false
    }
    fn is_native_macro(&self, _name: &str) -> bool {
        false
    }
    fn constructor_name(&self) -> &'static str {
        "constructor"
    }
    /// Permit a plain function-shaped call to identify a class constructor.
    /// Explicit CallKind::Constructor sites do not depend on this policy.
    fn permits_constructor_inference(&self) -> bool {
        true
    }
    fn namespace_separator(&self) -> &'static str {
        "."
    }
    fn method_separator(&self) -> &'static str {
        "."
    }
    fn type_reference_syntax(&self) -> crate::types::TypeReferenceSyntax {
        crate::types::TypeReferenceSyntax::default()
    }
    /// Identify one receiver class from an annotation, or retain uncertainty.
    fn type_reference(&self, raw: &str) -> Option<String> {
        crate::types::normalize_reference(raw, &self.type_reference_syntax())
    }
    fn count_lines(&self, source: &str) -> Loc {
        crate::metrics::basic_line_counts(source)
    }
    /// Reuse normalized line counts when available. The default retains the
    /// existing count_lines hook for third-party strategies supplying comments.
    fn line_counts(&self, source: &str, _path: &Path, _model: &FileModel) -> Loc {
        self.count_lines(source)
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    fn register_config(&self, _loader: &mut crate::config::ConfigLoader) {}
    fn version(&self) -> &'static str {
        "1"
    }
}

#[derive(Default, Clone)]
pub struct LanguageRegistry {
    strategies: BTreeMap<String, Arc<dyn LanguageStrategy>>,
    extensions: BTreeMap<String, String>,
}

impl LanguageRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn register<S: LanguageStrategy + 'static>(&mut self, strategy: S) {
        let id = strategy.language_id().to_string();
        for ext in strategy.file_extensions() {
            self.extensions
                .insert(ext.trim_start_matches('.').to_ascii_lowercase(), id.clone());
        }
        self.strategies.insert(id, Arc::new(strategy));
    }
    pub fn get(&self, language: &str) -> Option<&Arc<dyn LanguageStrategy>> {
        self.strategies.get(language)
    }
    pub fn by_extension(&self, extension: &str) -> Option<&Arc<dyn LanguageStrategy>> {
        self.extensions
            .get(&extension.trim_start_matches('.').to_ascii_lowercase())
            .and_then(|id| self.get(id))
    }
    pub fn languages(&self) -> Vec<&str> {
        self.strategies.keys().map(String::as_str).collect()
    }
    pub fn capabilities(&self) -> BTreeMap<String, Capabilities> {
        self.strategies
            .iter()
            .map(|(id, strategy)| (id.clone(), strategy.capabilities()))
            .collect()
    }
    pub fn register_config_keys(&self, loader: &mut crate::config::ConfigLoader) {
        for strategy in self.strategies.values() {
            loader.register_language(strategy.language_id());
            strategy.register_config(loader);
        }
    }
    pub fn language_for_path(
        &self,
        path: &Path,
        overrides: &BTreeMap<String, String>,
    ) -> Option<&Arc<dyn LanguageStrategy>> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        if let Some(id) = overrides.get(&ext) {
            self.get(id)
        } else {
            self.by_extension(&ext)
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AnalysisError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Config(String),
}

pub struct Analyzer {
    pub registry: LanguageRegistry,
    pub config: Config,
    pub metrics: crate::metrics::MetricRegistry,
    /// Web exploration needs dependency witnesses even when advisory rules are disabled.
    pub collect_dependency_facts: bool,
}

impl Analyzer {
    pub fn new(registry: LanguageRegistry, config: Config) -> Self {
        Self {
            registry,
            config,
            metrics: crate::metrics::MetricRegistry::new(),
            collect_dependency_facts: false,
        }
    }
    pub fn with_metrics(
        registry: LanguageRegistry,
        config: Config,
        metrics: crate::metrics::MetricRegistry,
    ) -> Self {
        Self {
            registry,
            config,
            metrics,
            collect_dependency_facts: false,
        }
    }
    pub fn scan(&self, root: &Path) -> Result<AnalysisReport, AnalysisError> {
        crate::report::scan(self, root)
    }
    pub fn scan_without_baseline(&self, root: &Path) -> Result<AnalysisReport, AnalysisError> {
        crate::report::scan_with_baseline(self, root, false)
    }
    pub fn ast(&self, path: &Path) -> Result<RawAst, AnalysisError> {
        let strategy = self
            .registry
            .language_for_path(path, &self.config.languages.extensions)
            .ok_or_else(|| {
                AnalysisError::Config(format!("unsupported file: {}", path.display()))
            })?;
        let source = std::fs::read_to_string(path)?;
        Ok(strategy.parse(source.trim_start_matches('\u{feff}'), path))
    }
}
