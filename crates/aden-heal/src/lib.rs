// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// All rights reserved.
//
// Aden: A Dense Referential Context Compiler
// Original author and maintainer: RioPlay
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
pub mod drift;
pub mod fuzzy;
pub mod report;
pub mod scanner;

pub use drift::{DriftEvent, DriftSeverity};
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
