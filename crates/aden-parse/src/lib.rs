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
//! Extraction engine: turns source files into `Vec<Document>`.
//!
//! Supports Rust (via tree-sitter) and PowerShell (via JSON bridge).

mod powershell;
#[cfg(feature = "rust-parser")]
mod rust;

use aden_core::{Document, Error, Result};
use std::path::Path;

/// Detect language from file extension and parse into Documents.
pub fn parse_file(path: &Path, source: &str) -> Result<Vec<Document>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    match ext {
        #[cfg(feature = "rust-parser")]
        "rs" => rust::extract_documents(path, source),
        "ps1" | "psm1" | "psd1" => powershell::extract_documents(path, source),
        _ => Err(Error::UnsupportedLanguage(ext.to_string())),
    }
}

/// Walk a directory and parse every supported source file.
pub fn parse_directory(dir: &Path) -> Result<Vec<Document>> {
    let mut docs = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| Error::Io(e.to_string()))? {
        let entry = entry.map_err(|e| Error::Io(e.to_string()))?;
        let path = entry.path();
        if path.is_file() {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            #[allow(unreachable_patterns)]
            if matches!(ext, "ps1" | "psm1" | "psd1") || cfg!(feature = "rust-parser") && ext == "rs" {
                let source = std::fs::read_to_string(&path)
                    .map_err(|e| Error::Io(e.to_string()))?;
                docs.extend(parse_file(&path, &source)?);
            }
        } else if path.is_dir() {
            docs.extend(parse_directory(&path)?);
        }
    }
    Ok(docs)
}

/// Generate an anchor from crate/file/symbol.
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
    attrs.insert("source_hash".to_string(), aden_core::stable_hash(source.as_bytes()));
    attrs.insert("last-verified".to_string(), aden_core::rfc3339_now());
    attrs.insert("node-type".to_string(), node_type.to_string());
    if let Some(path) = source_file {
        attrs.insert("source_file".to_string(), path.to_string_lossy().to_string());
    }
    if let Some(s) = span {
        attrs.insert("start_line".to_string(), s.start_line.to_string());
        attrs.insert("end_line".to_string(), s.end_line.to_string());
        attrs.insert("start_byte".to_string(), s.start_byte.to_string());
        attrs.insert("end_byte".to_string(), s.end_byte.to_string());
    }
    attrs
}
