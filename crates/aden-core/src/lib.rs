// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Universal schema for the Aden referential context compiler.
//!
//! This crate defines the knowledge graph types used across all other crates.
//! It has no dependencies on parsing, I/O, or emission logic.

pub mod contract;
pub mod filter;
pub mod overlay;
pub mod staging;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// Compute a stable non-cryptographic hash for change-detection.
pub fn stable_hash(bytes: &[u8]) -> String {
    let mut hasher = fnv::FnvHasher::default();
    bytes.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Compute a `stable_hash` of source text with line endings normalized to LF.
///
/// Source `source_hash` values are written at gen time and re-checked at heal/
/// check time. Without normalization a file checked out with CRLF on Windows
/// hashes differently from the same LF file on Linux, producing spurious
/// `StaleHash` drift on every Windows checkout. Both the writer
/// (`build_code_attributes`) and the reader (heal `Scanner`) MUST route source
/// text through this function so the hashes agree across platforms.
pub fn hash_source(text: &str) -> String {
    // Normalize CRLF and lone CR to LF, then hash.
    let normalized = if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text.to_string()
    };
    stable_hash(normalized.as_bytes())
}

/// Current UTC time formatted as RFC 3339 (`YYYY-MM-DDTHH:MM:SSZ`).
pub fn rfc3339_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock must be after epoch")
        .as_secs();
    rfc3339_from_secs(secs)
}

fn rfc3339_from_secs(secs: u64) -> String {
    let seconds_in_day = (secs % 86400) as u32;
    let hour = seconds_in_day / 3600;
    let minute = (seconds_in_day % 3600) / 60;
    let second = seconds_in_day % 60;
    let (year, month, day) = days_to_ymd(secs / 86400);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hour, minute, second
    )
}

fn days_to_ymd(mut days: u64) -> (u32, u32, u32) {
    // Algorithm from https://howardhinnant.github.io/date_algorithms.html
    days += 719468; // shift epoch from 1970-01-01 to 0000-03-01
    let era = days / 146097;
    let day_of_era = days % 146097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let mut y = (year_of_era + era * 400) as i64;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let d = day_of_year - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }
    (y as u32, m as u32, d as u32)
}

/// Unified error type for the Aden ecosystem.
#[derive(Debug, thiserror::Error, Serialize, Clone)]
pub enum Error {
    #[error("invalid anchor: {0}")]
    InvalidAnchor(String),
    #[error("missing mandatory attribute: {0}")]
    MissingAttribute(String),
    #[error("edge reference unresolved: {0} -> {1}")]
    UnresolvedEdge(String, String),
    #[error("IO error: {0}")]
    Io(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("unsupported language: {0}")]
    UnsupportedLanguage(String),
    #[error("generic: {0}")]
    Generic(String),
}

/// Result alias for aden-core operations.
pub type Result<T> = std::result::Result<T, Error>;

/// File location within source text: 1-based line range, 0-based byte offsets.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceSpan {
    /// Absolute path to the source file.
    pub file: String,
    /// 1-based start line (inclusive).
    pub start_line: usize,
    /// 1-based end line (inclusive). May equal `start_line` for single-line spans.
    pub end_line: usize,
    /// 0-based start byte offset in the file.
    pub start_byte: usize,
    /// 0-based end byte offset (exclusive).
    pub end_byte: usize,
}

impl SourceSpan {
    /// Build a `:start_line:` / `:end_line:` attribute pair for emission.
    pub fn to_attributes(&self) -> HashMap<String, String> {
        let mut attrs = HashMap::new();
        attrs.insert("source_file".to_string(), self.file.clone());
        attrs.insert("start_line".to_string(), self.start_line.to_string());
        attrs.insert("end_line".to_string(), self.end_line.to_string());
        attrs
    }

    /// Parse back from attributes emitted by `to_attributes`.
    pub fn from_attributes(attrs: &HashMap<String, String>) -> Option<Self> {
        let file = attrs.get("source_file")?.clone();
        let start_line = attrs.get("start_line")?.parse().ok()?;
        let end_line = attrs.get("end_line")?.parse().ok()?;
        let start_byte = attrs
            .get("start_byte")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let end_byte = attrs
            .get("end_byte")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        Some(Self {
            file,
            start_line,
            end_line,
            start_byte,
            end_byte,
        })
    }

