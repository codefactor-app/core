//! Descriptor-aware JS/TS property resolution. Data fields never become calls.
use super::*;

#[derive(Clone, Default)]
struct Descriptor {
    getter: Vec<String>,
    setter: Vec<String>,
    value_type: Option<String>,
    data_field: bool,
    methods: Vec<String>,
    mutated: bool,
}

struct MutationEffect {
    function: String,
    owner: Option<String>,
    is_static: Option<bool>,
    name: String,
}

#[derive(Default)]
pub(super) struct PropertyIndex {
    aliases: BTreeMap<(String, String), Vec<String>>,
    parents: BTreeMap<(String, String), Vec<String>>,
    members: BTreeMap<(String, String, String, bool), Descriptor>,
    unknown_keys: BTreeSet<(String, String, bool)>,
    mutations: Vec<MutationEffect>,
}

impl PropertyIndex {
    pub(super) fn new(
        files: &[FileModel],
        modules: &ModuleIndex<'_>,
        exports: &BTreeMap<&str, BTreeMap<String, String>>,
        types: &BTreeSet<(&str, &str)>,
    ) -> Self {
        let mut index = Self::default();
        let symbol_constants = files
            .iter()
            .filter(|file| matches!(file.language.as_str(), "javascript" | "typescript"))
            .flat_map(|file| {
                file.functions
                    .iter()
                    .filter(|function| function.is_module_initializer())
                    .flat_map(move |function| {
                        function
                            .calls
                            .iter()
                            .filter(|call| call.symbol_constant_assignment)
                            .filter_map(move |call| {
                                call.assignment_to.as_ref().map(|name| (file, name))
                            })
                    })
            })
            .map(|(file, name)| {
                let owner = format!("{}.{}", file.module, name);
                source_owner(file, &owner)
            })
            .collect::<BTreeSet<_>>();
        for file in files
            .iter()
            .filter(|file| matches!(file.language.as_str(), "javascript" | "typescript"))
        {
            for ty in file
                .types
                .iter()
                .filter(|ty| ty.kind != TypeKind::Interface)
            {
                let identity = source_owner(file, &ty.fqn);
                for language in ["javascript", "typescript"] {
                    for alias in [&ty.fqn, &identity] {
                        let owners = index
                            .aliases
                            .entry((language.into(), alias.clone()))
                            .or_default();
                        if !owners.contains(&identity) {
                            owners.push(identity.clone());
                        }
                    }
                    index.parents.insert(
                        (language.into(), identity.clone()),
                        ty.extends
                            .iter()
                            .map(|base| {
                                resolve_type_reference(
                                    file, base, &ty.fqn, &ty.range, modules, exports, types,
                                )
                            })
                            .collect(),
                    );
                    for field in &ty.fields {
                        if field
                            .symbol_key_reference
                            .as_ref()
                            .is_some_and(|reference| {
                                reference.starts_with("#symbol:")
                                    || symbol_constants.contains(&resolve_type_reference(
                                        file,
                                        reference,
                                        &ty.fqn,
                                        &field.range,
                                        modules,
                                        exports,
                                        types,
                                    ))
                            })
                        {
                            // A proven Symbol key is disjoint from every string
                            // member name in this index. Unknown keys still block.
                            continue;
                        }
                        if field.unknown_key {
                            index.unknown_keys.insert((
                                language.into(),
                                identity.clone(),
                                field.is_static,
                            ));
                            continue;
                        }
                        let value_type = field
                            .type_name
                            .as_ref()
                            .and_then(|raw| modules.type_reference(file, raw))
                            .map(|raw| {
                                resolve_type_reference(
                                    file,
                                    &raw,
                                    &ty.fqn,
                                    &field.range,
                                    modules,
                                    exports,
                                    types,
                                )
                            });
                        index.members.insert(
                            (
                                language.into(),
                                identity.clone(),
                                field.name.clone(),
                                field.is_static,
                            ),
                            Descriptor {
                                value_type,
                                data_field: true,
                                ..Descriptor::default()
                            },
                        );
                    }
                }
            }
            for function in file
                .functions
                .iter()
                .filter(|function| function.parent_function.is_none())
            {
                let Some(owner) = &function.parent_type else {
                    continue;
                };
                let owner = source_owner(file, owner);
                let is_static = function
                    .modifiers
                    .iter()
                    .any(|modifier| modifier == "static");
                let accessor = function
                    .modifiers
                    .iter()
                    .any(|modifier| modifier == "accessor");
                for language in ["javascript", "typescript"] {
                    let descriptor = index
                        .members
                        .entry((
                            language.into(),
                            owner.clone(),
                            function.name.clone(),
                            is_static,
                        ))
                        .or_default();
                    if descriptor.data_field {
                        continue;
                    }
                    if accessor && function.fqn.ends_with("#get") {
                        descriptor.getter.push(function_id(file, &function.fqn));
                        descriptor.value_type = function
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
                    } else if accessor && function.fqn.ends_with("#set") {
                        descriptor.setter.push(function_id(file, &function.fqn));
                    } else {
                        descriptor.methods.push(function_id(file, &function.fqn));
                    }
                }
            }
        }
        for file in files
            .iter()
            .filter(|file| matches!(file.language.as_str(), "javascript" | "typescript"))
        {
            for function in &file.functions {
                for mutation in &function.property_mutations {
                    let owner = index.owner_for_site(
                        file,
                        function,
                        &mutation.site,
                        &TypeContext::default(),
                        &BTreeMap::new(),
                        modules,
                        exports,
                        types,
                    );
                    let Some((owner, is_static)) = owner else {
                        continue;
                    };
                    let Some(identities) = index.aliases.get(&(file.language.clone(), owner))
                    else {
                        continue;
                    };
                    if identities.len() != 1 {
                        continue;
                    }
                    // An unknown local object is not evidence that every class
                    // in the project has changed. Cross-function effects require
                    // one proven owner; local uncertainty is retained below.
                    let owner = Some(identities[0].clone());
                    let is_static = Some(is_static && !mutation.prototype);
                    index.mutations.push(MutationEffect {
                        function: function_id(file, &function.fqn),
                        owner,
                        is_static,
                        name: mutation.site.name.clone(),
                    });
                }
            }
        }
        index
    }

