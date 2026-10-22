//! Human readable presentation shared by the terminal and local browser.
use analyzer_core::{metrics::builtin_metric_ids, rules::Severity, AnalysisReport};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write,
};

#[derive(Serialize)]
pub struct MetricHelp {
    pub id: String,
    pub title: String,
    pub description: String,
    /// What a reader should do about a finding, in one of four shapes:
    /// fix, test, reconsider the split, or leave it when justified.
    pub action: String,
}
pub fn metric_help(id: &str) -> MetricHelp {
    if let Some(rule) = analyzer_core::architecture::RULES
        .iter()
        .find(|r| r.id == id)
    {
        return MetricHelp { id: id.into(), title: rule.title.into(), description: format!("{}: статический архитектурный сигнал. Всегда предупреждение, не блокирует CI. Основания и ограничения указаны в находке.", rule.principle), action: "Пересмотреть архитектуру по разделу «Что проверить или изменить» находки; решение зависит от контекста проекта.".into() };
    }
    let base = id.split('.').next().unwrap_or(id);
    let (title, description, action) = match base {
        "file_lines" => ("Размер файла", "Все строки файла, включая комментарии и пустые строки. Помогает найти слишком большие файлы.", "Пересмотреть разделение: вынести независимые типы или функции в отдельные файлы. Можно оставить для сгенерированных файлов и табличных данных."),
        "functions_per_file" => ("Функции в файле", "Количество функций и методов с учётом настроек подсчёта. Помогает оценить объём ответственности файла.", "Пересмотреть разделение по обязанностям. Можно оставить для файлов-утилит с независимыми функциями."),
        "types_per_file" => ("Типы в файле", "Количество классов, структур и других типов. Большое число может указывать на необходимость разделить файл.", "Пересмотреть разделение: один публичный тип на файл. Можно оставить для мелких вспомогательных типов рядом с основным."),
        "methods_per_class" => ("Методы класса", "Количество методов с учётом настроек private и accessors. Помогает выявлять перегруженные классы.", "Пересмотреть обязанности класса вместе с LCOM4: выделить группы методов в отдельные классы."),
        "fields_per_class" => ("Поля класса", "Количество полей типа. Показывает объём состояния, которое хранит класс.", "Проверить, не смешано ли несколько состояний; сгруппировать связанные поля в объект значения."),
        "function_lines" => ("Длина функции", "Строки функции вместе с объявлением. Длинные функции сложнее читать и изменять.", "Проверить тестами основные ветви. Разделять, только если функция смешивает уровни абстракции; длинный линейный код можно оставить."),
        "function_body_lines" => ("Длина тела функции", "Строки тела функции без объявления. Показывает размер реализации.", "То же, что для длины функции: разделять при смешении уровней абстракции."),
        "function_params" => ("Параметры функции", "Количество входных параметров обычной функции или метода с учётом настроек получателя. Большое число усложняет вызов и тестирование.", "Сгруппировать связанные параметры в объект значения."),
        "constructor_params" => ("Параметры конструктора", "Количество параметров конструктора с учётом настроек получателя. Для Rust учитываются методы new по соглашению об именовании. Пороги по умолчанию: предупреждение выше 8, ошибка выше 12.", "Пересмотреть зависимости и разделение обязанностей. Можно оставить для DTO, если параметры описывают единое значение."),
        "algorithmic_time" => ("Алгоритмическая сложность по времени", "Рост числа операций в худшем случае относительно входных размеров. Ошибки превышения допустимы только для точного класса внутри модели; верхние границы и эвристики создают предупреждения.", "Проверить реальные размеры входов и наличие кеширования; оптимизировать только при подтверждённом росте."),
        "algorithmic_time_unknown" => ("Полнота анализа алгоритмической сложности", "Стоимость функции определить не удалось. Нарушение требования полноты не доказывает высокую сложность.", "Информационно: уточнить цели вызовов или отключить оценку для участка в algorithmic_complexity."),
        "cyclomatic_complexity" => ("Цикломатическая сложность", "Число независимых путей выполнения: базовый путь плюс ветвления. Помогает оценить необходимое количество сценариев проверки.", "Проверить тестами: каждой независимой ветви нужен сценарий. Рефакторинг нужен, если ветви относятся к разным обязанностям."),
        "nesting_depth" => ("Глубина вложенности", "Максимальная вложенность управляющих конструкций. Высокое значение затрудняет понимание логики.", "Исправить ранними выходами или выделением вложенного блока в функцию."),
        "return_points" => ("Точки возврата", "Количество выходов return. Показывает, в скольких местах выполнение функции может завершиться.", "Проверить, что все выходы покрыты тестами. Можно оставить для guard-проверок в начале функции."),
        "comment_ratio" => ("Доля комментариев", "Отношение строк комментариев ко всем строкам файла, от 0 до 1. Само по себе не оценивает полезность комментариев.", "Информационный показатель; действий не требует."),
        "inheritance_depth" => ("Глубина наследования", "Длина цепочки наследования известных анализатору типов. Длинная цепочка усложняет понимание поведения.", "Пересмотреть иерархию: предпочесть композицию глубокому наследованию."),
        "lcom4" => ("Связность класса (LCOM4)", "Число групп методов, связанных общими полями или вызовами. Значение больше 1 может указывать на несколько обязанностей класса. Пропускаются интерфейсы, enum, traits, абстрактные типы, классы с менее чем двумя нестатическими неабстрактными методами и классы без обращений к полям или вызовов между этими методами. Конструкторы участвуют в связности.", "Пересмотреть разделение класса по группам методов. Можно оставить для фасадов и адаптеров, намеренно объединяющих независимые операции."),
        "fan_in" => ("Входящие связи", "Количество входящих рёбер вызова функции в построенном графе. Показывает, насколько широко используется функция.", "Защитить тестами и стабильным контрактом: изменение затрагивает многих вызывающих."),
        "fan_out" => ("Исходящие связи", "Количество исходящих рёбер вызова функции. Помогает оценить количество её зависимостей.", "Пересмотреть зависимости: много исходящих вызовов — признак оркестратора; выделить этапы."),
        "halstead_volume" => ("Объём Холстеда", "Оценка объёма по количеству и разнообразию токенов. Приближённый показатель размера логики.", "Как длина функции: разделять при смешении уровней абстракции."),
        "maintainability_index" => ("Индекс сопровождаемости", "Оценка от 0 до 100 по объёму Холстеда, сложности и длине. Большее значение означает более простое сопровождение; это ориентир, а не гарантия качества.", "Ориентир для сравнения; действий по одному числу не требует."),
        "cognitive_complexity" => ("Когнитивная сложность", "Структурная модель Codefactor: решения, циклы и обработчики исключений с учётом вложенности; дополнительные ветки и последовательности логических операторов увеличивают оценку. Вызовы сами по себе её не увеличивают.", "Снижать вложенность: ранние выходы, выделение вложенных блоков. Можно оставить для плоского перебора вариантов по одному признаку."),
        "duplicate_functions" => ("Дубликаты функций", "Количество других функций с совпадающей последовательностью токенов (от 30 токенов). Помогает выявлять повторение реализации.", "Исправить: объединить реализации или выделить общую функцию."),
        "module_cycle" => ("Цикл зависимостей модуля", "1 — модуль участвует в цикле вызовов или обращений к значениям между модулями; 0 — цикл не найден. Одно нарушение объединяет сильносвязную компоненту с полным списком модулей и файлов. Циклы затрудняют независимое изменение модулей.", "Пересмотреть архитектуру: разорвать цикл интерфейсом, событием или переносом общего типа в отдельный модуль. Можно оставить, если модули — части одного компонента; тогда объедините их через architecture.modules."),
        _ => (base, "Дополнительная метрика или правило проекта. Значение сравнивается с настроенным порогом.", "Действие зависит от правила проекта."),
    };
    let suffix = match id.strip_prefix(base).unwrap_or("") {
        ".avg" => " · среднее",
        ".median" => " · медиана",
        ".max" => " · максимум",
        ".p90" => " · p90",
        ".p95" => " · p95",
        suffix => suffix,
    };
    MetricHelp {
        id: id.into(),
        title: format!("{title}{suffix}"),
        description: description.into(),
        action: action.into(),
    }
}
pub fn metric_catalog() -> Vec<MetricHelp> {
    builtin_metric_ids()
        .iter()
        .copied()
        .chain(["algorithmic_time", "algorithmic_time_unknown"])
        .chain(analyzer_core::architecture::RULES.iter().map(|r| r.id))
        .map(metric_help)
        .collect()
}
fn style(text: &str, code: &str, color: bool) -> String {
    if color {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.into()
    }
}
fn heading(out: &mut String, title: &str, color: bool) {
    writeln!(out, "\n{}\n{}", style(title, "1;36", color), "─".repeat(72)).unwrap();
}
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
fn glossary(out: &mut String, ids: BTreeSet<String>, color: bool) {
    heading(out, "Что означают метрики", color);
    for id in ids {
        let h = metric_help(&id);
        writeln!(
            out,
            "{} ({})\n  {}\n  Что делать: {}\n",
            style(&h.title, "1", color),
            clean(&id),
            wrapped(&h.description, 86),
            wrapped(&h.action, 86)
        )
        .unwrap();
    }
}
fn group_title(name: &str) -> String {
    if name == "project" {
        "Проект".into()
    } else if let Some(path) = name.strip_prefix("directory:") {
        format!("Каталог: {}", if path.is_empty() { "." } else { path })
    } else if let Some(module) = name.strip_prefix("module:") {
        format!("Модуль: {module}")
    } else {
        clean(name)
    }
}
fn scope_title(name: &str) -> &str {
    match name {
        "function" => "функция",
        "class" => "класс",
        "file" => "файл",
        "module" => "модуль",
        "aggregate" => "сводка",
        other => other,
    }
}
pub fn metrics(report: &AnalysisReport, group: &str, color: bool, explain: bool) -> String {
    let mut out = String::new();
    let mut ids = BTreeSet::new();
    heading(&mut out, "Codefactor · Метрики кода", color);
    writeln!(
        out,
        "Проект: {}\nФайлов: {} · Функций: {} · Типов: {}",
        clean(&report.root),
        report.summary.files,
        report.summary.functions,
        report.summary.types
    )
    .unwrap();
    let files = report
        .files
        .iter()
        .map(|f| (f.path.as_str(), f))
        .collect::<BTreeMap<_, _>>();
    let mut group_counts = BTreeMap::<(String, String), (usize, usize)>::new();
    let mut entity_status = BTreeMap::new();
    for v in &report.violations {
        let mut groups = BTreeSet::from(["project".to_string()]);
        if v.scope == "aggregate" {
            groups.insert(v.entity.clone());
        } else {
            let directory = std::path::Path::new(&v.file)
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            groups.insert(format!("directory:{directory}"));
            if let Some(file) = files.get(v.file.as_str()) {
                groups.insert(format!("module:{}", file.module));
            }
            entity_status.insert(
                (
                    v.file.as_str(),
                    v.entity.as_str(),
                    v.scope.as_str(),
                    v.rule.as_str(),
                ),
                &v.severity,
            );
        }
        for name in groups {
            let count = group_counts
                .entry((name, v.rule.split('.').next().unwrap_or(&v.rule).into()))
                .or_default();
            if v.severity == Severity::Error {
                count.0 += 1;
            } else {
                count.1 += 1;
            }
        }
    }
    if group == "file" {
        let mut by_entity = BTreeMap::new();
        for m in &report.metrics {
            by_entity
                .entry((&m.path, &m.entity, &m.scope, m.line))
                .or_insert_with(Vec::new)
                .push(m);
        }
        for ((path, entity, scope, line), values) in by_entity {
            heading(
                &mut out,
                &format!(
                    "{}:{} · {} · {}",
                    clean(path),
                    line,
                    scope_title(scope),
                    clean(entity)
                ),
                color,
            );
            for m in values {
                ids.insert(m.id.to_string());
                let status = entity_status
                    .get(&(
                        m.path.as_ref(),
                        m.entity.as_ref(),
                        m.scope.as_ref(),
                        m.id.as_ref(),
                    ))
                    .copied();
                let (label, code) = match status {
                    Some(Severity::Error) => ("ОШИБКА", "1;31"),
                    Some(Severity::Warn) => ("ПРЕДУПРЕЖДЕНИЕ", "33"),
                    None => ("", "0"),
                };
                let row = format!(
                    "  {:<36} {:>10.2}  [{}] {}",
                    metric_help(&m.id).title,
                    m.value,
                    m.id,
                    label
                );
                writeln!(out, "{}", style(&row, code, color && status.is_some())).unwrap();
            }
        }
    } else {
        for a in &report.aggregates {
            if !(a.name == group || a.name.starts_with(&format!("{group}:"))) {
                continue;
            }
            heading(&mut out, &group_title(&a.name), color);
            writeln!(
                out,
                "{:<36} {:>9} {:>9} {:>9} {:>9} {:>9}",
                "Метрика", "Среднее", "Медиана", "Максимум", "p90", "p95"
            )
            .unwrap();
            for (id, d) in &a.metrics {
                ids.insert(id.clone());
                let count = group_counts
                    .get(&(a.name.clone(), id.clone()))
                    .copied()
                    .unwrap_or_default();
                let row = format!(
                    "{:<36} {:>9.2} {:>9.2} {:>9.2} {:>9.2} {:>9.2}",
                    metric_help(id).title,
                    d.avg,
                    d.median,
                    d.max,
                    d.p90,
                    d.p95
                );
                writeln!(
                    out,
                    "{}",
                    style(
                        &row,
                        if count.0 > 0 { "1;31" } else { "33" },
                        color && (count.0 > 0 || count.1 > 0)
                    )
                )
                .unwrap();
                write!(out, "  {}", style(id, "2", color)).unwrap();
                if count.0 > 0 || count.1 > 0 {
                    write!(out, " · Ошибки: {} · Предупреждения: {}", count.0, count.1).unwrap();
                }
                out.push('\n');
            }
        }
        writeln!(out,"\nСреднее — сумма / число значений; медиана — середина распределения.\np90 и p95 — значения, ниже которых находятся 90% и 95% наблюдений.\nЦвет строк и счётчики отмечают нарушения в выбранной группе.\nСводные значения не являются оценкой качества без контекста и порогов.").unwrap();
    }
    if explain {
        glossary(&mut out, ids, color);
    }
    out
}
/// Console view options for `scan`.
#[derive(Clone, Copy, Debug)]
pub struct ScanView {
    /// Entries per section; `usize::MAX` removes the cap.
    pub limit: usize,
    /// Print the metric glossary and per-function reasoning.
    pub explain: bool,
    /// Skip the summary, key metrics and the algorithmic section.
    pub only_violations: bool,
    /// Restore the detail the compact view folds away: every cost estimate,
    /// every architecture rule status and complete module-cycle blocks.
    pub full: bool,
}

