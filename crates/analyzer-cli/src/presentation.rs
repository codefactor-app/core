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
}
pub fn metric_help(id: &str) -> MetricHelp {
    if let Some(rule) = analyzer_core::architecture::RULES
        .iter()
        .find(|r| r.id == id)
    {
        return MetricHelp { id: id.into(), title: rule.title.into(), description: format!("{}: статический архитектурный сигнал. Всегда предупреждение, не блокирует CI. Основания и ограничения указаны в находке.", rule.principle) };
    }
    let base = id.split('.').next().unwrap_or(id);
    let (title, description) = match base {
        "file_lines" => ("Размер файла", "Все строки файла, включая комментарии и пустые строки. Помогает найти слишком большие файлы."),
        "functions_per_file" => ("Функции в файле", "Количество функций и методов с учётом настроек подсчёта. Помогает оценить объём ответственности файла."),
        "types_per_file" => ("Типы в файле", "Количество классов, структур и других типов. Большое число может указывать на необходимость разделить файл."),
        "methods_per_class" => ("Методы класса", "Количество методов с учётом настроек private и accessors. Помогает выявлять перегруженные классы."),
        "fields_per_class" => ("Поля класса", "Количество полей типа. Показывает объём состояния, которое хранит класс."),
        "function_lines" => ("Длина функции", "Строки функции вместе с объявлением. Длинные функции сложнее читать и изменять."),
        "function_body_lines" => ("Длина тела функции", "Строки тела функции без объявления. Показывает размер реализации."),
        "function_params" => ("Параметры функции", "Количество входных параметров с учётом настроек получателя. Большое число усложняет вызов и тестирование."),
        "algorithmic_time" => ("Алгоритмическая сложность по времени", "Рост числа операций в худшем случае относительно входных размеров. Ошибки превышения допустимы только для точного класса внутри модели; верхние границы и эвристики создают предупреждения."),
        "algorithmic_time_unknown" => ("Полнота анализа алгоритмической сложности", "Стоимость функции определить не удалось. Нарушение требования полноты не доказывает высокую сложность."),
        "cyclomatic_complexity" => ("Цикломатическая сложность", "Число независимых путей выполнения: базовый путь плюс ветвления. Помогает оценить необходимое количество сценариев проверки."),
        "nesting_depth" => ("Глубина вложенности", "Максимальная вложенность управляющих конструкций. Высокое значение затрудняет понимание логики."),
        "return_points" => ("Точки возврата", "Количество выходов return. Показывает, в скольких местах выполнение функции может завершиться."),
        "comment_ratio" => ("Доля комментариев", "Отношение строк комментариев ко всем строкам файла, от 0 до 1. Само по себе не оценивает полезность комментариев."),
        "inheritance_depth" => ("Глубина наследования", "Длина цепочки наследования известных анализатору типов. Длинная цепочка усложняет понимание поведения."),
        "lcom4" => ("Связность класса (LCOM4)", "Число групп методов, связанных общими полями или вызовами. Значение больше 1 может указывать на несколько обязанностей класса. Для интерфейсов и enum метрика не вычисляется."),
        "fan_in" => ("Входящие связи", "Количество входящих рёбер вызова функции в построенном графе. Показывает, насколько широко используется функция."),
        "fan_out" => ("Исходящие связи", "Количество исходящих рёбер вызова функции. Помогает оценить количество её зависимостей."),
        "halstead_volume" => ("Объём Холстеда", "Оценка объёма по количеству и разнообразию токенов. Приближённый показатель размера логики."),
        "maintainability_index" => ("Индекс сопровождаемости", "Оценка от 0 до 100 по объёму Холстеда, сложности и длине. Большее значение означает более простое сопровождение; это ориентир, а не гарантия качества."),
        "cognitive_complexity" => ("Когнитивная сложность", "Структурная модель Codefactor: решения, циклы и обработчики исключений с учётом вложенности; дополнительные ветки и последовательности логических операторов увеличивают оценку. Вызовы сами по себе её не увеличивают."),
        "duplicate_functions" => ("Дубликаты функций", "Количество других функций с совпадающей последовательностью токенов (от 30 токенов). Помогает выявлять повторение реализации."),
        "module_cycle" => ("Цикл зависимостей модуля", "1 — модуль участвует в цикле вызовов или обращений к значениям между модулями; 0 — цикл не найден. Одно нарушение объединяет сильносвязную компоненту с полным списком модулей и файлов. Циклы затрудняют независимое изменение модулей."),
        _ => (base, "Дополнительная метрика или правило проекта. Значение сравнивается с настроенным порогом."),
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
            "{} ({})\n  {}\n",
            style(&h.title, "1", color),
            clean(&id),
            wrapped(&h.description, 86)
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
pub fn scan(
    report: &AnalysisReport,
    color: bool,
    limit: usize,
    explain: bool,
    only_violations: bool,
) -> String {
    let mut out = String::new();
    let mut ids = BTreeSet::new();
    let s = &report.summary;
    heading(&mut out, "Codefactor · Анализ кода", color);
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
    if !only_violations {
        heading(&mut out, "Сводка", color);
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
    writeln!(
        out,
        "\n  {}: {} · {}: {} · Ошибки разбора: {}",
        style("Ошибки", "1;31", color),
        s.errors,
        style("Предупреждения", "1;33", color),
        s.warnings,
        s.parse_errors
    )
    .unwrap();
    if !report.resolution_diagnostics.is_empty() {
        heading(&mut out, "Диагностика разрешения импортов", color);
        for diagnostic in &report.resolution_diagnostics {
            writeln!(out, "  {}", clean(diagnostic)).unwrap();
        }
    }
    if !report.strategy_capabilities.is_empty() {
        heading(&mut out, "Возможности стратегий", color);
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
    if !only_violations {
        if let Some(project) = report.aggregates.iter().find(|a| a.name == "project") {
            heading(&mut out, "Ключевые показатели", color);
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
    if !report.violations.is_empty() {
        heading(&mut out, "Распределение замечаний", color);
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
    if !only_violations {
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
        if !functions.is_empty() {
            heading(&mut out, "Алгоритмическая сложность функций · время", color);
            if explain {
                writeln!(out, "  Модель: операции исходного кода; встроенные скалярные операции O(1). Числовой размер — величина, не число битов; оптимизация компилятора не учитывается.\n  Θ действует внутри модели и указанных предположений; O — верхняя граница. Независимые размеры сохраняются, общий масштаб ограничивает каждый из них величиной N.").unwrap();
            }
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
    }
    heading(&mut out, "Нарушения по файлам", color);
    let mut sorted = report
        .violations
        .iter()
        .filter(|v| v.architecture.is_none())
        .collect::<Vec<_>>();
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
        let (label, code) = if v.severity == Severity::Error {
            ("ОШИБКА", "1;31")
        } else {
            ("ПРЕДУПРЕЖДЕНИЕ", "1;33")
        };
        let h = metric_help(&v.rule);
        ids.insert(v.rule.clone());
        writeln!(
            out,
            "  {} · строка {} · {}\n    {} [{}]\n    Значение: {}  >  порог: {}",
            style(label, code, color),
            v.line,
            clean(&v.entity),
            h.title,
            clean(&v.rule),
            v.value_text(),
            v.limit_text()
        )
        .unwrap();
        if v.algorithmic_complexity.is_some() {
            writeln!(out, "    {}", clean(&v.message())).unwrap();
        }
        if let Some(d) = &v.module_cycle {
            writeln!(out, "    Модули: {}", clean(&d.participants.join(", "))).unwrap();
            writeln!(
                out,
                "    Замкнутый путь: {}",
                clean(&d.cycle.path.join(" → "))
            )
            .unwrap();
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
    heading(
        &mut out,
        "Архитектура · предупреждения без блокировки CI",
        color,
    );
    writeln!(
        out,
        "  Профиль: {} · находок: {} · файлов: {} · неизвестных зависимостей: {}",
        report.architecture.profile,
        report.architecture.warnings,
        report.architecture.analyzed_files,
        report.architecture.unresolved_dependencies
    )
    .unwrap();
    for r in &report.architecture.rules {
        writeln!(out, "  {}: {} — {}", r.title, r.status, clean(&r.reason)).unwrap();
    }
    let architecture = report
        .violations
        .iter()
        .filter(|v| v.architecture.is_some())
        .collect::<Vec<_>>();
    for (index, v) in architecture.iter().take(limit).enumerate() {
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
        architecture_block(&mut out, "Факт", &d.fact, color);
        architecture_block(&mut out, "Оценка", &d.hypothesis, color);
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
                architecture_text(&mut out, &e.description, "         ");
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
        writeln!(
            out,
            "\n    {}",
            style("Что проверить или изменить:", "1", color)
        )
        .unwrap();
        for r in &d.recommendations {
            writeln!(out, "      {}", clean(&r.pattern)).unwrap();
            architecture_text(&mut out, &r.proposal, "        ");
            for c in &r.conditions {
                architecture_text(&mut out, &format!("• {c}"), "        ");
            }
        }
        writeln!(out, "\n    {}", style("Границы оценки:", "1", color)).unwrap();
        for l in &d.limitations {
            architecture_text(&mut out, &format!("• {l}"), "      ");
        }
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
    if !report.parse_errors.is_empty() {
        heading(
            &mut out,
            "Ошибки разбора · анализ может быть неполным",
            color,
        );
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
    if explain && !ids.is_empty() {
        glossary(&mut out, ids, color);
    }
    heading(&mut out, "Результат", color);
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
    out
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
