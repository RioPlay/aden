// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod proposal;
pub mod store;

pub use proposal::{Proposal, ProposalStatus};
pub use store::{list, load, persist};
