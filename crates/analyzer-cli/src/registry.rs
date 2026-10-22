//! Built-in language registration shared by CLI entry points and server tests.
use analyzer_core::LanguageRegistry;
pub(crate) fn registry() -> LanguageRegistry {
    let mut r = LanguageRegistry::new();
    r.register(analyzer_python::Python);
    r.register(analyzer_javascript::JavaScript);
    r.register(analyzer_typescript::TypeScript);
    r.register(analyzer_php::Php);
    r.register(analyzer_rust::Rust);
    r
}
