//! Ordered inheritance and trait composition. These rules deliberately keep
//! incomplete hierarchies uncertain instead of selecting an arbitrary ancestor.
use super::function_id;
use crate::{FileModel, LanguageRegistry, TypeKind, TypeModel};
use std::collections::{BTreeMap, BTreeSet};

type Parents<'a> = BTreeMap<&'a str, BTreeMap<&'a str, Vec<String>>>;

pub(super) fn super_arguments(receiver: &str) -> Option<Vec<String>> {
    let receiver = receiver.trim();
    let arguments = receiver
        .strip_prefix("super")?
        .trim()
        .strip_prefix('(')?
        .strip_suffix(')')?;
    Some(
        crate::resolution::split_arguments(arguments)
            .into_iter()
            .map(|argument| argument.trim().to_owned())
            .collect(),
    )
}

#[derive(Default)]
pub(super) struct Lookup {
    pub targets: Vec<String>,
    pub uncertain: bool,
    pub runtime_owner: Option<String>,
}

#[derive(Clone)]
struct PhpMember {
    target: String,
    visibility: String,
    owner: String,
    origin: String,
}

#[derive(Clone, Default)]
struct PhpMembers {
    methods: BTreeMap<String, Vec<PhpMember>>,
    uncertain: bool,
}

pub(super) struct InheritanceIndex {
    python: BTreeMap<String, Result<Vec<String>, String>>,
    php: BTreeMap<String, PhpMembers>,
    php_parents: BTreeMap<String, Vec<String>>,
    php_traits: BTreeSet<String>,
    /// Runtime consumer and lexical class where the trait body was injected.
    php_consumers: BTreeMap<String, Vec<(String, String)>>,
    pub diagnostics: Vec<String>,
    strategies: LanguageRegistry,
}

impl InheritanceIndex {
    pub fn new(
        files: &[FileModel],
        parents: &Parents<'_>,
        registry: &LanguageRegistry,
        resolve: impl Fn(&FileModel, &TypeModel, &str) -> String,
    ) -> Self {
        let mut result = Self {
            python: BTreeMap::new(),
            php: BTreeMap::new(),
            php_parents: parents
                .get("php")
                .into_iter()
                .flat_map(|parents| parents.iter())
                .map(|(owner, bases)| (owner.to_string(), bases.clone()))
                .collect(),
            php_traits: BTreeSet::new(),
            php_consumers: BTreeMap::new(),
            diagnostics: Vec::new(),
            strategies: registry.clone(),
        };
        let python_types = files
            .iter()
            .filter(|file| file.language == "python")
            .flat_map(|file| {
                file.types
                    .iter()
                    .filter(|ty| ty.kind == TypeKind::Class)
                    .map(move |ty| (ty.fqn.as_str(), (file, ty)))
            })
            .collect::<BTreeMap<_, _>>();
        let python_parents = parents.get("python").cloned().unwrap_or_default();
        for (owner, (file, _)) in &python_types {
            let mro = python_mro(
                owner,
                &python_types,
                &python_parents,
                &mut result.python,
                &mut BTreeSet::new(),
            );
            if let Err(reason) = mro {
                result.diagnostics.push(format!(
                    "{}: Python MRO for {owner} cannot be determined: {reason}",
                    file.path
                ));
            }
        }
        let php_types = files
            .iter()
            .filter(|file| file.language == "php")
            .flat_map(|file| {
                file.types
                    .iter()
                    .filter(|ty| matches!(ty.kind, TypeKind::Class | TypeKind::Trait))
                    .map(move |ty| (ty.fqn.as_str(), (file, ty)))
            })
            .collect::<BTreeMap<_, _>>();
        for (owner, (file, ty)) in &php_types {
            if ty.kind == TypeKind::Trait {
                result.php_traits.insert(owner.to_string());
            }
            let members = php_members(
                owner,
                &php_types,
                &resolve,
                &mut result.php,
                &mut BTreeSet::new(),
            );
            if members.uncertain && !ty.trait_uses.is_empty() {
                result.diagnostics.push(format!(
                    "{}: PHP trait composition for {owner} has an unknown trait or conflicting adaptation",
                    file.path
                ));
            }
        }
        for (owner, (_, ty)) in &php_types {
            if ty.kind != TypeKind::Class {
                continue;
            }
            let mut pending = vec![*owner];
            let mut visited = BTreeSet::new();
            let mut shadowed = BTreeSet::new();
            while let Some(base) = pending.pop() {
                if !visited.insert(base) {
                    continue;
                }
                if let Some(members) = result.php.get(base) {
                    for (name, candidates) in &members.methods {
                        if shadowed.insert(name.as_str()) {
                            for member in candidates {
                                result
                                    .php_consumers
                                    .entry(member.target.clone())
                                    .or_default()
                                    .push((owner.to_string(), member.owner.clone()));
                            }
                        }
                    }
                }
                pending.extend(
                    result
                        .php_parents
                        .get(base)
                        .into_iter()
                        .flatten()
                        .map(String::as_str),
                );
            }
        }
        result
    }

