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

/// Infer the owning project name for `path` by walking up to the nearest
/// language manifest. Shared by every shallow extractor so a file's anchor is
/// identical no matter which one parses it.
///
/// Critically, this resolves the project name consistently whether `path` is
/// absolute or relative: when the manifest sits at the current directory, the
/// matched ancestor is `"."`/`""` and `Path::file_name()` is `None`, so we
/// canonicalize to recover the real directory name. Without this, `gen`
/// (absolute paths) and the heal scanner (relative paths) produced divergent
/// `aden://module/{project}/…` anchors for the same file (`aden` vs `unknown`),
/// double-flagging every such symbol as MissingContract *and* OrphanAnchor.
pub(crate) fn infer_project_name(path: &Path) -> String {
    path.ancestors()
        .find(|p| {
            p.join("Cargo.toml").exists()
                || p.join("package.json").exists()
                || p.join("pyproject.toml").exists()
                || p.join("setup.py").exists()
                || p.join("go.mod").exists()
                || p.join("tsconfig.json").exists()
        })
        .and_then(dir_name)
        .unwrap_or_else(|| "unknown".to_string())
}

fn dir_name(p: &Path) -> Option<String> {
    if let Some(name) = p.file_name() {
        return Some(name.to_string_lossy().to_string());
    }
    std::fs::canonicalize(p)
        .ok()
        .and_then(|abs| abs.file_name().map(|n| n.to_string_lossy().to_string()))
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

#[cfg(test)]
mod tests {
    use super::infer_project_name;
    use std::fs;

    #[test]
    fn project_name_is_consistent_for_absolute_and_relative_paths() {
        // A manifest-bearing dir; a file whose nearest marker is that dir.
        let base = std::env::temp_dir().join("aden_infer_test_proj");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("sub")).unwrap();
        fs::write(base.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        fs::write(base.join("sub/script.ps1"), "function Foo {}\n").unwrap();

        // Absolute path resolves to the manifest dir's name.
        let abs = base.join("sub/script.ps1");
        let from_abs = infer_project_name(&abs);
        assert_eq!(from_abs, "aden_infer_test_proj");

        // Relative path whose manifest sits at cwd must resolve to the SAME name,
        // not "unknown" (the M10 regression: gen used absolute, heal used relative).
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(&base).unwrap();
        let from_rel = infer_project_name(std::path::Path::new("./sub/script.ps1"));
        std::env::set_current_dir(&prev).unwrap();
        assert_eq!(from_rel, from_abs, "relative path must match absolute");

        let _ = fs::remove_dir_all(&base);
    }
}
