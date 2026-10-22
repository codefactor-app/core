//! Ordered call resolution stages. Each stage preserves the previous dispatch precedence.
use super::*;
impl ProjectIndex<'_> {
    pub(super) fn resolve_ordinary_call(
        &self,
        state: &mut CallState,
        ctx: &mut CallContext<'_, '_, '_>,
        emission: &mut Emission<'_>,
    ) {
        self.initial_candidates(state, ctx);
        self.python_super_receiver(state, ctx);
        self.rust_receiver(state, ctx);
        self.parent_and_local_receiver(state, ctx);
        self.member_receiver(state, ctx);
        self.php_static_receiver(state, ctx);
        self.property_receiver(state, ctx);
        self.interface_candidates(state, ctx);
        self.import_candidates(state, ctx);
        self.normalize_candidates(state, ctx);
        self.external_candidates(state, ctx);
        self.classify_native(state, ctx);
        if self.emit_external(state, ctx, emission) {
            return;
        }
        self.direct_targets(state, ctx);
        self.ordered_inheritance(state, ctx);
        self.fallback_inheritance(state, ctx);
        self.rust_dispatch(state, ctx);
        self.assign_return_type(state, ctx);
        self.emit_runtime(state, ctx, emission);
        self.emit_unknown(state, ctx, emission);
        self.emit_resolved(state, ctx, emission);
    }
    fn initial_candidates(&self, state: &mut CallState, ctx: &mut CallContext<'_, '_, '_>) {
        let files = self.files;
        let file = ctx.file;
        let call = ctx.call;
        let context = ctx.context;
        let strategy = ctx.strategy;
        let imports = ctx.imports_index.at(&ctx.call.range);
        state.candidates = if call.binding_unknown {
            Vec::new()
        } else {
            strategy.resolve_project_call(call, context, file, files)
        };
        if !call.binding_unknown {
            state.candidates.extend(call.targets.clone());
        }
        state.imported_function = !call.binding_unknown
            && !call.local_binding
            && call.receiver.is_none()
            && matches!(file.language.as_str(), "javascript" | "typescript")
            && imports.contains_key(&call.name);
        if !call.binding_unknown
            && !call.local_binding
            && call.receiver.is_none()
            && matches!(file.language.as_str(), "javascript" | "typescript")
        {
            if let Some(imported) = imports.get(&call.name) {
                // Original resource metadata takes precedence over
                // parser candidates that flattened away its extension.
                state.candidates = vec![imported.clone()];
            }
        }
        state.qualified = state.candidates.clone();
        state.receiver_type = if call.binding_unknown {
            None
        } else {
            call.receiver_type.clone()
        };
        state.python_super_anchor = None;
    }
    fn python_super_receiver(&self, state: &mut CallState, ctx: &mut CallContext<'_, '_, '_>) {
        let modules = &self.modules;
        let exports = &self.exports;
        let types = &self.types;
        let file = ctx.file;
        let f = ctx.f;
        let call = ctx.call;
        let local_types = &mut *ctx.local_types;
        if file.language == "python" && !call.binding_unknown {
            if let Some(arguments) = call
                .receiver
                .as_deref()
                .and_then(inheritance::super_arguments)
            {
                if arguments.is_empty() {
                    state.python_super_anchor = f.parent_type.clone();
                    state.receiver_type = f.parent_type.clone();
                } else if arguments.len() == 2 {
                    let instance = &arguments[1];
                    state.receiver_type = f
                        .parameters
                        .iter()
                        .any(|parameter| parameter.implicit_receiver && parameter.name == *instance)
                        .then(|| f.parent_type.clone())
                        .flatten()
                        .or_else(|| local_types.get(instance).cloned());
                    if state.receiver_type.is_some() {
                        state.python_super_anchor = Some(resolve_type_reference(
                            file,
                            &arguments[0],
                            &f.fqn,
                            &call.range,
                            modules,
                            exports,
                            types,
                        ));
                    }
                }
            }
        }
    }
    fn rust_receiver(&self, state: &mut CallState, ctx: &mut CallContext<'_, '_, '_>) {
        let modules = &self.modules;
        let file = ctx.file;
        let f = ctx.f;
        let call = ctx.call;
        let normalized_symbols = &mut *ctx.normalized_symbols;
        if file.language == "rust" && !call.binding_unknown && !call.local_binding {
            if let Some(receiver) = &call.receiver {
                let target =
                    rust_import_path(file, &format!("{receiver}::{}", call.name), &f.fqn, modules);
                if modules.contains(&("rust", target.as_str())) {
                    state.receiver_type =
                        target.rsplit_once("::").map(|(owner, _)| owner.to_owned());
                    normalized_symbols.insert(target.clone(), target.clone());
                    state.qualified.push(target.clone());
                    state.candidates.push(target);
                }
            }
        }
    }
    fn parent_and_local_receiver(&self, state: &mut CallState, ctx: &mut CallContext<'_, '_, '_>) {
        let parents = &self.parents;
        let file = ctx.file;
        let f = ctx.f;
        let call = ctx.call;
        let context = ctx.context;
        let local_types = &mut *ctx.local_types;
        if !call.binding_unknown
            && file.language != "rust"
            && call
                .receiver
                .as_deref()
                .is_some_and(|r| ["parent", "super", "super()"].contains(&r))
        {
            state.receiver_type = if file.language == "python" {
                f.parent_type.clone()
            } else {
                f.parent_type
                    .as_ref()
                    .and_then(|t| {
                        parents
                            .get(file.language.as_str())
                            .and_then(|types| types.get(t.as_str()))
                    })
                    .filter(|p| p.len() == 1)
                    .and_then(|p| p.first().cloned())
            };
        }
        if !call.binding_unknown && state.receiver_type.is_none() {
            if let Some(recv) = &call.receiver {
                state.receiver_type = local_types
                    .get(recv)
                    .or_else(|| context.properties.get(recv))
                    .cloned();
            }
        }
    }
    fn member_receiver(&self, state: &mut CallState, ctx: &mut CallContext<'_, '_, '_>) {
        let modules = &self.modules;
        let exports = &self.exports;
        let runtime_exports = &self.runtime_exports;
        let types = &self.types;
        let constant_types = &self.constant_types;
        let file = ctx.file;
        let f = ctx.f;
        let call = ctx.call;

        let strategy = ctx.strategy;
        let local_types = &mut *ctx.local_types;
        let imports = ctx.imports_index.at(&ctx.call.range);
        state.qualified_receiver = false;
        // An imported module constant (`export const db = new Client()`)
        // keeps the type its declaring module gives it.
        state.imported_constant = call
            .receiver
            .as_deref()
            .filter(|_| !call.binding_unknown && !call.local_binding)
            .and_then(|receiver| {
                let root = receiver.split(['.', '?']).next()?;
                let target = imports.get(root)?;
                constant_types.get(&(
                    file.language.as_str(),
                    canonical_export(&file.language, target, runtime_exports),
                ))
            })
            .cloned();
        if let Some(ty) = state.imported_constant.as_ref().filter(|_| {
            call.receiver
                .as_deref()
                .is_some_and(|receiver| imports.contains_key(receiver))
        }) {
            state.receiver_type = Some(ty.clone());
            state.qualified_receiver = true;
        }
        if call.binding_unknown
            || (state.receiver_type.is_some() && state.imported_constant.is_none())
        {
            return;
        }
        let Some(receiver) = call.receiver.as_deref() else {
            return;
        };
        let path = receiver
            .replace("?->", ".")
            .replace("->", ".")
            .replace("?.", ".");
        let parts = path.split('.').collect::<Vec<_>>();
        if !(parts.len() > 1
            && parts.iter().all(|part| {
                !part.is_empty()
                    && part
                        .chars()
                        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '$'))
            }))
        {
            return;
        }
        let root = parts[0].trim_start_matches('$');
        let initial = state.imported_constant.clone().or_else(|| {
            call.receiver_root_type
                .as_ref()
                .or_else(|| local_types.get(root))
                .map(|initial| {
                    resolve_type_reference(
                        file,
                        initial,
                        &f.fqn,
                        &call.range,
                        modules,
                        exports,
                        types,
                    )
                })
        });
        let Some(initial) = initial else {
            return;
        };
        let (ty, external_member_path) = self.walk_member_path(file, strategy, &parts, initial);
        if ty.is_some() {
            state.qualified_receiver |= external_member_path || state.imported_constant.is_some();
            state.receiver_type = ty;
        } else if state.imported_constant.is_none() {
            state.receiver_type = None;
        }
    }
    fn external_member_owner(
        &self,
        file: &FileModel,
        strategy: &Arc<dyn crate::LanguageStrategy>,
        owner: &str,
    ) -> bool {
        let types = &self.types;
        let fields = &self.fields;
        let parents = &self.parents;
        let project_modules = &self.project_modules;

        let language = file.language.as_str();
        !owner.starts_with("<type-only>::")
            && !types.contains(&(language, owner))
            && !fields
                .get(language)
                .is_some_and(|known| known.contains_key(owner))
            && !parents
                .get(language)
                .is_some_and(|known| known.contains_key(owner))
            && !strategy.is_native_receiver(owner)
            && !project_modules.contains(&(language, owner))
            && !owner.char_indices().any(|(i, c)| {
                matches!(c, '.' | ':' | '\\') && project_modules.contains(&(language, &owner[..i]))
            })
    }
    fn walk_member_path(
        &self,
        file: &FileModel,
        strategy: &Arc<dyn crate::LanguageStrategy>,
        parts: &[&str],
        initial: String,
    ) -> (Option<String>, bool) {
        let fields = &self.fields;
        let parents = &self.parents;
        let external_bases = &self.external_bases;
        let inheritance = &self.inheritance;
        let mut ty = Some(initial);
        let mut external_member_path = false;
        for name in &parts[1..] {
            if let Some(owner) = ty
                .as_deref()
                .filter(|owner| self.external_member_owner(file, strategy, owner))
            {
                ty = Some(format!("{owner}.{name}"));
                external_member_path = true;
                continue;
            }
            // A member inherited from a library base class
            // (`class Db extends PrismaClient`).
            if let Some(base) = ty.as_deref().and_then(|owner| {
                external_bases.member_owner(&file.language, owner, name, fields, parents)
            }) {
                ty = Some(format!("{base}.{name}"));
                external_member_path = true;
                continue;
            }
            ty = ty.and_then(|owner| {
                if file.language == "python" {
                    return inheritance.python_order(&owner).and_then(|order| {
                        order
                            .iter()
                            .find_map(|base| {
                                fields
                                    .get("python")
                                    .and_then(|types| types.get(base.as_str()))
                                    .and_then(|fields| fields.get(*name))
                            })
                            .cloned()
                            .flatten()
                    });
                }
                declared_field_type(&file.language, &owner, name, fields, parents)
            });
        }

        (ty, external_member_path)
    }
    fn php_static_receiver(&self, state: &mut CallState, ctx: &mut CallContext<'_, '_, '_>) {
        let modules = &self.modules;
        let exports = &self.exports;
        let types = &self.types;
        let file = ctx.file;
        let f = ctx.f;
        let call = ctx.call;

        if file.language == "php"
            && call.kind == CallKind::Static
            && !call.binding_unknown
            && !call.local_binding
            && state.receiver_type.is_none()
        {
            if let Some(receiver) = call.receiver.as_deref().filter(|receiver| {
                !receiver.is_empty()
                    && !matches!(*receiver, "self" | "static" | "parent")
                    && receiver
                        .chars()
                        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '\\'))
            }) {
                let source_receiver = if receiver.starts_with('\\') {
                    receiver.to_owned()
                } else if !receiver.starts_with("namespace\\")
                    && file.imports.iter().any(|import| {
                        type_import_priority(file, import, receiver, &call.range).is_some()
                    })
                {
                    receiver.to_owned()
                } else {
                    format!("\\{}", php_namespace_reference(receiver, &f.fqn))
                };
                state.receiver_type = Some(resolve_type_reference(
                    file,
                    &source_receiver,
                    &f.fqn,
                    &call.range,
                    modules,
                    exports,
                    types,
                ));
            }
        }
    }
    fn property_receiver(&self, state: &mut CallState, ctx: &mut CallContext<'_, '_, '_>) {
        let modules = &self.modules;
        let exports = &self.exports;
        let types = &self.types;
        let property_index = &self.property_index;
        let file = ctx.file;
        let f = ctx.f;
        let call = ctx.call;
        let context = ctx.context;
        let local_types = &mut *ctx.local_types;
        if state.receiver_type.is_none()
            && matches!(file.language.as_str(), "javascript" | "typescript")
        {
            state.receiver_type = property_index.receiver_type(
                file,
                f,
                call,
                context,
                local_types,
                modules,
                exports,
                types,
            );
        }
    }
    fn interface_candidates(&self, state: &mut CallState, ctx: &mut CallContext<'_, '_, '_>) {
        let modules = &self.modules;
        let exports = &self.exports;
        let types = &self.types;
        let implementations = &self.implementations;
        let config = self.config;
        let file = ctx.file;
        let f = ctx.f;
        let call = ctx.call;
        let strategy = ctx.strategy;
        let normalized_symbols = &mut *ctx.normalized_symbols;
        let imports = ctx.imports_index.at(&ctx.call.range);

        state.interface_dispatch = false;
        if let Some(t) = state.receiver_type.as_deref() {
            let expanded = imports
                .get(t)
                .or_else(|| {
                    (!call.local_binding)
                        .then(|| {
                            call.receiver
                                .as_deref()
                                .and_then(|receiver| imports.get(receiver))
                        })
                        .flatten()
                })
                .filter(|_| !state.qualified_receiver)
                .cloned()
                .unwrap_or_else(|| {
                    if state.qualified_receiver || types.contains(&(file.language.as_str(), t)) {
                        return t.to_owned();
                    }
                    resolve_type_reference(file, t, &f.fqn, &call.range, modules, exports, types)
                });
            let canonical = canonical_export(&file.language, &expanded, exports);
            let t = canonical.as_str();
            state.receiver_type = Some(canonical.clone());
            let known = implementations
                .get(file.language.as_str())
                .and_then(|types| types.get(t))
                .map(Vec::as_slice)
                .unwrap_or_default();
            state.interface_dispatch = !known.is_empty();
            for ty in std::iter::once(t).chain(
                known
                    .iter()
                    .map(String::as_str)
                    .take(config.graph.max_implementations),
            ) {
                let a = format!("{ty}::{}", call.name);
                let b = format!("{ty}.{}", call.name);
                let native = format!("{ty}{}{}", strategy.method_separator(), call.name);
                if file.language == "rust" && modules.contains(&("rust", a.as_str())) {
                    normalized_symbols.insert(a.clone(), a.clone());
                }
                state
                    .qualified
                    .extend([a.clone(), b.clone(), native.clone()]);
                state.candidates.extend([a, b, native]);
            }
        }
    }
    fn import_candidates(&self, state: &mut CallState, ctx: &mut CallContext<'_, '_, '_>) {
        let file = ctx.file;
        let call = ctx.call;
        let strategy = ctx.strategy;
        let normalized_symbols = &mut *ctx.normalized_symbols;
        let imports = ctx.imports_index.at(&ctx.call.range);
        if !call.binding_unknown && !call.local_binding && !state.imported_function {
            state.candidates.push(call.name.clone());
        }
        for (alias, path) in imports.matching_prefixes(&call.name).filter(|(alias, _)| {
            !call.binding_unknown
                && !call.local_binding
                && (call.receiver.is_none()
                    || call.name.starts_with(&format!("{alias}."))
                    || call.name.starts_with(&format!("{alias}::")))
        }) {
            let expanded = if call.name == alias {
                Some(path.clone())
            } else if let Some(rest) = call.name.strip_prefix(&format!("{alias}.")) {
                Some(format!("{path}.{rest}"))
            } else {
                call.name
                    .strip_prefix(&format!("{alias}::"))
                    .map(|rest| format!("{path}::{rest}"))
            };
            if let Some(expanded) = expanded {
                if file.language == "rust" {
                    normalized_symbols.insert(expanded.clone(), expanded.clone());
                }
                state.qualified.push(expanded.clone());
                state.candidates.push(expanded)
            }
        }
        for sep in [strategy.namespace_separator(), ".", "::", "\\"]
            .into_iter()
            .filter(|_| {
                !call.binding_unknown
                    && !call.local_binding
                    && !state.imported_function
                    && file.language != "rust"
            })
        {
            state
                .candidates
                .push(format!("{}{}{}", file.module, sep, call.name));
        }
    }
    fn normalize_candidates(&self, state: &mut CallState, ctx: &mut CallContext<'_, '_, '_>) {
        let modules = &self.modules;
        let runtime_exports = &self.runtime_exports;
        let types = &self.types;
        let file = ctx.file;
        let f = ctx.f;
        let call = ctx.call;
        let strategy = ctx.strategy;
        let normalized_symbols = &mut *ctx.normalized_symbols;
        let mut normalize = |c: String| {
            normalize_symbol(
                normalized_symbols,
                c,
                file.language == "php" && !self.exports.contains_key(file.language.as_str()),
                |symbol| {
                    canonical_export(
                        &file.language,
                        &if file.language == "rust" {
                            rust_import_path(file, symbol, &f.fqn, modules)
                        } else {
                            resolve_import_path(file, symbol, modules)
                        },
                        runtime_exports,
                    )
                },
            )
        };
        state.candidates = std::mem::take(&mut state.candidates)
            .into_iter()
            .map(&mut normalize)
            .collect();
        state.qualified = std::mem::take(&mut state.qualified)
            .into_iter()
            .map(&mut normalize)
            .collect();
        state.constructor_type = (call.receiver.is_none()
            && !call.binding_unknown
            && (call.kind == CallKind::Constructor || strategy.permits_constructor_inference()))
        .then(|| {
            state
                .candidates
                .iter()
                .find(|c| types.contains(&(file.language.as_str(), c.as_str())))
                .cloned()
        })
        .flatten();
        if let Some(ty) = &state.constructor_type {
            state.candidates.push(format!(
                "{ty}{}{}",
                strategy.method_separator(),
                strategy.constructor_name()
            ));
        }
        state.candidates.sort();
        state.candidates.dedup();
    }
    fn external_candidates(&self, state: &mut CallState, ctx: &mut CallContext<'_, '_, '_>) {
        let parents = &self.parents;
        let fields = &self.fields;
        let source_modules = &self.source_modules;
        let external_bases = &self.external_bases;
        let file = ctx.file;
        let call = ctx.call;
        let strategy = ctx.strategy;
        let imports = ctx.imports_index.at(&ctx.call.range);
        state.external_candidates = if call.receiver.is_some() {
            state
                .qualified
                .iter()
                .filter(|name| has_multiple_segments_for(name, strategy.as_ref()))
                .cloned()
                .collect::<Vec<_>>()
        } else {
            state.candidates.clone()
        };
        // `this.$connect()` in `class Db extends PrismaClient`.
        if let Some(base) = state
            .receiver_type
            .as_deref()
            .filter(|_| !call.binding_unknown)
            .and_then(|owner| {
                external_bases.member_owner(&file.language, owner, &call.name, fields, parents)
            })
        {
            state.external_candidates.extend([
                format!("{base}.{}", call.name),
                format!("{base}::{}", call.name),
            ]);
        }
        if let Some(receiver) = call
            .receiver
            .as_ref()
            .filter(|_| !call.binding_unknown && !call.local_binding)
        {
            if !imports.contains_key(receiver) {
                state.external_candidates.extend([
                    format!("{receiver}.{}", call.name),
                    format!("{receiver}::{}", call.name),
                    format!("{receiver}{}{}", strategy.method_separator(), call.name),
                ]);
            }
            if let Some(imported) = imports.get(receiver) {
                state.external_candidates.extend([
                    format!("{imported}.{}", call.name),
                    format!("{imported}::{}", call.name),
                    format!("{imported}{}{}", strategy.method_separator(), call.name),
                ]);
            }
        }
        // Library patterns name the package specifier: the default
        // import is the package value (CJS/esModuleInterop), and an
        // installed but unparsed package must not hide the library.
        if matches!(file.language.as_str(), "javascript" | "typescript")
            && !call.binding_unknown
            && !call.local_binding
        {
            let symbol = call.receiver.as_deref().unwrap_or(&call.name);
            let (root, rest) = symbol.split_once('.').unwrap_or((symbol, ""));
            if let (Some(import), Some(resolved)) = (imports.import(root), imports.get(root)) {
                let owned = resolved.char_indices().any(|(i, c)| {
                    matches!(c, '.' | ':') && source_modules.contains(&resolved[..i])
                }) || source_modules.contains(resolved.as_str());
                if let Some(source) = import.source.as_deref().filter(|source| {
                    !owned && !source.starts_with(['.', '/', '#']) && !source.contains("://")
                }) {
                    let package = [".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs"]
                        .iter()
                        .find_map(|extension| source.strip_suffix(extension))
                        .unwrap_or(source);
                    let member = import.path.strip_prefix(package).unwrap_or("");
                    let mut base = format!(
                        "{}{}",
                        package.strip_prefix("node:").unwrap_or(package),
                        if member == ".default" { "" } else { member }
                    );
                    if !rest.is_empty() {
                        base = format!("{base}.{rest}");
                    }
                    state.external_candidates.push(if call.receiver.is_some() {
                        format!("{base}.{}", call.name)
                    } else {
                        base
                    });
                }
            }
        }
    }
    fn classify_native(&self, state: &mut CallState, ctx: &mut CallContext<'_, '_, '_>) {
        let types = &self.types;
        let functions = &self.functions;
        let project_modules = &self.project_modules;
        let file = ctx.file;
        let call = ctx.call;
        let strategy = ctx.strategy;
        let imports = ctx.imports_index.at(&ctx.call.range);
        state.has_definition = !call.binding_unknown
            && state.candidates.iter().any(|c| {
                (call.receiver.is_none() || state.qualified.contains(c))
                    && functions
                        .get(file.language.as_str())
                        .is_some_and(|symbols| symbols.contains_key(c.as_str()))
            });
        let project_owns = |name: &str| {
            project_modules.contains(&(file.language.as_str(), name))
                || name.char_indices().any(|(i, c)| {
                    (matches!(c, '.' | ':' | '\\')
                        || name[i..].starts_with(strategy.namespace_separator())
                        || name[i..].starts_with(strategy.method_separator()))
                        && project_modules.contains(&(file.language.as_str(), &name[..i]))
                })
        };
        let constructor_reference = (call.receiver.is_none()
            && !call.local_binding
            && state.constructor_type.is_none()
            && matches!(call.kind, CallKind::Constructor | CallKind::Function)
            && (call.kind == CallKind::Constructor || strategy.permits_constructor_inference()))
        .then(|| {
            imports
                .get(&call.name)
                .map(String::as_str)
                .unwrap_or(&call.name)
        })
        .filter(|ty| {
            (strategy.is_native_receiver(ty)
                || (imports.contains_key(&call.name) && strategy.is_native_function(ty)))
                && !project_owns(ty)
                && !types.contains(&(file.language.as_str(), *ty))
        });
        state.standard_receiver = state
            .receiver_type
            .as_deref()
            .filter(|ty| {
                !types.contains(&(file.language.as_str(), *ty))
                    && !project_owns(ty)
                    && strategy.is_native_receiver(ty)
            })
            .or(constructor_reference)
            .or_else(|| {
                call.receiver.as_deref().filter(|r| {
                    state.receiver_type.is_none()
                        && !call.local_binding
                        && !project_owns(r)
                        && !file.types.iter().any(|t| {
                            r == &t.name
                                || [
                                    strategy.namespace_separator(),
                                    strategy.method_separator(),
                                    ".",
                                    "::",
                                ]
                                .iter()
                                .any(|separator| r.starts_with(&format!("{}{separator}", t.name)))
                        })
                        && strategy.is_native_global_receiver(r)
                })
            })
            .map(str::to_owned);
        state.standard = !call.binding_unknown
            && !state.has_definition
            && (state.standard_receiver.is_some()
                || (!call.local_binding
                    && !imports.contains_key(&call.name)
                    && call.receiver.is_none()
                    && state.constructor_type.is_none()
                    && ((call.kind == CallKind::Function
                        && strategy.is_native_function(&call.name))
                        || (call.kind == CallKind::Macro
                            && strategy.is_native_macro(&call.name)))));
    }
    fn emit_external(
        &self,
        state: &mut CallState,
        ctx: &mut CallContext<'_, '_, '_>,
        emission: &mut Emission<'_>,
    ) -> bool {
        let config = self.config;

        let call = ctx.call;
        let from = ctx.from;
        let rules = ctx.rules;
        let graph = &mut *emission.graph;
        let generated = &mut *emission.generated;
        let edge_ids = &mut *emission.edge_ids;
        let external = rules
            .iter()
            .filter(|_| !call.binding_unknown && !state.has_definition)
            .find(|(rule, matcher)| {
                state.external_candidates.iter().any(|candidate| {
                    rule.pattern == *candidate
                        || matcher
                            .as_ref()
                            .is_some_and(|matcher| matcher.is_match(candidate))
                })
            })
            .map(|(rule, _)| rule);
        if let Some(rule) = external.filter(|_| !state.standard) {
            let id = format!(
                "external:{}:{}:{}",
                rule.category, rule.library, rule.operation
            );
            let arg = if config.external.capture_args == CaptureArgs::Literal {
                literal_argument(call, config)
            } else {
                None
            };
            let node = generated.entry(id.clone()).or_insert_with(|| GraphNode {
                id: id.clone(),
                label: format!("{} · {} · {}", rule.category, rule.library, rule.operation),
                kind: NodeKind::External,
                file: None,
                line: None,
                language: None,
                module: None,
                parent_type: None,
                vendor: false,
                package: None,
                category: Some(rule.category.clone()),
                library: Some(rule.library.clone()),
                operation: Some(rule.operation.clone()),
                argument: arg.clone(),
                algorithmic_complexity: None,
                cargo_target: None,
                cargo_target_id: None,
                source_fqn: None,
            });
            // A shared operation node may represent several resources.
            // Do not advertise the first literal as their common value.
            if node.argument != arg {
                node.argument = None;
            }
            graph.edges.push(GraphEdge {
                from: from.clone(),
                to: edge_ids
                    .entry(id.clone())
                    .or_insert_with(|| id.into())
                    .clone(),
                callsite: call.range.clone(),
                confidence: Confidence::Resolved,
                guard: call.guard.clone(),
                in_loop: call.in_loop.clone(),
                recursive: false,
                hidden_vendor: false,
                argument: arg,
            });
            return true;
        }
        false
    }
    fn direct_targets(&self, state: &mut CallState, ctx: &mut CallContext<'_, '_, '_>) {
        let modules = &self.modules;
        let exports = &self.exports;
        let types = &self.types;
        let functions = &self.functions;
        let property_index = &self.property_index;
        let file = ctx.file;
        let f = ctx.f;
        let call = ctx.call;
        let context = ctx.context;
        let strategy = ctx.strategy;
        let local_types = &mut *ctx.local_types;
        let imports = ctx.imports_index.at(&ctx.call.range);
        let receiver_known = call.receiver.is_none()
            || state.receiver_type.is_some()
            || call.confidence == Confidence::Resolved
            || call
                .receiver
                .as_ref()
                .is_some_and(|receiver| imports.contains_key(receiver));
        let usable_candidate = |candidate: &String| {
            call.receiver.is_none()
                || (receiver_known
                    && has_multiple_segments_for(candidate, strategy.as_ref())
                    && state.qualified.contains(candidate))
        };
        state.targets = Vec::new();
        // Lexical/file definitions take precedence over any project-wide
        // name match. A suffix is not evidence of a call relationship:
        // unrelated classes and nested callbacks often share the same name.
        for candidate in &state.candidates {
            if !usable_candidate(candidate) {
                continue;
            }
            if let Some(found) = functions
                .get(file.language.as_str())
                .and_then(|symbols| symbols.get(candidate.as_str()))
            {
                state.targets.extend(
                    found
                        .iter()
                        .filter(|(_, path)| path == &file.path)
                        .map(|(id, _)| id.clone()),
                );
            }
        }
        if state.targets.is_empty() {
            for candidate in &state.candidates {
                if !usable_candidate(candidate) {
                    continue;
                }
                // JS/TS/Python bare names live in a module scope. Fully
                // qualified import/type targets can identify another file.
                if !has_multiple_segments_for(candidate, strategy.as_ref())
                    && (call.receiver.is_some()
                        || matches!(
                            file.language.as_str(),
                            "javascript" | "typescript" | "python"
                        ))
                {
                    continue;
                }
                if let Some(found) = functions
                    .get(file.language.as_str())
                    .and_then(|symbols| symbols.get(candidate.as_str()))
                {
                    state.targets.extend(found.iter().map(|(id, _)| id.clone()));
                }
            }
        }
        if matches!(file.language.as_str(), "javascript" | "typescript")
            && property_index.blocks_direct_call(
                file,
                f,
                call,
                context,
                local_types,
                modules,
                exports,
                types,
            )
        {
            state.targets.clear();
        } else if state.blocked_property_descriptor {
            // A data field can replace a same-name native accessor
            // even when the ordinary symbol index contains it.
            state
                .targets
                .retain(|id| !id.ends_with("#get") && !id.ends_with("#set"));
        }
    }
    fn ordered_inheritance(&self, state: &mut CallState, ctx: &mut CallContext<'_, '_, '_>) {
        let types = &self.types;
        let functions = &self.functions;
        let inheritance = &self.inheritance;
        let file = ctx.file;
        let f = ctx.f;
        let call = ctx.call;
        let from = ctx.from;
        let strategy = ctx.strategy;
        state.inherited_standard_receiver = None;
        state.inheritance_uncertain = false;
        state.ordered_lookup = false;
        if file.language == "php" && !call.binding_unknown {
            if let Some(lookup) = inheritance.php_trait_lookup(
                from.as_ref(),
                f.parent_type.as_deref(),
                call.receiver.as_deref(),
                &call.name,
            ) {
                state.ordered_lookup = true;
                state.targets = lookup.targets;
                state.inheritance_uncertain = lookup.uncertain;
                state.inherited_standard_receiver = lookup.runtime_owner;
            }
        }
        if !call.binding_unknown && !state.ordered_lookup {
            if let Some(owner) = state
                .receiver_type
                .as_ref()
                .or(state.constructor_type.as_ref())
            {
                let method = if state.constructor_type.is_some() {
                    strategy.constructor_name()
                } else {
                    &call.name
                };
                if matches!(file.language.as_str(), "php" | "python")
                    && types.contains(&(file.language.as_str(), owner.as_str()))
                    && !state.interface_dispatch
                    && (file.language == "python" || inheritance.php_type(owner))
                {
                    state.ordered_lookup = true;
                    let lookup = if file.language == "php" {
                        inheritance.php_lookup(owner, method, f.parent_type.as_deref())
                    } else {
                        inheritance.python_lookup(
                            owner,
                            method,
                            state.python_super_anchor.as_deref(),
                            |base, name| {
                                functions
                                    .get("python")
                                    .and_then(|symbols| symbols.get(&format!("{base}.{name}")))
                                    .into_iter()
                                    .flatten()
                                    .map(|(id, _)| id.clone())
                                    .collect()
                            },
                        )
                    };
                    state.targets = lookup.targets;
                    state.inheritance_uncertain = lookup.uncertain;
                    state.inherited_standard_receiver = lookup.runtime_owner;
                }
            }
        }
    }
    fn fallback_inheritance(&self, state: &mut CallState, ctx: &mut CallContext<'_, '_, '_>) {
        let functions = &self.functions;
        let parents = &self.parents;
        let file = ctx.file;
        let call = ctx.call;
        let strategy = ctx.strategy;
        if !state.targets.is_empty()
            || state.ordered_lookup
            || call.binding_unknown
            || state.blocked_property_descriptor
        {
            return;
        }
        let Some(ty) = state
            .receiver_type
            .as_ref()
            .or(state.constructor_type.as_ref())
        else {
            return;
        };
        let method = if state.constructor_type.is_some() {
            strategy.constructor_name()
        } else {
            &call.name
        };
        let mut pending = parents
            .get(file.language.as_str())
            .and_then(|types| types.get(ty.as_str()))
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let mut visited = BTreeSet::new();
        while let Some(base) = pending.pop() {
            if !visited.insert(base) {
                continue;
            }
            let mut found = Vec::new();
            for sep in [strategy.method_separator(), ".", "::"] {
                if let Some(items) = functions
                    .get(file.language.as_str())
                    .and_then(|symbols| symbols.get(&format!("{base}{sep}{method}")))
                {
                    found.extend(items.iter().map(|(id, _)| id.clone()));
                }
            }
            if found.is_empty() {
                if strategy.is_native_method(base, method) {
                    state.inherited_standard_receiver = Some(base.to_owned());
                }
                pending.extend(
                    parents
                        .get(file.language.as_str())
                        .and_then(|types| types.get(base))
                        .into_iter()
                        .flatten()
                        .map(String::as_str),
                );
            } else {
                state.targets.extend(found);
            }
        }
    }
    fn rust_dispatch(&self, state: &mut CallState, ctx: &mut CallContext<'_, '_, '_>) {
        let modules = &self.modules;
        let exports = &self.exports;
        let types = &self.types;
        let rust_methods = &self.rust_methods;
        let rust_contracts = &self.rust_contracts;
        let rust_defaults = &self.rust_defaults;
        let file = ctx.file;
        let f = ctx.f;
        let call = ctx.call;
        let context = ctx.context;
        let local_types = &mut *ctx.local_types;
        if file.language == "rust" && call.receiver.is_some() {
            let explicit_contract = call.rust_trait.as_ref().map(|contract| {
                rust_contract_identity(file, contract, &f.fqn, &call.range, modules, exports, types)
            });
            let receiver_contract = state
                .receiver_type
                .as_ref()
                .filter(|receiver| rust_contracts.contains(*receiver));
            let contract = explicit_contract.as_ref().or(receiver_contract);
            if let Some(contract) = contract {
                let argument_owner = receiver_contract
                    .filter(|_| call.kind == CallKind::Static)
                    .and_then(|_| call.arguments.first())
                    .and_then(|argument| {
                        let variable = argument
                            .trim()
                            .trim_start_matches('&')
                            .trim_start()
                            .trim_start_matches("mut ")
                            .trim();
                        local_types
                            .get(variable)
                            .or_else(|| context.variables.get(variable))
                    })
                    .map(|owner| {
                        resolve_type_reference(
                            file,
                            owner,
                            &f.fqn,
                            &call.range,
                            modules,
                            exports,
                            types,
                        )
                    })
                    .filter(|owner| !rust_contracts.contains(owner));
                let concrete = explicit_contract
                    .as_ref()
                    .and(state.receiver_type.as_ref())
                    .or(argument_owner.as_ref());
                let implementation_matches = |target: &String| {
                    rust_methods
                        .get(target)
                        .is_some_and(|(owner, implementation, _)| {
                            implementation.as_ref().is_some_and(|implementation| {
                                implementation == contract
                                    || (!contract.contains('<')
                                        && implementation
                                            .strip_prefix(contract)
                                            .is_some_and(|suffix| suffix.starts_with('<')))
                            }) && concrete.is_none_or(|concrete| concrete == owner)
                        })
                };
                if state.targets.iter().any(implementation_matches) {
                    state.targets.retain(implementation_matches);
                    if concrete.is_some() {
                        state.interface_dispatch = false;
                    }
                } else {
                    state.targets.retain(|target| {
                        rust_defaults.contains(target)
                            && rust_methods
                                .get(target)
                                .is_some_and(|(owner, _, _)| owner == contract)
                    });
                    if concrete.is_some() {
                        state.interface_dispatch = false;
                    }
                }
            } else if state.targets.iter().any(|target| {
                rust_methods
                    .get(target)
                    .is_some_and(|(_, implementation, declaration)| {
                        implementation.is_none() && !declaration
                    })
            }) {
                // Inherent methods take precedence over trait methods in ordinary receiver dispatch.
                state.targets.retain(|target| {
                    rust_methods
                        .get(target)
                        .is_none_or(|(_, implementation, declaration)| {
                            implementation.is_none() && !declaration
                        })
                });
            }
        }
    }
    fn assign_return_type(&self, state: &mut CallState, ctx: &mut CallContext<'_, '_, '_>) {
        let returns_by_id = &self.returns_by_id;
        let call = ctx.call;
        let local_types = &mut *ctx.local_types;
        state.targets.sort();
        state.targets.dedup();
        if state.interface_dispatch && state.targets.len() > 1 {
            let interface = state.receiver_type.as_deref().unwrap_or("");
            state.targets.retain(|id| {
                !id.ends_with(&format!("::{interface}::{}", call.name))
                    && !id.ends_with(&format!(":{interface}::{}", call.name))
                    && !id.ends_with(&format!(".{interface}.{}", call.name))
            });
        }
        if let Some(var) = &call.assignment_to {
            let inferred = if state.constructor_type.is_some() {
                state.constructor_type.clone()
            } else if call.kind == CallKind::Constructor {
                call.receiver_type
                    .clone()
                    .or_else(|| Some(call.name.clone()))
            } else if state.inheritance_uncertain {
                None
            } else {
                let inferred = state
                    .targets
                    .iter()
                    .map(|id| returns_by_id.get(id).cloned())
                    .collect::<Option<BTreeSet<_>>>();
                inferred
                    .filter(|items| items.len() == 1)
                    .and_then(|items| items.into_iter().next())
            };
            if let Some(ty) = inferred {
                local_types.to_mut().insert(var.clone(), ty);
            } else if local_types.contains_key(var) {
                local_types.to_mut().remove(var);
            }
        }
    }
    fn emit_runtime(
        &self,
        state: &mut CallState,
        ctx: &mut CallContext<'_, '_, '_>,
        emission: &mut Emission<'_>,
    ) {
        let file = ctx.file;
        let call = ctx.call;
        let generated = &mut *emission.generated;
        state.targets.sort();
        state.targets.dedup();
        if state.targets.is_empty()
            && (state.standard || state.inherited_standard_receiver.is_some())
            || state.inheritance_uncertain && state.inherited_standard_receiver.is_some()
        {
            let standard_receiver = state
                .standard_receiver
                .as_deref()
                .or(state.inherited_standard_receiver.as_deref());
            let symbol = standard_receiver
                .map(|r| {
                    if call.receiver.is_none() {
                        if call.kind == CallKind::Constructor {
                            format!("{r}::constructor")
                        } else {
                            r.to_owned()
                        }
                    } else {
                        format!("{r}::{}", call.name)
                    }
                })
                .unwrap_or_else(|| call.name.trim_start_matches('\\').to_owned());
            let language = file.language.clone();
            let id = format!("external:runtime:{language}:{symbol}");
            generated.entry(id.clone()).or_insert_with(|| GraphNode {
                id: id.clone(),
                label: symbol.clone(),
                kind: NodeKind::External,
                file: None,
                line: None,
                language: Some(language.clone()),
                module: None,
                parent_type: standard_receiver.map(str::to_owned),
                vendor: false,
                package: None,
                category: Some("runtime".into()),
                library: Some(language),
                operation: Some(symbol),
                argument: None,
                algorithmic_complexity: None,
                cargo_target: None,
                cargo_target_id: None,
                source_fqn: None,
            });
            state.targets.push(id);
        }
    }
    fn emit_unknown(
        &self,
        state: &mut CallState,
        ctx: &mut CallContext<'_, '_, '_>,
        emission: &mut Emission<'_>,
    ) {
        let file = ctx.file;
        let call = ctx.call;
        let from = ctx.from;
        let strategy = ctx.strategy;
        let imports = ctx.imports_index.at(&ctx.call.range);
        let generated = &mut *emission.generated;
        if state.targets.is_empty() {
            // Unknown receivers must not merge unrelated methods with the same name.
            let known_type = state
                .constructor_type
                .as_deref()
                .or(state.receiver_type.as_deref())
                .map(|t| imports.get(t).map(String::as_str).unwrap_or(t))
                .or_else(|| {
                    if call.binding_unknown || call.kind != CallKind::Constructor {
                        return None;
                    }
                    imports.get(&call.name).map(String::as_str).or_else(|| {
                        file.types
                            .iter()
                            .find(|t| t.name == call.name || t.fqn == call.name)
                            .map(|t| t.fqn.as_str())
                    })
                });
            let method = if call.kind == CallKind::Constructor || state.constructor_type.is_some() {
                strategy.constructor_name()
            } else {
                call.name.as_str()
            };
            let label = if let Some(ty) = known_type {
                format!("{ty}::{method}")
            } else if let Some(receiver) = &call.receiver {
                format!("{receiver}->{}", call.name)
            } else {
                call.name.clone()
            };
            let id = if (call.receiver.is_some() && known_type.is_none()) || call.binding_unknown {
                format!("unknown:{}:{}:{label}", file.language, from)
            } else {
                format!("unknown:{}:{label}", file.language)
            };
            generated.entry(id.clone()).or_insert_with(|| GraphNode {
                id: id.clone(),
                label,
                kind: NodeKind::Unknown,
                file: None,
                line: None,
                language: Some(file.language.clone()),
                module: None,
                parent_type: known_type.map(str::to_owned),
                vendor: false,
                package: None,
                category: None,
                library: None,
                operation: None,
                argument: None,
                algorithmic_complexity: None,
                cargo_target: None,
                cargo_target_id: None,
                source_fqn: None,
            });
            state.targets.push(id);
        }
    }
    fn emit_resolved(
        &self,
        state: &mut CallState,
        ctx: &mut CallContext<'_, '_, '_>,
        emission: &mut Emission<'_>,
    ) {
        let uncertain_functions = &self.uncertain_functions;
        let call = ctx.call;
        let from = ctx.from;
        let graph = &mut *emission.graph;
        let edge_ids = &mut *emission.edge_ids;
        let confidence = if state.targets[0].starts_with("unknown:") {
            if state
                .candidates
                .iter()
                .any(|candidate| candidate.starts_with("<ambiguous-package>::"))
            {
                Confidence::Ambiguous
            } else {
                Confidence::Dynamic
            }
        } else if state.targets.len() > 1
            || state.inheritance_uncertain
            || state.interface_dispatch
            || uncertain_functions.contains(from.as_ref())
            || state
                .targets
                .iter()
                .any(|target| uncertain_functions.contains(target))
        {
            Confidence::Ambiguous
        } else {
            Confidence::Resolved
        };
        for to in std::mem::take(&mut state.targets) {
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
    }
}
