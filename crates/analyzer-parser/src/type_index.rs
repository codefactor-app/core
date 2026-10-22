//! Borrowed per-file type lookup independent of normalization.
use crate::support::Dialect;
use analyzer_core::ir::{TypeKind, TypeModel};
use std::collections::BTreeMap;
pub(crate) struct TypeIndex<'a> {
    full: BTreeMap<&'a str, &'a String>,
    namespaces: BTreeMap<&'a str, BTreeMap<&'a str, &'a String>>,
    separator: &'static str,
}
impl<'a> TypeIndex<'a> {
    pub(crate) fn new(types: &'a [TypeModel], dialect: Dialect) -> Self {
        let separator = match dialect {
            Dialect::Rust => "::",
            Dialect::Php => "\\",
            _ => ".",
        };
        let mut full = BTreeMap::new();
        let mut namespaces = BTreeMap::<_, BTreeMap<_, _>>::new();
        for ty in types
            .iter()
            .filter(|t| !matches!(t.kind, TypeKind::Namespace | TypeKind::Impl))
        {
            full.insert(ty.fqn.as_str(), &ty.fqn);
            if let Some(parent) = ty
                .fqn
                .strip_suffix(&ty.name)
                .and_then(|p| p.strip_suffix(separator))
            {
                namespaces
                    .entry(parent)
                    .or_default()
                    .entry(ty.name.as_str())
                    .or_insert(&ty.fqn);
            }
        }
        Self {
            full,
            namespaces,
            separator,
        }
    }
    pub(crate) fn scope<'b>(&'b self, owner: &'b str) -> ScopedTypes<'b, 'a> {
        ScopedTypes { index: self, owner }
    }
    pub(crate) fn qualified(&self, name: &str) -> Option<&&'a String> {
        self.full.get(name)
    }
}
pub(crate) struct ScopedTypes<'a, 'types> {
    index: &'a TypeIndex<'types>,
    owner: &'a str,
}
impl ScopedTypes<'_, '_> {
    pub(crate) fn get(&self, name: &str) -> Option<&String> {
        if self.index.separator == "\\" {
            if let Some(absolute) = name.strip_prefix('\\') {
                return self.index.full.get(absolute).copied();
            }
        }
        if let Some(ty) = self.index.full.get(name) {
            return Some(ty);
        }
        let mut context = self.owner;
        while let Some((parent, _)) = context.rsplit_once(self.index.separator) {
            if let Some(ty) = self
                .index
                .namespaces
                .get(parent)
                .and_then(|types| types.get(name))
            {
                return Some(ty);
            }
            context = parent;
        }
        None
    }
    pub(crate) fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }
}
