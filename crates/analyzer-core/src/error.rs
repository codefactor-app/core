//! Errors shared by public entry points and the scan pipeline.
#[derive(Debug, thiserror::Error)]
pub enum AnalysisError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Config(String),
}
