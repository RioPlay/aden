// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

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
    MarkdownDrift {
        md_path: String,
        expected_content: String,
        actual_content: String,
    },
    StaleMarkdown {
        md_path: String,
        source_files_changed: Vec<String>,
    },
    MissingMarkdownTemplate {
        md_path: String,
        template_source: String,
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
            DriftEvent::MarkdownDrift { .. } => DriftSeverity::High,
            DriftEvent::StaleMarkdown { .. } => DriftSeverity::Medium,
            DriftEvent::MissingMarkdownTemplate { .. } => DriftSeverity::Medium,
        }
    }
}
