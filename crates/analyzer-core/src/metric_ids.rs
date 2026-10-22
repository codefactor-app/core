//! Built-in metric vocabulary shared by configuration and calculation.
pub fn builtin_metric_ids() -> &'static [&'static str] {
    &[
        "file_lines",
        "functions_per_file",
        "types_per_file",
        "methods_per_class",
        "fields_per_class",
        "function_lines",
        "function_body_lines",
        "function_params",
        "constructor_params",
        "cyclomatic_complexity",
        "nesting_depth",
        "return_points",
        "comment_ratio",
        "inheritance_depth",
        "lcom4",
        "fan_in",
        "fan_out",
        "halstead_volume",
        "maintainability_index",
        "cognitive_complexity",
        "duplicate_functions",
        "module_cycle",
    ]
}
