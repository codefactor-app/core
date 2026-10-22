mod registry;
use registry::registry;
mod changes;
mod function_info;
mod logging;
mod presentation;
mod serve;

// Parsing allocates concurrently across Rayon workers. The musl system allocator
// serializes that workload; use per-thread heaps in the static Linux executable.
// Cargo's override feature also covers the C parser's malloc/free calls.
#[cfg(all(target_os = "linux", target_env = "musl"))]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

use analyzer_core::graph::{Direction, GraphFilter, RelationOptions, RelationPathMode};
use analyzer_core::report_markdown::{graph_markdown, scan_markdown};
use analyzer_core::rules::{read_baseline, write_baseline, BaselineError, Severity};
use analyzer_core::Confidence;
use analyzer_core::{AnalysisReport, Analyzer, BaselinePolicy, Config, ConfigLoader};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::io::{self, BufWriter, IsTerminal, Write};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "codefactor",
    version,
    about = "Static analyzer for code structure and architecture"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    #[arg(long, global = true)]
    verbose: bool,
    /// Уровень stderr-логов; переопределяет RUST_LOG и --verbose.
    #[arg(long, global = true, value_parser = ["off", "error", "warn", "info", "debug", "trace"])]
    log_level: Option<String>,
    /// Цвет: auto — только в терминале; always / never — принудительно.
    #[arg(long, global = true, value_enum, default_value = "auto")]
    color: ColorMode,
}
#[derive(Clone, ValueEnum)]
enum ColorMode {
    Auto,
    Always,
    Never,
}
impl ColorMode {
    fn enabled(&self) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::Auto => io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
        }
    }
}
#[derive(Subcommand)]
enum Command {
    Scan(ScanArgs),
    Metrics(MetricsArgs),
    Ast(AstArgs),
    Graph(GraphArgs),
    Baseline(BaselineArgs),
    Config(ConfigArgs),
    Serve(ServeArgs),
    Relations(RelationsArgs),
    External(ExternalArgs),
}
#[derive(Clone, Copy, ValueEnum)]
enum ArchitectureProfile {
    Strict,
    Extended,
}

#[derive(Args, Clone)]
struct Common {
    /// Файл baseline; относительный путь задаётся от корня анализа.
    #[arg(long, conflicts_with = "no_baseline")]
    baseline: Option<PathBuf>,
    /// Показать находки без подавления baseline.
    #[arg(long)]
    no_baseline: bool,
    /// Отключить архитектурный анализ.
    #[arg(long)]
    no_architecture: bool,
    /// strict: проверенные структурные признаки; extended: дополнительные гипотезы.
    #[arg(long, value_enum)]
    architecture_profile: Option<ArchitectureProfile>,
    #[arg(default_value = ".")]
    root: PathBuf,
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long, value_delimiter = ',')]
    lang: Vec<String>,
    #[arg(long)]
    strict_config: bool,
    #[arg(long)]
    fail_on_warn: bool,
    /// Завершить проверку с ошибкой, если часть исходников не удалось разобрать.
    #[arg(long)]
    fail_on_parse_errors: bool,
}
impl Common {
    fn baseline_policy(&self) -> BaselinePolicy {
        if self.no_baseline {
            BaselinePolicy::Disabled
        } else {
            self.baseline
                .clone()
                .map(BaselinePolicy::Path)
                .unwrap_or_default()
        }
    }
}
#[derive(Args)]
struct ScanArgs {
    #[command(flatten)]
    common: Common,
    #[arg(long, value_delimiter = ',')]
    format: Vec<String>,
    #[arg(long)]
    out: Option<PathBuf>,
    #[arg(long)]
    only_violations: bool,
    /// Число замечаний в консольном отчёте.
    #[arg(long, default_value_t = 50)]
    limit: usize,
    /// Вывести все замечания.
    #[arg(long)]
    all: bool,
    /// Скрыть справочник метрик.
    #[arg(long)]
    no_explain: bool,
    /// Показать всё, что компактный отчёт сворачивает: оценки всех функций,
    /// статусы всех архитектурных правил и полные блоки циклов модулей.
    #[arg(long)]
    full: bool,
    /// Сравнить находки с предыдущим полным или компактным JSON-отчётом.
    #[arg(long)]
    compare: Option<PathBuf>,
    /// Пути изменённых файлов: один на строку; '-' читает stdin.
    #[arg(long, conflicts_with = "git_changed")]
    changed_files: Option<PathBuf>,
    /// Файлы, изменённые относительно Git ref, включая untracked.
    #[arg(long, allow_hyphen_values = true)]
    git_changed: Option<String>,
    /// Код 1, если появились новые находки по сравнению с --compare.
    #[arg(long, requires = "compare")]
    fail_on_new: bool,
}
#[derive(Args)]
struct MetricsArgs {
    #[command(flatten)]
    common: Common,
    #[arg(long, value_enum, default_value = "project")]
    group_by: Group,
    #[arg(long, value_parser = ["console", "json"])]
    format: Option<String>,
    #[arg(long)]
    out: Option<PathBuf>,
    #[arg(long)]
    no_explain: bool,
}
#[derive(Clone, ValueEnum)]
enum Group {
    Project,
    Directory,
    Module,
    File,
}
#[derive(Args)]
struct AstArgs {
    path: PathBuf,
    #[arg(long, value_enum, default_value = "json")]
    format: AstFormat,
    #[arg(long)]
    function: Option<String>,
    #[arg(long)]
    out: Option<PathBuf>,
    #[arg(long)]
    config: Option<PathBuf>,
}
#[derive(Clone, ValueEnum)]
enum AstFormat {
    Json,
    Dot,
}
#[derive(Args)]
struct GraphArgs {
    #[command(subcommand)]
    command: GraphCommand,
}
#[derive(Subcommand)]
enum GraphCommand {
    Callers(GraphQueryArgs),
    Callees(GraphQueryArgs),
    Both(GraphQueryArgs),
    Paths {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        shortest: bool,
        #[arg(long, value_delimiter = ',', default_value = "json")]
        format: Vec<String>,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long, default_value_t = 100)]
        max_paths: usize,
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    Dead {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    Roots {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long)]
        config: Option<PathBuf>,
    },
}
#[derive(Args)]
struct GraphQueryArgs {
    selector: String,
    #[arg(long)]
    depth: Option<usize>,
    #[arg(long)]
    with_conditions: bool,
    #[arg(long)]
    summary: bool,
    #[arg(long,default_value_t=true,action=clap::ArgAction::Set)]
    with_external: bool,
    #[arg(long, value_delimiter = ',')]
    external_category: Vec<String>,
    #[arg(long)]
    language: Option<String>,
    #[arg(long)]
    module: Option<String>,
    #[arg(long)]
    directory: Option<String>,
    #[arg(long, value_delimiter = ',')]
    confidence: Vec<ConfidenceArg>,
    #[arg(long, value_delimiter = ',')]
    exclude: Vec<String>,
    #[arg(long, value_delimiter = ',', default_value = "json")]
    format: Vec<String>,
    #[arg(long)]
    out: Option<PathBuf>,
    #[arg(long, default_value = ".")]
    root: PathBuf,
    #[arg(long)]
    config: Option<PathBuf>,
}
#[derive(Clone, ValueEnum)]
enum ConfidenceArg {
    Resolved,
    Ambiguous,
    Dynamic,
}
#[derive(Args)]
struct BaselineArgs {
    #[command(subcommand)]
    command: BaselineCommand,
}
#[derive(Subcommand)]
enum BaselineCommand {
    Create {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        baseline: Option<PathBuf>,
    },
    Update {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        baseline: Option<PathBuf>,
    },
}
#[derive(Args)]
struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommand,
}
#[derive(Subcommand)]
enum ConfigCommand {
    Check {
        #[arg(long, default_value = "analyzer.yaml")]
        file: PathBuf,
        #[arg(long)]
        strict: bool,
    },
    Show {
        #[arg(long, default_value = "analyzer.yaml")]
        file: PathBuf,
        #[arg(long)]
        resolved: bool,
    },
    Keys,
}
#[derive(Args)]
struct ServeArgs {
    #[arg(long)]
    no_architecture: bool,
    #[arg(long, value_enum)]
    architecture_profile: Option<ArchitectureProfile>,
    #[arg(default_value = ".")]
    root: PathBuf,
    #[arg(long, default_value_t = 8080)]
    port: u16,
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    #[arg(long)]
    open: bool,
    #[arg(long)]
    watch: bool,
    #[arg(long)]
    config: Option<PathBuf>,
}
#[derive(Args)]
struct RelationsArgs {
    #[arg(long, value_delimiter = ',', required = true)]
    a: Vec<String>,
    #[arg(long, value_delimiter = ',', required = true)]
    b: Vec<String>,
    #[arg(long, value_delimiter = ',')]
    exclude_a: Vec<String>,
    #[arg(long, value_delimiter = ',')]
    exclude_b: Vec<String>,
    #[arg(long, value_enum, default_value = "function")]
    level: RelationLevel,
    #[arg(long)]
    transitive: Option<usize>,
    #[arg(long, value_parser = ["one_shortest", "all_shortest", "simple"], default_value = "one_shortest")]
    path_mode: String,
    #[arg(long, default_value_t = 100)]
    max_paths_per_pair: usize,
    #[arg(long, default_value_t = 10_000)]
    max_paths_total: usize,
    #[arg(long, default_value_t = 1_000_000)]
    work_budget: usize,
    #[arg(long, default_value = ".")]
    root: PathBuf,
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long, value_delimiter = ',', default_value = "json")]
    format: Vec<String>,
    #[arg(long)]
    out: Option<PathBuf>,
}
#[derive(Clone, ValueEnum)]
enum RelationLevel {
    File,
    Class,
    Function,
}
#[derive(Args)]
struct ExternalArgs {
    #[command(subcommand)]
    command: ExternalCommand,
}
#[derive(Subcommand)]
enum ExternalCommand {
    List {
        #[arg(long)]
        category: Option<String>,
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long)]
        config: Option<PathBuf>,
    },
}

