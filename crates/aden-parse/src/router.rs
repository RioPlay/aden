// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Router: maps file extensions to `LanguageExtractor` implementations.
//!
//! The router owns a dispatch table.  When `parse_file` is called the
//! extension is looked up; if no deep extractor is registered the router
//! falls back to a `GenericExtractor` when `tree-sitter-language-pack`
//! advertises support for the language.

use crate::extractor::LanguageExtractor;
#[cfg(feature = "generic")]
use crate::generic::GenericExtractor;
#[cfg(feature = "rust-deep")]
use crate::rust::RustExtractor;
use crate::python_resolver::PythonResolver;
use crate::go_resolver::GoResolver;
use crate::typescript_resolver::TypeScriptResolver;
use aden_core::{Document, Error, Result};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// Maps file extensions → extractor instances.
pub struct LanguageRouter {
    by_extension: HashMap<&'static str, Arc<dyn LanguageExtractor>>,
}

impl LanguageRouter {
    /// Build a router with all built-in extractors registered.
    pub fn new() -> Self {
        let mut router = Self {
            by_extension: HashMap::new(),
        };
        router.register_builtin();
        router
    }

    fn register_builtin(&mut self) {
        #[cfg(feature = "rust-deep")]
        {
            let rust: Arc<dyn LanguageExtractor> = Arc::new(RustExtractor::new());
            self.by_extension.insert("rs", Arc::clone(&rust));
        }
        {
            let python: Arc<dyn LanguageExtractor> = Arc::new(PythonResolver::new());
            self.by_extension.insert("py", Arc::clone(&python));
        }
        {
            let go: Arc<dyn LanguageExtractor> = Arc::new(GoResolver::new());
            self.by_extension.insert("go", Arc::clone(&go));
        }
        {
            let ts: Arc<dyn LanguageExtractor> = Arc::new(TypeScriptResolver::new());
            self.by_extension.insert("ts", Arc::clone(&ts));
            self.by_extension.insert("tsx", Arc::clone(&ts));
            self.by_extension.insert("js", Arc::clone(&ts));
            self.by_extension.insert("jsx", Arc::clone(&ts));
            self.by_extension.insert("mjs", Arc::clone(&ts));
            self.by_extension.insert("cjs", Arc::clone(&ts));
        }
    }

    /// Register a custom extractor.  Later registrations shadow earlier ones.
    pub fn register(&mut self, extractor: Box<dyn LanguageExtractor>) {
        let exts: Vec<&'static str> = extractor.file_extensions().to_vec();
        let shared: Arc<dyn LanguageExtractor> = Arc::from(extractor);
        for ext in exts {
            self.by_extension.insert(ext, Arc::clone(&shared));
        }
    }

    /// Extract `Document`s from a single source file.
    pub fn parse_file(&self, path: &Path, source: &str) -> Result<Vec<Document>> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        // 1. Try a deep extractor first.
        if let Some(extractor) = self.by_extension.get(ext) {
            return extractor.extract_documents(source, path);
        }

        // 2. Fall back to generic tree-sitter extraction if the language pack
        //    knows this extension.
        #[cfg(feature = "generic")]
        {
            if let Some(lang_id) = ext_to_language_pack_id(ext) {
                let generic = GenericExtractor::new(lang_id);
                return generic.extract_documents(source, path);
            }
        }

        // 3. Nothing handles this extension.
        Err(Error::UnsupportedLanguage(ext.to_string()))
    }
}

/// Map a file extension to the canonical name used by
/// `tree-sitter-language-pack`.  Returns `None` when the pack does not
/// advertise support.
#[cfg(feature = "generic")]
fn ext_to_language_pack_id(ext: &str) -> Option<&'static str> {
    // Common extension → language pack name mapping.
    // The pack supports 305+ languages; we enumerate the highest-value
    // ones here and leave the rest for future expansion.
    let lang = match ext {
        "py" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" => "typescript",
        "tsx" => "tsx",
        "go" => "go",
        "java" => "java",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" => "cpp",
        "rb" => "ruby",
        "cs" => "c_sharp", // may require downloading 'all' group in tree-sitter-language-pack
        "swift" => "swift",
        "kt" => "kotlin",
        "scala" => "scala",
        "zig" => "zig",
        "lua" => "lua",
        "hs" => "haskell",
        "ml" => "ocaml",
        "ex" | "exs" => "elixir",
        "erl" => "erlang",
        "gleam" => "gleam",
        "rs" => "rust", // Shallow fallback when rust-deep is disabled
        "php" => "php",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "sql" => "sql",
        "sh" | "bash" => "bash",
        "dockerfile" => "dockerfile",
        "html" => "html",
        "css" => "css",
        "scss" => "scss",
        "vue" => "vue",
        "svelte" => "svelte",
        "proto" => "protobuf",
        "tf" => "hcl",
        "cmake" => "cmake",
        // Modern / systems / emerging
        "dart" => "dart",
        "groovy" => "groovy",
        "jl" => "julia",
        "clj" | "cljs" | "cljc" => "clojure",
        "pl" | "pm" => "perl",
        "r" | "R" => "r",
        "m" | "mm" => "objc",
        "graphql" | "gql" => "graphql",
        "xml" => "xml",
        "md" | "markdown" => "markdown",
        "nix" => "nix",
        "roc" => "roc",
        "odin" => "odin",
        _ => return None,
    };
    Some(lang)
}

#[cfg(not(feature = "generic"))]
fn ext_to_language_pack_id(_ext: &str) -> Option<&'static str> {
    None
}
