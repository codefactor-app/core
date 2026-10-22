use analyzer_core::{
    CallSite, Capabilities, ExternalOperationRule, FileModel, LanguageStrategy, Loc, RawAst,
    TypeContext,
};
use analyzer_parser::runtime::external_catalog as catalog;
use analyzer_parser::{
    count_lines, normalize, parse, resolve_call as parser_resolve_call, Dialect,
};
use std::path::Path;

#[derive(Default, Clone, Copy)]
pub struct Python;
impl LanguageStrategy for Python {
    fn version(&self) -> &'static str {
        "12"
    }
    fn language_id(&self) -> &'static str {
        "python"
    }
    fn file_extensions(&self) -> &'static [&'static str] {
        &["py", "pyi"]
    }
    fn parse(&self, source: &str, path: &Path) -> RawAst {
        parse(
            source,
            path,
            tree_sitter_python::LANGUAGE.into(),
            Dialect::Python,
        )
    }
    fn normalize(&self, source: &str, path: &Path, _ast: &RawAst) -> FileModel {
        normalize(
            source,
            path,
            tree_sitter_python::LANGUAGE.into(),
            Dialect::Python,
        )
    }
    fn analyze(&self, source: &str, path: &Path) -> FileModel {
        normalize(
            source,
            path,
            tree_sitter_python::LANGUAGE.into(),
            Dialect::Python,
        )
    }
    fn line_counts(&self, source: &str, _path: &Path, model: &FileModel) -> Loc {
        if model.comment_ranges.is_some() {
            model.loc.clone()
        } else {
            self.count_lines(source)
        }
    }
    fn analyze_with_architecture(&self, source: &str, path: &Path, enabled: bool) -> FileModel {
        analyzer_parser::normalize_with_architecture(
            source,
            path,
            tree_sitter_python::LANGUAGE.into(),
            Dialect::Python,
            enabled,
        )
    }
    fn count_lines(&self, source: &str) -> Loc {
        count_lines(source, Dialect::Python)
    }
    fn resolve_call(
        &self,
        call: &CallSite,
        _context: &TypeContext,
        file: &FileModel,
    ) -> Vec<String> {
        parser_resolve_call(call, file, Dialect::Python)
    }
    fn is_native_function(&self, symbol: &str) -> bool {
        analyzer_parser::runtime::function(self.language_id(), symbol)
    }
    fn is_native_receiver(&self, symbol: &str) -> bool {
        analyzer_parser::runtime::receiver(self.language_id(), symbol)
    }
    fn is_native_global_receiver(&self, symbol: &str) -> bool {
        analyzer_parser::runtime::global_receiver(self.language_id(), symbol)
    }
    fn is_native_method(&self, owner: &str, name: &str) -> bool {
        analyzer_parser::runtime::method(self.language_id(), owner, name)
    }
    fn is_native_macro(&self, name: &str) -> bool {
        analyzer_parser::runtime::macro_name(self.language_id(), name)
    }
    fn constructor_name(&self) -> &'static str {
        "__init__"
    }
    fn namespace_separator(&self) -> &'static str {
        "."
    }
    fn method_separator(&self) -> &'static str {
        "."
    }
    fn type_reference_syntax(&self) -> analyzer_core::types::TypeReferenceSyntax {
        analyzer_parser::reference_syntax(Dialect::Python)
    }
    fn type_reference(&self, raw: &str) -> Option<String> {
        analyzer_parser::type_reference(self.language_id(), raw)
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            static_types: false,
            generics: false,
            inheritance: true,
            closures: true,
            guards: true,
        }
    }
    fn external_operations(&self) -> Vec<ExternalOperationRule> {
        catalog(&[
            ("db", "sqlite3.connect", "sqlite3"),
            ("db", "sqlite3.Connection.execute", "sqlite3"),
            ("db", "sqlalchemy.*.execute", "SQLAlchemy"),
            ("db", "psycopg.connect", "psycopg"),
            ("db", "psycopg.*.execute", "psycopg"),
            ("queue", "pika.BlockingConnection", "pika"),
            ("queue", "pika.*.basic_publish", "pika"),
            ("shell", "subprocess.run", "subprocess"),
            ("shell", "subprocess.Popen", "subprocess"),
            ("shell", "os.system", "os"),
            ("http", "requests.get", "requests"),
            ("http", "requests.post", "requests"),
            ("http", "httpx.get", "httpx"),
            ("tcp", "socket.create_connection", "socket"),
            ("tcp", "socket.*.connect", "socket"),
        ])
    }
}