fn config_loader() -> ConfigLoader {
    let mut loader = ConfigLoader::new();
    registry().register_config_keys(&mut loader);
    loader
}
fn project_config(root: &Path) -> Option<PathBuf> {
    let directory = if root.is_file() {
        root.parent().unwrap_or(Path::new("."))
    } else {
        root
    };
    ["analyzer.yaml", "analyzer.yml"]
        .into_iter()
        .map(|name| directory.join(name))
        .find(|path| path.is_file())
}
fn load_config(
    path: Option<&Path>,
    strict: bool,
    verbose: bool,
) -> anyhow::Result<(Config, Option<analyzer_core::config::ConfigLoad>)> {
    if let Some(path) = path {
        let loaded = config_loader().load(path, strict, verbose)?;
        for d in &loaded.diagnostics {
            log::warn!("{}: {}", d.path, d.message);
        }
        Ok((loaded.config.clone(), Some(loaded)))
    } else {
        Ok((Config::default(), None))
    }
}
fn scan(
    root: &Path,
    config_path: Option<&Path>,
    langs: &[String],
    strict: bool,
    verbose: bool,
) -> anyhow::Result<(
    Analyzer,
    AnalysisReport,
    Option<analyzer_core::config::ConfigLoad>,
)> {
    scan_options(
        root,
        config_path,
        langs,
        strict,
        verbose,
        false,
        None,
        false,
        BaselinePolicy::Default,
    )
}
fn scan_options(
    root: &Path,
    config_path: Option<&Path>,
    langs: &[String],
    strict: bool,
    verbose: bool,
    no_architecture: bool,
    architecture_profile: Option<ArchitectureProfile>,
    dependency_facts: bool,
    baseline: BaselinePolicy,
) -> anyhow::Result<(
    Analyzer,
    AnalysisReport,
    Option<analyzer_core::config::ConfigLoad>,
)> {
    let chosen = config_path
        .map(Path::to_path_buf)
        .or_else(|| project_config(root));
    let (mut config, loaded) = load_config(chosen.as_deref(), strict, verbose)?;
    if no_architecture {
        config.architecture.enabled = false;
    }
    if let Some(profile) = architecture_profile {
        config.architecture.profile = match profile {
            ArchitectureProfile::Strict => analyzer_core::architecture::Profile::Strict,
            ArchitectureProfile::Extended => analyzer_core::architecture::Profile::Extended,
        };
    }
    if !langs.is_empty() {
        config.languages.enabled = langs
            .iter()
            .map(|l| match l.as_str() {
                "ts" => "typescript".into(),
                "js" => "javascript".into(),
                other => other.to_string(),
            })
            .collect();
    }
    let registry = registry();
    let unknown = config
        .languages
        .enabled
        .iter()
        .filter(|language| registry.get(language).is_none())
        .map(|language| format!("{language:?}"))
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        anyhow::bail!(
            "Unknown language(s): {}. Available languages: {} (aliases: js, ts)",
            unknown.join(", "),
            registry.languages().join(", ")
        );
    }
    let mut analyzer = Analyzer::new(registry, config);
    analyzer.baseline = baseline;
    analyzer.collect_dependency_facts = dependency_facts;
    let progress = presentation::Progress::start();
    let mut report = analyzer.scan(root)?;
    drop(progress);
    report.config_path = chosen.map(|p| p.to_string_lossy().into_owned());
    Ok((analyzer, report, loaded))
}
fn baseline_command(command: BaselineCommand, verbose: bool) -> anyhow::Result<i32> {
    let (update, root, config, baseline) = match command {
        BaselineCommand::Create {
            root,
            config,
            baseline,
        } => (false, root, config, baseline),
        BaselineCommand::Update {
            root,
            config,
            baseline,
        } => (true, root, config, baseline),
    };
    let canonical = std::fs::canonicalize(&root)?;
    let base = if canonical.is_file() {
        canonical.parent().unwrap_or(&canonical)
    } else {
        &canonical
    };
    let policy = baseline.map(BaselinePolicy::Path).unwrap_or_default();
    let path = policy.path(base).expect("baseline writing is enabled");
    let previous = if update {
        match read_baseline(&path) {
            Ok(entries) => entries,
            Err(BaselineError::NotFound { .. }) => Default::default(),
            Err(error) => {
                log::warn!("{error}");
                Default::default()
            }
        }
    } else {
        Default::default()
    };
    let chosen = config.or_else(|| project_config(&root));
    let (config, _) = load_config(chosen.as_deref(), false, verbose)?;
    let report = Analyzer::new(registry(), config).scan_without_baseline(&root)?;
    let entries = report
        .violations
        .iter()
        .map(|v| v.fingerprint())
        .collect::<std::collections::BTreeSet<_>>();
    write_baseline(&path, &report.violations)?;
    println!(
        "Wrote {} baseline entries to {}",
        entries.len(),
        path.display()
    );
    if update {
        println!(
            "+{} новых, −{} удалено",
            entries.difference(&previous).count(),
            previous.difference(&entries).count()
        );
    }
    Ok(0)
}
#[tokio::main]
async fn main() {
    match run().await {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("error: {error:#}");
            let syntax = error.chain().any(|cause| {
                cause
                    .downcast_ref::<analyzer_core::config::ConfigError>()
                    .is_some_and(|e| matches!(e, analyzer_core::config::ConfigError::Syntax(_)))
            });
            std::process::exit(if syntax { 3 } else { 1 });
        }
    }
}
async fn run() -> anyhow::Result<i32> {
    let mut cli = Cli::parse();
    logging::init(cli.log_level.as_deref(), cli.verbose)?;
    cli.verbose |= log::log_enabled!(log::Level::Debug);
    let status = match cli.command {
        Command::Scan(a) => {
            let (analyzer, mut report, _) = scan_options(
                &a.common.root,
                a.common.config.as_deref(),
                &a.common.lang,
                a.common.strict_config,
                cli.verbose,
                a.common.no_architecture,
                a.common.architecture_profile,
                false,
                a.common.baseline_policy(),
            )?;
            changes::apply(
                &mut report,
                a.compare.as_deref(),
                a.changed_files.as_deref(),
                a.git_changed.as_deref(),
            )?;
            let formats = if a.format.is_empty() {
                analyzer.config.output.formats.clone()
            } else {
                a.format
            };
            let formats = if formats.is_empty() {
                vec!["console".into()]
            } else {
                formats
            };
            for format in &formats {
                if format == "console" || format == "text" {
                    let path = output_path(a.out.as_deref(), "txt", &formats);
                    write_output(
                        &presentation::scan(
                            &report,
                            path.is_none() && cli.color.enabled(),
                            presentation::ScanView {
                                limit: if a.all { usize::MAX } else { a.limit },
                                explain: !a.no_explain,
                                only_violations: a.only_violations,
                                full: a.full,
                            },
                        ),
                        path.as_deref(),
                    )?;
                } else {
                    emit_formats(
                        &report,
                        std::slice::from_ref(format),
                        output_path(
                            a.out.as_deref(),
                            if format == "markdown" { "md" } else { format },
                            &formats,
                        )
                        .as_deref(),
                        a.only_violations,
                    )?;
                }
            }
            exit_code(
                &report,
                a.common.fail_on_warn,
                a.common.fail_on_parse_errors,
                a.fail_on_new,
            )
        }
        Command::Metrics(a) => {
            let (analyzer, report, _) = scan_options(
                &a.common.root,
                a.common.config.as_deref(),
                &a.common.lang,
                a.common.strict_config,
                cli.verbose,
                a.common.no_architecture,
                a.common.architecture_profile,
                false,
                a.common.baseline_policy(),
            )?;
            let format = a.format.unwrap_or_else(|| {
                analyzer
                    .config
                    .output
                    .formats
                    .iter()
                    .find(|format| matches!(format.as_str(), "console" | "json"))
                    .cloned()
                    .unwrap_or_else(|| "console".into())
            });
            if format == "json" {
                match a.group_by {
                    Group::File => write_json_output(&report.metrics, a.out.as_deref())?,
                    group => {
                        let name = match group {
                            Group::Project => "project",
                            Group::Directory => "directory",
                            Group::Module => "module",
                            Group::File => unreachable!(),
                        };
                        let selected = report
                            .aggregates
                            .iter()
                            .filter(|g| g.name == name || g.name.starts_with(&format!("{name}:")))
                            .collect::<Vec<_>>();
                        write_json_output(&selected, a.out.as_deref())?;
                    }
                }
            } else {
                let name = match a.group_by {
                    Group::Project => "project",
                    Group::Directory => "directory",
                    Group::Module => "module",
                    Group::File => "file",
                };
                write_output(
                    &presentation::metrics(
                        &report,
                        name,
                        a.out.is_none() && cli.color.enabled(),
                        !a.no_explain,
                    ),
                    a.out.as_deref(),
                )?;
            }
            exit_code(
                &report,
                a.common.fail_on_warn,
                a.common.fail_on_parse_errors,
                false,
            )
        }
        Command::Ast(a) => {
            let chosen = a.config.clone().or_else(|| project_config(&a.path));
            let (config, _) = load_config(chosen.as_deref(), false, cli.verbose)?;
            let analyzer = Analyzer::new(registry(), config);
            let tree = analyzer.ast(&a.path)?;
            let selected = if let Some(selector) = a.function.as_deref() {
                let source = std::fs::read_to_string(&a.path)?;
                let strategy = analyzer
                    .registry
                    .language_for_path(&a.path, &analyzer.config.languages.extensions)
                    .ok_or_else(|| anyhow::anyhow!("unsupported file: {}", a.path.display()))?;
                let model =
                    strategy.normalize(source.trim_start_matches('\u{feff}'), &a.path, &tree);
                let function = model
                    .functions
                    .iter()
                    .find(|f| f.fqn == selector || f.name == selector)
                    .ok_or_else(|| anyhow::anyhow!("function not found: {selector}"))?;
                let mut found = Vec::new();
                tree.root.find(
                    &|n| {
                        n.kind == analyzer_core::AstKind::Function
                            && n.range.start_line == function.range.start_line
                            && n.range.start_column == function.range.start_column
                    },
                    &mut found,
                );
                Some(
                    *found
                        .first()
                        .ok_or_else(|| anyhow::anyhow!("AST node not found for: {selector}"))?,
                )
            } else {
                None
            };
            let incomplete = tree
                .errors
                .iter()
                .any(|error| error.message.starts_with("detailed AST incomplete"));
            for error in tree
                .errors
                .iter()
                .filter(|error| error.message.starts_with("detailed AST incomplete"))
            {
                log::warn!("{}: {}", error.range.file, error.message);
            }
            let value = if let Some(node) = selected {
                serde_json::json!({"function":a.function,"node":node,"parse_errors":tree.errors})
            } else {
                serde_json::to_value(&tree)?
            };
            let output = if matches!(a.format, AstFormat::Json) {
                serde_json::to_string_pretty(&value)?
            } else {
                ast_dot(selected.unwrap_or(&tree.root))
            };
            write_output(&output, a.out.as_deref())?;
            if incomplete {
                2
            } else {
                0
            }
        }
        Command::Graph(a) => graph_command(a.command, cli.verbose)?,
        Command::Baseline(a) => baseline_command(a.command, cli.verbose)?,
        Command::Config(a) => config_command(a.command, cli.verbose)?,
        Command::Serve(a) => {
            let (analyzer, report, loaded) = scan_options(
                &a.root,
                a.config.as_deref(),
                &[],
                false,
                cli.verbose,
                a.no_architecture,
                a.architecture_profile,
                true,
                BaselinePolicy::Default,
            )?;
            serve::run(
                analyzer, report, a.root, loaded, a.host, a.port, a.open, a.watch,
            )
            .await?;
            0
        }
        Command::Relations(a) => {
            let (analyzer, r, _) = scan(&a.root, a.config.as_deref(), &[], false, cli.verbose)?;
            let mut relation = r.graph.relations_with_config(
                &RelationOptions {
                    a: a.a,
                    b: a.b,
                    exclude_a: a.exclude_a,
                    exclude_b: a.exclude_b,
                    transitive_depth: a.transitive,
                    path_mode: match a.path_mode.as_str() {
                        "all_shortest" => RelationPathMode::AllShortest,
                        "simple" => RelationPathMode::Simple,
                        _ => RelationPathMode::OneShortest,
                    },
                    max_paths_per_pair: a.max_paths_per_pair,
                    max_paths_total: a.max_paths_total,
                    work_budget: a.work_budget,
                },
                &analyzer.config,
            );
            let level = match a.level {
                RelationLevel::File => "file",
                RelationLevel::Class => "class",
                RelationLevel::Function => "function",
            };
            relation.regroup_matrix(level);
            emit_relations(&relation, &a.format, a.out.as_deref())?;
            0
        }
        Command::External(a) => match a.command {
            ExternalCommand::List {
                category,
                root,
                config,
            } => {
                let (_, r, _) = scan(&root, config.as_deref(), &[], false, cli.verbose)?;
                for n in r.graph.external_list(category.as_deref()) {
                    println!(
                        "{}\t{}\t{}",
                        n.category.as_deref().unwrap_or(""),
                        n.library.as_deref().unwrap_or(""),
                        n.operation.as_deref().unwrap_or(&n.label)
                    );
                }
                0
            }
        },
    };
    Ok(status)
}

