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
//! Universal schema for the Aden referential context compiler.
//!
//! This crate defines the knowledge graph types used across all other crates.
//! It has no dependencies on parsing, I/O, or emission logic.

pub mod filter;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// Compute a stable non-cryptographic hash for change-detection.
pub fn stable_hash(bytes: &[u8]) -> String {
    let mut hasher = fnv::FnvHasher::default();
    bytes.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
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
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", year, month, day, hour, minute, second)
}

fn days_to_ymd(mut days: u64) -> (u32, u32, u32) {
    // Algorithm from https://howardhinnant.github.io/date_algorithms.html
    days += 719468; // shift epoch from 1970-01-01 to 0000-03-01
    let era = days / 146097;
    let day_of_era = days % 146097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
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
        let start_byte = attrs.get("start_byte").and_then(|s| s.parse().ok()).unwrap_or(0);
        let end_byte = attrs.get("end_byte").and_then(|s| s.parse().ok()).unwrap_or(0);
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
                self.attributes.insert(key.clone(), "[REDACTED]".to_string());
            }
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
            _ => Err(format!("Unknown profile mode: {} (expected: internal, public)", s)),
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProfileConfig {
    pub mode: ProfileMode,
    /// Attribute keys to redact when mode == Public.
    #[serde(default = "default_redact_fields")]
    pub redact_fields: Vec<String>,
    /// Directories that are only visible in Internal mode.
    #[serde(default = "default_private_dirs")]
    pub private_dirs: Vec<String>,
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
        if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| toml::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            Self::default()
        }
    }

    /// Is the given path inside a private directory?
    pub fn is_private(&self, path: &std::path::Path) -> bool {
        if self.profile.mode == ProfileMode::Internal {
            return false;
        }
        let path_str = path.to_string_lossy();
        self.profile.private_dirs.iter().any(|d| path_str.contains(d))
    }
}

/// The kind of node a Document represents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum NodeType {
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
    Listing { language: Option<String>, code: String },
    Admonition { kind: AdmonitionKind, text: String },
    DescriptionList(Vec<(String, String)>),
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
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum EdgeType {
    Uses,
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
