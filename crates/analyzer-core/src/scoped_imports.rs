//! Source-scoped alias lookup shared by normalization and graph resolution.
use crate::ir::{Import, SourceRange};
use std::collections::BTreeMap;

pub fn alias(import: &Import) -> &str {
    import.alias.as_deref().unwrap_or_else(|| {
        import
            .path
            .rsplit(['.', ':', '/', '\\'])
            .find(|s| !s.is_empty())
            .unwrap_or(&import.path)
    })
}

pub fn applies(import: &Import, range: &SourceRange) -> bool {
    import.scope.as_ref().is_none_or(|scope| {
        (scope.start_line, scope.start_column) <= (range.start_line, range.start_column)
            && (range.start_line, range.start_column) <= (scope.end_line, scope.end_column)
    })
}

struct Entry<'a> {
    import: &'a Import,
    value: String,
}

/// Aliases are indexed once; entries with equal scope starts retain declaration order.
pub struct ScopedImports<'a> {
    aliases: BTreeMap<&'a str, Vec<Entry<'a>>>,
}
impl<'a> ScopedImports<'a> {
    pub fn new(entries: impl IntoIterator<Item = (&'a Import, String)>) -> Self {
        let mut aliases = BTreeMap::<_, Vec<_>>::new();
        for (import, value) in entries {
            aliases
                .entry(alias(import))
                .or_default()
                .push(Entry { import, value });
        }
        for entries in aliases.values_mut() {
            entries.sort_by_key(|e| {
                e.import
                    .scope
                    .as_ref()
                    .map(|s| (s.start_line, s.start_column))
                    .unwrap_or_default()
            });
        }
        Self { aliases }
    }

    pub fn at<'s>(&'s self, range: &'s SourceRange) -> ImportScope<'s, 'a> {
        ImportScope { index: self, range }
    }
}

pub struct ImportScope<'a, 'imports> {
    index: &'a ScopedImports<'imports>,
    range: &'a SourceRange,
}
impl ImportScope<'_, '_> {
    pub fn get(&self, name: &str) -> Option<&String> {
        self.index
            .aliases
            .get(name)?
            .iter()
            .rev()
            .find(|e| applies(e.import, self.range))
            .map(|e| &e.value)
    }

    pub fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Only aliases equal to a symbol or followed by `.` / `::` can expand it.
    /// They are all prefixes, so increasing byte offsets preserve alias sort order.
    pub fn matching_prefixes<'a>(
        &'a self,
        symbol: &'a str,
    ) -> impl Iterator<Item = (&'a str, &'a String)> {
        symbol
            .char_indices()
            .filter_map(|(i, ch)| {
                (ch == '.' || (ch == ':' && symbol[i..].starts_with("::"))).then_some(i)
            })
            .chain(std::iter::once(symbol.len()))
            .filter_map(move |end| {
                let name = &symbol[..end];
                self.get(name).map(|value| (name, value))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn materialized<'a>(imports: &'a [Import], range: &SourceRange) -> BTreeMap<&'a str, String> {
        let mut applicable = imports
            .iter()
            .filter(|i| applies(i, range))
            .collect::<Vec<_>>();
        applicable.sort_by_key(|i| {
            i.scope
                .as_ref()
                .map(|s| (s.start_line, s.start_column))
                .unwrap_or_default()
        });
        applicable
            .into_iter()
            .map(|i| (alias(i), i.path.clone()))
            .collect()
    }

    #[test]
    fn alias_lookup_preserves_scope_boundaries_ties_and_prefix_order() {
        let mut imports = Vec::new();
        for (n, name) in ["", "a", "a.b", "a::b", "é", "ab", "missing", "a"]
            .iter()
            .enumerate()
        {
            for scope in [
                None,
                Some((2, 3, 5, 7)),
                Some((3, 1, 4, 2)),
                Some((2, 3, 2, 3)),
                Some((8, 0, 1, 0)),
            ] {
                imports.push(Import {
                    alias: Some((*name).into()),
                    path: format!("target{n}_{}", imports.len()),
                    scope: scope.map(|(start_line, start_column, end_line, end_column)| {
                        SourceRange {
                            start_line,
                            start_column,
                            end_line,
                            end_column,
                            ..Default::default()
                        }
                    }),
                    ..Default::default()
                });
            }
        }
        imports.push(Import {
            path: "pkg::fallback".into(),
            ..Default::default()
        });
        let index = ScopedImports::new(imports.iter().map(|i| (i, i.path.clone())));
        for line in 0..10 {
            for column in 0..10 {
                let range = SourceRange {
                    start_line: line,
                    start_column: column,
                    ..Default::default()
                };
                let expected = materialized(&imports, &range);
                let scope = index.at(&range);
                for name in ["", "a", "a.b", "a::b", "é", "ab", "fallback", "absent"] {
                    assert_eq!(scope.get(name), expected.get(name));
                }
                for symbol in [
                    "", "a", "a.b.f", "a::b::f", "a.b::f", "é.f", "ab.c", "::a", ".a", "absent.f",
                ] {
                    let old = expected
                        .iter()
                        .filter(|(name, _)| {
                            symbol == **name
                                || symbol.strip_prefix(*name).is_some_and(|rest| {
                                    rest.starts_with('.') || rest.starts_with("::")
                                })
                        })
                        .map(|(name, value)| (*name, value))
                        .collect::<Vec<_>>();
                    assert_eq!(scope.matching_prefixes(symbol).collect::<Vec<_>>(), old);
                }
            }
        }
    }
}