fn graph_command(cmd: GraphCommand, verbose: bool) -> anyhow::Result<i32> {
    match cmd {
        GraphCommand::Callers(a) => graph_query(a, Direction::Callers, verbose),
        GraphCommand::Callees(a) => graph_query(a, Direction::Callees, verbose),
        GraphCommand::Both(a) => graph_query(a, Direction::Both, verbose),
        GraphCommand::Paths {
            from,
            to,
            shortest,
            format,
            out,
            root,
            config,
            max_paths,
        } => {
            let (_, r, _) = scan(&root, config.as_deref(), &[], false, verbose)?;
            let result = r.graph.paths(&from, &to, shortest, max_paths);
            emit_paths(&result, &r.graph, &format, out.as_deref())?;
            Ok(0)
        }
        GraphCommand::Dead { root, config } => {
            let (_, r, _) = scan(&root, config.as_deref(), &[], false, verbose)?;
            for n in r.graph.dead_functions() {
                println!("{}", n.label)
            }
            Ok(0)
        }
        GraphCommand::Roots { root, config } => {
            let (_, r, _) = scan(&root, config.as_deref(), &[], false, verbose)?;
            for n in r.graph.roots() {
                println!("{}", n.label)
            }
            Ok(0)
        }
    }
}
fn graph_query(a: GraphQueryArgs, dir: Direction, verbose: bool) -> anyhow::Result<i32> {
    let (analyzer, r, _) = scan(&a.root, a.config.as_deref(), &[], false, verbose)?;
    let mut filter = GraphFilter::all();
    filter.with_external = a.with_external;
    filter.external_categories = a.external_category;
    filter.language = a.language;
    filter.module = a.module;
    filter.directory = a.directory;
    filter.exclude = a.exclude;
    filter.confidence = a
        .confidence
        .into_iter()
        .map(|c| match c {
            ConfidenceArg::Resolved => Confidence::Resolved,
            ConfidenceArg::Ambiguous => Confidence::Ambiguous,
            ConfidenceArg::Dynamic => Confidence::Dynamic,
        })
        .collect();
    let q = r
        .graph
        .query(&a.selector, dir, a.depth, &filter, &analyzer.config);
    if a.summary {
        println!(
            "functions={} classes={} files={} external={} unresolved={}",
            q.coverage.functions.len(),
            q.coverage.classes.len(),
            q.coverage.files.len(),
            q.coverage.external.len(),
            q.coverage.unresolved.len()
        );
        return Ok(0);
    }
    emit_graph_query(&q, &a.format, a.out.as_deref(), a.with_conditions)?;
    Ok(0)
}
fn emit_graph_query(
    q: &analyzer_core::graph::GraphQuery,
    formats: &[String],
    out: Option<&Path>,
    with_conditions: bool,
) -> anyhow::Result<()> {
    let mut value = serde_json::to_value(q)?;
    let accumulated = if with_conditions {
        Some(accumulated_conditions(q))
    } else {
        None
    };
    if let Some(paths) = &accumulated {
        if let Some(edges) = value["edges"].as_array_mut() {
            for (edge, conditions) in edges.iter_mut().zip(&paths.paths) {
                edge["accumulated_conditions"] = serde_json::to_value(conditions)?;
            }
        }
        value["accumulated_conditions_metadata"] = serde_json::to_value(&paths.metadata)?;
        if paths.metadata.truncated {
            log::warn!("accumulated conditions truncated by work/output limits; graph coverage is unchanged");
        }
    }
    for format in formats {
        let (ext, body) = match format.as_str() {
            "json" => ("json", serde_json::to_string_pretty(&value)?),
            "md" | "markdown" => ("md", graph_query_text(q, accumulated.as_ref())),
            "console" => ("txt", graph_query_text(q, accumulated.as_ref())),
            "dot" => (
                "dot",
                format!(
                    "{}{}",
                    condition_export_comment(accumulated.as_ref(), "//"),
                    to_dot(&value)
                ),
            ),
            "mermaid" => (
                "mmd",
                format!(
                    "{}{}",
                    condition_export_comment(accumulated.as_ref(), "%%"),
                    to_mermaid(&value)
                ),
            ),
            _ => anyhow::bail!("unsupported graph format: {format}"),
        };
        let path = output_path(out, ext, formats);
        write_output(&body, path.as_deref())?;
    }
    Ok(())
}
fn graph_query_text(
    q: &analyzer_core::graph::GraphQuery,
    paths: Option<&AccumulatedConditions>,
) -> String {
    let mut out = graph_markdown(q);
    if let Some(paths) = paths {
        out.push_str("\n## Accumulated path conditions\n\n");
        if paths.metadata.truncated {
            out.push_str(
                "Conditions are truncated by work/output limits; graph coverage is unchanged.\n\n",
            );
        }
        for (edge, alternatives) in q.edges.iter().zip(&paths.paths) {
            out.push_str(&format!(
                "- `{}` → `{}` at `{}`:{}\n",
                edge.from, edge.to, edge.callsite.file, edge.callsite.start_line
            ));
            for guards in alternatives {
                out.push_str(&format!(
                    "  - {}\n",
                    if guards.is_empty() {
                        "always".into()
                    } else {
                        guards
                            .iter()
                            .map(|g| format!("{}: {} [raw: {}]", g.branch, g.normalized, g.raw))
                            .collect::<Vec<_>>()
                            .join(" ∧ ")
                    }
                ));
            }
        }
    }
    out
}

