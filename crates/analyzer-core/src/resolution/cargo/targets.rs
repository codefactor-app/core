//! Cargo target identity is distinct from its physical source root.
use super::*;
#[derive(Default)]
pub(super) struct Builder(BTreeMap<(String, String, String), Target>);
impl Builder {
    pub(super) fn add(&mut self, target: Target) {
        self.0
            .entry((
                target.package.clone(),
                target.kind.clone(),
                target.name.clone(),
            ))
            .or_insert(target);
    }
    pub(super) fn finish(self, context: &mut ResolutionContext) {
        let mut counts = BTreeMap::<String, usize>::new();
        for target in self.0.values() {
            *counts.entry(target.root.clone()).or_default() += 1;
        }
        fn escape(value: &str) -> String {
            value
                .replace('%', "%25")
                .replace('[', "%5B")
                .replace(']', "%5D")
                .replace(':', "%3A")
                .replace('@', "%40")
        }
        for target in self.0.into_values() {
            let id = if counts[&target.root] == 1 {
                target.root.clone()
            } else {
                format!(
                    "{}#target[{}:{}@{}]",
                    target.root,
                    target.kind,
                    escape(&target.name),
                    escape(if target.package.is_empty() {
                        "."
                    } else {
                        &target.package
                    })
                )
            };
            context.cargo_roots.insert(target.root.clone());
            context
                .cargo_root_packages
                .entry(target.root.clone())
                .or_default()
                .insert(target.package.clone());
            context.cargo_targets.insert(id, target);
        }
    }
}
impl ResolutionContext {
    pub(crate) fn rust_target_ids(&self, root: &str) -> Vec<&str> {
        self.cargo_targets
            .iter()
            .filter(|(_, target)| target.root == root)
            .map(|(id, _)| id.as_str())
            .collect()
    }
    pub(crate) fn rust_target_root<'a>(&'a self, id: &str) -> Option<&'a str> {
        self.cargo_targets
            .get(id)
            .map(|target| target.root.as_str())
    }
    pub(in crate::resolution) fn library_target(&self, package: &str) -> Option<&str> {
        self.cargo_targets
            .iter()
            .find(|(_, target)| target.package == package && target.kind == "lib")
            .map(|(id, _)| id.as_str())
    }
}
