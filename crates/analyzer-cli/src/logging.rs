//! Stderr logger with global and per-module level filters.
use log::{LevelFilter, Log, Metadata, Record};
use std::{io::Write, str::FromStr};

struct Logger {
    default: LevelFilter,
    directives: Vec<(String, LevelFilter)>,
}
impl Logger {
    fn parse(
        explicit: Option<&str>,
        environment: Option<&str>,
        verbose: bool,
    ) -> anyhow::Result<Self> {
        let mut logger = Self {
            default: if verbose {
                LevelFilter::Debug
            } else {
                LevelFilter::Warn
            },
            directives: vec![],
        };
        if let Some(level) = explicit {
            logger.default = LevelFilter::from_str(level)?;
            return Ok(logger);
        }
        if let Some(filter) = environment {
            for directive in filter
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                if let Some((target, level)) = directive.split_once('=') {
                    if target.is_empty()
                        || !target
                            .chars()
                            .all(|c| c.is_alphanumeric() || c == '_' || c == ':')
                    {
                        anyhow::bail!("invalid RUST_LOG target: {target}");
                    }
                    logger
                        .directives
                        .push((target.into(), LevelFilter::from_str(level)?));
                } else if let Ok(level) = LevelFilter::from_str(directive) {
                    logger.default = level;
                } else {
                    anyhow::bail!(
                        "invalid RUST_LOG directive: {directive}; expected LEVEL or MODULE=LEVEL"
                    );
                }
            }
            logger
                .directives
                .sort_by_key(|(target, _)| std::cmp::Reverse(target.len()));
        }
        Ok(logger)
    }
    fn level(&self, target: &str) -> LevelFilter {
        self.directives
            .iter()
            .find(|(prefix, _)| {
                target == prefix
                    || target
                        .strip_prefix(prefix)
                        .is_some_and(|rest| rest.starts_with("::"))
            })
            .map(|(_, level)| *level)
            .unwrap_or(self.default)
    }
}
impl Log for Logger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= self.level(metadata.target())
    }
    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            let _ = writeln!(
                std::io::stderr().lock(),
                "{} {}: {}",
                record.level(),
                record.target(),
                record.args()
            );
        }
    }
    fn flush(&self) {
        let _ = std::io::stderr().flush();
    }
}
pub fn init(level: Option<&str>, verbose: bool) -> anyhow::Result<()> {
    let environment = std::env::var("RUST_LOG").ok();
    let logger = Logger::parse(level, environment.as_deref(), verbose)?;
    let max = logger
        .directives
        .iter()
        .map(|(_, level)| *level)
        .chain(std::iter::once(logger.default))
        .max()
        .unwrap_or(LevelFilter::Off);
    log::set_logger(Box::leak(Box::new(logger)))?;
    log::set_max_level(max);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_level_wins_and_most_specific_module_filter_is_used() {
        let logger = Logger::parse(
            None,
            Some("error,analyzer_core=debug,analyzer_core::report=trace"),
            false,
        )
        .unwrap();
        assert_eq!(logger.level("analyzer_core::report"), LevelFilter::Trace);
        assert_eq!(logger.level("analyzer_core::graph"), LevelFilter::Debug);
        assert_eq!(logger.level("analyzer_core_extra"), LevelFilter::Error);
        assert_eq!(
            Logger::parse(Some("off"), Some("trace"), true)
                .unwrap()
                .level("analyzer_core"),
            LevelFilter::Off
        );
        assert!(Logger::parse(None, Some("core=unknown"), false).is_err());
    }
}