pub fn scan(report: &AnalysisReport, color: bool, view: ScanView) -> String {
    let mut out = String::new();
    let mut ids = BTreeSet::new();
    let view = ScanPresentation {
        report,
        color,
        limit: view.limit,
        only_violations: view.only_violations,
        explain: view.explain,
        full: view.full,
        cards: architecture_card_index(report),
    };
    view.header(&mut out, &mut ids);
    view.aggregates(&mut out, &mut ids);
    view.rule_totals(&mut out);
    view.source_inventory(&mut out);
    view.numeric_violations(&mut out, &mut ids);
    view.architecture(&mut out);
    view.parse_errors(&mut out);
    if view.explain && !ids.is_empty() {
        glossary(&mut out, ids, color);
    }
    view.result(&mut out);
    out
}

/// Architecture card numbers keyed by sorted cycle participants, so a numeric
/// `module_cycle` finding can point at the card that already explains it.
fn architecture_card_index(report: &AnalysisReport) -> BTreeMap<Vec<String>, usize> {
    let mut index = BTreeMap::new();
    for (position, v) in report
        .violations
        .iter()
        .filter(|v| v.architecture.is_some())
        .enumerate()
    {
        if v.rule != "arch_cycle" {
            continue;
        }
        let d = v.architecture.as_ref().unwrap();
        index
            .entry(sorted_participants(&d.participants))
            .or_insert(position + 1);
    }
    index
}
fn sorted_participants(participants: &[String]) -> Vec<String> {
    let mut sorted = participants.to_vec();
    sorted.sort();
    sorted.dedup();
    sorted
}
/// Growth beyond linear in at least one size variable.
fn superlinear(e: &analyzer_core::algorithmic::ComplexityEstimate) -> bool {
    use analyzer_core::algorithmic::GrowthClass;
    let linear = GrowthClass {
        polynomial: 1,
        logarithmic: 0,
    };
    e.expression.as_ref().is_some_and(|g| g.class() > linear)
}
/// How far a numeric value is above its threshold, e.g. ` (×1.3)`.
fn excess_text(v: &analyzer_core::rules::Violation) -> String {
    if v.algorithmic_complexity.is_some() || v.architecture.is_some() {
        return String::new();
    }
    match (v.value, v.limit) {
        (Some(value), Some(limit)) if limit > 0.0 => format!(" (×{:.1})", value / limit),
        _ => String::new(),
    }
}
struct ScanPresentation<'a> {
    report: &'a AnalysisReport,
    color: bool,
    limit: usize,
    only_violations: bool,
    explain: bool,
    full: bool,
    cards: BTreeMap<Vec<String>, usize>,
}
impl ScanPresentation<'_> {
    fn header(&self, out: &mut String, ids: &mut BTreeSet<String>) {
        let report = self.report;
        let color = self.color;
        let only_violations = self.only_violations;
        let s = &self.report.summary;
        heading(out, "Codefactor · Анализ кода", color);
        writeln!(
            out,
            "Проект: {}\nКритерии: {}",
            clean(&report.root),
            clean(
                report
                    .config_path
                    .as_deref()
                    .unwrap_or("встроенные настройки")
            )
        )
        .unwrap();
        if let Some(path) = &report.baseline_path {
            writeln!(
                out,
                "Baseline: {} (скрыто {}, устаревших {})",
                clean(path),
                s.baseline_suppressed,
                s.baseline_stale
            )
            .unwrap();
        }
        if !only_violations {
            heading(out, "Сводка", color);
            for (label, value) in [
                ("Файлы", s.files),
                ("Функции", s.functions),
                ("Типы", s.types),
                ("Файлы зависимостей", s.vendor_files),
            ] {
                writeln!(out, "  {label:<24} {value:>8}").unwrap();
            }
            writeln!(
                out,
                "  {:<24} {} / {}",
                "Разобрано / кеш", s.parsed_files, s.cache_hits
            )
            .unwrap();
        }
        write!(
            out,
            "\n  {}: {} · {}: {} · Ошибки разбора: {}",
            style("Ошибки", "1;31", color),
            s.errors,
            style("Предупреждения", "1;33", color),
            s.warnings,
            s.parse_errors
        )
        .unwrap();
        if s.baseline_suppressed > 0 {
            write!(out, " · скрыто baseline: {}", s.baseline_suppressed).unwrap();
        }
        if s.baseline_stale > 0 {
            write!(out, " · устаревших записей baseline: {}", s.baseline_stale).unwrap();
        }
        out.push('\n');
        self.changes(out, ids);
        if !report.diagnostics.is_empty() {
            heading(out, "Диагностика анализа", color);
            for diagnostic in &report.diagnostics {
                writeln!(out, "  {}", clean(diagnostic)).unwrap();
            }
        }
        if !report.resolution_diagnostics.is_empty() {
            heading(out, "Диагностика разрешения импортов", color);
            for diagnostic in &report.resolution_diagnostics {
                writeln!(out, "  {}", clean(diagnostic)).unwrap();
            }
        }
        if !report.strategy_capabilities.is_empty() {
            heading(out, "Возможности стратегий", color);
            for (language, caps) in &report.strategy_capabilities {
                let mark = |value| if value { "да" } else { "нет" };
                writeln!(
                out,
                "  {}: типы={} · generics={} · наследование/traits={} · замыкания={} · условия={}",
                clean(language),
                mark(caps.static_types),
                mark(caps.generics),
                mark(caps.inheritance),
                mark(caps.closures),
                mark(caps.guards)
            )
                .unwrap();
            }
        }
    }
    fn changes(&self, out: &mut String, ids: &mut BTreeSet<String>) {
        let Some(changes) = &self.report.changes else {
            return;
        };
        heading(out, "Анализ изменений", self.color);
        if changes.compared {
            writeln!(
                out,
                "  Новых: {} · Исправлено: {} · Без изменений: {}",
                changes.new.len(),
                changes.fixed.len(),
                changes.unchanged
            )
            .unwrap();
        }
        if !changes.compared || !changes.changed_files.is_empty() || changes.filtered_out.total > 0
        {
            writeln!(
                out,
                "  Файлов в фильтре: {} · Скрыто замечаний: {} (ошибок: {}, предупреждений: {}, архитектурных: {})",
                changes.changed_files.len(),
                changes.filtered_out.total,
                changes.filtered_out.errors,
                changes.filtered_out.warnings,
                changes.filtered_out.architecture_warnings
            )
            .unwrap();
        }
        if !changes.compared {
            return;
        }
        if !changes.new.is_empty() {
            writeln!(out, "\n{}", style("Новые замечания", "1", self.color)).unwrap();
            let mut new = changes.new.iter().collect::<Vec<_>>();
            new.sort_by_key(|v| {
                (
                    if v.severity == Severity::Error { 0 } else { 1 },
                    &v.file,
                    v.line,
                    &v.rule,
                )
            });
            for v in new.into_iter().take(self.limit) {
                if let Some(diagnostic) = &v.architecture {
                    writeln!(
                        out,
                        "\n  {} · {}:{} · {}\n    {} [{}]\n    Факт: {}\n    Подробности: раздел «Архитектура»; --all показывает все карточки.",
                        style("ПРЕДУПРЕЖДЕНИЕ", "1;33", self.color),
                        clean(&v.file),
                        v.line,
                        clean(&v.entity),
                        clean(&diagnostic.title),
                        clean(&v.rule),
                        clean(&diagnostic.fact)
                    )
                    .unwrap();
                } else {
                    writeln!(
                        out,
                        "\n{}",
                        style(
                            &clean(if v.file.is_empty() {
                                &v.entity
                            } else {
                                &v.file
                            }),
                            "1",
                            self.color
                        )
                    )
                    .unwrap();
                    ids.insert(v.rule.clone());
                    self.write_violation(out, v);
                }
            }
            if changes.new.len() > self.limit {
                writeln!(
                    out,
                    "  Показано новых: {} из {}. Полный список: --all или --format json.",
                    self.limit,
                    changes.new.len()
                )
                .unwrap();
            }
        }
        if !changes.fixed.is_empty() {
            writeln!(
                out,
                "\n{}",
                style("Исправленные замечания", "1", self.color)
            )
            .unwrap();
            for v in changes.fixed.iter().take(self.limit) {
                let (label, code) = if v.severity == Severity::Error {
                    ("ОШИБКА", "1;31")
                } else {
                    ("ПРЕДУПРЕЖДЕНИЕ", "1;33")
                };
                writeln!(
                    out,
                    "  {} · {}:{} · {} [{}] · {}",
                    style(label, code, self.color),
                    clean(&v.file),
                    v.line,
                    clean(&metric_help(&v.rule).title),
                    clean(&v.rule),
                    clean(&v.entity)
                )
                .unwrap();
            }
            if changes.fixed.len() > self.limit {
                writeln!(
                    out,
                    "  Показано исправленных: {} из {}. Полный список: --all или --format json.",
                    self.limit,
                    changes.fixed.len()
                )
                .unwrap();
            }
        }
    }
    fn aggregates(&self, out: &mut String, ids: &mut BTreeSet<String>) {
        let report = self.report;
        let color = self.color;
        let only_violations = self.only_violations;
        if !only_violations {
            if let Some(project) = report.aggregates.iter().find(|a| a.name == "project") {
                heading(out, "Ключевые показатели", color);
                writeln!(
                    out,
                    "{:<36} {:>10} {:>10} {:>10}",
                    "Метрика", "Среднее", "Максимум", "p95"
                )
                .unwrap();
                for id in [
                    "file_lines",
                    "function_lines",
                    "function_params",
                    "cyclomatic_complexity",
                    "nesting_depth",
                    "maintainability_index",
                ] {
                    if let Some(d) = project.metrics.get(id) {
                        ids.insert(id.to_string());
                        writeln!(
                            out,
                            "{:<36} {:>10.2} {:>10.2} {:>10.2}",
                            metric_help(id).title,
                            d.avg,
                            d.max,
                            d.p95
                        )
                        .unwrap();
                    }
                }
                writeln!(out, "p95: 95% значений не превышают этот показатель.").unwrap();
            }
        }
    }
    fn rule_totals(&self, out: &mut String) {
        let report = self.report;
        let color = self.color;
        if !report.violations.is_empty() {
            heading(out, "Распределение замечаний", color);
            let mut counts = BTreeMap::<&str, (usize, usize)>::new();
            for v in &report.violations {
                let count = counts.entry(&v.rule).or_default();
                if v.severity == Severity::Error {
                    count.0 += 1;
                } else {
                    count.1 += 1;
                }
            }
            writeln!(
                out,
                "{:<36} {:>10} {:>16}",
                "Метрика", "Ошибки", "Предупреждения"
            )
            .unwrap();
            for (id, (errors, warnings)) in counts {
                writeln!(
                    out,
                    "{:<36} {:>10} {:>16}",
                    metric_help(id).title,
                    errors,
                    warnings
                )
                .unwrap();
            }
        }
    }
    fn source_inventory(&self, out: &mut String) {
        let explain = self.explain;
        let report = self.report;
        let color = self.color;
        let limit = self.limit;
        let only_violations = self.only_violations;
        if only_violations {
            return;
        }

        let functions = report
            .files
            .iter()
            .filter(|f| !f.vendor)
            .flat_map(|file| {
                file.functions
                    .iter()
                    .filter_map(move |f| f.algorithmic_complexity.as_ref().map(|e| (file, f, e)))
            })
            .collect::<Vec<_>>();
        if functions.is_empty() {
            return;
        }

        use analyzer_core::algorithmic::EstimateStatus;
        heading(out, "Алгоритмическая сложность функций · время", color);
        let count = |status: EstimateStatus| {
            functions
                .iter()
                .filter(|(_, _, e)| e.status == status)
                .count()
        };
        let disabled = count(EstimateStatus::Disabled);
        let unsupported = count(EstimateStatus::Unsupported);
        writeln!(
            out,
            "  Функций: {} · точных: {} · верхних границ: {} · эвристических: {} · неизвестных: {} · не поддерживается: {} · отключено: {}",
            functions.len(),
            count(EstimateStatus::Tight),
            count(EstimateStatus::UpperBound),
            count(EstimateStatus::Heuristic),
            count(EstimateStatus::Unknown),
            unsupported,
            disabled
        )
        .unwrap();
        if disabled + unsupported == functions.len() && !self.full {
            writeln!(
                out,
                "  Оценки не вычислялись. Полный список функций: --full или --format json."
            )
            .unwrap();
            return;
        }
        if explain {
            writeln!(out, "  Модель: операции исходного кода; встроенные скалярные операции O(1). Числовой размер — величина, не число битов; оптимизация компилятора не учитывается.\n  Θ действует внутри модели и указанных предположений; O — верхняя граница. Независимые размеры сохраняются, общий масштаб ограничивает каждый из них величиной N.").unwrap();
        }
        let flagged = report
            .violations
            .iter()
            .filter(|v| v.algorithmic_complexity.is_some())
            .map(|v| (v.file.as_str(), v.entity.as_str()))
            .collect::<BTreeSet<_>>();
        let functions = if self.full {
            functions
        } else {
            let shown = functions
                .iter()
                .filter(|(file, function, e)| {
                    matches!(
                        e.status,
                        EstimateStatus::Unknown | EstimateStatus::Heuristic
                    ) || superlinear(e)
                        || flagged.contains(&(file.path.as_str(), function.fqn.as_str()))
                })
                .copied()
                .collect::<Vec<_>>();
            let hidden = functions.len() - shown.len();
            if hidden > 0 {
                writeln!(
                    out,
                    "  Скрыто {hidden} функций с линейной или меньшей оценкой, без нарушений и неизвестных стоимостей: --full показывает все."
                )
                .unwrap();
            }
            shown
        };
        for (file, function, e) in functions.iter().take(limit) {
            writeln!(
                out,
                "  {}:{} · {}\n    {} · {}{}",
                clean(&file.path),
                function.range.start_line,
                clean(&function.fqn),
                clean(e.formula.as_deref().unwrap_or("—")),
                e.status.label(),
                e.normalized
                    .as_ref()
                    .map(|n| format!(" · общий масштаб {n}"))
                    .unwrap_or_default()
            )
            .unwrap();
            if explain {
                for reason in &e.unknown_reasons {
                    writeln!(out, "    Причина: {}", clean(reason)).unwrap();
                }
                for assumption in &e.assumptions {
                    writeln!(out, "    Предположение: {}", clean(assumption)).unwrap();
                }
                for step in e
                    .explanations
                    .iter()
                    .filter(|s| matches!(s.rule.as_ref(), "call" | "loop" | "library"))
                {
                    writeln!(
                        out,
                        "    {}:{}: {}",
                        clean(&step.range.file),
                        step.range.start_line,
                        clean(&step.detail)
                    )
                    .unwrap();
                }
            }
        }
        if functions.len() > limit {
            writeln!(
                out,
                "  Показано {} из {}. Полный список: --all или --format json.",
                limit,
                functions.len()
            )
            .unwrap();
        }
    }
    /// One numeric or algorithmic finding. A module cycle that an architecture
    /// card already explains is shortened to a pointer unless `full` is set.
    fn write_violation(&self, out: &mut String, v: &analyzer_core::rules::Violation) {
        let color = self.color;
        let (label, code) = if v.severity == Severity::Error {
            ("ОШИБКА", "1;31")
        } else {
            ("ПРЕДУПРЕЖДЕНИЕ", "1;33")
        };
        let h = metric_help(&v.rule);
        let card = v
            .module_cycle
            .as_ref()
            .filter(|_| !self.full)
            .and_then(|d| self.cards.get(&sorted_participants(&d.participants)))
            .copied();
        // The representative file of a cycle is an alphabetical choice at line 1;
        // the first witness is where the reader can actually look.
        let witness = card
            .and_then(|_| v.module_cycle.as_ref())
            .and_then(|d| d.evidence.first())
            .map(|e| (e.range.start_line, &*e.range.file));
        let (line, location) = match witness {
            Some((line, file)) if *file != v.file => (line, format!(" · {}", clean(file))),
            Some((line, _)) => (line, String::new()),
            None => (v.line, String::new()),
        };
        writeln!(
            out,
            "  {} · строка {}{} · {}\n    {} [{}]\n    Значение: {}  >  порог: {}{}",
            style(label, code, color),
            line,
            location,
            clean(&v.entity),
            clean(&h.title),
            clean(&v.rule),
            clean(&v.value_text()),
            clean(&v.limit_text()),
            excess_text(v)
        )
        .unwrap();
        if v.algorithmic_complexity.is_some() {
            writeln!(out, "    {}", clean(&v.message())).unwrap();
        }
        self.module_cycle_details(out, v, card);
    }
    fn numeric_violations(&self, out: &mut String, ids: &mut BTreeSet<String>) {
        let report = self.report;
        let color = self.color;
        let limit = self.limit;
        // Exact values distinguish duplicate fingerprints at different source
        // locations. Consume only the occurrences already classified as new.
        let mut new = BTreeMap::<String, usize>::new();
        if let Some(changes) = self
            .report
            .changes
            .as_ref()
            .filter(|changes| changes.compared)
        {
            for violation in changes.new.iter().filter(|v| v.architecture.is_none()) {
                *new.entry(serde_json::to_string(violation).expect("violations serialize"))
                    .or_default() += 1;
            }
        }
        let has_new = !new.is_empty();
        let mut sorted = report
            .violations
            .iter()
            .filter(|v| v.architecture.is_none())
            .filter(|v| {
                if new.is_empty() {
                    return true;
                }
                let key = serde_json::to_string(v).expect("violations serialize");
                match new.get_mut(&key) {
                    Some(remaining) if *remaining > 0 => {
                        *remaining -= 1;
                        false
                    }
                    _ => true,
                }
            })
            .collect::<Vec<_>>();
        if sorted.is_empty() && self.report.changes.is_some() && !report.violations.is_empty() {
            return;
        }
        heading(
            out,
            if has_new {
                "Остальные нарушения по файлам"
            } else {
                "Нарушения по файлам"
            },
            color,
        );
        sorted.sort_by_key(|v| {
            (
                if v.severity == Severity::Error { 0 } else { 1 },
                &v.file,
                v.line,
                &v.rule,
            )
        });
        let mut last = None;
        for v in sorted.iter().take(limit) {
            let key = (&v.file, &v.severity);
            if last != Some(key) {
                writeln!(
                    out,
                    "\n{}",
                    style(
                        &clean(if v.file.is_empty() {
                            &v.entity
                        } else {
                            &v.file
                        }),
                        "1",
                        color
                    )
                )
                .unwrap();
                last = Some(key);
            }
            ids.insert(v.rule.clone());
            self.write_violation(out, v);
        }
        if sorted.is_empty() {
            writeln!(
                out,
                "{}",
                style("Нарушений по заданным критериям не найдено.", "1;32", color)
            )
            .unwrap();
        }
        if sorted.len() > limit {
            writeln!(out,"\nПоказано {} из {}. Ошибки идут первыми.\nПолный список: --all или --format json --out report.json",limit,sorted.len()).unwrap();
        }
    }
    fn module_cycle_details(
        &self,
        out: &mut String,
        v: &analyzer_core::rules::Violation,
        card: Option<usize>,
    ) {
        if let Some(d) = &v.module_cycle {
            writeln!(out, "    Модули: {}", clean(&d.participants.join(", "))).unwrap();
            writeln!(
                out,
                "    Замкнутый путь: {}",
                clean(&d.cycle.path.join(" → "))
            )
            .unwrap();
            if let Some(number) = card {
                writeln!(
                    out,
                    "    Подробности: Архитектура · ПРЕДУПРЕЖДЕНИЕ #{number}"
                )
                .unwrap();
                return;
            }
            writeln!(out, "    Файлы компоненты:").unwrap();
            for file in &d.files {
                writeln!(out, "      {}", clean(file)).unwrap();
            }
            writeln!(out, "    Основания · места в коде:").unwrap();
            for evidence in &d.evidence {
                writeln!(
                    out,
                    "      {}:{} — {}",
                    clean(&evidence.range.file),
                    evidence.range.start_line,
                    clean(&evidence.description)
                )
                .unwrap();
            }
            if d.evidence_total > d.evidence.len() {
                writeln!(
                    out,
                    "      Показано {} из {} свидетельств.",
                    d.evidence.len(),
                    d.evidence_total
                )
                .unwrap();
            }
        }
    }
    fn architecture(&self, out: &mut String) {
        let report = self.report;
        let color = self.color;
        let limit = self.limit;
        heading(out, "Архитектура · предупреждения без блокировки CI", color);
        writeln!(
            out,
            "  Профиль: {} · находок: {} · файлов: {} · неизвестных зависимостей: {}",
            report.architecture.profile,
            report.architecture.warnings,
            report.architecture.analyzed_files,
            report.architecture.unresolved_dependencies
        )
        .unwrap();
        // Rules that ran get their own line; rules that did not are folded by
        // reason so a strict profile does not print seven identical excuses.
        let mut folded = BTreeMap::<(&str, &str), usize>::new();
        for r in &report.architecture.rules {
            if self.full || !matches!(r.status.as_str(), "skipped" | "disabled") {
                writeln!(out, "  {}: {} — {}", r.title, r.status, clean(&r.reason)).unwrap();
            } else {
                *folded
                    .entry((r.status.as_str(), r.reason.as_str()))
                    .or_default() += 1;
            }
        }
        for ((status, reason), count) in folded {
            let noun = match count {
                1 => "правило",
                2..=4 => "правила",
                _ => "правил",
            };
            writeln!(out, "  {count} {noun}: {status} — {}", clean(reason)).unwrap();
        }
        let architecture = report
            .violations
            .iter()
            .filter(|v| v.architecture.is_some())
            .collect::<Vec<_>>();
        for (index, v) in architecture.iter().take(limit).enumerate() {
            self.architecture_finding(out, v, index);
        }
        if architecture.len() > limit {
            writeln!(
                out,
                "  Показано {} из {}. Полный список: --all или --format json.",
                limit,
                architecture.len()
            )
            .unwrap();
        }
        for d in &report.architecture.diagnostics {
            writeln!(out, "  {}", clean(d)).unwrap();
        }
    }
    fn architecture_finding(
        &self,
        out: &mut String,
        v: &analyzer_core::rules::Violation,
        index: usize,
    ) {
        let color = self.color;
        let d = v.architecture.as_ref().unwrap();
        writeln!(
            out,
            "\n  {}\n  {} · {} · {} [{}]\n    {}:{}",
            "─".repeat(72),
            style(&format!("ПРЕДУПРЕЖДЕНИЕ #{}", index + 1), "1;33", color),
            clean(&d.title),
            clean(&d.principle),
            clean(&v.rule),
            clean(&v.file),
            v.line
        )
        .unwrap();
        architecture_block(out, "Факт", &d.fact, color);
        architecture_block(out, "Оценка", &d.hypothesis, color);
        if let Some(cycle) = &d.cycle {
            writeln!(out, "\n    {}", style("Замкнутый путь:", "1", color)).unwrap();
            for (step, module) in cycle.path.iter().enumerate() {
                writeln!(
                    out,
                    "      {}. {}{}",
                    step + 1,
                    clean(module),
                    if step + 1 == cycle.path.len() {
                        " (возврат)"
                    } else {
                        ""
                    }
                )
                .unwrap();
            }
            writeln!(out, "\n    Виды связей · мест в коде:\n      вызовы: {} · обращения к свойствам и переменным: {}",
                d.measurements.get("call_sites").unwrap_or(&0.0),
                d.measurements.get("value_access_sites").unwrap_or(&0.0)).unwrap();
        }
        writeln!(
            out,
            "\n    {}",
            style("Основания · места в коде:", "1", color)
        )
        .unwrap();
        self.architecture_evidence(out, d);
        writeln!(
            out,
            "\n    {}",
            style("Что проверить или изменить:", "1", color)
        )
        .unwrap();
        for r in &d.recommendations {
            writeln!(out, "      {}", clean(&r.pattern)).unwrap();
            architecture_text(out, &r.proposal, "        ");
            for c in &r.conditions {
                architecture_text(out, &format!("• {c}"), "        ");
            }
        }
        writeln!(out, "\n    {}", style("Границы оценки:", "1", color)).unwrap();
        for l in &d.limitations {
            architecture_text(out, &format!("• {l}"), "      ");
        }
    }
    fn architecture_evidence(
        &self,
        out: &mut String,
        d: &analyzer_core::architecture::ArchitectureDiagnostic,
    ) {
        for (number, e) in d.evidence.iter().enumerate() {
            if let Some(edge) = &e.dependency {
                writeln!(
                    out,
                    "      {}. {}\n         {}:{}\n         → {}",
                    number + 1,
                    dependency_title(&edge.kind),
                    clean(&edge.from_file),
                    e.range.start_line,
                    clean(&edge.to_file)
                )
                .unwrap();
            } else {
                writeln!(
                    out,
                    "      {}. {}:{}",
                    number + 1,
                    clean(&e.range.file),
                    e.range.start_line
                )
                .unwrap();
                architecture_text(out, &e.description, "         ");
            }
        }
        if d.evidence_total > d.evidence.len() {
            writeln!(
                out,
                "      Свидетельств показано {} из {}",
                d.evidence.len(),
                d.evidence_total
            )
            .unwrap();
        }
    }
    fn parse_errors(&self, out: &mut String) {
        let report = self.report;
        let color = self.color;
        let limit = self.limit;
        if !report.parse_errors.is_empty() {
            heading(out, "Ошибки разбора · анализ может быть неполным", color);
            for e in report.parse_errors.iter().take(limit) {
                writeln!(
                    out,
                    "  {}:{}:{}\n    {}",
                    clean(&e.range.file),
                    e.range.start_line,
                    e.range.start_column,
                    clean(&e.message)
                )
                .unwrap();
            }
            if report.parse_errors.len() > limit {
                writeln!(
                    out,
                    "  Показано {} из {}. Полный список: --all.",
                    limit,
                    report.parse_errors.len()
                )
                .unwrap();
            }
        }
    }
    fn result(&self, out: &mut String) {
        let color = self.color;
        let s = &self.report.summary;
        heading(out, "Результат", color);
        let (text, code) = if s.errors > 0 || s.parse_errors > 0 {
            (
                "Найдены ошибки. Начните с отмеченных файлов и строк.",
                "1;31",
            )
        } else if s.warnings > 0 {
            (
                "Найдены предупреждения. Проверьте их с учётом контекста проекта.",
                "1;33",
            )
        } else {
            ("Заданные проверки пройдены.", "1;32")
        };
        writeln!(out, "{}", style(text, code, color)).unwrap();
    }
}

