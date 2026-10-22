//! Cost configuration schema registration; no evaluator dependency.
use super::schema::KeySpec;
use crate::algorithmic::model::parse_limit;
use std::sync::Arc;
pub(super) fn keys() -> Vec<KeySpec> {
    let mut keys = Vec::new();
    let mapping: crate::config::Validator = Arc::new(|v| {
        if v.is_mapping() {
            Ok(())
        } else {
            Err("expected mapping".into())
        }
    });
    keys.push(KeySpec::new(
        "algorithmic_complexity",
        "Symbolic worst-case execution cost",
        mapping.clone(),
    ));
    for key in [
        "algorithmic_complexity.by_language",
        "algorithmic_complexity.by_entity",
        "algorithmic_complexity.by_language.*",
        "algorithmic_complexity.by_entity.*",
        "algorithmic_complexity.by_path.*",
    ] {
        keys.push(KeySpec::new(key, "Complexity overrides", mapping.clone()));
    }
    keys.push(KeySpec::new(
        "algorithmic_complexity.by_path",
        "Ordered path overrides",
        Arc::new(|v| {
            if v.is_sequence() {
                Ok(())
            } else {
                Err("expected list of path overrides".into())
            }
        }),
    ));
    keys.push(KeySpec::new(
        "algorithmic_complexity.by_path.*.glob",
        "Path override glob",
        Arc::new(|v| {
            let glob = v.as_str().ok_or("expected glob string")?;
            globset::Glob::new(glob)
                .map(|_| ())
                .map_err(|e| format!("invalid algorithmic path glob: {e}"))
        }),
    ));
    for scope in [
        "algorithmic_complexity",
        "algorithmic_complexity.by_language.*",
        "algorithmic_complexity.by_entity.*",
        "algorithmic_complexity.by_path.*",
    ] {
        let optional = scope != "algorithmic_complexity";
        for field in ["enabled", "heuristics"] {
            keys.push(KeySpec::new(
                format!("{scope}.{field}"),
                "Boolean complexity setting",
                Arc::new(move |v| {
                    if v.is_bool() || (optional && v.is_null()) {
                        Ok(())
                    } else {
                        Err("expected boolean".into())
                    }
                }),
            ));
        }
        keys.push(KeySpec::new(
            format!("{scope}.unknown"),
            "Unknown cost severity",
            Arc::new(move |v| {
                if (optional && v.is_null())
                    || v.as_str()
                        .is_some_and(|s| ["info", "warn", "error"].contains(&s))
                {
                    Ok(())
                } else {
                    Err("expected info, warn or error".into())
                }
            }),
        ));
        keys.push(KeySpec::new(
            format!("{scope}.limits"),
            "Symbolic complexity thresholds",
            mapping.clone(),
        ));
        for field in ["warn", "error"] {
            keys.push(KeySpec::new(
                format!("{scope}.limits.{field}"),
                "Symbolic complexity threshold",
                Arc::new(|v| {
                    if v.is_null() {
                        return Ok(());
                    }
                    let limit = v.as_str().ok_or("expected symbolic complexity string")?;
                    parse_limit(limit).map(|_| ())
                }),
            ));
        }
    }
    keys
}