fn condition_export_comment(paths: Option<&AccumulatedConditions>, prefix: &str) -> String {
    paths
        .map(|paths| {
            format!(
                "{prefix} accumulated_conditions_metadata: {}\n",
                serde_json::to_string(&paths.metadata).expect("serializable condition metadata")
            )
        })
        .unwrap_or_default()
}

#[derive(Serialize)]
struct ConditionMetadata {
    truncated: bool,
    work_used: usize,
    work_budget: usize,
    max_paths_per_node: usize,
    max_paths_per_edge: usize,
    max_paths_total: usize,
    guard_reference_budget: usize,
    output_byte_budget: usize,
}

struct AccumulatedConditions {
    paths: Vec<Vec<Vec<analyzer_core::Guard>>>,
    metadata: ConditionMetadata,
}

fn accumulated_conditions(q: &analyzer_core::graph::GraphQuery) -> AccumulatedConditions {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    const MAX_NODE_PATHS: usize = 128;
    const MAX_EDGE_PATHS: usize = 256;
    const MAX_PATHS: usize = 4096;
    const MAX_WORK: usize = 100_000;
    const MAX_GUARDS: usize = 65_536;
    const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
    let ids = q
        .roots
        .iter()
        .map(String::as_str)
        .chain(
            q.edges
                .iter()
                .flat_map(|e| [e.from.as_ref(), e.to.as_ref()]),
        )
        .collect::<BTreeSet<_>>()
        .into_iter()
        .enumerate()
        .map(|(index, id)| (id, index))
        .collect::<BTreeMap<_, _>>();
    let mut adjacency = vec![Vec::<(usize, usize)>::new(); ids.len()];
    let mut guards = Vec::<&analyzer_core::Guard>::new();
    let mut guard_ids = BTreeMap::new();
    let mut edge_guards = Vec::new();
    for (index, edge) in q.edges.iter().enumerate() {
        let (from, to) = if q.direction == Direction::Callers {
            (&edge.to, &edge.from)
        } else {
            (&edge.from, &edge.to)
        };
        adjacency[ids[from.as_ref()]].push((index, ids[to.as_ref()]));
        if q.direction == Direction::Both {
            adjacency[ids[to.as_ref()]].push((index, ids[from.as_ref()]));
        }
        edge_guards.push(
            edge.guard
                .iter()
                .map(|guard| {
                    *guard_ids.entry(guard).or_insert_with(|| {
                        let id = guards.len();
                        guards.push(guard);
                        id
                    })
                })
                .collect::<Vec<_>>(),
        );
    }
    let guard_bytes = guards
        .iter()
        // Include JSON escaping and reserve space for pretty-print indentation.
        // This bounds accumulated guard output independently of the base graph.
        .map(|g| serde_json::to_vec(g).expect("serializable guard").len() + 512)
        .collect::<Vec<_>>();
    let mut result = vec![BTreeSet::<Vec<usize>>::new(); q.edges.len()];
    let mut seen = BTreeSet::new();
    let mut node_paths = vec![0usize; ids.len()];
    let mut queue = VecDeque::new();
    let mut truncated = false;
    for root in q.roots.iter().take(MAX_PATHS) {
        let node = ids[root.as_str()];
        queue.push_back((node, Vec::<usize>::new(), vec![node]));
    }
    truncated |= q.roots.len() > MAX_PATHS;
    let mut work = 0usize;
    let mut path_count = 0usize;
    let mut guard_count = 0usize;
    let mut output_bytes = 0usize;
    while let Some((node, conditions, path)) = queue.pop_front() {
        // Both-direction queries can revisit the same node/conditions along
        // different simple routes. Their visited sets affect legal continuations.
        let mut route = if q.direction == Direction::Both {
            path.clone()
        } else {
            Vec::new()
        };
        route.sort_unstable();
        if !seen.insert((node, conditions.clone(), route)) {
            continue;
        }
        if node_paths[node] >= MAX_NODE_PATHS {
            truncated = true;
            continue;
        }
        node_paths[node] += 1;
        for &(index, target) in &adjacency[node] {
            let cost = path.len() + conditions.len() + edge_guards[index].len() + 1;
            if cost > MAX_WORK.saturating_sub(work) {
                truncated = true;
                queue.clear();
                break;
            }
            work += cost;
            if path.contains(&target) {
                continue;
            }
            let mut accumulated = conditions.clone();
            accumulated.extend(&edge_guards[index]);
            if !result[index].contains(&accumulated) {
                let bytes = accumulated
                    .iter()
                    .map(|&guard| guard_bytes[guard])
                    .sum::<usize>();
                if result[index].len() >= MAX_EDGE_PATHS
                    || path_count >= MAX_PATHS
                    || accumulated.len() > MAX_GUARDS.saturating_sub(guard_count)
                    || bytes > MAX_OUTPUT_BYTES.saturating_sub(output_bytes)
                {
                    truncated = true;
                    continue;
                }
                path_count += 1;
                guard_count += accumulated.len();
                output_bytes += bytes;
                result[index].insert(accumulated.clone());
            }
            let mut next_path = path.clone();
            next_path.push(target);
            queue.push_back((target, accumulated, next_path));
        }
    }
    AccumulatedConditions {
        paths: result
            .into_iter()
            .map(|paths| {
                paths
                    .into_iter()
                    .map(|path| {
                        path.into_iter()
                            .map(|guard| guards[guard].clone())
                            .collect()
                    })
                    .collect()
            })
            .collect(),
        metadata: ConditionMetadata {
            truncated,
            work_used: work,
            work_budget: MAX_WORK,
            max_paths_per_node: MAX_NODE_PATHS,
            max_paths_per_edge: MAX_EDGE_PATHS,
            max_paths_total: MAX_PATHS,
            guard_reference_budget: MAX_GUARDS,
            output_byte_budget: MAX_OUTPUT_BYTES,
        },
    }
}
fn config_command(cmd: ConfigCommand, _verbose: bool) -> anyhow::Result<i32> {
    match cmd {
        ConfigCommand::Check { file, strict } => {
            let loaded = config_loader().load(&file, strict, true)?;
            for d in &loaded.diagnostics {
                println!(
                    "{}:{}: {}",
                    d.source.as_deref().unwrap_or(""),
                    d.line.unwrap_or(0),
                    d.message
                )
            }
            println!(
                "{} key(s) skipped; {} include file(s) skipped",
                loaded.skipped_keys.len(),
                loaded.skipped_files.len()
            );
            Ok(
                if strict && (!loaded.skipped_keys.is_empty() || !loaded.diagnostics.is_empty()) {
                    3
                } else {
                    0
                },
            )
        }
        ConfigCommand::Show { file, resolved } => {
            let loaded = config_loader().load(&file, false, true)?;
            if resolved {
                println!("{}", serde_yaml::to_string(&loaded.resolved)?);
                println!("# value sources");
                for (k, v) in loaded.provenance {
                    println!("# {k}: {}:{}", v.file, v.line)
                }
            } else {
                println!("{}", std::fs::read_to_string(file)?)
            }
            Ok(0)
        }
        ConfigCommand::Keys => {
            for k in config_loader().keys() {
                println!("{}\t{}", k.name, k.description)
            }
            Ok(0)
        }
    }
}
fn output_path(out: Option<&Path>, format: &str, formats: &[String]) -> Option<PathBuf> {
    out.map(|p| {
        if p.extension().is_none() {
            p.with_extension(format)
        } else if formats.len() > 1 {
            p.with_extension(format)
        } else {
            p.to_path_buf()
        }
    })
}
fn emit_formats(
    report: &AnalysisReport,
    formats: &[String],
    out: Option<&Path>,
    only_violations: bool,
) -> anyhow::Result<()> {
    for format in formats {
        if format == "json" {
            let path = output_path(out, "json", formats);
            if only_violations {
                write_json_output(
                    &ViolationsReport {
                        version: &report.version,
                        schema_version: &report.schema_version,
                        report_type: "violations",
                        root: &report.root,
                        config_path: report.config_path.as_deref(),
                        strategy_capabilities: &report.strategy_capabilities,
                        summary: &report.summary,
                        violations: &report.violations,
                        parse_errors: &report.parse_errors,
                        resolution_diagnostics: &report.resolution_diagnostics,
                        baseline_path: report.baseline_path.as_deref(),
                        diagnostics: &report.diagnostics,
                        changes: report.changes.as_ref(),
                    },
                    path.as_deref(),
                )?;
            } else {
                write_json_output(report, path.as_deref())?;
            }
            continue;
        }
        let (ext, body) = match format.as_str() {
            "md" | "markdown" => ("md", scan_markdown(report, only_violations)),
            "sarif" => ("sarif", sarif(report)?),
            _ => anyhow::bail!("unsupported report format: {format}"),
        };
        let path = output_path(out, ext, formats);
        write_output(&body, path.as_deref())?;
    }
    Ok(())
}
#[derive(Serialize)]
struct ViolationsReport<'a> {
    version: &'a str,
    schema_version: &'a str,
    report_type: &'static str,
    root: &'a str,
    config_path: Option<&'a str>,
    strategy_capabilities: &'a std::collections::BTreeMap<String, analyzer_core::Capabilities>,
    summary: &'a analyzer_core::report::Summary,
    violations: &'a [analyzer_core::rules::Violation],
    parse_errors: &'a [analyzer_core::ParseError],
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    resolution_diagnostics: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    baseline_path: Option<&'a str>,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    diagnostics: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    changes: Option<&'a analyzer_core::report::diff::ChangeSummary>,
}
fn emit_paths(
    result: &analyzer_core::graph::PathResult,
    graph: &analyzer_core::graph::CallGraph,
    formats: &[String],
    out: Option<&Path>,
) -> anyhow::Result<()> {
    for format in formats {
        let (ext, body) = match format.as_str() {
            "json" => ("json", serde_json::to_string_pretty(result)?),
            "md" | "markdown" | "console" => ("md", paths_markdown(result, graph)),
            _ => anyhow::bail!("unsupported paths format: {format}"),
        };
        let path = output_path(out, ext, formats);
        write_output(&body, path.as_deref())?;
    }
    Ok(())
}
fn paths_markdown(
    result: &analyzer_core::graph::PathResult,
    graph: &analyzer_core::graph::CallGraph,
) -> String {
    let names = graph
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n.label.as_str()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut out = format!(
        "# Call paths\n\nPaths: {} · Truncated: {}.\n\n",
        result.paths.len(),
        result.truncated
    );
    for (index, path) in result.paths.iter().enumerate() {
        out.push_str(&format!("## Path {}\n\n", index + 1));
        for (step, id) in path.iter().enumerate() {
            let name = names.get(id.as_str()).copied().unwrap_or(id);
            out.push_str(&format!(
                "{}{}. `{}`\n",
                "  ".repeat(step),
                step + 1,
                name.replace('`', "'")
            ));
            if let Some(next) = path.get(step + 1) {
                for edge in graph
                    .edges
                    .iter()
                    .filter(|e| e.from.as_ref() == id && e.to.as_ref() == next)
                {
                    out.push_str(&format!(
                        "{}- callsite: `{}`:{}; confidence: {:?}{}{}\n",
                        "  ".repeat(step + 1),
                        edge.callsite.file,
                        edge.callsite.start_line,
                        edge.confidence,
                        guard_summary(edge),
                        loop_summary(edge)
                    ));
                }
            }
        }
        out.push('\n');
    }
    if result.paths.is_empty() {
        out.push_str("No path found.\n");
    }
    out
}
fn emit_relations(
    result: &analyzer_core::graph::RelationResult,
    formats: &[String],
    out: Option<&Path>,
) -> anyhow::Result<()> {
    for format in formats {
        let (ext, body) = match format.as_str() {
            "json" => ("json", serde_json::to_string_pretty(result)?),
            "md" | "markdown" | "console" => ("md", relations_markdown(result)),
            _ => anyhow::bail!("unsupported relations format: {format}"),
        };
        let path = output_path(out, ext, formats);
        write_output(&body, path.as_deref())?;
    }
    Ok(())
}
fn relations_markdown(result: &analyzer_core::graph::RelationResult) -> String {
    let mut out = format!(
        "# Relations between sets\n\nSet A: {} symbols · Set B: {} symbols · Overlap: {}.\n\n",
        result.a.len(),
        result.b.len(),
        result.overlap.len()
    );
    for warning in &result.warnings {
        out.push_str(&format!(
            "Warning: {}\n\n",
            warning.replace(['\n', '\r'], " ")
        ));
    }
    for (name, values) in [
        ("A", &result.a),
        ("B", &result.b),
        ("Overlap", &result.overlap),
    ] {
        out.push_str(&format!("## Set {name}\n\n"));
        for value in values {
            out.push_str(&format!("- `{}`\n", value.replace('`', "'")));
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "Route mode: {:?} · Direct callsites: {} · Returned routes: {} · Work: {}/{}.\n\n",
        result.path_mode,
        result.direct_call_count,
        result.route_count,
        result.work_used,
        result.options.work_budget
    ));
    if result.truncated {
        out.push_str(&format!(
            "Routes truncated: {}. Direct call arrays remain complete.\n\n",
            result.truncation_reasons.join(", ")
        ));
    }
    out.push_str("## Matrix\n\n| Source | Target | Routes | Direct calls |\n|---|---|---:|---:|\n");
    for (source, targets) in &result.matrix {
        for (target, count) in targets {
            out.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                source.replace('|', "\\|"),
                target.replace('|', "\\|"),
                count,
                result
                    .direct_call_matrix
                    .get(source)
                    .and_then(|rows| rows.get(target))
                    .copied()
                    .unwrap_or(0)
            ));
        }
    }
    out.push('\n');
    for (name, edges) in [("A → B", &result.a_to_b), ("B → A", &result.b_to_a)] {
        out.push_str(&format!("## {name} ({})\n\n", edges.len()));
        for edge in edges {
            out.push_str(&format!(
                "- `{}` → `{}` at `{}`:{} ({:?}){}{}\n",
                edge.from,
                edge.to,
                edge.callsite.file,
                edge.callsite.start_line,
                edge.confidence,
                guard_summary(edge),
                loop_summary(edge)
            ));
        }
        out.push('\n');
    }
    for (name, paths) in [
        ("A → B", &result.paths_a_to_b),
        ("B → A", &result.paths_b_to_a),
    ] {
        let indirect = paths
            .iter()
            .filter(|path| path.edges.len() > 1)
            .collect::<Vec<_>>();
        if indirect.is_empty() {
            continue;
        }
        out.push_str(&format!("## Indirect {name} ({})\n\n", indirect.len()));
        for path in indirect {
            out.push_str(&format!("### `{}` → `{}`\n\n", path.from, path.to));
            for edge in &path.edges {
                out.push_str(&format!(
                    "- `{}` → `{}` at `{}`:{} ({:?}){}{}\n",
                    edge.from,
                    edge.to,
                    edge.callsite.file,
                    edge.callsite.start_line,
                    edge.confidence,
                    guard_summary(edge),
                    loop_summary(edge)
                ));
            }
            out.push('\n');
        }
    }
    out
}
fn guard_summary(edge: &analyzer_core::graph::GraphEdge) -> String {
    if edge.guard.is_empty() {
        String::new()
    } else {
        format!(
            "; conditions: {}",
            edge.guard
                .iter()
                .map(|g| format!("{} {} [raw: {}]", g.branch, g.normalized, g.raw))
                .collect::<Vec<_>>()
                .join(" ∧ ")
        )
    }
}
fn loop_summary(edge: &analyzer_core::graph::GraphEdge) -> String {
    if edge.in_loop.is_empty() {
        String::new()
    } else {
        format!(
            "; loops: {}",
            edge.in_loop
                .iter()
                .map(|l| format!("{} (line {})", l.header, l.line))
                .collect::<Vec<_>>()
                .join(" → ")
        )
    }
}
fn write_output(text: &str, path: Option<&Path>) -> anyhow::Result<()> {
    if let Some(p) = path {
        std::fs::write(p, text)?
    } else {
        println!("{text}")
    }
    Ok(())
}
fn write_json_output<T: Serialize>(value: &T, path: Option<&Path>) -> anyhow::Result<()> {
    fn stream<T: Serialize, W: Write>(value: &T, output: W) -> anyhow::Result<()> {
        let mut writer = BufWriter::with_capacity(64 * 1024, output);
        serde_json::to_writer(&mut writer, value)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        Ok(())
    }
    match path {
        Some(path) => stream(value, std::fs::File::create(path)?),
        None => stream(value, io::stdout().lock()),
    }
}

