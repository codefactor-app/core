//! Ordered graph assembly. Symbol indexes and per-call resolution have separate lifetimes.
use super::*;
mod calls;
mod context;
use context::*;

pub(super) fn assemble(
    files: &[FileModel],
    registry: &LanguageRegistry,
    config: &Config,
    context: &crate::resolution::ResolutionContext,
) -> CallGraph {
    let cargo_files = cargo::models(files, context, config);
    let files = cargo_files.as_deref().unwrap_or(files);
    let cfg = CfgInputs::new(files, context, config);
    let active_files = cfg.active_files(files);
    let files = active_files.as_deref().unwrap_or(files);
    let mut index = ProjectIndex::new(
        files,
        registry,
        config,
        context,
        cfg.uncertain_functions(files),
    );
    let mut graph = CallGraph {
        source_packages: files
            .iter()
            .filter(|f| !f.vendor)
            .map(|f| {
                (
                    f.path.clone(),
                    context.source_package_names(&f.path, &f.language),
                )
            })
            .filter(|(_, names)| !names.is_empty())
            .collect(),
        ..CallGraph::default()
    };
    index.register_declarations(&mut graph);
    graph.resolution_diagnostics = index.inheritance.diagnostics.clone();
    index.resolve_calls(&mut graph);
    drop(index);
    graph.reindex();
    graph
}
struct CfgInputs<'a> {
    rust_configs: BTreeMap<String, crate::config::RustResolutionConfig>,
    context: &'a crate::resolution::ResolutionContext,
    config: &'a Config,
}
impl<'a> CfgInputs<'a> {
    fn new(
        files: &[FileModel],
        context: &'a crate::resolution::ResolutionContext,
        config: &'a Config,
    ) -> Self {
        let rust_configs = files
            .iter()
            .filter(|file| file.language == "rust")
            .map(|file| {
                (
                    file.module.clone(),
                    context.rust_config_for(
                        file.cargo_target.as_deref().unwrap_or(&file.path),
                        &config.resolution.rust,
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();
        Self {
            rust_configs,
            context,
            config,
        }
    }
    fn inactive(&self, file: &FileModel, conditions: &[String]) -> bool {
        let rust_configs = &self.rust_configs;
        let context = self.context;
        let config = self.config;

        let rust_config = rust_configs
            .get(&file.module)
            .unwrap_or(&config.resolution.rust);
        context.rust_root_enabled(
            file.cargo_target.as_deref().unwrap_or(&file.path),
            rust_config,
        ) == Some(false)
            || conditions.iter().any(|condition| {
                crate::resolution::evaluate_cfg(condition, rust_config) == Some(false)
            })
    }
    fn active_files(&self, files: &[FileModel]) -> Option<Vec<FileModel>> {
        let inactive = |f: &FileModel, c: &[String]| self.inactive(f, c);
        let active_files = files
            .iter()
            .any(|file| {
                file.language == "rust"
                    && (file
                        .functions
                        .iter()
                        .any(|function| inactive(file, &function.rust_cfg))
                        || file.types.iter().any(|ty| inactive(file, &ty.rust_cfg)))
            })
            .then(|| {
                files
                    .iter()
                    .map(|file| {
                        let mut file = file.clone();
                        if file.language == "rust" {
                            let file_context = file.clone();
                            file.functions
                                .retain(|function| !inactive(&file_context, &function.rust_cfg));
                            file.types
                                .retain(|ty| !inactive(&file_context, &ty.rust_cfg));
                        }
                        file
                    })
                    .collect::<Vec<_>>()
            });
        active_files
    }
    fn uncertain_functions(&self, files: &[FileModel]) -> HashSet<String> {
        let rust_configs = &self.rust_configs;
        let context = self.context;
        let config = self.config;
        let uncertain_functions = files
            .iter()
            .filter(|file| file.language == "rust")
            .flat_map(|file| {
                file.functions
                    .iter()
                    .filter(|function| {
                        let rust_config = rust_configs
                            .get(&file.module)
                            .unwrap_or(&config.resolution.rust);
                        context
                            .rust_root_enabled(
                                file.cargo_target.as_deref().unwrap_or(&file.path),
                                rust_config,
                            )
                            .is_none()
                            || function.rust_cfg.iter().any(|condition| {
                                crate::resolution::evaluate_cfg(condition, rust_config).is_none()
                            })
                    })
                    .map(|function| function_id(file, &function.fqn))
            })
            .collect::<HashSet<_>>();
        uncertain_functions
    }
}

impl<'a> ProjectIndex<'a> {
    fn new(
        files: &'a [FileModel],
        registry: &'a LanguageRegistry,
        config: &'a Config,
        context: &'a crate::resolution::ResolutionContext,
        uncertain_functions: HashSet<String>,
    ) -> Self {
        let project_modules = files
            .iter()
            .map(|f| (f.language.as_str(), f.module.as_str()))
            .collect::<BTreeSet<_>>();
        // JS/TS imports resolving here are project code, never a library package.
        let source_modules = files
            .iter()
            .filter(|f| !f.vendor && matches!(f.language.as_str(), "javascript" | "typescript"))
            .flat_map(|f| [f.module.as_str(), f.path.as_str()])
            .collect::<BTreeSet<_>>();
        let modules = files
            .iter()
            .flat_map(|file| {
                std::iter::once((file.language.as_str(), file.module.as_str()))
                    .chain(
                        matches!(file.language.as_str(), "javascript" | "typescript")
                            .then_some((file.language.as_str(), file.path.as_str())),
                    )
                    .chain(
                        file.types
                            .iter()
                            .map(|t| (file.language.as_str(), t.fqn.as_str())),
                    )
                    .chain(
                        file.functions
                            .iter()
                            .map(|f| (file.language.as_str(), f.fqn.as_str())),
                    )
            })
            .collect::<BTreeSet<_>>();
        let modules = ModuleIndex::new(modules)
            .with_registry(registry)
            .with_files(files, context, config);
        let file_refs = files.iter().collect::<Vec<_>>();
        let exports = export_bindings(&file_refs, &modules, None);
        let runtime_exports = export_bindings(&file_refs, &modules, Some(&exports));
        let types = known_types(files);
        let property_index = properties::PropertyIndex::new(files, &modules, &exports, &types);
        let inheritance = inheritance::InheritanceIndex::empty(registry);
        let mut index = Self {
            files,
            registry,
            config,
            context,
            uncertain_functions,
            browser: BrowserBindings::new(files, context),
            project_modules,
            source_modules,
            modules,
            exports,
            runtime_exports,
            types,
            property_index,
            inheritance,
            parents: BTreeMap::new(),
            external_bases: ExternalBases::default(),
            fields: BTreeMap::new(),
            constant_types: BTreeMap::new(),
            returns_by_id: BTreeMap::new(),
            implementations: BTreeMap::new(),
            rust_methods: HashMap::new(),
            rust_contracts: HashSet::new(),
            rust_defaults: HashSet::new(),
            functions: BTreeMap::new(),
        };
        index.index_hierarchy();
        index.index_fields();
        index
    }
    fn index_hierarchy(&mut self) {
        let files = self.files;
        let registry = self.registry;
        let modules = &self.modules;
        let exports = &self.exports;
        let types = &self.types;
        let source_modules = &self.source_modules;
        let mut parents = BTreeMap::<&str, BTreeMap<&str, Vec<String>>>::new();
        let mut external_bases = ExternalBases::default();
        for file in files {
            for t in &file.types {
                for base in &t.extends {
                    let resolved = resolve_type_reference(
                        file, base, &t.fqn, &t.range, modules, exports, types,
                    );
                    if types.contains(&(file.language.as_str(), resolved.as_str()))
                        || modules.native_receiver(&file.language, &resolved)
                        || file.language == "python"
                    {
                        parents
                            .entry(file.language.as_str())
                            .or_default()
                            .entry(t.fqn.as_str())
                            .or_default()
                            .push(resolved);
                    } else if matches!(file.language.as_str(), "javascript" | "typescript")
                        && resolved != *base
                        && (!resolved.starts_with(['<', '.', '/'])
                            || resolved.starts_with("<unresolved>::"))
                        && !resolved.starts_with("unresolved-import(")
                        && !source_modules.contains(resolved.as_str())
                        && !resolved
                            .char_indices()
                            .any(|(i, c)| c == '.' && source_modules.contains(&resolved[..i]))
                    {
                        external_bases
                            .bases
                            .entry((file.language.as_str(), t.fqn.as_str()))
                            .or_default()
                            .push(resolved);
                    }
                }
            }
        }
        if !external_bases.bases.is_empty() {
            for file in files {
                for t in &file.types {
                    if let Some(suffix) =
                        matches!(file.language.as_str(), "javascript" | "typescript")
                            .then(|| t.fqn.strip_prefix(&file.module))
                            .flatten()
                    {
                        external_bases.aliases.insert(
                            (file.language.as_str(), format!("{}{suffix}", file.path)),
                            t.fqn.as_str(),
                        );
                    }
                }
            }
        }
        let inheritance =
            inheritance::InheritanceIndex::new(files, &parents, registry, |file, ty, raw| {
                resolve_type_reference(file, raw, &ty.fqn, &ty.range, modules, exports, types)
            });
        self.parents = parents;
        self.external_bases = external_bases;
        self.inheritance = inheritance;
    }
    fn index_fields(&mut self) {
        let files = self.files;
        let modules = &self.modules;
        let exports = &self.exports;
        let types = &self.types;
        // Field types belong to their declaration's scope, including imported aliases.
        let mut fields = BTreeMap::<&str, BTreeMap<&str, BTreeMap<&str, Option<String>>>>::new();
        for file in files {
            for ty in &file.types {
                for field in &ty.fields {
                    if field.is_static {
                        continue;
                    }
                    let identity = field
                        .type_name
                        .as_ref()
                        .filter(|raw| modules.type_reference(file, raw).is_some())
                        .map(|raw| {
                            resolve_type_reference(
                                file,
                                raw,
                                &ty.fqn,
                                &field.range,
                                modules,
                                exports,
                                types,
                            )
                        });
                    // An untyped override still shadows a base field.
                    fields
                        .entry(file.language.as_str())
                        .or_default()
                        .entry(ty.fqn.as_str())
                        .or_default()
                        .insert(field.name.as_str(), identity);
                }
            }
        }
        // A native getter can provide the declared receiver type of a later
        // method/property operation. Setter-only descriptors still shadow bases.
        for file in files
            .iter()
            .filter(|file| matches!(file.language.as_str(), "javascript" | "typescript"))
        {
            for function in file.functions.iter().filter(|function| {
                function.parent_function.is_none()
                    && !function
                        .modifiers
                        .iter()
                        .any(|modifier| modifier == "static")
            }) {
                let Some(owner) = function.parent_type.as_deref() else {
                    continue;
                };
                let member = fields
                    .entry(file.language.as_str())
                    .or_default()
                    .entry(owner)
                    .or_default()
                    .entry(function.name.as_str())
                    .or_insert(None);
                if function
                    .modifiers
                    .iter()
                    .any(|modifier| modifier == "accessor")
                    && function.fqn.ends_with("#get")
                {
                    *member = function
                        .return_type
                        .as_ref()
                        .and_then(|raw| modules.type_reference(file, raw))
                        .map(|raw| {
                            resolve_type_reference(
                                file,
                                &raw,
                                &function.fqn,
                                &function.range,
                                modules,
                                exports,
                                types,
                            )
                        });
                }
            }
        }
        self.fields = fields;
        self.index_constant_types();
    }
    fn index_constant_types(&mut self) {
        let files = self.files;
        let modules = &self.modules;
        let exports = &self.exports;
        let types = &self.types;
        // Exported module constants resolve their type in the declaring module.
        let constant_types = files
            .iter()
            .flat_map(|file| {
                file.constants.iter().filter_map(|constant| {
                    let symbol = format!("{}.{}", file.module, constant.name);
                    let raw = constant.type_name.as_deref()?;
                    let ty = resolve_type_reference(
                        file,
                        raw,
                        &symbol,
                        &constant.range,
                        modules,
                        exports,
                        types,
                    );
                    // ESM imports name the declaring file (`src/db.ts.db`).
                    let alias = format!("{}.{}", file.path, constant.name);
                    let language = file.language.as_str();
                    Some([((language, symbol), ty.clone()), ((language, alias), ty)])
                })
            })
            .flatten()
            .collect::<BTreeMap<_, _>>();
        self.constant_types = constant_types;
    }
    fn register_declarations(&mut self, graph: &mut CallGraph) {
        for file in self.files {
            self.register_interfaces(file);
            for f in &file.functions {
                let id = function_id(file, &f.fqn);
                let owner = self.function_owner(file, f);
                self.register_rust_method(file, f, &id, &owner);
                self.register_node(file, f, &id, &owner, graph);
                self.register_function_aliases(file, f, &id, &owner);
                self.register_return_type(file, f, &id);
            }
        }
        let functions = &mut self.functions;
        let implementations = &mut self.implementations;
        let js = functions.get("javascript").cloned().unwrap_or_default();
        let ts = functions.get("typescript").cloned().unwrap_or_default();
        for (language, additions) in [("typescript", js), ("javascript", ts)] {
            for (symbol, targets) in additions {
                functions
                    .entry(language)
                    .or_default()
                    .entry(symbol)
                    .or_default()
                    .extend(targets);
            }
        }
        for language in implementations.values_mut() {
            for known in language.values_mut() {
                known.sort();
                known.dedup();
            }
        }
    }
    fn register_interfaces(&mut self, file: &'a FileModel) {
        let Self {
            modules,
            exports,
            types,
            implementations,
            ..
        } = self;
        for t in &file.types {
            for interface in &t.implements {
                let identity = resolve_type_reference(
                    file, interface, &t.fqn, &t.range, modules, exports, types,
                );
                implementations
                    .entry(file.language.as_str())
                    .or_default()
                    .entry(identity)
                    .or_default()
                    .push(if t.kind == TypeKind::Impl {
                        t.name
                            .rsplit_once(" for ")
                            .map(|(_, target)| {
                                resolve_type_reference(
                                    file, target, &t.fqn, &t.range, modules, exports, types,
                                )
                            })
                            .unwrap_or_else(|| t.fqn.clone())
                    } else {
                        t.fqn.clone()
                    });
            }
        }
    }
    fn function_owner(&mut self, file: &'a FileModel, f: &FunctionModel) -> Option<String> {
        let Self {
            modules,
            exports,
            types,
            ..
        } = self;
        let owner = f.parent_type.as_ref().map(|parent| {
            let target = if file.language == "rust"
                && !types.contains(&(file.language.as_str(), parent.as_str()))
            {
                parent
                    .strip_prefix(&format!("{}::", file.module))
                    .unwrap_or(parent)
            } else {
                parent.as_str()
            };
            resolve_type_reference(file, target, &f.fqn, &f.range, modules, exports, types)
        });

        owner
    }
    fn register_rust_method(
        &mut self,
        file: &'a FileModel,
        f: &FunctionModel,
        id: &str,
        owner: &Option<String>,
    ) {
        let Self {
            modules,
            exports,
            types,
            functions,
            rust_methods,
            rust_contracts,
            rust_defaults,
            ..
        } = self;
        if file.language == "rust" && f.parent_function.is_none() {
            if let Some(owner) = &owner {
                let contract = f.rust_trait.as_ref().map(|contract| {
                    rust_contract_identity(
                        file, contract, &f.fqn, &f.range, modules, exports, types,
                    )
                });
                let declaration = file
                    .types
                    .iter()
                    .any(|ty| ty.fqn == *owner && ty.kind == TypeKind::Trait);
                if declaration {
                    rust_contracts.insert(owner.clone());
                    if f.body_range != f.range {
                        rust_defaults.insert(id.to_owned());
                    }
                }
                rust_methods.insert(id.to_owned(), (owner.clone(), contract, declaration));
                functions
                    .entry("rust")
                    .or_default()
                    .entry(format!("{owner}::{}", f.name))
                    .or_default()
                    .push((id.to_owned(), file.path.clone()));
            }
        }
    }
    fn register_node(
        &mut self,
        file: &'a FileModel,
        f: &FunctionModel,
        id: &str,
        owner: &Option<String>,
        graph: &mut CallGraph,
    ) {
        let Self { context, .. } = self;
        graph.nodes.push(GraphNode {
            id: id.to_owned(),
            label: f.fqn.clone(),
            kind: NodeKind::Function,
            file: Some(file.path.clone()),
            line: Some(f.range.start_line),
            language: Some(file.language.clone()),
            module: Some(file.module.clone()),
            parent_type: owner.clone(),
            vendor: file.vendor,
            package: vendor_package(&file.path, file.vendor),
            category: None,
            library: None,
            operation: None,
            argument: None,
            algorithmic_complexity: None,
            cargo_target: file.cargo_target.as_deref().map(|target| {
                context
                    .rust_target_root(target)
                    .unwrap_or(target)
                    .to_owned()
            }),
            cargo_target_id: file.cargo_target.clone(),
            source_fqn: file
                .cargo_source_module
                .as_ref()
                .map(|source| f.fqn.replacen(&file.module, source, 1)),
        });
    }
    fn register_function_aliases(
        &mut self,
        file: &'a FileModel,
        f: &FunctionModel,
        id: &str,
        owner: &Option<String>,
    ) {
        let Self { functions, .. } = self;
        functions
            .entry(file.language.as_str())
            .or_default()
            .entry(f.fqn.clone())
            .or_default()
            .push((id.to_owned(), file.path.clone()));
        if matches!(file.language.as_str(), "javascript" | "typescript") {
            if let Some(suffix) = f.fqn.strip_prefix(&file.module) {
                let source_fqn = format!("{}{suffix}", file.path);
                functions
                    .entry(file.language.as_str())
                    .or_default()
                    .entry(source_fqn.clone())
                    .or_default()
                    .push((id.to_owned(), file.path.clone()));
                if f.accessor == Some(AccessorKind::Getter) {
                    if let Some(property) = source_fqn.strip_suffix("#get") {
                        functions
                            .entry(file.language.as_str())
                            .or_default()
                            .entry(property.to_owned())
                            .or_default()
                            .push((id.to_owned(), file.path.clone()));
                    }
                }
            }
        }
        if f.accessor == Some(AccessorKind::Getter) {
            if let Some(property) = f.fqn.strip_suffix("#get") {
                functions
                    .entry(file.language.as_str())
                    .or_default()
                    .entry(property.to_owned())
                    .or_default()
                    .push((id.to_owned(), file.path.clone()));
            }
        }
        if f.parent_function.is_none() {
            if let Some(owner) = owner
                .clone()
                .filter(|owner| Some(owner) != f.parent_type.as_ref())
            {
                let separator = if matches!(file.language.as_str(), "php" | "rust") {
                    "::"
                } else {
                    "."
                };
                functions
                    .entry(file.language.as_str())
                    .or_default()
                    .entry(format!("{owner}{separator}{}", f.name))
                    .or_default()
                    .push((id.to_owned(), file.path.clone()));
            }
        }
    }
    fn register_return_type(&mut self, file: &'a FileModel, f: &FunctionModel, id: &str) {
        let Self {
            modules,
            exports,
            types,
            returns_by_id,
            ..
        } = self;
        if let Some(ty) = f
            .return_type
            .as_ref()
            .filter(|ty| modules.type_reference(file, ty).is_some())
        {
            // A factory's return type belongs to its defining module,
            // not the caller's scope (which may import it under an alias).
            let resolved_type =
                resolve_type_reference(file, ty, &f.fqn, &f.range, modules, exports, types);
            returns_by_id.insert(id.to_owned(), resolved_type);
        }
    }
    fn resolve_calls(&self, graph: &mut CallGraph) {
        let modules = &self.modules;
        let runtime_exports = &self.runtime_exports;
        let files = self.files;
        let registry = self.registry;
        let config = self.config;
        let mut edge_ids = BTreeMap::<String, Arc<str>>::new();
        let mut generated = BTreeMap::<String, GraphNode>::new();
        let mut catalogs =
            BTreeMap::<String, Vec<(ExternalOperationRule, Option<globset::GlobMatcher>)>>::new();
        for file in files {
            let mut normalized_symbols = HashMap::new();
            let Some(strategy) = registry.get(&file.language) else {
                continue;
            };
            let rules = catalogs.entry(file.language.clone()).or_insert_with(|| {
                config
                    .external
                    .rules
                    .iter()
                    .cloned()
                    .chain(strategy.external_operations())
                    .map(|rule| {
                        let matcher = compile_pattern(&rule.pattern);
                        (rule, matcher)
                    })
                    .collect()
            });
            let imports_index =
                crate::scoped_imports::ScopedImports::new(file.imports.iter().map(|i| {
                    (
                        i,
                        canonical_export(
                            &file.language,
                            &if i.type_only {
                                format!("<type-only>::import:{}", i.path)
                            } else {
                                resolve_scoped_import(file, i, modules)
                            },
                            runtime_exports,
                        ),
                    )
                }));
            let mut input = FileCalls {
                file,
                strategy,
                rules,
                imports_index: &imports_index,
                normalized_symbols: &mut normalized_symbols,
            };
            for f in &file.functions {
                self.resolve_function_calls(
                    f,
                    &mut input,
                    &mut Emission {
                        graph,
                        generated: &mut generated,
                        edge_ids: &mut edge_ids,
                    },
                );
            }
        }
        graph.nodes.extend(generated.into_values());
    }
    fn bind_browser_call<'call>(
        &self,
        file: &FileModel,
        call: &'call CallSite,
        property_access: bool,
    ) -> Cow<'call, CallSite> {
        let browser_files = &self.browser.files;
        let script_symbols = &self.browser.symbols;
        let script_blocked_name = |name: &str| self.browser.blocked_name(name);
        let script_target = |name: &str| self.browser.target(name);
        let script_call;
        let bound = if !property_access
            && browser_files.contains(file.path.as_str())
            && call
                .script_global
                .as_deref()
                .is_some_and(|name| script_symbols.contains_key(name) || script_blocked_name(name))
        {
            let target = call.script_global.as_deref().and_then(script_target);
            script_call = CallSite {
                confidence: if target.is_some() {
                    Confidence::Resolved
                } else {
                    Confidence::Dynamic
                },
                binding_unknown: target.is_none(),
                targets: target.into_iter().collect(),
                local_binding: true,
                ..call.clone()
            };
            Cow::Owned(script_call)
        } else {
            Cow::Borrowed(call)
        };

        bound
    }
    fn erased_value_binding(
        &self,
        file: &FileModel,
        call: &CallSite,
        imports_index: &crate::scoped_imports::ScopedImports<'_>,
    ) -> bool {
        let modules = &self.modules;
        let runtime_exports = &self.runtime_exports;
        let erased_value_binding = !call.local_binding && {
            let name = call.receiver.as_deref().unwrap_or(&call.name);
            let (root, rest) = name.split_once('.').unwrap_or((name, ""));
            let scope = imports_index.at(&call.range);
            let imported = scope.get(root).map(|target| {
                if rest.is_empty() {
                    target.clone()
                } else {
                    format!("{target}.{rest}")
                }
            });
            imported
                .iter()
                .map(String::as_str)
                .chain(call.receiver_type.as_deref())
                .chain(call.targets.iter().map(String::as_str))
                .any(|target| {
                    target.starts_with("<type-only>::")
                        || canonical_export(
                            &file.language,
                            &resolve_import_path(file, target, modules),
                            runtime_exports,
                        )
                        .starts_with("<type-only>::")
                })
        };

        erased_value_binding
    }
    fn resolve_property_call(
        &self,
        ctx: &mut CallContext<'_, '_, '_>,
        access: Option<&PropertyAccess>,
        erased_value_binding: bool,
        emission: &mut Emission<'_>,
    ) -> bool {
        let modules = &self.modules;
        let exports = &self.exports;
        let types = &self.types;
        let property_index = &self.property_index;
        let file = ctx.file;
        let f = ctx.f;
        let call = ctx.call;
        let context = ctx.context;
        let local_types = &mut *ctx.local_types;
        let from = ctx.from;
        let graph = &mut *emission.graph;
        let edge_ids = &mut *emission.edge_ids;
        let property_targets = access
            .map(|access| {
                property_index.resolve(
                    file,
                    f,
                    access,
                    context,
                    local_types,
                    modules,
                    exports,
                    types,
                )
            })
            .or_else(|| {
                (matches!(file.language.as_str(), "javascript" | "typescript")
                    && call.receiver.is_some())
                .then(|| {
                    property_index.callable_getter(
                        file,
                        f,
                        call,
                        context,
                        local_types,
                        modules,
                        exports,
                        types,
                    )
                })
                .flatten()
            })
            .filter(|_| !erased_value_binding);
        if let Some(targets) = property_targets {
            self.assign_property_return(call, access, &targets, local_types);
            let confidence = if targets.len() > 1 || !call.receiver_types.is_empty() {
                Confidence::Ambiguous
            } else {
                Confidence::Resolved
            };
            for to in targets {
                graph.edges.push(GraphEdge {
                    from: from.clone(),
                    recursive: from.as_ref() == to,
                    to: edge_ids
                        .entry(to.clone())
                        .or_insert_with(|| to.into())
                        .clone(),
                    callsite: call.range.clone(),
                    confidence: confidence.clone(),
                    guard: call.guard.clone(),
                    in_loop: call.in_loop.clone(),
                    hidden_vendor: false,
                    argument: None,
                });
            }
            return true;
        }

        false
    }
    fn assign_property_return(
        &self,
        call: &CallSite,
        access: Option<&PropertyAccess>,
        targets: &[String],
        local_types: &mut Cow<'_, BTreeMap<String, String>>,
    ) {
        let returns_by_id = &self.returns_by_id;
        if access.is_some()
            || targets
                .iter()
                .any(|target| !target.ends_with("#get") && !target.ends_with("#set"))
        {
            if let Some(variable) = &call.assignment_to {
                let inferred = call
                    .receiver_types
                    .is_empty()
                    .then(|| {
                        targets
                            .iter()
                            .map(|target| returns_by_id.get(target).cloned())
                            .collect::<Option<BTreeSet<_>>>()
                            .filter(|types| types.len() == 1)
                            .and_then(|types| types.into_iter().next())
                    })
                    .flatten();
                if let Some(ty) = inferred {
                    local_types.to_mut().insert(variable.clone(), ty);
                } else {
                    local_types.to_mut().remove(variable);
                }
            }
        }
    }
    fn resolve_function_calls(
        &self,
        f: &FunctionModel,
        input: &mut FileCalls<'_, '_>,
        emission: &mut Emission<'_>,
    ) {
        let file = input.file;
        let strategy = input.strategy;
        let rules = input.rules;
        let imports_index = input.imports_index;
        let normalized_symbols = &mut *input.normalized_symbols;

        let modules = &self.modules;
        let exports = &self.exports;
        let types = &self.types;
        let property_index = &self.property_index;
        // Rust bare paths are lexical: two inline modules can use the
        // same spelling for different declarations.
        if file.language == "rust" {
            normalized_symbols.clear();
        }
        let from: Arc<str> = function_id(file, &f.fqn).into();
        let context = strategy.infer_types(file, f);
        let mut local_types = Cow::Borrowed(&context.variables);
        let calls = ordered_calls(f);
        for (call, access) in calls {
            let browser_call = self.bind_browser_call(file, call, access.is_some());
            let call = browser_call.as_ref();
            let erased_value_binding = self.erased_value_binding(file, call, imports_index);
            let erased_call;
            let call = if erased_value_binding {
                erased_call = CallSite {
                    confidence: Confidence::Dynamic,
                    binding_unknown: true,
                    receiver_type: None,
                    receiver_root_type: None,
                    receiver_types: Vec::new(),
                    targets: Vec::new(),
                    ..call.clone()
                };
                &erased_call
            } else {
                call
            };
            let mut ctx = CallContext {
                file,
                f,
                call,
                context: &context,
                from: &from,
                strategy,
                rules,
                imports_index,
                local_types: &mut local_types,
                normalized_symbols: &mut *normalized_symbols,
            };
            if self.resolve_property_call(&mut ctx, access, erased_value_binding, emission) {
                continue;
            }
            let unresolved_union;
            let call = if !call.receiver_types.is_empty() {
                unresolved_union = CallSite {
                    binding_unknown: true,
                    receiver_type: None,
                    receiver_root_type: None,
                    targets: Vec::new(),
                    ..call.clone()
                };
                &unresolved_union
            } else {
                call
            };
            let blocked_property_descriptor =
                matches!(file.language.as_str(), "javascript" | "typescript")
                    && property_index.blocks_inherited_call(
                        file,
                        f,
                        call,
                        &context,
                        ctx.local_types,
                        modules,
                        exports,
                        types,
                    );

            let mut state = CallState {
                blocked_property_descriptor,
                ..CallState::default()
            };
            let mut ctx = CallContext { call, ..ctx };
            self.resolve_ordinary_call(&mut state, &mut ctx, emission);
        }
    }
}
fn known_types(files: &[FileModel]) -> KnownTypes {
    let esm_type_aliases = files
        .iter()
        .filter(|file| matches!(file.language.as_str(), "javascript" | "typescript"))
        .flat_map(|file| {
            file.types.iter().filter_map(|ty| {
                if matches!(ty.kind, TypeKind::Namespace | TypeKind::Impl) {
                    return None;
                }
                ty.fqn
                    .strip_prefix(&file.module)
                    .map(|suffix| (file.language.as_str(), format!("{}{suffix}", file.path)))
            })
        })
        .collect::<Vec<_>>();
    let types = files
        .iter()
        .flat_map(|file| {
            file.types
                .iter()
                .filter(|t| !matches!(t.kind, TypeKind::Namespace | TypeKind::Impl))
                .flat_map(|t| {
                    let compatible = match file.language.as_str() {
                        "javascript" => Some("typescript"),
                        "typescript" => Some("javascript"),
                        _ => None,
                    };
                    std::iter::once((file.language.as_str(), t.fqn.as_str()))
                        .chain(compatible.map(|language| (language, t.fqn.as_str())))
                })
        })
        .chain(esm_type_aliases.iter().flat_map(|(language, alias)| {
            let other = if *language == "javascript" {
                "typescript"
            } else {
                "javascript"
            };
            [(*language, alias.as_str()), (other, alias.as_str())]
        }))
        .collect::<KnownTypes>();
    types
}

fn ordered_calls(f: &FunctionModel) -> Vec<(&CallSite, Option<&PropertyAccess>)> {
    let mut calls = f
        .calls
        .iter()
        .map(|call| (call, None))
        .chain(
            f.property_accesses
                .iter()
                .map(|access| (&access.site, Some(access))),
        )
        .collect::<Vec<_>>();
    calls.sort_by_key(|(call, _)| (call.range.start_line, call.range.start_column));
    calls
}
