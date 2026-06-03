// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod patch;
pub mod proposal;
pub mod store;
pub mod stub;

pub use patch::{DriftEvent as PatchDriftEvent, generate_patch};
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