    fn descriptors<'a>(
        &'a self,
        language: &str,
        owner: &str,
        name: &str,
        is_static: bool,
    ) -> (Vec<&'a Descriptor>, bool) {
        let mut pending = vec![owner.to_owned()];
        let mut visited = BTreeSet::new();
        let mut found = Vec::new();
        let mut blocked = false;
        while let Some(owner) = pending.pop() {
            let owners = self
                .aliases
                .get(&(language.into(), owner.clone()))
                .cloned()
                .unwrap_or_else(|| vec![owner]);
            for owner in owners {
                if !visited.insert(owner.clone()) {
                    continue;
                }
                if self
                    .unknown_keys
                    .contains(&(language.into(), owner.clone(), is_static))
                {
                    // An unevaluated computed data key may replace this own
                    // descriptor or any inherited one. Do not guess its value.
                    blocked = true;
                    continue;
                }
                if let Some(member) =
                    self.members
                        .get(&(language.into(), owner.clone(), name.into(), is_static))
                {
                    // The complete own descriptor shadows the parent, even if it has
                    // only a setter, getter, method or untyped data field.
                    found.push(member);
                } else {
                    pending.extend(
                        self.parents
                            .get(&(language.into(), owner))
                            .into_iter()
                            .flatten()
                            .cloned(),
                    );
                }
            }
        }
        (found, blocked)
    }

    fn value_type(
        &self,
        language: &str,
        owner: &str,
        name: &str,
        is_static: bool,
    ) -> Option<String> {
        let (descriptors, blocked) = self.descriptors(language, owner, name, is_static);
        if blocked {
            return None;
        }
        let values = descriptors
            .iter()
            .filter_map(|descriptor| descriptor.value_type.as_deref())
            .collect::<BTreeSet<_>>();
        (descriptors
            .iter()
            .all(|descriptor| descriptor.value_type.is_some())
            && values.len() == 1)
            .then(|| values.into_iter().next().unwrap().to_owned())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn resolve(
        &self,
        file: &FileModel,
        function: &FunctionModel,
        access: &PropertyAccess,
        context: &TypeContext,
        locals: &BTreeMap<String, String>,
        modules: &ModuleIndex<'_>,
        exports: &BTreeMap<&str, BTreeMap<String, String>>,
        types: &BTreeSet<(&str, &str)>,
    ) -> Vec<String> {
        let site = &access.site;
        let (descriptors, blocked) = self.for_site(
            file, function, site, context, locals, modules, exports, types,
        );
        if blocked && site.receiver_types.is_empty() {
            return Vec::new();
        }
        descriptors
            .into_iter()
            .flat_map(|descriptor| match access.access {
                AccessorKind::Getter => descriptor.getter,
                AccessorKind::Setter => descriptor.setter,
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn callable_getter(
        &self,
        file: &FileModel,
        function: &FunctionModel,
        site: &CallSite,
        context: &TypeContext,
        locals: &BTreeMap<String, String>,
        modules: &ModuleIndex<'_>,
        exports: &BTreeMap<&str, BTreeMap<String, String>>,
        types: &BTreeSet<(&str, &str)>,
    ) -> Option<Vec<String>> {
        let (descriptors, blocked) = self.for_site(
            file, function, site, context, locals, modules, exports, types,
        );
        if blocked && site.receiver_types.is_empty() {
            return None;
        }
        descriptors
            .iter()
            .any(|descriptor| {
                !descriptor.getter.is_empty()
                    || !descriptor.setter.is_empty()
                    || !descriptor.methods.is_empty()
            })
            .then(|| {
                descriptors
                    .into_iter()
                    .flat_map(|descriptor| descriptor.getter.into_iter().chain(descriptor.methods))
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect()
            })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn blocks_inherited_call(
        &self,
        file: &FileModel,
        function: &FunctionModel,
        site: &CallSite,
        context: &TypeContext,
        locals: &BTreeMap<String, String>,
        modules: &ModuleIndex<'_>,
        exports: &BTreeMap<&str, BTreeMap<String, String>>,
        types: &BTreeSet<(&str, &str)>,
    ) -> bool {
        if site.receiver.is_none() {
            return false;
        }
        let (descriptors, blocked) = self.for_site(
            file, function, site, context, locals, modules, exports, types,
        );
        blocked
            || descriptors.iter().any(|descriptor| descriptor.data_field)
            || (descriptors.is_empty()
                && self.known_class_receiver(
                    file, function, site, context, locals, modules, exports, types,
                ))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn blocks_direct_call(
        &self,
        file: &FileModel,
        function: &FunctionModel,
        site: &CallSite,
        context: &TypeContext,
        locals: &BTreeMap<String, String>,
        modules: &ModuleIndex<'_>,
        exports: &BTreeMap<&str, BTreeMap<String, String>>,
        types: &BTreeSet<(&str, &str)>,
    ) -> bool {
        if site.receiver.is_none() {
            return false;
        }
        let (descriptors, blocked) = self.for_site(
            file, function, site, context, locals, modules, exports, types,
        );
        blocked
            || (descriptors.is_empty()
                && self.known_class_receiver(
                    file, function, site, context, locals, modules, exports, types,
                ))
            || descriptors
                .iter()
                .any(|descriptor| descriptor.mutated && descriptor.data_field)
    }

    #[allow(clippy::too_many_arguments)]
    fn known_class_receiver(
        &self,
        file: &FileModel,
        function: &FunctionModel,
        site: &CallSite,
        context: &TypeContext,
        locals: &BTreeMap<String, String>,
        modules: &ModuleIndex<'_>,
        exports: &BTreeMap<&str, BTreeMap<String, String>>,
        types: &BTreeSet<(&str, &str)>,
    ) -> bool {
        self.owner_for_site(
            file, function, site, context, locals, modules, exports, types,
        )
        .is_some_and(|(owner, _)| self.aliases.contains_key(&(file.language.clone(), owner)))
    }

    #[allow(clippy::too_many_arguments)]
    fn for_site(
        &self,
        file: &FileModel,
        function: &FunctionModel,
        site: &CallSite,
        context: &TypeContext,
        locals: &BTreeMap<String, String>,
        modules: &ModuleIndex<'_>,
        exports: &BTreeMap<&str, BTreeMap<String, String>>,
        types: &BTreeSet<(&str, &str)>,
    ) -> (Vec<Descriptor>, bool) {
        let alternatives = if site.receiver_types.is_empty() {
            vec![site.clone()]
        } else {
            site.receiver_types
                .iter()
                .map(|ty| {
                    let mut alternative = site.clone();
                    alternative.receiver_types.clear();
                    if site
                        .receiver
                        .as_ref()
                        .is_some_and(|receiver| receiver.contains('.'))
                    {
                        alternative.receiver_type = None;
                        alternative.receiver_root_type = Some(ty.clone());
                    } else {
                        alternative.receiver_type = Some(ty.clone());
                    }
                    alternative
                })
                .collect()
        };
        let mut all = Vec::new();
        let mut any_blocked = false;
        for alternative in alternatives {
            let Some((owner, is_static)) = self.owner_for_site(
                file,
                function,
                &alternative,
                context,
                locals,
                modules,
                exports,
                types,
            ) else {
                any_blocked |= site.binding_unknown || !site.receiver_types.is_empty();
                continue;
            };
            let (descriptors, mut blocked) =
                self.descriptors(&file.language, &owner, &site.name, is_static);
            let mut descriptors = descriptors.into_iter().cloned().collect::<Vec<_>>();
            if !is_static && self.instance_field_shadows(&file.language, &owner, &site.name) {
                descriptors.retain(|descriptor| descriptor.data_field);
                blocked |= descriptors.is_empty();
            }
            let mut own_change = false;
            for effect in &self.mutations {
                if effect.function != function_id(file, &function.fqn)
                    && (effect.name == "*" || effect.name == site.name)
                    && effect.is_static.is_none_or(|kind| kind == is_static)
                    && effect.owner.as_ref().is_some_and(|target| {
                        self.owner_applies(&file.language, &owner, target, &site.name, is_static)
                    })
                {
                    descriptors.clear();
                    blocked = true;
                }
            }
            for mutation in &function.property_mutations {
                if (mutation.site.range.end_line, mutation.site.range.end_column)
                    > (site.range.start_line, site.range.start_column)
                    || (mutation.site.name != "*" && mutation.site.name != site.name)
                {
                    continue;
                }
                let target = self.owner_for_site(
                    file,
                    function,
                    &mutation.site,
                    context,
                    locals,
                    modules,
                    exports,
                    types,
                );
                if let Some((target, target_static)) = &target {
                    if (*target_static && !mutation.prototype) != is_static
                        || !self.owner_applies(
                            &file.language,
                            &owner,
                            target,
                            &site.name,
                            is_static,
                        )
                    {
                        continue;
                    }
                }
                if mutation.prototype
                    && !is_static
                    && self.instance_field_shadows(&file.language, &owner, &site.name)
                {
                    continue;
                }
                let same_instance = mutation.site.receiver_identity.is_some()
                    && mutation.site.receiver_identity == alternative.receiver_identity;
                let proven_owner = target.is_some()
                    && mutation.site.receiver_types.is_empty()
                    && (mutation.prototype || is_static || same_instance);
                if !proven_owner
                    || mutation.unknown
                    || mutation.site.name == "*"
                    || !mutation.site.guard.is_empty()
                    || !mutation.site.in_loop.is_empty()
                {
                    descriptors.clear();
                    blocked = true;
                    continue;
                }
                let mut descriptor = if blocked {
                    Descriptor::default()
                } else if own_change && descriptors.len() == 1 {
                    descriptors.pop().unwrap()
                } else if mutation.prototype || is_static {
                    target
                        .as_ref()
                        .and_then(|(owner, _)| {
                            self.own_descriptor(&file.language, owner, &site.name, is_static)
                        })
                        .cloned()
                        .unwrap_or_default()
                } else {
                    Descriptor::default()
                };
                if mutation.data {
                    descriptor = Descriptor {
                        data_field: true,
                        ..Descriptor::default()
                    };
                } else {
                    if descriptor.data_field {
                        descriptor = Descriptor::default();
                    }
                    descriptor.methods.clear();
                    if mutation.getter_present {
                        descriptor.getter =
                            self.descriptor_function(file, mutation.getter.as_ref());
                        descriptor.value_type = mutation
                            .getter
                            .as_ref()
                            .and_then(|range| file.functions.iter().find(|f| f.range == *range))
                            .and_then(|f| {
                                f.return_type.as_ref().map(|raw| {
                                    resolve_type_reference(
                                        file, raw, &f.fqn, &f.range, modules, exports, types,
                                    )
                                })
                            });
                    }
                    if mutation.setter_present {
                        descriptor.setter =
                            self.descriptor_function(file, mutation.setter.as_ref());
                    }
                }
                descriptors = vec![descriptor];
                descriptors[0].mutated = true;
                blocked = false;
                own_change = true;
            }
            any_blocked |= blocked;
            all.extend(descriptors);
        }
        (all, any_blocked)
    }

    fn descriptor_function(&self, file: &FileModel, range: Option<&SourceRange>) -> Vec<String> {
        range
            .and_then(|range| {
                file.functions
                    .iter()
                    .find(|function| function.range == *range)
            })
            .map(|function| vec![function_id(file, &function.fqn)])
            .unwrap_or_default()
    }

    fn own_descriptor(
        &self,
        language: &str,
        owner: &str,
        name: &str,
        is_static: bool,
    ) -> Option<&Descriptor> {
        let owners = self
            .aliases
            .get(&(language.into(), owner.into()))
            .cloned()
            .unwrap_or_else(|| vec![owner.into()]);
        (owners.len() == 1)
            .then(|| {
                self.members
                    .get(&(language.into(), owners[0].clone(), name.into(), is_static))
            })
            .flatten()
    }

    fn instance_field_shadows(&self, language: &str, owner: &str, name: &str) -> bool {
        let mut pending = vec![owner.to_owned()];
        let mut visited = BTreeSet::new();
        while let Some(owner) = pending.pop() {
            let owners = self
                .aliases
                .get(&(language.into(), owner.clone()))
                .cloned()
                .unwrap_or_else(|| vec![owner]);
            for owner in owners {
                if !visited.insert(owner.clone()) {
                    continue;
                }
                if self
                    .unknown_keys
                    .contains(&(language.into(), owner.clone(), false))
                    || self
                        .members
                        .get(&(language.into(), owner.clone(), name.into(), false))
                        .is_some_and(|descriptor| descriptor.data_field)
                {
                    return true;
                }
                pending.extend(
                    self.parents
                        .get(&(language.into(), owner))
                        .into_iter()
                        .flatten()
                        .cloned(),
                );
            }
        }
        false
    }

    fn owner_applies(
        &self,
        language: &str,
        receiver: &str,
        target: &str,
        name: &str,
        is_static: bool,
    ) -> bool {
        let target = self
            .aliases
            .get(&(language.into(), target.into()))
            .cloned()
            .unwrap_or_else(|| vec![target.into()]);
        let mut pending = vec![receiver.to_owned()];
        let mut visited = BTreeSet::new();
        while let Some(owner) = pending.pop() {
            let owners = self
                .aliases
                .get(&(language.into(), owner.clone()))
                .cloned()
                .unwrap_or_else(|| vec![owner]);
            for owner in owners {
                if !visited.insert(owner.clone()) {
                    continue;
                }
                if target.contains(&owner) {
                    return true;
                }
                if self.members.contains_key(&(
                    language.into(),
                    owner.clone(),
                    name.into(),
                    is_static,
                )) || self
                    .unknown_keys
                    .contains(&(language.into(), owner.clone(), is_static))
                {
                    continue;
                }
                pending.extend(
                    self.parents
                        .get(&(language.into(), owner))
                        .into_iter()
                        .flatten()
                        .cloned(),
                );
            }
        }
        false
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn receiver_type(
        &self,
        file: &FileModel,
        function: &FunctionModel,
        site: &CallSite,
        context: &TypeContext,
        locals: &BTreeMap<String, String>,
        modules: &ModuleIndex<'_>,
        exports: &BTreeMap<&str, BTreeMap<String, String>>,
        types: &BTreeSet<(&str, &str)>,
    ) -> Option<String> {
        self.owner_for_site(
            file, function, site, context, locals, modules, exports, types,
        )
        .map(|(owner, _)| owner)
    }

    #[allow(clippy::too_many_arguments)]
    fn owner_for_site(
        &self,
        file: &FileModel,
        function: &FunctionModel,
        site: &CallSite,
        context: &TypeContext,
        locals: &BTreeMap<String, String>,
        modules: &ModuleIndex<'_>,
        exports: &BTreeMap<&str, BTreeMap<String, String>>,
        types: &BTreeSet<(&str, &str)>,
    ) -> Option<(String, bool)> {
        if site.binding_unknown {
            return None;
        }
        let receiver = site.receiver.as_ref()?;
        let parts = receiver.split('.').collect::<Vec<_>>();
        let root = parts[0];
        let resolve = |raw: &str| {
            let owner = resolve_type_reference(
                file,
                raw,
                &function.fqn,
                &site.range,
                modules,
                exports,
                types,
            );
            // A lexical class has a physical source identity. A same-named
            // declaration in a sibling .js/.ts file is not another receiver.
            if file
                .types
                .iter()
                .any(|ty| ty.kind != TypeKind::Interface && ty.fqn == owner)
            {
                source_owner(file, &owner)
            } else {
                owner
            }
        };
        let mut is_static = !site.local_binding
            && file
                .types
                .iter()
                .any(|ty| ty.name == root || ty.fqn == receiver.as_str());
        if root == "this" || root == "super" {
            is_static = function
                .modifiers
                .iter()
                .any(|modifier| matches!(modifier.as_str(), "static" | "lexical_static_this"));
        }
        let initial = site.receiver_type.as_ref().map(|raw| resolve(raw));
        let (mut owner, remaining) = if let Some(owner) = initial {
            if !site.local_binding
                && !matches!(root, "this" | "super")
                && self
                    .aliases
                    .contains_key(&(file.language.clone(), owner.clone()))
            {
                is_static = true;
            }
            (Some(owner), &parts[parts.len()..])
        } else {
            let raw = site
                .receiver_root_type
                .as_ref()
                .or_else(|| locals.get(root))
                .or_else(|| context.variables.get(root));
            let owner = raw.map(|raw| resolve(raw)).or_else(|| {
                if root == "super" {
                    function
                        .parent_type
                        .as_ref()
                        .and_then(|parent| {
                            self.parents
                                .get(&(file.language.clone(), source_owner(file, parent)))
                        })
                        .filter(|parents| parents.len() == 1)
                        .map(|parents| parents[0].clone())
                } else if !site.local_binding {
                    let candidate = resolve(receiver);
                    self.aliases
                        .contains_key(&(file.language.clone(), candidate.clone()))
                        .then_some(candidate)
                } else {
                    None
                }
            });
            (owner, &parts[1..])
        };
        for property in remaining {
            let affected_globally = self.mutations.iter().any(|effect| {
                effect.function != function_id(file, &function.fqn)
                    && (effect.name == "*" || effect.name == *property)
                    && effect.is_static.is_none_or(|kind| kind == is_static)
                    && owner.as_ref().is_some_and(|owner| {
                        effect.owner.as_ref().is_some_and(|target| {
                            self.owner_applies(&file.language, owner, target, property, is_static)
                        })
                    })
            });
            let affected_locally = function.property_mutations.iter().any(|mutation| {
                (mutation.site.range.end_line, mutation.site.range.end_column)
                    <= (site.range.start_line, site.range.start_column)
                    && (mutation.site.name == "*" || mutation.site.name == *property)
            });
            if affected_globally || affected_locally {
                return None;
            }
            owner = owner
                .and_then(|owner| self.value_type(&file.language, &owner, property, is_static));
            is_static = false;
        }
        owner.map(|owner| (owner, is_static))
    }
}

fn source_owner(file: &FileModel, owner: &str) -> String {
    owner
        .strip_prefix(&file.module)
        .map(|suffix| format!("{}{suffix}", file.path))
        .unwrap_or_else(|| owner.to_owned())
}