    pub fn python_order(&self, owner: &str) -> Option<&[String]> {
        self.python.get(owner)?.as_ref().ok().map(Vec::as_slice)
    }

    pub fn php_type(&self, owner: &str) -> bool {
        self.php.contains_key(owner)
    }

    pub fn python_lookup(
        &self,
        owner: &str,
        method: &str,
        super_anchor: Option<&str>,
        mut declared: impl FnMut(&str, &str) -> Vec<String>,
    ) -> Lookup {
        let mut result = Lookup::default();
        let Some(order) = self.python_order(owner) else {
            result.uncertain = self.python.contains_key(owner);
            return result;
        };
        // A super() body can execute on a subclass instance. Consider every
        // known applicable MRO; cooperative diamonds can advance to a sibling.
        let orders = if let Some(anchor) = super_anchor {
            self.python
                .values()
                .filter_map(|order| order.as_ref().ok())
                .filter(|order| order.iter().any(|ty| ty == owner))
                .filter_map(|order| {
                    order
                        .iter()
                        .position(|ty| ty == anchor)
                        .map(|position| &order[position + 1..])
                })
                .collect::<Vec<_>>()
        } else {
            vec![order]
        };
        for order in orders {
            for base in order {
                let found = declared(base, method);
                if !found.is_empty() {
                    result.targets.extend(found);
                    break;
                }
                if self
                    .strategies
                    .get("python")
                    .is_some_and(|strategy| strategy.is_native_method(base, method))
                {
                    result.runtime_owner = Some(base.clone());
                    break;
                }
            }
        }
        result.targets.sort();
        result.targets.dedup();
        result.uncertain |= result.targets.len() > 1
            || (!result.targets.is_empty() && result.runtime_owner.is_some());
        result
    }

    pub fn php_lookup(&self, owner: &str, method: &str, caller: Option<&str>) -> Lookup {
        let mut result = Lookup::default();
        let mut pending = vec![owner];
        let mut visited = BTreeSet::new();
        while let Some(owner) = pending.pop() {
            if !visited.insert(owner) {
                result.uncertain = true;
                continue;
            }
            if let Some(members) = self.php.get(owner) {
                result.uncertain |= members.uncertain;
                if let Some(found) = members.methods.get(&method.to_ascii_lowercase()) {
                    for member in found {
                        let accessible = match member.visibility.as_str() {
                            "private" => caller == Some(member.owner.as_str()),
                            "protected" => caller.is_some_and(|caller| {
                                caller == member.owner
                                    || self.php_is_subclass(caller, &member.owner)
                            }),
                            _ => true,
                        };
                        if accessible {
                            result.targets.push(member.target.clone());
                        }
                    }
                    // An inaccessible declaration still prevents a public
                    // ancestor from being invented as the call's target.
                    continue;
                }
            } else if self
                .strategies
                .get("php")
                .is_some_and(|strategy| strategy.is_native_method(owner, method))
            {
                result.runtime_owner = Some(owner.into());
                continue;
            } else {
                result.uncertain = true;
            }
            pending.extend(
                self.php_parents
                    .get(owner)
                    .into_iter()
                    .flatten()
                    .map(String::as_str),
            );
        }
        result.targets.sort();
        result.targets.dedup();
        result.uncertain |= result.targets.len() > 1
            || (!result.targets.is_empty() && result.runtime_owner.is_some());
        result
    }