    /// Human-readable location string: `file.rs:42` or `file.rs:42–45`.
    pub fn display_location(&self) -> String {
        if self.start_line == self.end_line {
            format!("{}:{}", self.file, self.start_line)
        } else {
            format!("{}:{}–{}", self.file, self.start_line, self.end_line)
        }
    }
}

/// A node in the Aden knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Document {
    /// Globally unique anchor. Must match `^[a-z0-9_-]+$`.
    pub anchor: String,
    /// Semantic node type.
    pub node_type: NodeType,
    /// AsciiDoc-style `:key: value` metadata.
    pub attributes: HashMap<String, String>,
    /// Content blocks.
    pub blocks: Vec<Block>,
    /// Optional precise source location for this document.
    pub source_span: Option<SourceSpan>,
    /// Document-level metadata.
    pub metadata: Option<DocumentMetadata>,
    /// Confidence in this document's claims (0.0-1.0).
    pub confidence: f64,
}

/// Document-level metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DocumentMetadata {
    pub author: Option<String>,
    pub email: Option<String>,
    pub revision: Option<String>,
    pub version: Option<String>,
    pub date: Option<String>,
    pub copyright: Option<String>,
    pub license: Option<String>,
}

impl Document {
    /// Merge source-span attributes into the attribute map.
    pub fn with_span(mut self, span: &SourceSpan) -> Self {
        for (k, v) in span.to_attributes() {
            self.attributes.insert(k, v);
        }
        self.source_span = Some(span.clone());
        self
    }

    /// Strip/redact sensitive attributes according to profile.
    pub fn sanitize(&mut self, config: &AdenConfig) {
        if config.profile.mode == ProfileMode::Internal {
            return;
        }
        for key in &config.profile.redact_fields {
            if self.attributes.contains_key(key) {
                self.attributes
                    .insert(key.clone(), "[REDACTED]".to_string());
            }
        }
    }

    /// Mark as self-reference if source path matches any pattern in config.
    /// Self-references get low confidence (0.1) to prevent self-bias.
    pub fn mark_self_reference(&mut self, config: &AdenConfig) {
        let source_file = match self.attributes.get("source_file") {
            Some(s) => s,
            None => return,
        };
        let is_self = config
            .profile
            .self_reference_patterns
            .iter()
            .any(|pattern| source_file.contains(pattern));
        if is_self {
            self.confidence = 0.1;
        }
    }
}

/// Runtime profile for Aden: controls what is emitted vs redacted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum ProfileMode {
    /// Internal development — full fidelity, all contracts visible.
    #[default]
    Internal,
    /// Public-facing — sensitive fields redacted, private contracts hidden.
    Public,
}

impl std::str::FromStr for ProfileMode {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "internal" | "dev" | "developer" => Ok(ProfileMode::Internal),
            "public" | "open" | "external" => Ok(ProfileMode::Public),
            _ => Err(format!(
                "Unknown profile mode: {} (expected: internal, public)",
                s
            )),
        }
    }
}

/// Aden configuration loaded from `aden.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AdenConfig {
    /// Profile controls redaction and private-directory visibility.
    #[serde(default)]
    pub profile: ProfileConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileConfig {
    pub mode: ProfileMode,
    /// Attribute keys to redact when mode == Public.
    #[serde(default = "default_redact_fields")]
    pub redact_fields: Vec<String>,
    /// Directories that are only visible in Internal mode.
    #[serde(default = "default_private_dirs")]
    pub private_dirs: Vec<String>,
    /// Document anchor patterns that are private (proposals, not implementation).
    /// ADRs, retrospectives, kickoffs, research notes are proposals - not reality.
    #[serde(default = "default_private_patterns")]
    pub private_patterns: Vec<String>,
    /// Patterns for the tool's own files. Documents matching these get low confidence (0.1)
    /// to prevent self-bias when the tool is used as a general-purpose knowledge compiler.
    #[serde(default = "default_self_reference_patterns")]
    pub self_reference_patterns: Vec<String>,
}