fn sarif(r: &AnalysisReport) -> anyhow::Result<String> {
    let compared = r.changes.as_ref().is_some_and(|changes| changes.compared);
    let mut new = std::collections::BTreeMap::<String, usize>::new();
    if let Some(changes) = r.changes.as_ref().filter(|_| compared) {
        for violation in &changes.new {
            *new.entry(serde_json::to_string(violation)?).or_default() += 1;
        }
    }
    let results = r
        .violations
        .iter()
        .map(|v| {
            let mut result = serde_json::json!({
                "ruleId":v.rule,
                "level":if v.severity==Severity::Error {"error"} else {"warning"},
                "message":{"text":v.message()}
            });
            if compared {
                let key = serde_json::to_string(v).expect("serializable violation");
                let is_new = new.get_mut(&key).is_some_and(|remaining| {
                    if *remaining == 0 {
                        false
                    } else {
                        *remaining -= 1;
                        true
                    }
                });
                result["baselineState"] = serde_json::json!(if is_new { "new" } else { "unchanged" });
            }
            if let Some(detail) = &v.algorithmic_complexity {
                result["properties"] = serde_json::json!({"algorithmic_complexity":detail});
            }
            if let Some(detail) = &v.architecture {
                result["properties"] = serde_json::json!({"architecture": detail, "non_blocking": true});
                result["relatedLocations"] = serde_json::json!(detail.evidence.iter().enumerate().map(|(i,e)|serde_json::json!({"id": i+1, "message":{"text":e.description}, "physicalLocation":{"artifactLocation":{"uri":source_uri(&e.range.file)},"region":{"startLine":e.range.start_line.max(1)}}})).collect::<Vec<_>>());
            }
            if let Some(detail) = &v.module_cycle {
                result["properties"] = serde_json::json!({"module_cycle": detail});
                result["relatedLocations"] = serde_json::json!(detail.files.iter().enumerate().map(|(i,file)| {
                    let line = detail.affected_ranges().find(|range| range.file.as_ref() == file).map_or(1, |range| range.start_line.max(1));
                    serde_json::json!({"id": i+1, "message":{"text":"File participating in the module dependency cycle"}, "physicalLocation":{"artifactLocation":{"uri":source_uri(file)},"region":{"startLine":line}}})
                }).collect::<Vec<_>>());
            }
            if !v.file.is_empty() && v.line > 0 {
                result["locations"] = serde_json::json!([{"physicalLocation":{
                    "artifactLocation":{"uri":source_uri(&v.file)},"region":{"startLine":v.line}
                }}]);
            }
            result
        })
        .collect::<Vec<_>>();
    let rules = r
        .violations
        .iter()
        .map(|v| v.rule.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|id| serde_json::json!({"id":id,"shortDescription":{"text":id}}))
        .collect::<Vec<_>>();
    let notifications = r.parse_errors.iter().map(|error| serde_json::json!({
        "descriptor": {"id": "parse_error"},
        "level": "error",
        "message": {"text": error.message},
        "locations": [{"physicalLocation": {
            "artifactLocation": {"uri": source_uri(&error.range.file)},
            "region": {"startLine": error.range.start_line.max(1), "startColumn": error.range.start_column.max(1)}
        }}]
    })).collect::<Vec<_>>();
    Ok(serde_json::to_string_pretty(
        &serde_json::json!({"$schema":"https://json.schemastore.org/sarif-2.1.0.json","version":"2.1.0","runs":[{"tool":{"driver":{"name":"Codefactor","rules":rules}},"properties":{"strategy_capabilities":r.strategy_capabilities},"invocations":[{"executionSuccessful":r.parse_errors.is_empty(),"toolExecutionNotifications":notifications}],"results":results}]}),
    )?)
}
fn source_uri(path: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut uri = String::with_capacity(path.len());
    for byte in path.bytes() {
        if cfg!(windows) && byte == b'\\' {
            uri.push('/');
        } else if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            uri.push('%');
            uri.push(char::from(HEX[(byte >> 4) as usize]));
            uri.push(char::from(HEX[(byte & 15) as usize]));
        }
    }
    uri
}
fn to_dot(v: &serde_json::Value) -> String {
    let mut s = String::from("digraph calls {\n");
    if let Some(nodes) = v["nodes"].as_array() {
        for n in nodes {
            s.push_str(&format!(
                "  {:?} [label={:?}];\n",
                n["id"].as_str().unwrap_or(""),
                graph_node_label(n)
            ));
        }
    }
    if let Some(edges) = v["edges"].as_array() {
        for e in edges {
            s.push_str(&format!(
                "  {:?} -> {:?};\n",
                e["from"].as_str().unwrap_or(""),
                e["to"].as_str().unwrap_or("")
            ));
        }
    }
    s.push_str("}\n");
    s
}
fn graph_node_label(node: &serde_json::Value) -> String {
    let label = node["label"].as_str().unwrap_or("");
    let estimate = &node["algorithmic_complexity"];
    if let Some(formula) = estimate["formula"].as_str() {
        format!(
            "{label} · {formula} · {}",
            estimate["status"].as_str().unwrap_or("")
        )
    } else if let Some(status) = estimate["status"].as_str() {
        format!("{label} · {status}")
    } else {
        label.into()
    }
}
fn to_mermaid(v: &serde_json::Value) -> String {
    let mut s = String::from("flowchart LR\n");
    let ids = v["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, n)| (n["id"].as_str().unwrap_or("").to_string(), format!("n{i}")))
        .collect::<std::collections::BTreeMap<_, _>>();
    for n in v["nodes"].as_array().into_iter().flatten() {
        let id = &ids[n["id"].as_str().unwrap_or("")];
        let label = graph_node_label(n).replace(['"', '\n', '\r'], "'");
        s.push_str(&format!("  {id}[\"{label}\"]\n"));
    }
    for e in v["edges"].as_array().into_iter().flatten() {
        let identity = |id: &str| {
            ids.get(id)
                .cloned()
                .unwrap_or_else(|| id.replace(|c: char| !c.is_ascii_alphanumeric(), "_"))
        };
        let a = identity(e["from"].as_str().unwrap_or(""));
        let b = identity(e["to"].as_str().unwrap_or(""));
        let guard = e["guard"]
            .as_array()
            .and_then(|g| g.last())
            .and_then(|g| g["normalized"].as_str())
            .unwrap_or("");
        s.push_str(&format!(
            "  {a} -->|{}| {b}\n",
            guard.replace(['|', '\n', '\r'], "/")
        ));
    }
    s
}
fn ast_dot(root: &analyzer_core::AstNode) -> String {
    fn visit(
        node: &analyzer_core::AstNode,
        parent: Option<usize>,
        next: &mut usize,
        out: &mut String,
    ) {
        let id = *next;
        *next += 1;
        let label = format!(
            "{:?}\\n{}:{}",
            node.kind, node.range.start_line, node.raw_kind
        );
        out.push_str(&format!("  n{id} [label={label:?}];\n"));
        if let Some(p) = parent {
            out.push_str(&format!("  n{p} -> n{id};\n"));
        }
        for child in &node.children {
            visit(child, Some(id), next, out);
        }
    }
    let mut output = String::from("digraph ast {\n  node [shape=box,fontname=monospace];\n");
    let mut next = 0;
    visit(root, None, &mut next, &mut output);
    output.push_str("}\n");
    output
}
fn exit_code(r: &AnalysisReport, fail_warn: bool, fail_parse: bool, fail_new: bool) -> i32 {
    if r.summary.errors > 0 {
        2
    } else if fail_parse && !r.parse_errors.is_empty() {
        1
    } else if fail_new && r.changes.as_ref().is_some_and(|c| !c.new.is_empty()) {
        1
    } else if fail_warn
        && r.violations
            .iter()
            .any(|v| v.severity == Severity::Warn && v.architecture.is_none())
    {
        1
    } else {
        0
    }
}
