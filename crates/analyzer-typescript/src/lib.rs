use analyzer_core::{
    CallSite, Capabilities, ExternalOperationRule, FileModel, LanguageStrategy, Loc, RawAst,
    TypeContext,
};
use analyzer_parser::{
    count_lines, normalize, parse, resolve_call as parser_resolve_call, Dialect,
};
use std::path::Path;
#[derive(Default, Clone, Copy)]
pub struct TypeScript;
impl LanguageStrategy for TypeScript {
    fn version(&self) -> &'static str {
        "12"
    }
    fn language_id(&self) -> &'static str {
        "typescript"
    }
    fn file_extensions(&self) -> &'static [&'static str] {
        &["ts", "tsx", "mts", "cts"]
    }
    fn parse(&self, source: &str, path: &Path) -> RawAst {
        parse(
            source,
            path,
            if path.extension().and_then(|x| x.to_str()) == Some("tsx") {
                tree_sitter_typescript::LANGUAGE_TSX.into()
            } else {
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
            },
            Dialect::TypeScript,
        )
    }
    fn normalize(&self, source: &str, path: &Path, _ast: &RawAst) -> FileModel {
        normalize(
            source,
            path,
            if path.extension().and_then(|x| x.to_str()) == Some("tsx") {
                tree_sitter_typescript::LANGUAGE_TSX.into()
            } else {
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
            },
            Dialect::TypeScript,
        )
    }
    fn analyze(&self, source: &str, path: &Path) -> FileModel {
        normalize(
            source,
            path,
            if path.extension().and_then(|x| x.to_str()) == Some("tsx") {
                tree_sitter_typescript::LANGUAGE_TSX.into()
            } else {
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
            },
            Dialect::TypeScript,
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
            if path.extension().and_then(|x| x.to_str()) == Some("tsx") {
                tree_sitter_typescript::LANGUAGE_TSX.into()
            } else {
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
            },
            Dialect::TypeScript,
            enabled,
        )
    }
    fn count_lines(&self, source: &str) -> Loc {
        count_lines(source, Dialect::TypeScript)
    }
    fn resolve_call(
        &self,
        call: &CallSite,
        _context: &TypeContext,
        file: &FileModel,
    ) -> Vec<String> {
        parser_resolve_call(call, file, Dialect::TypeScript)
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
        "constructor"
    }
    fn namespace_separator(&self) -> &'static str {
        "."
    }
    fn method_separator(&self) -> &'static str {
        "."
    }
    fn type_reference_syntax(&self) -> analyzer_core::types::TypeReferenceSyntax {
        analyzer_parser::reference_syntax(Dialect::TypeScript)
    }
    fn type_reference(&self, raw: &str) -> Option<String> {
        analyzer_parser::type_reference(self.language_id(), raw)
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            static_types: true,
            generics: true,
            inheritance: true,
            closures: true,
            guards: true,
        }
    }
    fn external_operations(&self) -> Vec<ExternalOperationRule> {
        catalog(&[
            ("db", "pg.*.query", "pg"),
            ("db", "mysql2.*.execute", "mysql2"),
            ("db", "Prisma.*", "Prisma"),
            ("queue", "amqplib.*.publish", "amqplib"),
            ("shell", "child_process.execFile", "child_process"),
            ("shell", "child_process.exec", "child_process"),
            ("http", "fetch", "fetch"),
            ("http", "axios.get", "axios"),
            ("http", "http.request", "http"),
            ("tcp", "net.connect", "net"),
        ])
    }
}
fn catalog(items: &[(&str, &str, &str)]) -> Vec<ExternalOperationRule> {
    items
        .iter()
        .map(|(category, pattern, library)| ExternalOperationRule {
            category: (*category).into(),
            pattern: (*pattern).into(),
            library: (*library).into(),
            operation: pattern.rsplit('.').next().unwrap_or(pattern).into(),
        })
        .collect()
}
