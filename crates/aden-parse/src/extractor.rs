// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! The `LanguageExtractor` trait abstracts over per-language AST traversal.
//!
//! Any language — deep or shallow — implements this trait so that `aden gen`
//! and `aden parse_directory` can remain language-agnostic.

use aden_core::{Document, Result};
use std::path::Path;

/// A language-specific extraction engine.
///
/// Implementors parse source text into a tree-sitter AST (or other
/// representation) and emit Aden `Document` records.  Deep resolvers
/// (e.g. Rust) additionally produce call-graph edges; shallow/generic
/// extractors may return no edges and only populate symbols.
pub trait LanguageExtractor: Send + Sync {
    /// Canonical language name (e.g. `"rust"`, `"python"`).
    fn language_id(&self) -> &'static str;

    /// File extensions this extractor handles, *without* the leading dot.
    fn file_extensions(&self) -> &'static [&'static str];

    /// Parse `source` at `path` and emit zero or more `Document`s.
    fn extract_documents(&self, source: &str, path: &Path) -> Result<Vec<Document>>;
}

/// Build a safe anchor fragment from crate/file/symbol components.
pub(crate) fn make_anchor(crate_name: &str, file_name: &str, symbol: &str) -> String {
    format!("aden://module/{crate_name}/{file_name}#{symbol}")
}

/// Build mandatory attributes for a code-emitted Document, optionally including source span.
pub(crate) fn build_code_attributes(
    source: &str,
    node_type: &str,
    source_file: Option<&std::path::Path>,
    span: Option<&aden_core::SourceSpan>,
) -> std::collections::HashMap<String, String> {
    let mut attrs = std::collections::HashMap::new();

    let hash_source = if let Some(path) = source_file {
        // Try to read file, skip on binary/invalid UTF-8

        std::fs::read_to_string(path).unwrap_or_else(|e| {
            if e.kind() == std::io::ErrorKind::InvalidData {
                String::new() // Binary file - use empty for hash
            } else {
                source.to_string() // Other errors - fall back to provided source
            }
        })
    } else {
        source.to_string()
    };
    attrs.insert(
        "source_hash".to_string(),
        aden_core::hash_source(&hash_source),
    );
    attrs.insert("last-verified".to_string(), aden_core::rfc3339_now());
    attrs.insert("node-type".to_string(), node_type.to_string());
    if let Some(path) = source_file {
        attrs.insert(
            "source_file".to_string(),
            path.to_string_lossy().to_string(),
        );
    }
    if let Some(s) = span {
        attrs.insert("start_line".to_string(), s.start_line.to_string());
        attrs.insert("end_line".to_string(), s.end_line.to_string());
        attrs.insert("start_byte".to_string(), s.start_byte.to_string());
        attrs.insert("end_byte".to_string(), s.end_byte.to_string());
    }
    attrs
}