/// An elapsed-time indicator, only on an interactive stderr. Drop also clears it on errors.
pub struct Progress {
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Progress {
    pub fn start() -> Self {
        use std::io::IsTerminal;
        if !std::io::stderr().is_terminal() {
            return Self {
                stop: None,
                thread: None,
            };
        }
        let (stop, receiver) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            use std::io::Write;
            let started = std::time::Instant::now();
            let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let mut frame = 0;
            loop {
                eprint!(
                    "\r\x1b[2K{} Анализ кода… {} с",
                    frames[frame % frames.len()],
                    started.elapsed().as_secs()
                );
                let _ = std::io::stderr().flush();
                if receiver
                    .recv_timeout(std::time::Duration::from_millis(120))
                    .is_ok()
                {
                    break;
                }
                frame += 1;
            }
            eprint!("\r\x1b[2K");
        });
        Self {
            stop: Some(stop),
            thread: Some(thread),
        }
    }
}
impl Drop for Progress {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn wrapped(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut column = 0;
    for word in text.split_whitespace() {
        let length = word.chars().count();
        if column > 0 && column + length + 1 > width {
            out.push_str("\n  ");
            column = 0;
        }
        if column > 0 {
            out.push(' ');
            column += 1;
        }
        out.push_str(word);
        column += length;
    }
    out
}

fn architecture_block(out: &mut String, label: &str, text: &str, color: bool) {
    writeln!(out, "\n    {}", style(&format!("{label}:"), "1", color)).unwrap();
    architecture_text(out, text, "      ");
}
fn architecture_text(out: &mut String, text: &str, indent: &str) {
    let width = 92 - indent.chars().count();
    let mut line = String::new();
    for word in clean(text).split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            writeln!(out, "{indent}{line}").unwrap();
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        writeln!(out, "{indent}{line}").unwrap();
    }
}
fn dependency_title(kind: &str) -> &str {
    match kind {
        "import" => "Импорт",
        "metadata_reference" => "Ссылка из PHP-атрибута",
        "class_reference" => "Ссылка на класс (::class / константа)",
        "call" => "Вызов",
        "member_access" => "Обращение к свойству / константе",
        "variable_access" => "Обращение к переменной",
        "construction" => "Создание объекта",
        "inheritance" => "Наследование",
        "implementation" => "Реализация интерфейса",
        "field_type" => "Тип поля",
        "parameter_type" => "Тип параметра",
        "return_type" => "Тип результата",
        "type_reference" => "Ссылка на тип",
        other => other,
    }
}
