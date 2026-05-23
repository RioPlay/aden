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
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DriftEvent {
    StaleHash {
        target_path: String,
        expected_hash: String,
        actual_hash: String,
    },
    SignatureMismatch {
        anchor: String,
        contract_path: String,
        expected_sig: Vec<String>,
        actual_sig: Vec<String>,
    },
    MissingContract {
        source_path: String,
        anchor: String,
        symbol_name: String,
    },
    OrphanAnchor {
        anchor: String,
        contract_path: String,
    },
    BrokenReference {
        contract_path: String,
        ref_anchor: String,
        line: usize,
    },
    DeadLink {
        contract_path: String,
        include_path: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DriftSeverity {
    Critical,
    High,
    Medium,
    Low,
}

impl DriftSeverity {
    pub fn weight(self) -> f64 {
        match self {
            DriftSeverity::Critical => 0.3,
            DriftSeverity::High => 0.2,
            DriftSeverity::Medium => 0.1,
            DriftSeverity::Low => 0.05,
        }
    }
}

impl DriftEvent {
    pub fn severity(&self) -> DriftSeverity {
        match self {
            DriftEvent::StaleHash { .. } => DriftSeverity::High,
            DriftEvent::SignatureMismatch { .. } => DriftSeverity::High,
            DriftEvent::MissingContract { .. } => DriftSeverity::Medium,
            DriftEvent::OrphanAnchor { .. } => DriftSeverity::Medium,
            DriftEvent::BrokenReference { .. } => DriftSeverity::Critical,
            DriftEvent::DeadLink { .. } => DriftSeverity::Low,
        }
    }
}