impl Default for ProfileConfig {
    fn default() -> Self {
        Self {
            mode: ProfileMode::default(),
            redact_fields: default_redact_fields(),
            private_dirs: default_private_dirs(),
            private_patterns: default_private_patterns(),
            self_reference_patterns: default_self_reference_patterns(),
        }
    }
}

fn default_private_patterns() -> Vec<String> {
    vec![
        "adr-*".to_string(),      // Architecture Decision Records
        "retro-*".to_string(),    // Incident retrospectives
        "kickoff-*".to_string(),  // Project kickoffs
        "research-*".to_string(), // Research notes
        "design-*".to_string(),   // Pre-implementation designs
    ]
}

fn default_self_reference_patterns() -> Vec<String> {
    vec![
        ".aden/contracts/".to_string(),
        ".aden/proposals/".to_string(),
        ".agent/".to_string(),
    ]
}

fn default_redact_fields() -> Vec<String> {
    vec![
        "source_file".to_string(),
        "author_email".to_string(),
        "author".to_string(),
        "commit".to_string(),
        "internal_url".to_string(),
    ]
}

fn default_private_dirs() -> Vec<String> {
    vec![
        ".aden/private".to_string(),
        ".ci".to_string(),
        "tools/internal".to_string(),
    ]
}

impl AdenConfig {
    /// Load `aden.toml` from the given directory, or return defaults if absent.
    pub fn load(dir: &std::path::Path) -> Self {
        let path = dir.join("aden.toml");
        if path.exists()
            && let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(config) = toml::from_str::<AdenConfig>(&text)
        {
            return config;
        }
        Self::default()
    }

    /// Is the given path inside a private directory?
    pub fn is_private(&self, path: &std::path::Path) -> bool {
        if self.profile.mode == ProfileMode::Internal {
            return false;
        }
        let path_str = path.to_string_lossy();
        // Check private directories
        if self
            .profile
            .private_dirs
            .iter()
            .any(|d| path_str.contains(d))
        {
            return true;
        }
        // Check private patterns (ADRs, retros, kickoffs, etc.)
        // These are proposals, not implementation - don't leak to public
        for pattern in &self.profile.private_patterns {
            if glob_match(&path_str, pattern) {
                return true;
            }
        }
        false
    }

    /// Is this anchor a private pattern (ADR, retro, etc.)?
    pub fn is_private_anchor(&self, anchor: &str) -> bool {
        if self.profile.mode == ProfileMode::Internal {
            return false;
        }
        for pattern in &self.profile.private_patterns {
            if glob_match(anchor, pattern) {
                return true;
            }
        }
        false
    }
}

fn glob_match(text: &str, pattern: &str) -> bool {
    // Simple glob matching: adr-* matches adr-001, adr-005, etc.
    if let Some(star_pos) = pattern.find('*') {
        let prefix = &pattern[..star_pos];
        let suffix = &pattern[star_pos + 1..];
        text.starts_with(prefix) && (suffix.is_empty() || text.ends_with(suffix))
    } else {
        text == pattern
    }
}

/// The kind of node a Document represents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum NodeType {
    #[default]
    Module,
    Function,
    Type,
    Script,
    Adr,
    Runbook,
    Plan,
    Context,
    Manifest,
    Spec,
    Note,
}

/// A content block inside a Document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Block {
    Table(Table),
    Paragraph(String),
    Listing {
        language: Option<String>,
        code: String,
    },
    Admonition {
        kind: AdmonitionKind,
        text: String,
    },
    DescriptionList(Vec<(String, String)>),
    Checklist(Vec<ChecklistItem>),
    /// A block that requires human/AI completion before the contract is valid.
    /// The required_fields indicate what needs to be filled in.
    /// When this block has empty content, it signals the contract needs completion.
    Incomplete {
        required_fields: Vec<String>,
        hint: String,
    },
}

/// A checklist item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChecklistItem {
    pub checked: bool,
    pub text: String,
}

/// Kinds of admonition blocks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AdmonitionKind {
    Note,
    Tip,
    Warning,
    Important,
    Caution,
}

