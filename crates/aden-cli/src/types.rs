use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Incremental generation cache: maps contract file path → metadata.
#[derive(Default, Serialize, Deserialize)]
pub struct GenCache {
    pub entries: HashMap<String, GenCacheEntry>,
}

#[derive(Serialize, Deserialize)]
pub struct GenCacheEntry {
    pub source_mtime: u64,
    pub source_path: String,
}

/// Intent classification for natural-language queries.
#[derive(Debug)]
pub enum QueryIntent {
    Debug,    // "Why does X fail?"
    Usage,    // "How do I use X?"
    Explain,  // "What does X do?"
    Refactor, // "Refactor X"
    Impact,   // "What depends on X?"
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
            OwaspSeverity::Info    => write!(f, "INFO"),
            OwaspSeverity::Low     => write!(f, "LOW"),
            OwaspSeverity::Medium  => write!(f, "MED"),
            OwaspSeverity::High    => write!(f, "HIGH"),
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
