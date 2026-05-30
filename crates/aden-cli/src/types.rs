// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Incremental generation cache: maps contract file path → metadata.
#[derive(Default, Serialize, Deserialize)]
pub struct GenCache {
    pub entries: std::collections::HashMap<String, GenCacheEntry>,
}

#[derive(Serialize, Deserialize)]
pub struct GenCacheEntry {
    pub source_mtime: u64,
    pub source_path: String,
    /// Anchors this source file contributed to the store on its last index.
    /// Used to prune deleted symbols: on re-index, any previously-recorded
    /// anchor absent from the fresh parse is stale and gets removed. Defaults
    /// to empty for caches written before this field existed (back-compat).
    #[serde(default)]
    pub anchors: Vec<String>,
}

/// Intent classification for natural-language queries.
#[derive(Debug)]
pub enum QueryIntent {
    Debug,    // "Why does X fail?"
    Usage,    // "How do I use X?"
    Explain,  // "What does X do?"
    Refactor, // "Refactor X"
    Impact,   // "What depends on X?"
    List,     // "list all modules", "show me all functions"
    Compare,  // "compare X and Y"
    Count,    // "how many tests", "count the functions"
    General,  // default
}

/// Severity of an OWASP finding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum OwaspSeverity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

impl std::fmt::Display for OwaspSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OwaspSeverity::Info => write!(f, "INFO"),
            OwaspSeverity::Low => write!(f, "LOW"),
            OwaspSeverity::Medium => write!(f, "MED"),
            OwaspSeverity::High => write!(f, "HIGH"),
            OwaspSeverity::Critical => write!(f, "CRIT"),
        }
    }
}

/// A single OWASP-style finding.
pub struct OwaspFinding {
    pub owasp_id: &'static str,
    pub category: &'static str,
    pub severity: OwaspSeverity,
    pub file: PathBuf,
    pub line: usize,
    pub snippet: String,
    pub description: &'static str,
    pub remediation: &'static str,
}

/// Anchor pattern classification used as a secondary tiebreaker during routing.
///
/// Priorities intentionally treat symbol anchors (`aden://...#symbol`) as
/// higher-value than module index pages when the search score is comparable.
/// The search index score is the primary signal; this is only used to break
/// ties between equal-scoring results of different structural types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnchorPattern {
    /// Concrete symbol: function, struct, enum, method (aden://module/...#name)
    Symbol,
    /// Architecture decision record (adr-*)
    Adr,
    /// Planning document (plan-*)
    Plan,
    /// Use-case document (use-case-*)
    UseCase,
    /// Module index page (mod-*)
    Module,
    /// Agent / onboarding document (agent-*)
    Agent,
    /// Top-level readme / landing page
    Readme,
    /// Anything else
    Generic,
}

impl AnchorPattern {
    /// Tiebreaker score. Only used when search scores are within noise range.
    /// Symbol beats Module: a function-level answer is more useful than a
    /// module index page when both score similarly.
    pub fn tiebreak(&self) -> i32 {
        match self {
            AnchorPattern::Symbol   => 100,
            AnchorPattern::Adr      => 80,
            AnchorPattern::Plan     => 70,
            AnchorPattern::UseCase  => 60,
            AnchorPattern::Module   => 50,
            AnchorPattern::Agent    => 40,
            AnchorPattern::Generic  => 30,
            AnchorPattern::Readme   => 10,
        }
    }

    pub fn from_anchor(anchor: &str) -> Self {
        if anchor.contains('#') {
            AnchorPattern::Symbol
        } else if anchor.starts_with("adr-") {
            AnchorPattern::Adr
        } else if anchor.starts_with("plan-") {
            AnchorPattern::Plan
        } else if anchor.starts_with("use-case-") {
            AnchorPattern::UseCase
        } else if anchor.starts_with("mod-") {
            AnchorPattern::Module
        } else if anchor.starts_with("agent-") {
            AnchorPattern::Agent
        } else if anchor == "readme" {
            AnchorPattern::Readme
        } else {
            AnchorPattern::Generic
        }
    }
}
