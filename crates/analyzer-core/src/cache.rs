use crate::ir::FileModel;
use sha2::{Digest, Sha256};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(serde::Serialize)]
struct CacheEntryRef<'a> {
    model: &'a FileModel,
    function_costs: Vec<Option<&'a crate::algorithmic::model::CostNode>>,
    architecture_facts: Option<&'a crate::architecture::model::FileFacts>,
}
#[derive(serde::Deserialize)]
struct CacheEntry {
    model: FileModel,
    function_costs: Vec<Option<crate::algorithmic::model::CostNode>>,
    #[serde(default)]
    architecture_facts: Option<crate::architecture::model::FileFacts>,
}

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub struct Cache {
    directory: PathBuf,
}
impl Cache {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }
    pub fn content_hash(source: &str, language: &str, version: &str) -> String {
        Self::content_hash_parts([source], language, version)
    }
    /// Hash concatenated source parts without allocating their joined representation.
    pub fn content_hash_parts<'a>(
        parts: impl IntoIterator<Item = &'a str>,
        language: &str,
        version: &str,
    ) -> String {
        let mut hasher = Sha256::new();
        for part in parts {
            hasher.update(part.as_bytes());
        }
        hasher.update(language.as_bytes());
        hasher.update(version.as_bytes());
        format!("{:x}", hasher.finalize())
    }
    pub fn get(&self, key: &str) -> Option<FileModel> {
        let path = self.path(key);
        let bytes = std::fs::read(path).ok()?;
        let mut model = if let Ok(entry) = serde_json::from_slice::<CacheEntry>(&bytes) {
            let mut model = entry.model;
            model.architecture_facts = entry.architecture_facts.map(std::sync::Arc::new);
            if entry.function_costs.len() != model.functions.len() {
                return None;
            }
            for (function, cost) in model.functions.iter_mut().zip(entry.function_costs) {
                function.cost_ir = cost.map(std::sync::Arc::new);
            }
            model
        } else {
            serde_json::from_slice::<FileModel>(&bytes).ok()?
        };
        model.intern_range_paths();
        Some(model)
    }
    pub fn put(&self, key: &str, model: &FileModel) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.directory)?;
        let path = self.path(key);
        let entry = CacheEntryRef {
            model,
            architecture_facts: model.architecture_facts.as_deref(),
            function_costs: model
                .functions
                .iter()
                .map(|f| f.cost_ir.as_deref())
                .collect(),
        };
        let (temp, file) = loop {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let temp = path.with_extension(format!("{}.{sequence}.tmp", std::process::id()));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
            {
                Ok(file) => break (temp, file),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err),
            }
        };
        let result = (|| {
            let mut writer = BufWriter::new(file);
            serde_json::to_writer(&mut writer, &entry).map_err(std::io::Error::other)?;
            writer.flush()?;
            drop(writer);
            std::fs::rename(&temp, path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result
    }
    pub fn directory(&self) -> &Path {
        &self.directory
    }
    fn path(&self, key: &str) -> PathBuf {
        self.directory.join(format!("{key}.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn segmented_hash_preserves_existing_keys() {
        for parts in [
            vec!["", "\0", "", "\0", ""],
            vec!["src/модуль.rs", "\0", "", "\0", "fn café() {}\r\n"],
            vec!["bad.rs", "\0", "invalid UTF-8", "\0", "\u{fffd}"],
        ] {
            assert_eq!(
                Cache::content_hash_parts(parts.iter().copied(), "rust", "core-ir-10:strategy-6"),
                Cache::content_hash(&parts.concat(), "rust", "core-ir-10:strategy-6")
            );
        }
    }
    #[test]
    fn private_cost_ir_roundtrips_and_legacy_entries_remain_readable() {
        use crate::{
            algorithmic::{ArgumentSize, CallCost, CostKind, CostNode, SizeExpr},
            ir::{FunctionModel, SourceRange},
        };
        use std::sync::Arc;
        let directory = std::env::temp_dir().join(format!(
            "analyzer-cache-cost-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let cache = Cache::new(directory.clone());
        let range = SourceRange {
            file: Arc::from("cost.rs"),
            ..SourceRange::default()
        };
        let mut model = FileModel {
            path: "cost.rs".into(),
            ..FileModel::default()
        };
        model.functions.push(FunctionModel {
            range: range.clone(),
            body_range: range.clone(),
            cost_ir: Some(Arc::new(CostNode {
                range: range.clone(),
                kind: CostKind::Sequence(vec![
                    CostNode {
                        range: range.clone(),
                        kind: CostKind::Unknown("unknown size".into()),
                    },
                    CostNode {
                        range,
                        kind: CostKind::Call(Box::new(CallCost {
                            arguments: vec![ArgumentSize::unknown("unknown size")],
                            receiver: Some(ArgumentSize::unknown("unknown size")),
                            builtin: None,
                        })),
                    },
                ]),
            })),
            ..FunctionModel::default()
        });
        let public_json = serde_json::to_string(&model).unwrap();
        assert!(!public_json.contains("cost_ir"));
        cache.put("new", &model).unwrap();
        let decoded = cache.get("new").unwrap();
        let function = &decoded.functions[0];
        let cost = function.cost_ir.as_ref().unwrap();
        let CostKind::Sequence(items) = &cost.kind else {
            panic!("sequence lost")
        };
        let CostKind::Unknown(reason) = &items[0].kind else {
            panic!("reason lost")
        };
        let CostKind::Call(call) = &items[1].kind else {
            panic!("call lost")
        };
        for arg in call.arguments.iter().chain(call.receiver.iter()) {
            for expr in [&arg.value, &arg.length] {
                let SizeExpr::Unknown(shared) = expr else {
                    panic!("size lost")
                };
                assert!(Arc::ptr_eq(reason, shared));
            }
        }
        assert_eq!(
            serde_json::to_value(cost).unwrap(),
            serde_json::to_value(model.functions[0].cost_ir.as_ref().unwrap()).unwrap()
        );
        assert!(Arc::ptr_eq(&function.range.file, &cost.range.file));
        std::fs::write(cache.path("legacy"), public_json).unwrap();
        assert!(cache.get("legacy").unwrap().functions[0].cost_ir.is_none());
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn concurrent_writers_publish_complete_entries() {
        let directory = std::env::temp_dir().join(format!(
            "analyzer-cache-concurrent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let cache = Cache::new(directory.clone());
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            let mut writers = Vec::new();
            for _ in 0..8 {
                let cache = &cache;
                let barrier = &barrier;
                writers.push(scope.spawn(move || {
                    let model = FileModel {
                        path: "x".repeat(256 * 1024),
                        ..FileModel::default()
                    };
                    barrier.wait();
                    cache.put("same-content", &model)
                }));
            }
            for writer in writers {
                writer.join().unwrap().unwrap();
            }
        });
        assert_eq!(cache.get("same-content").unwrap().path.len(), 256 * 1024);
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