/// A table block with optional header row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Table {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

/// A named entity extracted from source code.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub visibility: Visibility,
    pub doc_comment: Option<String>,
    /// Optional precise source location for this symbol.
    pub source_span: Option<SourceSpan>,
}

/// Classification of a Symbol.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SymbolKind {
    Function,
    Struct,
    Enum,
    Trait,
    Impl,
    Module,
    Field,
    Variant,
}

/// Visibility qualifier.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Visibility {
    Public,
    Crate,
    Private,
    Super,
}

/// Function signature metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionSig {
    pub name: String,
    pub parameters: Vec<Parameter>,
    pub return_type: Option<String>,
    pub visibility: Visibility,
    pub is_async: bool,
    pub is_unsafe: bool,
}

/// A single parameter.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Parameter {
    pub name: String,
    pub ty: String,
    pub default_value: Option<String>,
}

/// Type definition metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TypeDef {
    pub name: String,
    pub kind: TypeKind,
    pub fields: Vec<FieldDef>,
    pub trait_bounds: Vec<String>,
}

/// Classification of a type definition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TypeKind {
    Struct,
    Enum,
    Union,
}

/// Field inside a type definition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FieldDef {
    pub name: String,
    pub ty: String,
    pub visibility: Visibility,
}

/// Semantic contract describing invariants, side effects, and error behavior.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Contract {
    pub name: String,
    pub invariants: Vec<String>,
    pub side_effects: Vec<String>,
    pub error_conditions: Vec<String>,
}

/// Typed edge between two Documents, optionally carrying a call-site location.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Edge {
    pub source_anchor: String,
    pub target_anchor: String,
    pub edge_type: EdgeType,
    /// Optional precise source location where this edge originates (e.g. a call site).
    pub source_span: Option<SourceSpan>,
}

/// Strict enumeration of edge types.
/// Code edges: structural relationships in source code.
/// Semantic edges: conceptual relationships (brain-like networks).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum EdgeType {
    // === Code Edges (structural) ===
    Uses,
    /// Inverse of Uses: target is pointed to by source.
    UsedBy,
    Implements,
    Tests,
    Documents,
    Constrains,
    Justifies,
    Invokes,
    Requires,
    Mutates,
    Calls,
    Supersedes,
    Amends,
    Verifies,
    // === Semantic Edges (conceptual/brain-like) ===
    /// Inheritance/subsumption (dog IS-A animal)
    IsA,
    /// Structural composition (wheel PART-OF car)
    PartOf,
    /// General relatedness (bidirectional feel)
    RelatesTo,
    /// Analogical relationship (similarTo)
    SimilarTo,
    /// Direct causation (cause EFFECT)
    Causes,
    /// Logical entailment (premise IMPLIES conclusion)
    Implies,
    /// Alternate naming (synonym)
    SynonymOf,
    /// Opposite concept (antonym)
    AntonymOf,
    /// Hebbian co-activation (frequently co-occur)
    AssociatedWith,
    /// Learning sequence (must-know before)
    PrerequisiteFor,
    /// Provides explanation for
    Explains,
    /// Deterministic equivalence (May IS-EQUIVALENT-TO 5, midnight IS-EQUIVALENT-TO 00:00)
    IsEquivalentTo,
}

impl EdgeType {
    /// Returns true if this is a semantic (conceptual) edge type.
    pub fn is_semantic(&self) -> bool {
        matches!(
            self,
            EdgeType::IsA
                | EdgeType::PartOf
                | EdgeType::RelatesTo
                | EdgeType::SimilarTo
                | EdgeType::Causes
                | EdgeType::Implies
                | EdgeType::SynonymOf
                | EdgeType::AntonymOf
                | EdgeType::AssociatedWith
                | EdgeType::PrerequisiteFor
                | EdgeType::Explains
                | EdgeType::IsEquivalentTo
        )
    }

    /// Returns true if this is a code (structural) edge type.
    pub fn is_code(&self) -> bool {
        !self.is_semantic()
    }

