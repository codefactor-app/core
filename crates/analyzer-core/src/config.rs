pub(crate) mod algorithmic;
pub(crate) mod architecture;
mod loading;
use crate::ir::ExternalOperationRule;
use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser};
use yaml_rust2::scanner::Marker;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub config_version: u32,
    pub core: CoreConfig,
    pub paths: PathsConfig,
    pub languages: LanguagesConfig,
    pub vendor: VendorConfig,
    pub limits: LimitsConfig,
    pub sets: BTreeMap<String, Vec<String>>,
    pub external: ExternalConfig,
    pub metrics: MetricsConfig,
    pub graph: GraphConfig,
    pub resolution: ResolutionConfig,
    pub cache: CacheConfig,
    pub output: OutputConfig,
    pub algorithmic_complexity: crate::algorithmic::settings::AlgorithmicConfig,
    pub architecture: crate::config::architecture::ArchitectureConfig,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            config_version: 1,
            core: CoreConfig::default(),
            paths: PathsConfig::default(),
            languages: LanguagesConfig::default(),
            vendor: VendorConfig::default(),
            limits: LimitsConfig::default(),
            sets: BTreeMap::new(),
            external: ExternalConfig::default(),
            metrics: MetricsConfig::default(),
            graph: GraphConfig::default(),
            resolution: ResolutionConfig::default(),
            cache: CacheConfig::default(),
            output: OutputConfig::default(),
            algorithmic_complexity: crate::algorithmic::settings::AlgorithmicConfig::default(),
            architecture: crate::config::architecture::ArchitectureConfig::default(),
        }
    }
}

