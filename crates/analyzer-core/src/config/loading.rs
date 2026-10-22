//! Include expansion preserves source-local provenance before merging documents.
use super::*;

pub(super) struct Expansion<'a> {
    pub loader: &'a ConfigLoader,
    pub file: &'a Path,
    pub stack: &'a mut Vec<PathBuf>,
    pub depth: usize,
    pub state: &'a mut LoadState,
    pub path: &'a str,
}

impl Expansion<'_> {
    pub fn value(&mut self, value: Value) -> Value {
        match value {
            Value::Mapping(map) => self.mapping(map),
            Value::Sequence(items) => self.sequence(items),
            other => other,
        }
    }

    fn paths(&mut self, pattern: &str) -> Vec<PathBuf> {
        let children = expand_glob(self.file.parent().unwrap_or(Path::new(".")), pattern);
        if children.is_empty() {
            self.state.warn(
                self.path,
                &format!("include matched no files: {pattern}"),
                self.file,
            );
            self.state.skipped_files.push(pattern.into());
        }
        children
    }

    fn read(&mut self, child: &Path) -> Option<Value> {
        let text = match std::fs::read_to_string(child) {
            Ok(text) => text,
            Err(error) => {
                self.state.warn(
                    self.path,
                    &format!("included file unreadable: {error}"),
                    child,
                );
                self.state.skipped_files.push(child.display().to_string());
                return None;
            }
        };
        match serde_yaml::from_str(&text) {
            Ok(value) => Some(value),
            Err(error) => {
                self.state.warn(
                    self.path,
                    &format!("included YAML syntax error: {error}"),
                    child,
                );
                self.state.skipped_files.push(child.display().to_string());
                None
            }
        }
    }

    fn mapping(&mut self, mut map: Mapping) -> Value {
        if self.path.is_empty() && map.remove(Value::String("core".into())).is_some() {
            self.state.warn(
                "core",
                "core settings in included YAML are ignored; use the primary config",
                self.file,
            );
        }
        let include = if self.path == "paths" {
            None
        } else {
            map.remove(Value::String("include".into()))
        };
        let mut base = Value::Mapping(Mapping::new());
        if let Some(include) = include {
            for pattern in include_patterns(include) {
                for child in self.paths(&pattern) {
                    let Some(value) = self.read(&child) else {
                        continue;
                    };
                    let value = self.loader.expand_at(
                        value,
                        &child,
                        self.stack,
                        self.depth + 1,
                        self.state,
                        self.path,
                    );
                    if value.is_mapping() {
                        merge(&mut base, value);
                    } else {
                        self.state.warn(
                            self.path,
                            "mapping include must contain a mapping",
                            &child,
                        );
                    }
                }
            }
        }
        let mut explicit = Mapping::new();
        for (key, value) in map {
            if let Value::String(name) = &key {
                let path = child_path(self.path, name);
                self.state.provenance.insert(
                    path.clone(),
                    SourceLocation {
                        file: self.file.display().to_string(),
                        line: find_key_line(self.file, &path),
                    },
                );
                explicit.insert(
                    key,
                    self.loader
                        .expand_inner(value, self.file, self.stack, self.depth, self.state, &path),
                );
            }
        }
        merge(&mut base, Value::Mapping(explicit));
        base
    }

    fn include_sequence(&mut self, include: Value, out: &mut Vec<Value>) {
        for pattern in include_patterns(include) {
            for child in self.paths(&pattern) {
                let Some(value) = self.read(&child) else {
                    continue;
                };
                let mut local = LoadState::default();
                let value = self.loader.expand_at(
                    value,
                    &child,
                    self.stack,
                    self.depth + 1,
                    &mut local,
                    self.path,
                );
                if let Value::Sequence(items) = value {
                    merge_indexed_provenance(self.state, local, self.path, out.len());
                    out.extend(items);
                } else {
                    self.state
                        .warn(self.path, "list include must contain a list", &child);
                }
            }
        }
    }

    fn sequence(&mut self, items: Vec<Value>) -> Value {
        let mut out = Vec::new();
        for (index, item) in items.into_iter().enumerate() {
            let include = item
                .as_mapping()
                .filter(|map| map.len() == 1)
                .and_then(|map| map.get(Value::String("include".into())))
                .cloned();
            if let Some(include) = include {
                self.include_sequence(include, &mut out);
                continue;
            }
            let mut local = LoadState::default();
            let expanded = self.loader.expand_inner(
                item,
                self.file,
                self.stack,
                self.depth,
                &mut local,
                &format!("{}.{index}", self.path),
            );
            merge_item_provenance(self.state, local, self.path, index, out.len());
            out.push(expanded);
        }
        Value::Sequence(out)
    }
}
