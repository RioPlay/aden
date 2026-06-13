// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Extraction engine: turns source files into `Vec<Document>`.
//!
//! Architecture: `LanguageRouter` dispatches by extension to typed
//! `LanguageExtractor` implementations.  Deep resolvers (Rust) ship
//! first-class call-graph edges; the generic fallback uses
//! `tree-sitter-language-pack` to cover 305+ languages with symbol
//! extraction only (no call resolution).

pub mod asciidoc;
pub mod c_resolver;
pub mod csharp_resolver;
pub mod csv;
mod extractor;
pub mod generic;
pub mod go_resolver;
pub mod java_resolver;
pub mod kotlin_resolver;
pub mod markdown;
pub mod php_resolver;
pub mod plaintext;
pub mod python_resolver;
pub mod router;
pub mod ruby_resolver;
pub mod rust;
pub mod tree_sitter_common;
pub mod typescript_resolver;

pub use extractor::LanguageExtractor;
pub use router::{LanguageRouter, supported_extensions};

/// Process-wide tree-sitter language registry for the slim (default) build.
///
/// With only `dynamic-loading` (the `grammars-download` feature OFF),
/// tree-sitter-language-pack's built-in `get_language()` does NOT register the
/// on-disk download cache, so `.so` files fetched by an earlier
/// `grammars-download` build would not be found. We fix that by building our own
/// `LanguageRegistry` with the cache directory pre-registered, then routing every
/// slim-build grammar lookup through it. The cache path mirrors the hard-coded
/// convention used by `DownloadManager::default_cache_dir` in tslp itself:
/// `~/.cache/tree-sitter-language-pack/v{version}/libs/`. Compiled only in the
/// slim path; the `grammars-download` build delegates to tslp's own
/// (download-aware) `get_language` instead, which registers the cache itself.
#[cfg(all(feature = "generic", not(feature = "grammars-download")))]
static TS_REGISTRY: std::sync::LazyLock<tree_sitter_language_pack::LanguageRegistry> =
    std::sync::LazyLock::new(|| {
        use tree_sitter_language_pack::LanguageRegistry;
        let reg = LanguageRegistry::new();
        // Register the download cache so previously-fetched .so files are found
        // even when the `download` feature (and its ureq/sha2/tar/zstd stack) is
        // not compiled in. The version string here must match the tslp version in
        // Cargo.lock; update it when bumping the tree-sitter-language-pack dep.
        const TSLP_VERSION: &str = "1.8.1";
        if let Some(cache_dir) = dirs::cache_dir() {
            let libs = cache_dir
                .join("tree-sitter-language-pack")
                .join(format!("v{TSLP_VERSION}"))
                .join("libs");
            reg.add_extra_libs_dir(libs);
        }
        reg
    });

/// Resolve a tree-sitter language by name.
///
/// `grammars-download` build: delegate to tslp's own `get_language`, which
/// registers the on-disk cache and fetches missing grammars on demand (pulling
/// the ureq/sha2/tar/zstd stack). This restores the full 300+ language pack.
#[cfg(all(feature = "generic", feature = "grammars-download"))]
pub(crate) fn get_ts_language(
    name: &str,
) -> std::result::Result<tree_sitter::Language, tree_sitter_language_pack::Error> {
    tree_sitter_language_pack::get_language(name)
}

/// Slim build (default): look up the grammar in the build-time language set plus
/// the pre-registered on-disk cache, with no network. Grammars outside that set
/// resolve to an error, which the generic extractor degrades to an empty result.
#[cfg(all(feature = "generic", not(feature = "grammars-download")))]
pub(crate) fn get_ts_language(
    name: &str,
) -> std::result::Result<tree_sitter::Language, tree_sitter_language_pack::Error> {
    TS_REGISTRY.get_language(name)
}

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
///
/// SECURITY (DoS hardening): the language extractors run on untrusted source
/// from arbitrary repositories. A bug in any one extractor (e.g. an
/// out-of-bounds slice on a malformed comment) would otherwise panic and abort
/// the entire `gen`/index run. We contain each file's parse in `catch_unwind`
/// so a single malformed file degrades to a skipped file (`ParseError`) that
/// callers already handle, rather than taking down the whole batch.
pub fn parse_file(path: &Path, source: &str) -> Result<Vec<Document>> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        parse_file_inner(path, source)
    }));
    match result {
        Ok(r) => r,
        Err(_) => Err(Error::Parse(format!(
            "parser panicked on {} (skipped; this file did not abort the run)",
            path.display()
        ))),
    }
}

fn parse_file_inner(path: &Path, source: &str) -> Result<Vec<Document>> {
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

fn parse_directory_inner(
    dir: &Path,
    depth: usize,
    file_count: &mut usize,
) -> Result<Vec<Document>> {
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
            if let Ok(meta) = std::fs::metadata(&path)
                && meta.len() > MAX_FILE_SIZE
            {
                eprintln!(
                    "[aden] WARNING: Skipping {} ({} bytes > {} MiB limit). Consider using 'aden gen <file>' instead.",
                    path.display(),
                    meta.len(),
                    MAX_FILE_SIZE / (1024 * 1024)
                );
                continue;
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
