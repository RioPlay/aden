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
