//! Public report facade. Models and execution have separate dependency directions.
pub mod diff;
pub(crate) mod model;
pub use crate::pipeline::{scan, scan_with_baseline};
pub use diff::{apply_changes, diff, ChangeSummary, FindingCounts, PreviousReport};
pub use model::*;
