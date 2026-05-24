// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// All rights reserved.
//
// Aden: A Dense Referential Context Compiler
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Extraction engine: turns source files into `Vec<Document>`.
//!
//! Architecture: `LanguageRouter` dispatches by extension to typed
//! `LanguageExtractor` implementations.  Deep resolvers (Rust) ship
//! first-class call-graph edges; the generic fallback uses
//! `tree-sitter-language-pack` to cover 305+ languages with symbol
//! extraction only (no call resolution).

mod extractor;
pub mod c_resolver;
pub mod csharp_resolver;
pub mod generic;
pub mod go_resolver;
pub mod java_resolver;
pub mod kotlin_resolver;
mod powershell;
pub mod php_resolver;
pub mod python_resolver;
#[cfg(feature = "rust-deep")]
pub mod rust;
pub mod ruby_resolver;
pub mod router;
pub mod tree_sitter_common;
pub mod typescript_resolver;

pub use extractor::LanguageExtractor;
pub(crate) use extractor::make_anchor;
pub use router::LanguageRouter;

#[cfg(test)]
mod tests;

use aden_core::{Document, Error, Result};
use std::path::Path;

/// Global router instance.  Created lazily so that language pack
/// parsers are fetched on first use.
static ROUTER: std::sync::OnceLock<LanguageRouter> = std::sync::OnceLock::new();

fn get_router() -> &'static LanguageRouter {
    ROUTER.get_or_init(LanguageRouter::new)
}

/// Detect language from file extension and parse into Documents.
pub fn parse_file(path: &Path, source: &str) -> Result<Vec<Document>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    // PowerShell stays on the JSON bridge for now.
    if matches!(ext, "ps1" | "psm1" | "psd1") {
        return powershell::extract_documents(path, source);
    }

    get_router().parse_file(path, source)
}

/// Walk a directory and parse every supported source file.
/// Includes DoS guards: depth limit, file count limit, and size checks.
pub fn parse_directory(dir: &Path) -> Result<Vec<Document>> {
    parse_directory_inner(dir, 0, &mut 0)
}

const MAX_SCAN_DEPTH: usize = 20;
const MAX_FILES_SCANNED: usize = 5_000;
const MAX_FILE_SIZE: u64 = 1024 * 1024; // 1 MB

fn parse_directory_inner(dir: &Path, depth: usize, file_count: &mut usize) -> Result<Vec<Document>> {
    if depth > MAX_SCAN_DEPTH {
        return Ok(Vec::new());
    }
    let mut docs = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| Error::Io(e.to_string()))? {
        let entry = entry.map_err(|e| Error::Io(e.to_string()))?;
        let path = entry.path();

        // SECURITY: Skip symlinks to prevent traversal outside the repo.
        if entry.file_type().map(|ft| ft.is_symlink()).unwrap_or(false) {
            continue;
        }

        if path.is_file() {
            *file_count += 1;
            if *file_count > MAX_FILES_SCANNED {
                eprintln!(
                    "[aden] WARNING: Scan limit reached ({} files). Remaining files skipped. Consider raising limit.",
                    MAX_FILES_SCANNED
                );
                return Ok(docs);
            }
            // Skip files too large to avoid DoS
            if let Ok(meta) = std::fs::metadata(&path) {
                if meta.len() > MAX_FILE_SIZE {
                    eprintln!(
                        "[aden] WARNING: Skipping {} ({} bytes > {} MiB limit). Consider using 'aden gen <file>' instead.",
                        path.display(),
                        meta.len(),
                        MAX_FILE_SIZE / (1024 * 1024)
                    );
                    continue;
                }
            }
            let source = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                    // Non-UTF-8 file (binary blob, encoded text); skip gracefully.
                    continue;
                }
                Err(e) => return Err(Error::Io(e.to_string())),
            };
            match parse_file(&path, &source) {
                Ok(file_docs) => docs.extend(file_docs),
                Err(Error::UnsupportedLanguage(_)) => {
                    // Silently skip files for which we have no extractor.
                }
                Err(e) => return Err(e),
            }
        } else if path.is_dir() {
            docs.extend(parse_directory_inner(&path, depth + 1, file_count)?);
        }
    }
    Ok(docs)
}
