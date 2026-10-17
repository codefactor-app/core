//! Recognize language-owned calls after project symbols have been considered.
//! Qualified imports and types are accepted; arbitrary receiver variable names are not.
use crate::runtime_symbols::*;

pub fn function(language: &str, symbol: &str) -> bool {
    let name = symbol.trim_start_matches('\\');
    match language {
        "php" => PHP_FUNCTIONS
            .binary_search(&name.to_ascii_lowercase().as_str())
            .is_ok(),
        "python" => PYTHON_FUNCTIONS.binary_search(&name).is_ok(),
        "javascript" | "typescript" => JAVASCRIPT_GLOBALS.binary_search(&name).is_ok(),
        "rust" => matches!(
            name,
            "drop" | "size_of" | "size_of_val" | "align_of" | "align_of_val"
        ),
        _ => false,
    }
}

pub fn macro_name(language: &str, name: &str) -> bool {
    language == "rust"
        && matches!(
            name.trim_end_matches('!'),
            "println"
                | "print"
                | "eprintln"
                | "eprint"
                | "format"
                | "format_args"
                | "vec"
                | "dbg"
                | "panic"
                | "assert"
                | "assert_eq"
                | "assert_ne"
                | "debug_assert"
                | "debug_assert_eq"
                | "debug_assert_ne"
                | "todo"
                | "unimplemented"
                | "unreachable"
                | "matches"
                | "include"
                | "include_str"
                | "include_bytes"
                | "env"
                | "option_env"
                | "concat"
                | "stringify"
                | "file"
                | "line"
                | "column"
                | "module_path"
        )
}

/// Used only when an import or inferred type establishes the receiver's identity.
pub fn receiver(language: &str, receiver: &str) -> bool {
    let name = receiver.trim_start_matches('\\');
    match language {
        "php" => PHP_TYPES
            .binary_search(&name.to_ascii_lowercase().as_str())
            .is_ok(),
        "python" => PYTHON_RECEIVERS
            .binary_search(&name.split('.').next().unwrap_or(name))
            .is_ok(),
        "javascript" | "typescript" => {
            let module = name
                .strip_prefix("node:")
                .unwrap_or(name)
                .split('.')
                .next()
                .unwrap_or(name);
            global_receiver(language, name)
                || NODE_MODULES.binary_search(&module).is_ok()
                || matches!(name, "string" | "number" | "boolean" | "bigint" | "symbol")
        }
        "rust" => global_receiver(language, name),
        _ => false,
    }
}

/// Unbound identifiers must be actual language globals, rather than module-like names.
pub fn global_receiver(language: &str, name: &str) -> bool {
    match language {
        "javascript" | "typescript" => JAVASCRIPT_GLOBALS
            .binary_search(&name.split('.').next().unwrap_or(name))
            .is_ok(),
        "rust" => {
            name.starts_with("std::")
                || name.starts_with("core::")
                || name.starts_with("alloc::")
                || matches!(
                    name.split("::").next().unwrap_or(name),
                    "String"
                        | "Vec"
                        | "Option"
                        | "Result"
                        | "Box"
                        | "Iterator"
                        | "IntoIterator"
                        | "str"
                        | "char"
                        | "bool"
                        | "u8"
                        | "u16"
                        | "u32"
                        | "u64"
                        | "u128"
                        | "usize"
                        | "i8"
                        | "i16"
                        | "i32"
                        | "i64"
                        | "i128"
                        | "isize"
                        | "f32"
                        | "f64"
                )
        }
        _ => false,
    }
}

/// A project subclass can inherit a native method without becoming a native type.
pub fn method(language: &str, owner: &str, name: &str) -> bool {
    let ty = owner.trim_start_matches('\\');
    let (table, ty, name) = match language {
        "php" => (
            PHP_METHODS,
            ty.to_ascii_lowercase(),
            name.to_ascii_lowercase(),
        ),
        "python" => (
            PYTHON_METHODS,
            ty.strip_prefix("builtins.").unwrap_or(ty).to_owned(),
            name.to_owned(),
        ),
        "javascript" | "typescript" => (JAVASCRIPT_METHODS, ty.to_owned(), name.to_owned()),
        _ => return false,
    };
    table
        .binary_search_by(|(owner, _)| owner.cmp(&ty.as_str()))
        .ok()
        .is_some_and(|i| {
            table[i]
                .1
                .split_ascii_whitespace()
                .any(|method| method == name)
        })
}
