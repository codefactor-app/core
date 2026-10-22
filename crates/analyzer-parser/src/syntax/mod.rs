//! Pure syntax and source facts, shared by normalization and optional analyses.
use crate::support::*;
use analyzer_core::ir::*;
use std::collections::{BTreeMap, BTreeSet};
use tree_sitter::Node;
mod control;
pub(crate) use control::*;
mod declarations;
pub(crate) use declarations::*;
mod accessors;
pub(crate) use accessors::*;
mod imports;
pub(crate) use imports::*;
mod lines;
pub(crate) use lines::*;