    pub fn php_trait_lookup(
        &self,
        body: &str,
        trait_owner: Option<&str>,
        receiver: Option<&str>,
        method: &str,
    ) -> Option<Lookup> {
        trait_owner.filter(|owner| self.php_traits.contains(*owner))?;
        let receiver = receiver
            .filter(|receiver| ["$this", "this", "self", "static", "parent"].contains(receiver))?;
        let consumers = self.php_consumers.get(body)?;
        let mut result = Lookup::default();
        for (consumer, scope) in consumers {
            let private = self
                .php
                .get(scope)
                .and_then(|members| members.methods.get(&method.to_ascii_lowercase()))
                .is_some_and(|members| members.iter().any(|member| member.visibility == "private"));
            let owners = if receiver == "parent" {
                self.php_parents
                    .get(scope)
                    .map(Vec::as_slice)
                    .unwrap_or_default()
            } else if receiver == "self" || private && ["$this", "this"].contains(&receiver) {
                std::slice::from_ref(scope)
            } else {
                std::slice::from_ref(consumer)
            };
            for owner in owners {
                let lookup = self.php_lookup(owner, method, Some(scope));
                result.uncertain |= lookup.uncertain || lookup.targets.is_empty();
                result.targets.extend(lookup.targets);
                result.runtime_owner = result.runtime_owner.or(lookup.runtime_owner);
            }
        }
        result.targets.sort();
        result.targets.dedup();
        result.uncertain |= result.targets.len() > 1
            || (!result.targets.is_empty() && result.runtime_owner.is_some());
        Some(result)
    }

    fn php_is_subclass(&self, owner: &str, base: &str) -> bool {
        let mut pending = vec![owner];
        let mut visited = BTreeSet::new();
        while let Some(owner) = pending.pop() {
            if owner == base {
                return true;
            }
            if visited.insert(owner) {
                pending.extend(
                    self.php_parents
                        .get(owner)
                        .into_iter()
                        .flatten()
                        .map(String::as_str),
                );
            }
        }
        false
    }
}

fn python_mro(
    owner: &str,
    types: &BTreeMap<&str, (&FileModel, &TypeModel)>,
    parents: &BTreeMap<&str, Vec<String>>,
    cache: &mut BTreeMap<String, Result<Vec<String>, String>>,
    visiting: &mut BTreeSet<String>,
) -> Result<Vec<String>, String> {
    if let Some(order) = cache.get(owner) {
        return order.clone();
    }
    if !visiting.insert(owner.into()) {
        return Err(format!("cyclic inheritance at {owner}"));
    }
    let result = (|| {
        let bases = if let Some((_, ty)) = types.get(owner) {
            if ty.inheritance_unknown {
                return Err("dynamic base expression or custom metaclass".into());
            }
            parents.get(owner).cloned().unwrap_or_default()
        } else if owner == "object" {
            return Ok(vec![owner.into()]);
        } else if let Some(bases) = python_builtin_bases(owner) {
            bases.iter().map(|base| base.to_string()).collect()
        } else {
            return Err(format!("unknown base {owner}"));
        };
        if bases.iter().collect::<BTreeSet<_>>().len() != bases.len() {
            return Err("duplicate base class".into());
        }
        let bases = if bases.is_empty() {
            vec!["object".into()]
        } else {
            bases
                .into_iter()
                .map(|base| {
                    if !types.contains_key(base.as_str()) {
                        if let Some(builtin) = base.strip_prefix("builtins.") {
                            if builtin == "object" || python_builtin_bases(builtin).is_some() {
                                return builtin.to_string();
                            }
                        }
                    }
                    base
                })
                .collect()
        };
        let mut sequences = bases
            .iter()
            .map(|base| python_mro(base, types, parents, cache, visiting))
            .collect::<Result<Vec<_>, _>>()?;
        sequences.push(bases);
        let mut order = vec![owner.into()];
        while sequences.iter().any(|sequence| !sequence.is_empty()) {
            let next = sequences
                .iter()
                .filter_map(|sequence| sequence.first())
                .find(|head| {
                    sequences
                        .iter()
                        .all(|sequence| !sequence.iter().skip(1).any(|tail| tail == *head))
                })
                .cloned()
                .ok_or_else(|| "inconsistent C3 base ordering".to_string())?;
            for sequence in &mut sequences {
                if sequence.first() == Some(&next) {
                    sequence.remove(0);
                }
            }
            order.push(next);
        }
        Ok(order)
    })();
    visiting.remove(owner);
    cache.insert(owner.into(), result.clone());
    result
}

