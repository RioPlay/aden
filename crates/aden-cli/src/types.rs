// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Logic version of the gen emission pipeline. The gen cache skips files by
/// mtime, so a change to WHAT gen emits (parser edge macros, linker edge
/// types) would otherwise never reach symbols whose source did not change.
/// Bump this whenever emission logic changes; a stale cache is discarded
/// wholesale and every file reparses once. Same invalidation pattern as
/// `CURRENT_INDEX_VERSION` (aden-index) and `CACHE_LOGIC_VERSION` (aden-heal).
///
/// 2: Wave 1 graph types — `Tests`/`Implements`/`Mutates` emission
///    (graph-type-roadmap). Caches written before this field existed
///    deserialize as version 0 and are invalidated.
/// 3: Prose cross-reference channel — parsers extract `<<target>>`/`xref:`/
///    `[text](#frag)` into the `doc_refs` attribute (`ref:` prefix) and the
///    linker resolves them against doc anchor fragments only. Without the
///    bump, mtime-skipped docs would never re-emit their refs.
/// 4: Wave 3 episodic edges — parsers extract supersede-context refs into the
///    `doc_supersedes` attribute (`Supersedes`); the linker co-emits
///    `Justifies` for ADR-doc mentions and `AssociatedWith` from git
///    co-change. Without the bump, mtime-skipped docs would never re-emit
///    their supersede refs.
pub const GEN_LOGIC_VERSION: u32 = 4;

/// Incremental generation cache: maps contract file path → metadata.
#[derive(Default, Serialize, Deserialize)]
pub struct GenCache {
    /// See [`GEN_LOGIC_VERSION`]. `default` (0) marks pre-versioning caches.
    #[serde(default)]
    pub version: u32,
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
#[derive(Clone, Debug)]
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

impl std::str::FromStr for QueryIntent {
    type Err = String;

    /// Parse an intent from its variant name, case-insensitively. Used by
    /// `aden ask --intent <INTENT>` to bypass automatic classification.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "debug" => Ok(QueryIntent::Debug),
            "usage" => Ok(QueryIntent::Usage),
            "explain" => Ok(QueryIntent::Explain),
            "refactor" => Ok(QueryIntent::Refactor),
            "impact" => Ok(QueryIntent::Impact),
            "list" => Ok(QueryIntent::List),
            "compare" => Ok(QueryIntent::Compare),
            "count" => Ok(QueryIntent::Count),
            "general" => Ok(QueryIntent::General),
            other => Err(format!(
                "invalid intent '{}'. Valid: debug, usage, explain, refactor, impact, list, compare, count, general",
                other
            )),
        }
    }
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

/// A single OWASP-aligned finding.
#[derive(Debug)]
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

/// A thresholded co-change pair: each side is `(file-level anchor, repo-
/// relative source file)`. The file paths let the linker synthesize the
/// file-level node when the store has none (symbols hang off `mod-<crate>`
/// hubs; the file grain only exists where co-change demands it).
pub type CochangePair = ((String, String), (String, String));

impl AnchorPattern {
    /// True when the anchor lives in the prose-document scheme (`aden://doc/…`):
    /// a heading, declared `[[anchor]]`, or section of a Markdown/AsciiDoc/…
    /// document. These carry a `#fragment` just like code symbols, so the bare
    /// `contains('#')` Symbol test cannot distinguish them — yet routing,
    /// thin-stub handling, and overview questions all need to. Keyed purely on
    /// the anchor SCHEME (never filenames or formats), so it is polyglot by
    /// construction: anything the parsers emit into the doc scheme qualifies.
    ///
    /// Deliberately a parallel predicate rather than a new `from_anchor`
    /// variant: re-classifying `aden://doc/…#x` away from `Symbol` would change
    /// the structural tiebreak for every existing query; this predicate lets
    /// the conceptual-routing path discriminate docs without perturbing the
    /// default selection order.
    pub fn is_prose_doc(anchor: &str) -> bool {
        anchor.starts_with("aden://doc/")
    }

    /// Tiebreaker score. Only used when search scores are within noise range.
    /// Symbol beats Module: a function-level answer is more useful than a
    /// module index page when both score similarly.
    pub fn tiebreak(&self) -> i32 {
        match self {
            AnchorPattern::Symbol => 100,
            AnchorPattern::Adr => 80,
            AnchorPattern::Plan => 70,
            AnchorPattern::UseCase => 60,
            AnchorPattern::Module => 50,
            AnchorPattern::Agent => 40,
            AnchorPattern::Generic => 30,
            AnchorPattern::Readme => 10,
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
