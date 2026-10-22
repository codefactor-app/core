//! Normalize trait composition from syntax nodes; namespace imports use a
//! different grammar node and never enter this path.
use crate::support::{named_children, range, snippet};
use analyzer_core::{TraitAdaptation, TraitUse};
use std::path::Path;
use tree_sitter::Node;

pub(super) fn normalize(node: Node<'_>, source: &str, path: &Path) -> TraitUse {
    let mut result = TraitUse {
        range: range(node, path),
        ..Default::default()
    };
    for child in named_children(node) {
        if child.kind() != "use_list" {
            result.traits.push(snippet(child, source).trim().into());
            continue;
        }
        for clause in named_children(child) {
            let children = named_children(clause);
            let Some(member) = children.first() else {
                continue;
            };
            let raw = snippet(*member, source).trim();
            let (trait_name, method) = raw
                .rsplit_once("::")
                .map(|(owner, method)| (Some(owner.trim().into()), method.trim()))
                .unwrap_or((None, raw));
            let mut adaptation = TraitAdaptation {
                trait_name,
                method: method.into(),
                ..Default::default()
            };
            for target in children.iter().skip(1) {
                let raw = snippet(*target, source).trim().to_owned();
                if clause.kind() == "use_instead_of_clause" {
                    adaptation.instead_of.push(raw);
                } else if target.kind() == "visibility_modifier" {
                    adaptation.visibility = Some(raw);
                } else {
                    adaptation.alias = Some(raw);
                }
            }
            result.adaptations.push(adaptation);
        }
    }
    result
}