// Only documented builtin class hierarchies are known here. Runtime receiver
// catalogs also contain modules and cannot prove that a name is a base class.
fn python_builtin_bases(owner: &str) -> Option<&'static [&'static str]> {
    Some(match owner {
        "bool" => &["int"],
        "Exception" | "GeneratorExit" | "KeyboardInterrupt" | "SystemExit"
        | "BaseExceptionGroup" => &["BaseException"],
        "ExceptionGroup" => &["BaseExceptionGroup", "Exception"],
        "ArithmeticError" | "AssertionError" | "AttributeError" | "BufferError" | "EOFError"
        | "ImportError" | "LookupError" | "MemoryError" | "NameError" | "OSError"
        | "ReferenceError" | "RuntimeError" | "StopAsyncIteration" | "StopIteration"
        | "SyntaxError" | "SystemError" | "TypeError" | "ValueError" | "Warning" => &["Exception"],
        "FloatingPointError" | "OverflowError" | "ZeroDivisionError" => &["ArithmeticError"],
        "ModuleNotFoundError" => &["ImportError"],
        "IndexError" | "KeyError" => &["LookupError"],
        "UnboundLocalError" => &["NameError"],
        "BlockingIOError" | "ChildProcessError" | "ConnectionError" | "FileExistsError"
        | "FileNotFoundError" | "InterruptedError" | "IsADirectoryError" | "NotADirectoryError"
        | "PermissionError" | "ProcessLookupError" | "TimeoutError" => &["OSError"],
        "BrokenPipeError"
        | "ConnectionAbortedError"
        | "ConnectionRefusedError"
        | "ConnectionResetError" => &["ConnectionError"],
        "NotImplementedError" | "PythonFinalizationError" | "RecursionError" => &["RuntimeError"],
        "IndentationError" => &["SyntaxError"],
        "TabError" => &["IndentationError"],
        "UnicodeError" => &["ValueError"],
        "UnicodeDecodeError" | "UnicodeEncodeError" | "UnicodeTranslateError" => &["UnicodeError"],
        "BytesWarning"
        | "DeprecationWarning"
        | "EncodingWarning"
        | "FutureWarning"
        | "ImportWarning"
        | "PendingDeprecationWarning"
        | "ResourceWarning"
        | "RuntimeWarning"
        | "SyntaxWarning"
        | "UnicodeWarning"
        | "UserWarning" => &["Warning"],
        "BaseException" | "bytearray" | "bytes" | "classmethod" | "complex" | "dict"
        | "enumerate" | "filter" | "float" | "frozenset" | "int" | "list" | "map"
        | "memoryview" | "property" | "range" | "reversed" | "set" | "slice" | "staticmethod"
        | "str" | "super" | "tuple" | "type" | "zip" => &["object"],
        _ => return None,
    })
}