/// Package launcher settings. The core recognizes these but never downloads itself.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct CoreConfig {
    pub version: String,
}
impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            version: "latest".into(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PathsConfig {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LanguagesConfig {
    pub enabled: Vec<String>,
    pub extensions: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct VendorConfig {
    pub paths: Vec<String>,
    pub mode: VendorMode,
    pub parse: VendorParse,
}
impl Default for VendorConfig {
    fn default() -> Self {
        Self {
            paths: vec![
                "vendor/**".into(),
                "node_modules/**".into(),
                "target/**".into(),
                ".venv/**".into(),
            ],
            mode: VendorMode::Collapsed,
            parse: VendorParse::Lazy,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VendorMode {
    #[default]
    Collapsed,
    Expanded,
    Hidden,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VendorParse {
    #[default]
    Lazy,
    Full,
    None,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Threshold {
    pub warn: Option<f64>,
    pub error: Option<f64>,
    /// `false` switches the rule off at this level; a later override can set
    /// it back to `true`. Absent means "inherit".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}
impl Default for Threshold {
    fn default() -> Self {
        Self {
            warn: None,
            error: None,
            enabled: None,
        }
    }
}
impl Threshold {
    pub fn new(warn: f64, error: f64) -> Self {
        Self {
            warn: Some(warn),
            error: Some(error),
            enabled: None,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PathLimits {
    pub glob: String,
    #[serde(flatten)]
    pub metrics: BTreeMap<String, Threshold>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EntityGlobLimits {
    pub glob: String,
    #[serde(flatten)]
    pub metrics: BTreeMap<String, Threshold>,
}

/// FQNs are opaque strings: PHP backslashes are literal and namespace
/// separators do not restrict a wildcard match.
pub(crate) fn entity_glob(pattern: &str) -> Result<globset::Glob, globset::Error> {
    globset::GlobBuilder::new(pattern)
        .backslash_escape(false)
        .literal_separator(false)
        .build()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LimitsConfig {
    #[serde(flatten)]
    pub global: BTreeMap<String, Threshold>,
    pub by_language: BTreeMap<String, BTreeMap<String, Threshold>>,
    pub by_path: Vec<PathLimits>,
    pub by_entity_glob: Vec<EntityGlobLimits>,
    pub by_entity: BTreeMap<String, BTreeMap<String, Threshold>>,
    pub aggregates: BTreeMap<String, Threshold>,
}
impl Default for LimitsConfig {
    fn default() -> Self {
        let global = BTreeMap::from([
            ("file_lines".into(), Threshold::new(400.0, 800.0)),
            ("function_lines".into(), Threshold::new(40.0, 100.0)),
            ("functions_per_file".into(), Threshold::new(20.0, 40.0)),
            ("methods_per_class".into(), Threshold::new(15.0, 30.0)),
            ("function_params".into(), Threshold::new(4.0, 7.0)),
            ("constructor_params".into(), Threshold::new(8.0, 12.0)),
            ("cyclomatic_complexity".into(), Threshold::new(10.0, 20.0)),
        ]);
        Self {
            global,
            by_language: BTreeMap::new(),
            by_path: Vec::new(),
            by_entity_glob: Vec::new(),
            by_entity: BTreeMap::new(),
            aggregates: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ExternalConfig {
    pub capture_args: CaptureArgs,
    pub max_arg_length: usize,
    pub redact: Vec<String>,
    pub rules: Vec<ExternalOperationRule>,
    pub show_in_hidden_vendor: bool,
}
impl Default for ExternalConfig {
    fn default() -> Self {
        Self {
            capture_args: CaptureArgs::Literal,
            max_arg_length: 160,
            redact: Vec::new(),
            rules: Vec::new(),
            show_in_hidden_vendor: true,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureArgs {
    None,
    #[default]
    Literal,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct MetricsConfig {
    pub count_implicit_receiver: bool,
    pub count_variadic_parameters: bool,
    pub count_private_methods: bool,
    pub count_accessors: bool,
    pub count_nested_functions: bool,
    pub histogram_bins: usize,
}
impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            count_implicit_receiver: false,
            count_variadic_parameters: true,
            count_private_methods: true,
            count_accessors: true,
            count_nested_functions: true,
            histogram_bins: 10,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct GraphConfig {
    pub max_implementations: usize,
}
impl Default for GraphConfig {
    fn default() -> Self {
        Self {
            max_implementations: 50,
        }
    }
}

/// Static resolver inputs. Missing Rust inputs mean unknown, rather than the
/// machine running the analyzer determining the analyzed project's target.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ResolutionConfig {
    pub node_conditions: Vec<String>,
    pub rust: RustResolutionConfig,
}
impl Default for ResolutionConfig {
    fn default() -> Self {
        Self {
            node_conditions: vec!["node".into()],
            rust: RustResolutionConfig::default(),
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RustResolutionConfig {
    pub target: Option<String>,
    pub features: Option<Vec<String>>,
    pub cfg: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheConfig {
    pub enabled: bool,
    pub path: String,
}
impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: ".analyzer-cache".into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputConfig {
    pub formats: Vec<String>,
}
impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            formats: vec!["console".into()],
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ConfigDiagnostic {
    pub path: String,
    pub message: String,
    pub source: Option<String>,
    pub line: Option<usize>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConfigLoad {
    pub config: Config,
    pub resolved: Value,
    pub provenance: BTreeMap<String, SourceLocation>,
    pub diagnostics: Vec<ConfigDiagnostic>,
    pub skipped_keys: Vec<String>,
    pub skipped_files: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SourceLocation {
    pub file: String,
    pub line: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read root config: {0}")]
    Io(#[from] std::io::Error),
    #[error("root config syntax error: {0}")]
    Syntax(#[from] serde_yaml::Error),
    #[error("strict config error: {0}")]
    Strict(String),
}

pub use schema::{Handler, KeySpec, Validator};
mod schema;

#[derive(Clone, Copy)]
struct ValidationMode {
    strict: bool,
    verbose: bool,
}

pub struct ConfigLoader {
    keys: BTreeMap<String, KeySpec>,
    aliases: BTreeMap<String, String>,
    metric_ids: std::collections::BTreeSet<String>,
    language_ids: std::collections::BTreeSet<String>,
}
impl Default for ConfigLoader {
    fn default() -> Self {
        Self::new()
    }
}
impl ConfigLoader {
    pub fn new() -> Self {
        let mut loader = Self {
            keys: BTreeMap::new(),
            aliases: BTreeMap::new(),
            metric_ids: std::collections::BTreeSet::new(),
            language_ids: std::collections::BTreeSet::new(),
        };
        loader.register_builtin();
        for id in crate::metric_ids::builtin_metric_ids() {
            loader.register_metric(id, "Built-in metric");
        }
        loader
    }
    pub fn register_key(&mut self, spec: KeySpec) {
        if let Some(id) = spec.name.strip_prefix("limits.") {
            if !id.contains('.')
                && !id.contains('*')
                && !matches!(
                    id,
                    "by_language" | "by_path" | "by_entity_glob" | "by_entity" | "aggregates"
                )
            {
                self.metric_ids.insert(id.into());
            }
        }
        self.keys.insert(spec.name.clone(), spec);
    }
    pub fn register_metric(&mut self, id: &str, description: &str) {
        self.metric_ids.insert(id.into());
        let validator: Validator = Arc::new(|v| {
            if v.is_mapping() {
                Ok(())
            } else {
                Err("expected {warn, error, enabled} mapping".into())
            }
        });
        self.register_key(KeySpec::new(format!("limits.{id}"), description, validator));
    }
    pub fn register_language(&mut self, id: &str) {
        self.language_ids.insert(id.into());
    }
    pub fn register_alias(&mut self, old: impl Into<String>, new: impl Into<String>) {
        self.aliases.insert(old.into(), new.into());
    }
    pub fn keys(&self) -> Vec<&KeySpec> {
        self.keys.values().collect()
    }
    pub fn load(
        &self,
        path: &Path,
        strict: bool,
        verbose: bool,
    ) -> Result<ConfigLoad, ConfigError> {
        let source = std::fs::read_to_string(path)?;
        let mut raw: Value = serde_yaml::from_str(&source)?;
        // Launcher settings belong to the primary file, never to analysis includes.
        let core = raw
            .as_mapping_mut()
            .and_then(|map| map.remove(Value::String("core".into())));
        let mut state = LoadState::default();
        let mut stack = Vec::new();
        let mut resolved = self.expand(raw, path, &mut stack, 0, &mut state);
        if let (Some(core), Some(map)) = (core, resolved.as_mapping_mut()) {
            for key in ["core", "core.version"] {
                state.provenance.insert(
                    key.into(),
                    SourceLocation {
                        file: path.display().to_string(),
                        line: find_key_line(path, key),
                    },
                );
            }
            map.insert(Value::String("core".into()), core);
        }
        // The short spelling from the specification is equivalent to languages.enabled.
        if let Value::Mapping(map) = &mut resolved {
            let key = Value::String("languages".into());
            if let Some(Value::Sequence(enabled)) = map.get(&key).cloned() {
                let mut nested = Mapping::new();
                nested.insert(Value::String("enabled".into()), Value::Sequence(enabled));
                map.insert(key, Value::Mapping(nested));
            }
        }
        for (old, new) in &self.aliases {
            if let Some(value) = take_path(&mut resolved, old) {
                if lookup_path(&resolved, new).is_none() {
                    insert_path(&mut resolved, new, value);
                }
                state.diagnostics.push(ConfigDiagnostic {
                    path: old.clone(),
                    message: format!("deprecated key; use {new}"),
                    source: state.provenance.get(old).map(|s| s.file.clone()),
                    line: state.provenance.get(old).map(|s| s.line),
                });
            }
        }
        self.normalize_languages(&mut resolved, &mut state);
        let mut value = resolved.clone();
        self.validate(&mut value, "", &mut state, strict, verbose);
        let mut config = Config::default();
        // Merge the YAML over serialized defaults so missing nested values keep their defaults.
        let mut defaults = serde_yaml::to_value(&config).expect("default config serializes");
        merge(&mut defaults, value.clone());
        match serde_yaml::from_value::<Config>(defaults.clone()) {
            Ok(c) => config = c,
            Err(err) => {
                state.diagnostics.push(ConfigDiagnostic {
                    path: "".into(),
                    message: format!("invalid config value: {err}"),
                    source: Some(path.display().to_string()),
                    line: None,
                });
            }
        }
        for (name, spec) in &self.keys {
            if let Some(handler) = &spec.handler {
                if let Some(v) = lookup_path(&value, name) {
                    handler(&mut config, v);
                }
            }
        }
        if strict && !state.skipped_keys.is_empty() {
            return Err(ConfigError::Strict(format!(
                "unknown keys: {}",
                state.skipped_keys.join(", ")
            )));
        }
        if !verbose && !strict {
            state
                .diagnostics
                .retain(|d| !d.message.starts_with("unknown key"));
        }
        Ok(ConfigLoad {
            config,
            resolved: defaults,
            provenance: state.provenance,
            diagnostics: state.diagnostics,
            skipped_keys: state.skipped_keys,
            skipped_files: state.skipped_files,
        })
    }
    fn normalize_languages(&self, resolved: &mut Value, state: &mut LoadState) {
        let Some(languages) = resolved
            .as_mapping_mut()
            .and_then(|m| m.get_mut(Value::String("languages".into())))
        else {
            return;
        };
        let Some(map) = languages.as_mapping_mut() else {
            return;
        };
        if let Some(Value::Sequence(enabled)) = map.get_mut(Value::String("enabled".into())) {
            enabled.retain_mut(|value| {
                let Some(id) = value.as_str() else {
                    return true;
                };
                let canonical = match id {
                    "ts" => "typescript",
                    "js" => "javascript",
                    other => other,
                };
                if !self.language_ids.is_empty() && !self.language_ids.contains(canonical) {
                    state.diagnostics.push(ConfigDiagnostic {
                        path: "languages.enabled".into(),
                        message: format!("unknown language skipped: {id}"),
                        source: state
                            .provenance
                            .get("languages.enabled")
                            .or_else(|| state.provenance.get("languages"))
                            .map(|s| s.file.clone()),
                        line: state
                            .provenance
                            .get("languages.enabled")
                            .or_else(|| state.provenance.get("languages"))
                            .map(|s| s.line),
                    });
                    false
                } else {
                    *value = Value::String(canonical.into());
                    true
                }
            });
        }
        if let Some(Value::Mapping(extensions)) = map.get_mut(Value::String("extensions".into())) {
            for (extension, value) in extensions.iter_mut() {
                let Some(id) = value.as_str() else { continue };
                let canonical = match id {
                    "ts" => "typescript",
                    "js" => "javascript",
                    other => other,
                };
                if !self.language_ids.is_empty() && !self.language_ids.contains(canonical) {
                    state.diagnostics.push(ConfigDiagnostic {
                        path: format!("languages.extensions.{}", extension.as_str().unwrap_or("")),
                        message: format!("unknown language skipped: {id}"),
                        source: None,
                        line: None,
                    });
                    *value = Value::Null;
                } else {
                    *value = Value::String(canonical.into());
                }
            }
            extensions.retain(|_, value| !value.is_null());
        }
    }
    fn register_builtin(&mut self) {
        self.register_key(KeySpec::new(
            "core.version",
            "Core release: latest or X.Y.Z",
            Arc::new(|v| {
                if v.as_str().is_some_and(|s| {
                    s == "latest" || {
                        let parts: Vec<_> = s.split('.').collect();
                        parts.len() == 3
                            && parts
                                .iter()
                                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                    }
                }) {
                    Ok(())
                } else {
                    Err("expected latest or X.Y.Z".into())
                }
            }),
        ));
        for key in algorithmic::keys().into_iter().chain(architecture::keys()) {
            self.register_key(key);
        }
        let any: Validator = Arc::new(|_| Ok(()));
        for key in [
            "config_version",
            "core",
            "paths",
            "paths.include",
            "paths.exclude",
            "languages",
            "languages.enabled",
            "languages.extensions",
            "languages.extensions.*",
            "vendor",
            "vendor.paths",
            "vendor.mode",
            "vendor.parse",
            "limits",
            "limits.*",
            "limits.by_language",
            "limits.by_language.*",
            "limits.by_language.*.*",
            "limits.by_path",
            "limits.by_path.*",
            "limits.by_path.*.glob",
            "limits.by_path.*.*",
            "limits.by_entity_glob",
            "limits.by_entity_glob.*",
            "limits.by_entity_glob.*.glob",
            "limits.by_entity_glob.*.*",
            "limits.by_entity",
            "limits.by_entity.*",
            "limits.by_entity.*.*",
            "limits.aggregates",
            "limits.aggregates.*",
            "sets",
            "sets.*",
            "external",
            "external.capture_args",
            "external.max_arg_length",
            "external.redact",
            "external.rules",
            "external.show_in_hidden_vendor",
            "metrics",
            "metrics.count_implicit_receiver",
            "metrics.count_variadic_parameters",
            "metrics.count_private_methods",
            "metrics.count_accessors",
            "metrics.count_nested_functions",
            "metrics.histogram_bins",
            "graph",
            "graph.max_implementations",
            "resolution",
            "resolution.node_conditions",
            "resolution.rust",
            "resolution.rust.target",
            "resolution.rust.features",
            "resolution.rust.cfg",
            "cache",
            "cache.enabled",
            "cache.path",
            "output",
            "output.formats",
        ] {
            self.register_key(KeySpec::new(key, "Built-in configuration key", any.clone()));
        }
        // Validate container shapes before walking their children. Otherwise a bad
        // section can make deserialization discard every valid sibling setting.
        for path in [
            "core",
            "paths",
            "languages",
            "languages.extensions",
            "vendor",
            "limits",
            "limits.by_language",
            "limits.by_language.*",
            "limits.by_entity",
            "limits.aggregates",
            "sets",
            "external",
            "metrics",
            "graph",
            "resolution",
            "resolution.rust",
            "cache",
            "output",
        ] {
            self.register_key(KeySpec::new(
                path,
                "Mapping",
                Arc::new(|v| {
                    if v.is_mapping() {
                        Ok(())
                    } else {
                        Err("expected mapping".into())
                    }
                }),
            ));
        }
        for path in [
            "cache.path",
            "resolution.rust.target",
            "languages.extensions.*",
            "limits.by_path.*.glob",
            "limits.by_entity_glob.*.glob",
            "external.rules.*.category",
            "external.rules.*.pattern",
            "external.rules.*.library",
            "external.rules.*.operation",
        ] {
            let optional = path == "resolution.rust.target";
            self.register_key(KeySpec::new(
                path,
                "String",
                Arc::new(move |v| {
                    if v.is_string() || (optional && v.is_null()) {
                        Ok(())
                    } else {
                        Err("expected string".into())
                    }
                }),
            ));
        }
        for path in ["limits.by_path", "limits.by_entity_glob"] {
            self.register_key(KeySpec::new(
                path,
                "Ordered limit list",
                Arc::new(|v| {
                    if v.as_sequence()
                        .is_some_and(|items| items.iter().all(Value::is_mapping))
                    {
                        Ok(())
                    } else {
                        Err("expected list of mappings".into())
                    }
                }),
            ));
            self.register_key(KeySpec::new(
                format!("{path}.*"),
                "Limit mapping",
                Arc::new(|v| {
                    if v.is_mapping() {
                        Ok(())
                    } else {
                        Err("expected limit mapping".into())
                    }
                }),
            ));
        }
        self.register_key(KeySpec::new("external.rules", "External operation catalog", Arc::new(|v| {
            if v.as_sequence().is_some_and(|items| items.iter().all(|item| {
                item.as_mapping().is_some_and(|map| ["category", "pattern", "library", "operation"]
                    .iter().all(|field| map.get(Value::String((*field).into())).is_some_and(Value::is_string)))
            })) { Ok(()) } else { Err("expected list of rules with string category, pattern, library and operation".into()) }
        })));
        for path in [
            "paths.include",
            "paths.exclude",
            "languages.enabled",
            "vendor.paths",
            "external.redact",
            "output.formats",
            "resolution.node_conditions",
            "resolution.rust.features",
            "resolution.rust.cfg",
            "sets.*",
        ] {
            let optional = path.starts_with("resolution.rust.");
            self.register_key(KeySpec::new(
                path,
                "String list",
                Arc::new(move |v| {
                    if (optional && v.is_null())
                        || v.as_sequence()
                            .is_some_and(|items| items.iter().all(Value::is_string))
                    {
                        Ok(())
                    } else {
                        Err("expected list of strings".into())
                    }
                }),
            ));
        }
        for path in [
            "metrics.count_implicit_receiver",
            "metrics.count_variadic_parameters",
            "metrics.count_private_methods",
            "metrics.count_accessors",
            "metrics.count_nested_functions",
            "cache.enabled",
            "external.show_in_hidden_vendor",
        ] {
            self.register_key(KeySpec::new(
                path,
                "Boolean",
                Arc::new(|v| {
                    if v.is_bool() {
                        Ok(())
                    } else {
                        Err("expected boolean".into())
                    }
                }),
            ));
        }
        for (path, options) in [
            ("vendor.mode", vec!["collapsed", "expanded", "hidden"]),
            ("vendor.parse", vec!["lazy", "full", "none"]),
            ("external.capture_args", vec!["none", "literal"]),
        ] {
            self.register_key(KeySpec::new(
                path,
                "Enum",
                Arc::new(move |v| {
                    if v.as_str().is_some_and(|s| options.contains(&s)) {
                        Ok(())
                    } else {
                        Err(format!("expected one of {}", options.join(", ")))
                    }
                }),
            ));
        }
        for path in [
            "external.max_arg_length",
            "metrics.histogram_bins",
            "graph.max_implementations",
            "config_version",
        ] {
            self.register_key(KeySpec::new(
                path,
                "Positive integer",
                Arc::new(|v| {
                    if v.as_u64().is_some_and(|n| n > 0) {
                        Ok(())
                    } else {
                        Err("expected positive integer".into())
                    }
                }),
            ));
        }
        for path in [
            "limits.*.warn",
            "limits.*.error",
            "limits.by_language.*.*.warn",
            "limits.by_language.*.*.error",
            "limits.by_path.*.*.warn",
            "limits.by_path.*.*.error",
            "limits.by_entity_glob.*.*.warn",
            "limits.by_entity_glob.*.*.error",
            "limits.by_entity.*.*.warn",
            "limits.by_entity.*.*.error",
            "limits.aggregates.*.warn",
            "limits.aggregates.*.error",
        ] {
            self.register_key(KeySpec::new(
                path,
                "Nonnegative threshold",
                Arc::new(|v| {
                    if v.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0) {
                        Ok(())
                    } else {
                        Err("expected nonnegative number".into())
                    }
                }),
            ));
        }
        for path in [
            "limits.*.enabled",
            "limits.by_language.*.*.enabled",
            "limits.by_path.*.*.enabled",
            "limits.by_entity_glob.*.*.enabled",
            "limits.by_entity.*.*.enabled",
            "limits.aggregates.*.enabled",
        ] {
            self.register_key(KeySpec::new(
                path,
                "Switch the rule off (false) or back on (true) at this level",
                Arc::new(|v| {
                    if v.as_bool().is_some() {
                        Ok(())
                    } else {
                        Err("expected boolean".into())
                    }
                }),
            ));
        }
        let threshold: Validator = Arc::new(|v| {
            if v.is_mapping() {
                Ok(())
            } else {
                Err("expected {warn, error, enabled} mapping".into())
            }
        });
        for path in [
            "limits.*",
            "limits.by_language.*.*",
            "limits.by_path.*.*",
            "limits.by_entity_glob.*.*",
            "limits.by_entity.*.*",
            "limits.aggregates.*",
        ] {
            self.register_key(KeySpec::new(path, "Metric threshold", threshold.clone()));
        }
        if let Some(spec) = self.keys.get_mut("metrics.count_variadic_parameters") {
            spec.description =
                "Count each declared variadic/rest parameter in function_params and constructor_params".into();
            spec.default = Some(Value::Bool(true));
        }
        self.register_alias("exclude", "paths.exclude");
    }
    fn expand(
        &self,
        value: Value,
        file: &Path,
        stack: &mut Vec<PathBuf>,
        depth: usize,
        state: &mut LoadState,
    ) -> Value {
        self.expand_at(value, file, stack, depth, state, "")
    }
    fn expand_at(
        &self,
        value: Value,
        file: &Path,
        stack: &mut Vec<PathBuf>,
        depth: usize,
        state: &mut LoadState,
        path: &str,
    ) -> Value {
        if depth > 10 {
            state.warn("include", "include depth exceeds 10", file);
            return Value::Null;
        }
        let canonical = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
        if stack.contains(&canonical) {
            state.warn("include", "cyclic include skipped", file);
            state.skipped_files.push(file.display().to_string());
            return Value::Null;
        }
        stack.push(canonical);
        let expanded = self.expand_inner(value, file, stack, depth, state, path);
        stack.pop();
        expanded
    }
    fn expand_inner(
        &self,
        value: Value,
        file: &Path,
        stack: &mut Vec<PathBuf>,
        depth: usize,
        state: &mut LoadState,
        path: &str,
    ) -> Value {
        loading::Expansion {
            loader: self,
            file,
            stack,
            depth,
            state,
            path,
        }
        .value(value)
    }
    fn validate(
        &self,
        value: &mut Value,
        path: &str,
        state: &mut LoadState,
        strict: bool,
        verbose: bool,
    ) {
        self.validate_at(value, path, path, state, strict, verbose);
    }
    fn validate_at(
        &self,
        value: &mut Value,
        path: &str,
        schema_path: &str,
        state: &mut LoadState,
        strict: bool,
        verbose: bool,
    ) {
        if let Value::Mapping(map) = value {
            let keys: Vec<Value> = map.keys().cloned().collect();
            for k in keys {
                self.validate_entry(
                    map,
                    k,
                    path,
                    schema_path,
                    state,
                    ValidationMode { strict, verbose },
                );
            }

            validate_growth_limits(map, path, schema_path, state);
            if schema_path == "architecture" {
                sanitize_architecture_relations(map, path, state);
            }
        }
    }
    fn validate_entry(
        &self,
        map: &mut Mapping,
        k: Value,
        path: &str,
        schema_path: &str,
        state: &mut LoadState,
        mode: ValidationMode,
    ) {
        let ValidationMode { strict, verbose } = mode;
        let Some(name) = k.as_str() else {
            map.remove(&k);
            state.unknown(path, strict, verbose);
            return;
        };
        let p = child_path(path, name);
        // Entity names may contain dots. Keep their diagnostic/provenance
        // paths intact while treating the name as one schema map key.
        let schema_key = if matches!(
            schema_path,
            "algorithmic_complexity.by_entity" | "algorithmic_complexity.by_language"
        ) {
            child_path(schema_path, "*")
        } else {
            child_path(schema_path, name)
        };
        if path == "limits.by_entity" {
            if let Some(v) = map.get_mut(&k) {
                if v.is_mapping() {
                    self.validate_at(v, &p, &schema_key, state, strict, verbose);
                } else {
                    map.remove(&k);
                    state.invalid(&p, "expected entity limit mapping");
                }
            }
            return;
        }
        if let Some(metric) = metric_name_at(path, name, map.get(&k).unwrap()) {
            self.validate_metric(map, &k, metric, &p, state, mode);
            return;
        }
        let Some(spec) = self.match_key(&schema_key) else {
            map.remove(&k);
            state.unknown(&p, strict, verbose);
            return;
        };
        if let Some(v) = map.get_mut(&k) {
            if let Err(message) = (spec.validator)(v) {
                map.remove(&k);
                state.invalid(&p, &message);
                return;
            }
            match v {
                Value::Mapping(_) => self.validate_at(v, &p, &schema_key, state, strict, verbose),
                Value::Sequence(items) => {
                    self.validate_items(items, &p, &schema_key, state, strict, verbose);
                }
                _ => {}
            }
        }
    }

    fn validate_metric(
        &self,
        map: &mut Mapping,
        k: &Value,
        metric: &str,
        p: &str,
        state: &mut LoadState,
        mode: ValidationMode,
    ) {
        let ValidationMode { strict, verbose } = mode;
        if !self.metric_ids.contains(metric) {
            map.remove(k);
            state.unknown(p, strict, verbose);
            return;
        }
        let Some(v) = map.get_mut(k) else { return };
        if !v.is_mapping() {
            map.remove(k);
            state.invalid(p, "expected {warn, error, enabled} mapping");
            return;
        }
        let threshold = v.as_mapping_mut().unwrap();
        for field in threshold.keys().cloned().collect::<Vec<_>>() {
            let field_name = field.as_str().unwrap_or("");
            let full = child_path(p, field_name);
            let value = threshold.get(&field);
            let valid = match field_name {
                "warn" | "error" => value
                    .and_then(Value::as_f64)
                    .is_some_and(|n| n.is_finite() && n >= 0.0),
                "enabled" => value.and_then(Value::as_bool).is_some(),
                _ => false,
            };
            if !valid {
                threshold.remove(&field);
                match field_name {
                    "warn" | "error" => state.invalid(&full, "expected nonnegative number"),
                    "enabled" => state.invalid(&full, "expected boolean"),
                    _ => state.unknown(&full, strict, verbose),
                }
            }
        }
    }
    fn validate_items(
        &self,
        items: &mut Vec<Value>,
        p: &str,
        schema_key: &str,
        state: &mut LoadState,
        strict: bool,
        verbose: bool,
    ) {
        let mut kept = Vec::with_capacity(items.len());
        let mut kept_indices = Vec::with_capacity(items.len());
        for (i, mut item) in std::mem::take(items).into_iter().enumerate() {
            let item_path = child_path(p, &i.to_string());
            let item_schema = child_path(schema_key, &i.to_string());
            if let Some(spec) = self.match_key(&item_schema) {
                if let Err(message) = (spec.validator)(&item) {
                    if message.starts_with("unknown architecture rule") {
                        state.unknown(&item_path, strict, verbose);
                    } else {
                        state.invalid(&item_path, &message);
                    }
                    continue;
                }
            }
            if item.is_mapping() {
                self.validate_at(&mut item, &item_path, &item_schema, state, strict, verbose);
            }
            if let Some(message) = dependent_item_error(&item_schema, &item) {
                state.invalid(&item_path, &message);
                continue;
            }
            kept_indices.push(i);
            kept.push(item);
        }
        state.reindex_sequence(p, &kept_indices);
        *items = kept;
    }
    fn match_key(&self, path: &str) -> Option<&KeySpec> {
        self.keys.get(path).or_else(|| {
            self.keys
                .iter()
                .filter(|(pattern, _)| wildcard_match(pattern, path))
                .max_by_key(|(pattern, _)| pattern.len())
                .map(|(_, spec)| spec)
        })
    }
}

fn validate_growth_limits(map: &mut Mapping, path: &str, schema_path: &str, state: &mut LoadState) {
    if schema_path.starts_with("algorithmic_complexity.") && schema_path.ends_with(".limits") {
        let warn = map
            .get(Value::String("warn".into()))
            .and_then(Value::as_str);
        let error = map
            .get(Value::String("error".into()))
            .and_then(Value::as_str);
        if let (Some(warn), Some(error)) = (warn, error) {
            if let (Ok(warn), Ok(error)) = (
                crate::algorithmic::model::parse_limit(warn),
                crate::algorithmic::model::parse_limit(error),
            ) {
                if warn > error {
                    map.remove(Value::String("warn".into()));
                    state.invalid(&child_path(path, "warn"), "warn must not exceed error");
                }
            }
        }
    }
}

#[derive(Default)]
struct LoadState {
    provenance: BTreeMap<String, SourceLocation>,
    diagnostics: Vec<ConfigDiagnostic>,
    skipped_keys: Vec<String>,
    skipped_files: Vec<String>,
}
impl LoadState {
    fn reindex_sequence(&mut self, path: &str, kept_indices: &[usize]) {
        let prefix = format!("{path}.");
        let indices = kept_indices
            .iter()
            .enumerate()
            .map(|(new, old)| (*old, new))
            .collect::<BTreeMap<_, _>>();
        self.provenance = std::mem::take(&mut self.provenance)
            .into_iter()
            .filter_map(|(key, location)| {
                let Some(rest) = key.strip_prefix(&prefix) else {
                    return Some((key, location));
                };
                let (index, tail) = rest.split_once('.').unwrap_or((rest, ""));
                let Ok(index) = index.parse::<usize>() else {
                    return Some((key, location));
                };
                indices.get(&index).map(|new| {
                    (
                        format!(
                            "{prefix}{new}{}",
                            if tail.is_empty() {
                                String::new()
                            } else {
                                format!(".{tail}")
                            }
                        ),
                        location,
                    )
                })
            })
            .collect();
    }
    fn invalid(&mut self, path: &str, message: &str) {
        self.diagnostics.push(ConfigDiagnostic {
            path: path.into(),
            message: format!("invalid value, default used: {message}"),
            source: self.provenance.get(path).map(|s| s.file.clone()),
            line: self.provenance.get(path).map(|s| s.line),
        });
    }
    fn unknown(&mut self, path: &str, strict: bool, verbose: bool) {
        self.skipped_keys.push(path.into());
        if strict || verbose {
            self.diagnostics.push(ConfigDiagnostic {
                path: path.into(),
                message: "unknown key skipped".into(),
                source: self.provenance.get(path).map(|s| s.file.clone()),
                line: self.provenance.get(path).map(|s| s.line),
            });
        }
    }
    fn warn(&mut self, path: &str, message: &str, file: &Path) {
        self.diagnostics.push(ConfigDiagnostic {
            path: path.into(),
            message: message.into(),
            source: Some(file.display().to_string()),
            line: None,
        });
    }
}

fn child_path(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.into()
    } else {
        format!("{path}.{name}")
    }
}
fn dependent_item_error(schema_path: &str, value: &Value) -> Option<String> {
    let map = value.as_mapping()?;
    let field = |name: &str| map.get(Value::String(name.into()));
    if wildcard_match("limits.by_entity_glob.*", schema_path) {
        match field("glob").and_then(Value::as_str) {
            Some(pattern) if !pattern.is_empty() => {
                entity_glob(pattern).err().map(|e| e.to_string())
            }
            _ => Some("entity override requires a nonempty valid glob".into()),
        }
    } else if wildcard_match("algorithmic_complexity.by_path.*", schema_path)
        && field("glob").is_none()
    {
        Some("path override requires a valid glob".into())
    } else if (wildcard_match("architecture.layers.*", schema_path)
        || wildcard_match("architecture.modules.*", schema_path))
        && (field("name").is_none()
            || !field("selectors")
                .and_then(Value::as_sequence)
                .is_some_and(|s| !s.is_empty()))
    {
        Some(
            if schema_path.starts_with("architecture.modules.") {
                "module requires a nonempty name and selectors"
            } else {
                "layer requires a nonempty name and selectors"
            }
            .into(),
        )
    } else if wildcard_match("architecture.forbidden_dependencies.*", schema_path)
        && (field("from").is_none() || field("to").is_none())
    {
        Some("boundary requires from and to layer names".into())
    } else {
        None
    }
}
fn sanitize_architecture_relations(map: &mut Mapping, path: &str, state: &mut LoadState) {
    if let Some(Value::Sequence(modules)) = map.get_mut(Value::String("modules".into())) {
        let mut names = std::collections::BTreeSet::new();
        let mut kept_indices = Vec::new();
        let mut index = 0;
        modules.retain(|module| {
            let name = module
                .as_mapping()
                .unwrap()
                .get(Value::String("name".into()))
                .unwrap()
                .as_str()
                .unwrap();
            let keep = names.insert(name.to_owned());
            if keep {
                kept_indices.push(index);
            } else {
                state.invalid(
                    &format!("{path}.modules.{index}.name"),
                    "module name must be unique",
                );
            }
            index += 1;
            keep
        });
        state.reindex_sequence(&format!("{path}.modules"), &kept_indices);
    }
    let mut names = std::collections::BTreeSet::new();
    if let Some(Value::Sequence(layers)) = map.get_mut(Value::String("layers".into())) {
        let mut index = 0;
        let mut kept_indices = Vec::new();
        layers.retain(|layer| {
            let name = layer
                .as_mapping()
                .unwrap()
                .get(Value::String("name".into()))
                .unwrap()
                .as_str()
                .unwrap();
            let keep = names.insert(name.to_string());
            if !keep {
                state.invalid(
                    &format!("{path}.layers.{index}.name"),
                    "layer name must be unique",
                );
            }
            if keep {
                kept_indices.push(index);
            }
            index += 1;
            keep
        });
        state.reindex_sequence(&format!("{path}.layers"), &kept_indices);
    }
    if let Some(Value::Sequence(boundaries)) =
        map.get_mut(Value::String("forbidden_dependencies".into()))
    {
        let mut index = 0;
        let mut kept_indices = Vec::new();
        boundaries.retain(|boundary| {
            let fields = boundary.as_mapping().unwrap();
            let from = fields
                .get(Value::String("from".into()))
                .unwrap()
                .as_str()
                .unwrap();
            let to = fields
                .get(Value::String("to".into()))
                .unwrap()
                .as_str()
                .unwrap();
            let keep = from != to && names.contains(from) && names.contains(to);
            if !keep {
                state.invalid(
                    &format!("{path}.forbidden_dependencies.{index}"),
                    "boundary must name two different configured layers",
                );
            }
            if keep {
                kept_indices.push(index);
            }
            index += 1;
            keep
        });
        state.reindex_sequence(&format!("{path}.forbidden_dependencies"), &kept_indices);
    }
}

fn merge_item_provenance(
    state: &mut LoadState,
    local: LoadState,
    path: &str,
    source_index: usize,
    target_index: usize,
) {
    let source_prefix = format!("{path}.{source_index}");
    let target_prefix = format!("{path}.{target_index}");
    for (key, location) in local.provenance {
        let mapped = key
            .strip_prefix(&source_prefix)
            .map(|tail| format!("{target_prefix}{tail}"))
            .unwrap_or(key);
        state.provenance.insert(mapped, location);
    }
    state.diagnostics.extend(local.diagnostics);
    state.skipped_keys.extend(local.skipped_keys);
    state.skipped_files.extend(local.skipped_files);
}
fn merge_indexed_provenance(state: &mut LoadState, local: LoadState, path: &str, offset: usize) {
    let prefix = if path.is_empty() {
        String::new()
    } else {
        format!("{path}.")
    };
    for (key, location) in local.provenance {
        let mapped = key
            .strip_prefix(&prefix)
            .and_then(|rest| {
                let (index, tail) = rest.split_once('.').unwrap_or((rest, ""));
                index.parse::<usize>().ok().map(|index| {
                    format!(
                        "{prefix}{}{}",
                        index + offset,
                        if tail.is_empty() {
                            String::new()
                        } else {
                            format!(".{tail}")
                        }
                    )
                })
            })
            .unwrap_or(key);
        state.provenance.insert(mapped, location);
    }
    state.diagnostics.extend(local.diagnostics);
    state.skipped_keys.extend(local.skipped_keys);
    state.skipped_files.extend(local.skipped_files);
}

fn include_patterns(v: Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s],
        Value::Sequence(a) => a
            .into_iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}
fn expand_glob(base: &Path, pattern: &str) -> Vec<PathBuf> {
    let candidate = base.join(pattern);
    if !pattern.contains(['*', '?', '[']) {
        return vec![candidate];
    }
    let glob = match globset::Glob::new(pattern) {
        Ok(g) => g.compile_matcher(),
        Err(_) => return vec![candidate],
    };
    let mut out = Vec::new();
    for item in ignore::Walk::new(base) {
        if let Ok(item) = item {
            if item.path().is_file() {
                if let Ok(rel) = item.path().strip_prefix(base) {
                    if glob.is_match(rel) {
                        out.push(item.path().to_path_buf());
                    }
                }
            }
        }
    }
    out.sort();
    out
}
fn find_key_line(file: &Path, path: &str) -> usize {
    let Ok(source) = std::fs::read_to_string(file) else {
        return 1;
    };
    let mut sink = MarkedEvents::default();
    if Parser::new_from_str(&source).load(&mut sink, true).is_err() {
        return 1;
    }
    let mut positions = BTreeMap::new();
    let mut cursor = 0;
    while cursor < sink.0.len() {
        if matches!(
            sink.0[cursor].0,
            Event::MappingStart(..) | Event::SequenceStart(..)
        ) {
            collect_key_lines(&sink.0, &mut cursor, "", &mut positions);
            break;
        }
        cursor += 1
    }
    if let Some(line) = positions.get(path) {
        return *line;
    }
    // Included documents start at their own root. Match the longest path
    // suffix, preserving sequence indices to disambiguate repeated keys.
    let parts = path.split('.').collect::<Vec<_>>();
    for start in 1..parts.len() {
        if let Some(line) = positions.get(&parts[start..].join(".")) {
            return *line;
        }
    }
    1
}
#[derive(Default)]
struct MarkedEvents(Vec<(Event, Marker)>);
impl MarkedEventReceiver for MarkedEvents {
    fn on_event(&mut self, event: Event, mark: Marker) {
        self.0.push((event, mark));
    }
}
fn collect_key_lines(
    events: &[(Event, Marker)],
    cursor: &mut usize,
    path: &str,
    lines: &mut BTreeMap<String, usize>,
) {
    if *cursor >= events.len() {
        return;
    }
    match &events[*cursor].0 {
        Event::MappingStart(..) => {
            *cursor += 1;
            while *cursor < events.len() && !matches!(events[*cursor].0, Event::MappingEnd) {
                let (key, line) = match &events[*cursor] {
                    (Event::Scalar(key, ..), mark) => (key.clone(), mark.line()),
                    _ => {
                        collect_key_lines(events, cursor, path, lines);
                        continue;
                    }
                };
                *cursor += 1;
                let next = if path.is_empty() {
                    key
                } else {
                    format!("{path}.{key}")
                };
                lines.insert(next.clone(), line);
                collect_key_lines(events, cursor, &next, lines);
            }
            *cursor += 1;
        }
        Event::SequenceStart(..) => {
            *cursor += 1;
            let mut index = 0;
            while *cursor < events.len() && !matches!(events[*cursor].0, Event::SequenceEnd) {
                let next = if path.is_empty() {
                    index.to_string()
                } else {
                    format!("{path}.{index}")
                };
                collect_key_lines(events, cursor, &next, lines);
                index += 1
            }
            *cursor += 1;
        }
        _ => *cursor += 1,
    }
}
fn wildcard_match(pattern: &str, path: &str) -> bool {
    let a: Vec<_> = pattern.split('.').collect();
    let b: Vec<_> = path.split('.').collect();
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| *x == "*" || *x == y)
}
fn metric_name_at<'a>(path: &str, name: &'a str, value: &Value) -> Option<&'a str> {
    if path == "limits" {
        return (!matches!(
            name,
            "by_language" | "by_path" | "by_entity_glob" | "by_entity" | "aggregates"
        ))
        .then_some(name);
    }
    if path == "limits.aggregates" {
        return name.rsplit_once('.').and_then(|(metric, stat)| {
            matches!(stat, "avg" | "median" | "max" | "p90" | "p95").then_some(metric)
        });
    }
    let parts = path.split('.').collect::<Vec<_>>();
    if parts.len() == 3
        && parts[0] == "limits"
        && matches!(parts[1], "by_language" | "by_path" | "by_entity_glob")
        && name != "glob"
    {
        return Some(name);
    }
    if path.starts_with("limits.by_entity.")
        && value
            .as_mapping()
            .is_some_and(|m| m.values().all(|v| !v.is_mapping()))
    {
        return Some(name);
    }
    None
}
fn lookup_path<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = v;
    for part in path.split('.') {
        cur = cur.as_mapping()?.get(Value::String(part.into()))?;
    }
    Some(cur)
}
fn take_path(v: &mut Value, path: &str) -> Option<Value> {
    let mut parts = path.split('.').collect::<Vec<_>>();
    let last = parts.pop()?;
    let mut cur = v;
    for p in parts {
        cur = cur.as_mapping_mut()?.get_mut(Value::String(p.into()))?;
    }
    cur.as_mapping_mut()?.remove(Value::String(last.into()))
}
fn insert_path(v: &mut Value, path: &str, value: Value) {
    let mut parts = path.split('.').collect::<Vec<_>>();
    let Some(last) = parts.pop() else { return };
    let mut cur = v;
    for p in parts {
        let Some(map) = cur.as_mapping_mut() else {
            return;
        };
        cur = map
            .entry(Value::String(p.into()))
            .or_insert_with(|| Value::Mapping(Mapping::new()));
    }
    if let Some(map) = cur.as_mapping_mut() {
        map.insert(Value::String(last.into()), value);
    }
}
pub fn merge(base: &mut Value, overlay: Value) {
    match (base, overlay) {
        (Value::Mapping(a), Value::Mapping(b)) => {
            for (k, v) in b {
                if let Some(old) = a.get_mut(&k) {
                    merge(old, v);
                } else {
                    a.insert(k, v);
                }
            }
        }
        (a, b) => *a = b,
    }
}

/// Register symbolic cost settings in a custom loader.
pub fn register_algorithmic_config(loader: &mut ConfigLoader) {
    for key in algorithmic::keys() {
        loader.register_key(key);
    }
}
/// Register architecture settings in a custom loader.
pub fn register_architecture_config(loader: &mut ConfigLoader) {
    for key in architecture::keys() {
        loader.register_key(key);
    }
}
