// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod drift;
pub mod fuzzy;
pub mod reconcile;
pub mod report;
pub mod scanner;

pub use drift::{DriftEvent, DriftSeverity};
pub use reconcile::{Reconciliation, reconcile_contract};
pub use report::{HealthReport, generate};
pub use scanner::Scanner;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum HealError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("core error: {0}")]
    Core(#[from] aden_core::Error),
    #[error("graph parse error: {0}")]
    GraphParse(#[from] aden_graph::parser::ParseError),
    #[error("graph error: {0}")]
    Graph(#[from] aden_graph::graph::GraphError),
}