    /// Weight for spreading activation (0.0-1.0).
    pub fn activation_weight(&self) -> f64 {
        match self {
            EdgeType::IsA => 1.0,
            EdgeType::PartOf => 0.9,
            EdgeType::Causes => 0.9,
            EdgeType::SimilarTo => 0.7,
            EdgeType::PrerequisiteFor => 0.7,
            EdgeType::Implies => 0.7,
            EdgeType::RelatesTo => 0.5,
            EdgeType::AssociatedWith => 0.5,
            EdgeType::Explains => 0.5,
            EdgeType::IsEquivalentTo => 0.95,
            EdgeType::SynonymOf => 0.8,
            EdgeType::AntonymOf => 0.4,
            // Code edges get medium weight in semantic context
            EdgeType::Uses => 0.8,
            EdgeType::UsedBy => 0.8,
            EdgeType::Calls => 0.8,
            EdgeType::Implements => 0.8,
            EdgeType::Tests => 0.6,
            EdgeType::Verifies => 0.6,
            EdgeType::Documents => 0.5,
            EdgeType::Constrains => 0.7,
            EdgeType::Justifies => 0.5,
            EdgeType::Invokes => 0.7,
            EdgeType::Requires => 0.6,
            EdgeType::Mutates => 0.6,
            EdgeType::Supersedes => 0.5,
            EdgeType::Amends => 0.5,
        }
    }
}

// ── Accreditation ────────────────────────────────────────

/// Third-party dependency and its license metadata.
/// Used by `aden licenses` to generate attribution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LicenseInfo {
    pub name: String,
    pub version: String,
    pub license: Option<String>,
    pub repository: Option<String>,
    pub authors: Option<String>,
}

/// A machine-readable notice of all third-party dependencies.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ThirdPartyNotice {
    pub top_level: Vec<LicenseInfo>,
    pub transitive: Vec<LicenseInfo>,
}

impl ThirdPartyNotice {
    /// Render as Markdown — suitable for `NOTICE.md` or `THIRDPARTY.md`.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# Third-Party Dependencies\n\n");
        out.push_str("This project uses the following open-source packages.\n");
        out.push_str("Generated by `aden licenses`.\n\n");

        if !self.top_level.is_empty() {
            out.push_str("## Direct Dependencies\n\n");
            out.push_str("| Package | Version | License |\n");
            out.push_str("|---------|---------|---------|\n");
            for dep in &self.top_level {
                let lic = dep.license.as_deref().unwrap_or("Unknown");
                out.push_str(&format!("| {} | {} | {} |\n", dep.name, dep.version, lic));
            }
            out.push('\n');
        }

        if !self.transitive.is_empty() {
            out.push_str("## Transitive Dependencies (selected)\n\n");
            out.push_str("| Package | Version | License |\n");
            out.push_str("|---------|---------|---------|\n");
            for dep in &self.transitive {
                let lic = dep.license.as_deref().unwrap_or("Unknown");
                out.push_str(&format!("| {} | {} | {} |\n", dep.name, dep.version, lic));
            }
            out.push('\n');
        }

        out.push_str("## License Notices\n\n");
        out.push_str("For full transitive dependency details, see `Cargo.lock`.\n");
        out.push_str("License texts are available in the respective package repositories.\n\n");
        out.push_str("---\n");
        out.push_str("Generated by Aden.\n");
        out
    }
}

#[cfg(test)]
mod crlf_tests {
    use super::*;

    #[test]
    fn hash_source_is_line_ending_agnostic() {
        // The same logical content with LF, CRLF, and CR must hash identically,
        // so a Windows CRLF checkout does not falsely drift against an LF contract.
        let lf = "fn main() {\n    let x = 1;\n}\n";
        let crlf = "fn main() {\r\n    let x = 1;\r\n}\r\n";
        let cr = "fn main() {\r    let x = 1;\r}\r";
        assert_eq!(hash_source(lf), hash_source(crlf));
        assert_eq!(hash_source(lf), hash_source(cr));
    }

    #[test]
    fn hash_source_still_detects_real_changes() {
        assert_ne!(hash_source("fn a() {}"), hash_source("fn b() {}"));
    }
}
