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
pub mod patch;
pub mod proposal;
pub mod store;
pub mod stub;

pub use patch::{generate_patch, DriftEvent as PatchDriftEvent};
pub use proposal::{Proposal, ProposalStatus};
pub use store::{apply, list, load, persist};
pub use stub::{generate_stub, write_stub};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProposeError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("generation failed: {0}")]
    Generation(String),
    #[error("invalid data: {0}")]
    InvalidData(String),
}