fn php_members(
    owner: &str,
    types: &BTreeMap<&str, (&FileModel, &TypeModel)>,
    resolve: &impl Fn(&FileModel, &TypeModel, &str) -> String,
    cache: &mut BTreeMap<String, PhpMembers>,
    visiting: &mut BTreeSet<String>,
) -> PhpMembers {
    if let Some(members) = cache.get(owner) {
        return members.clone();
    }
    let Some((file, ty)) = types.get(owner) else {
        return PhpMembers {
            uncertain: true,
            ..Default::default()
        };
    };
    if !visiting.insert(owner.into()) {
        return PhpMembers {
            uncertain: true,
            ..Default::default()
        };
    }
    let mut result = PhpMembers::default();
    for usage in &ty.trait_uses {
        let traits = usage
            .traits
            .iter()
            .map(|raw| {
                let identity = resolve(file, ty, raw);
                let mut members = php_members(&identity, types, resolve, cache, visiting);
                if types
                    .get(identity.as_str())
                    .is_none_or(|(_, ty)| ty.kind != TypeKind::Trait)
                {
                    members.uncertain = true;
                }
                (identity, members)
            })
            .collect::<BTreeMap<_, _>>();
        let mut composed = PhpMembers::default();
        for (identity, members) in &traits {
            composed.uncertain |= members.uncertain;
            for (method, candidates) in &members.methods {
                composed.methods.entry(method.clone()).or_default().extend(
                    candidates.iter().cloned().map(|mut member| {
                        member.origin = identity.clone();
                        member
                    }),
                );
            }
        }
        for adaptation in usage
            .adaptations
            .iter()
            .filter(|a| !a.instead_of.is_empty())
        {
            let method = adaptation.method.to_ascii_lowercase();
            let source = adaptation
                .trait_name
                .as_ref()
                .map(|raw| resolve(file, ty, raw));
            if source
                .as_ref()
                .and_then(|source| traits.get(source))
                .and_then(|members| members.methods.get(&method))
                .is_none()
            {
                composed.uncertain = true;
                continue;
            }
            for loser in &adaptation.instead_of {
                let identity = resolve(file, ty, loser);
                if traits
                    .get(&identity)
                    .and_then(|members| members.methods.get(&method))
                    .is_some()
                {
                    if let Some(selected) = composed.methods.get_mut(&method) {
                        selected.retain(|member| member.origin != identity);
                    }
                } else {
                    composed.uncertain = true;
                }
            }
        }
        for adaptation in usage.adaptations.iter().filter(|a| a.instead_of.is_empty()) {
            let method = adaptation.method.to_ascii_lowercase();
            let source = adaptation
                .trait_name
                .as_ref()
                .map(|raw| resolve(file, ty, raw));
            let selected = if let Some(source) = &source {
                traits
                    .get(source)
                    .and_then(|members| members.methods.get(&method))
            } else {
                composed.methods.get(&method)
            };
            let Some(selected) = selected.cloned() else {
                composed.uncertain = true;
                continue;
            };
            let mut selected = selected;
            if let Some(source) = source {
                for member in &mut selected {
                    member.origin = source.clone();
                }
            }
            if let Some(visibility) = &adaptation.visibility {
                for member in &mut selected {
                    member.visibility = visibility.clone();
                }
            }
            if let Some(alias) = &adaptation.alias {
                composed
                    .methods
                    .entry(alias.to_ascii_lowercase())
                    .or_default()
                    .extend(selected);
            } else if let Some(existing) = composed.methods.get_mut(&method) {
                for member in existing {
                    if let Some(replacement) = selected
                        .iter()
                        .find(|item| item.target == member.target && item.origin == member.origin)
                    {
                        *member = replacement.clone();
                    }
                }
            }
        }
        result.uncertain |= composed.uncertain;
        for (method, candidates) in composed.methods {
            result.methods.entry(method).or_default().extend(candidates);
        }
    }
    // Methods declared on the class/outer trait override imported methods.
    for function in file.functions.iter().filter(|function| {
        function.parent_type.as_deref() == Some(owner)
            && function.parent_function.is_none()
            && !function
                .modifiers
                .iter()
                .any(|modifier| modifier == "abstract")
    }) {
        result.methods.insert(
            function.name.to_ascii_lowercase(),
            vec![PhpMember {
                target: function_id(file, &function.fqn),
                visibility: function
                    .modifiers
                    .iter()
                    .find(|modifier| {
                        ["private", "protected", "public"].contains(&modifier.as_str())
                    })
                    .cloned()
                    .unwrap_or_else(|| "public".into()),
                owner: owner.into(),
                origin: owner.into(),
            }],
        );
    }
    for members in result.methods.values_mut() {
        members.sort_by(|left, right| left.target.cmp(&right.target));
        members.dedup_by(|left, right| left.target == right.target);
        if members.len() > 1 {
            result.uncertain = true;
        }
        for member in members {
            member.owner = owner.into();
        }
    }
    visiting.remove(owner);
    cache.insert(owner.into(), result.clone());
    result
}
