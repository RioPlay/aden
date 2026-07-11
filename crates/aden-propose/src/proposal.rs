// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    pub id: String,
    pub target_path: PathBuf,
    pub drift_type: String,
    pub confidence: f64,
    pub status: ProposalStatus,
    pub rationale: String,
    pub patch_asciidoc: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProposalStatus {
    PendingReview,
    Approved,
    Rejected,
    Applied,
}

impl fmt::Display for ProposalStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProposalStatus::PendingReview => write!(f, "PENDING_REVIEW"),
            ProposalStatus::Approved => write!(f, "APPROVED"),
            ProposalStatus::Rejected => write!(f, "REJECTED"),
            ProposalStatus::Applied => write!(f, "APPLIED"),
        }
    }
}

impl FromStr for ProposalStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "PENDING_REVIEW" => Ok(ProposalStatus::PendingReview),
            "APPROVED" => Ok(ProposalStatus::Approved),
            "REJECTED" => Ok(ProposalStatus::Rejected),
            "APPLIED" => Ok(ProposalStatus::Applied),
            other => Err(format!("unknown ProposalStatus: {}", other)),
        }
    }
}
