// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Router: maps file extensions to `LanguageExtractor` implementations.
//!
//! The router owns a dispatch table.  When `parse_file` is called the
//! extension is looked up; if no deep extractor is registered the router
//! falls back to a `GenericExtractor` when `tree-sitter-language-pack`
//! advertises support for the language.

use crate::asciidoc::AsciiDocExtractor;
use crate::c_resolver::CResolver;
use crate::csharp_resolver::CSharpResolver;
use crate::csv::CsvExtractor;
use crate::extractor::LanguageExtractor;
#[cfg(feature = "generic")]
use crate::generic::GenericExtractor;
use crate::go_resolver::GoResolver;
use crate::java_resolver::JavaResolver;
use crate::kotlin_resolver::KotlinResolver;
use crate::markdown::MarkdownExtractor;
use crate::php_resolver::PhpResolver;
use crate::plaintext::PlainTextExtractor;
use crate::python_resolver::PythonResolver;
use crate::ruby_resolver::RubyResolver;
#[cfg(feature = "rust-deep")]
use crate::rust::RustExtractor;
use crate::typescript_resolver::TypeScriptResolver;
use aden_core::{Document, Error, Result};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// Maps file extensions → extractor instances.
pub struct LanguageRouter {
    by_extension: HashMap<&'static str, Arc<dyn LanguageExtractor>>,
}

impl Default for LanguageRouter {
    fn default() -> Self {
        Self::new()
    }
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
        {
            let c: Arc<dyn LanguageExtractor> = Arc::new(CResolver::new());
            self.by_extension.insert("c", Arc::clone(&c));
            self.by_extension.insert("h", Arc::clone(&c));
        }
        {
            let java: Arc<dyn LanguageExtractor> = Arc::new(JavaResolver::new());
            self.by_extension.insert("java", Arc::clone(&java));
        }
        {
            let kotlin: Arc<dyn LanguageExtractor> = Arc::new(KotlinResolver::new());
            self.by_extension.insert("kt", Arc::clone(&kotlin));
            self.by_extension.insert("kts", Arc::clone(&kotlin));
        }
        {
            let csharp: Arc<dyn LanguageExtractor> = Arc::new(CSharpResolver::new());
            self.by_extension.insert("cs", Arc::clone(&csharp));
        }
        {
            let ruby: Arc<dyn LanguageExtractor> = Arc::new(RubyResolver::new());
            self.by_extension.insert("rb", Arc::clone(&ruby));
        }
        {
            let php: Arc<dyn LanguageExtractor> = Arc::new(PhpResolver::new());
            self.by_extension.insert("php", Arc::clone(&php));
        }
        {
            let md: Arc<dyn LanguageExtractor> = Arc::new(MarkdownExtractor::new());
            self.by_extension.insert("md", Arc::clone(&md));
            self.by_extension.insert("markdown", Arc::clone(&md));
            self.by_extension.insert("mdown", Arc::clone(&md));
            self.by_extension.insert("mkd", Arc::clone(&md));
            self.by_extension.insert("mkdn", Arc::clone(&md));
        }
        {
            let adoc: Arc<dyn LanguageExtractor> = Arc::new(AsciiDocExtractor::new());
            self.by_extension.insert("adoc", Arc::clone(&adoc));
            self.by_extension.insert("asciidoc", Arc::clone(&adoc));
            self.by_extension.insert("asc", Arc::clone(&adoc));
        }
        {
            let txt: Arc<dyn LanguageExtractor> = Arc::new(PlainTextExtractor::new());
            self.by_extension.insert("txt", Arc::clone(&txt));
            self.by_extension.insert("text", Arc::clone(&txt));
        }
        {
            let csv: Arc<dyn LanguageExtractor> = Arc::new(CsvExtractor::new());
            self.by_extension.insert("csv", Arc::clone(&csv));
            self.by_extension.insert("tsv", Arc::clone(&csv));
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

    /// Returns true when a registered (deep) extractor handles this extension.
    pub fn has_extractor(&self, ext: &str) -> bool {
        self.by_extension.contains_key(ext)
    }

    /// All file extensions handled by a registered deep extractor.
    pub fn deep_extensions(&self) -> Vec<&'static str> {
        self.by_extension.keys().copied().collect()
    }

    /// Extract `Document`s from a single source file.
    pub fn parse_file(&self, path: &Path, source: &str) -> Result<Vec<Document>> {
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let mut ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

        // Extensionless file detection (e.g., Makefile, Dockerfile, CMakeLists.txt).
        if ext.is_empty() {
            ext = match file_name {
                "Makefile" | "makefile" | "GNUmakefile" => "makefile",
                "Dockerfile" | "dockerfile" => "dockerfile",
                "BUILD" | "WORKSPACE" => "bzl",
                _ => ext,
            };
            // Known config/plaintext files without extensions: skip.
            // Files WITH extensions (README.md, etc.) pass through to extractors.
            if file_name == "Kconfig"
                || file_name == "Build"
                || file_name == "config"
                || file_name == "settings"
                || file_name.starts_with("COPYING")
                || file_name.starts_with("AUTHORS")
                || file_name.starts_with("INSTALL")
            {
                return Ok(Vec::new());
            }
        } else if file_name == "CMakeLists.txt" {
            ext = "cmake";
        }

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
        // Core deep-resolved languages (already have dedicated extractors)
        "py" => "python",
        "rs" => "rust",
        "go" => "go",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" => "typescript",
        "tsx" => "tsx",

        // JVM ecosystem
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "scala" => "scala",
        "cs" => "csharp",
        "fs" | "fsx" => "fsharp",
        "fsi" => "fsharp_signature",
        "clj" | "cljs" | "cljc" => "clojure",

        // Systems / C family
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" => "cpp",
        "zig" => "zig",
        "odin" => "odin",
        "m" | "mm" => "objc",
        "swift" => "swift",
        "d" => "d",

        // Web front-end
        "html" => "html",
        "css" => "css",
        "scss" => "scss",
        "vue" => "vue",
        "svelte" => "svelte",
        "astro" => "astro",
        "graphql" | "gql" => "graphql",

        // Data & config
        "json" => "json",
        "json5" => "json5",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "xml" => "xml",
        "proto" => "proto",

        // Dynamic / scripting
        "rb" => "ruby",
        "php" => "php",
        "lua" => "lua",
        "pl" | "pm" => "perl",
        "r" | "R" => "r",
        "sh" | "bash" => "bash",
        "zsh" => "zsh",
        "fish" => "fish",
        "awk" => "awk",

        // Functional / typed
        "hs" => "haskell",
        "ml" => "ocaml",
        "mli" => "ocaml_interface",
        "ex" | "exs" => "elixir",
        "erl" => "erlang",
        "gleam" => "gleam",
        "elm" => "elm",
        "lean" => "lean",

        // Lisp family
        "lisp" | "cl" => "commonlisp",
        "el" => "elisp",
        "scm" => "scheme",
        "rkt" => "racket",

        // DevOps / ops
        "sql" => "sql",
        "dockerfile" => "dockerfile",
        "makefile" | "Makefile" | "GNUmakefile" | "mk" => "make",
        "cmake" => "cmake",
        "tf" | "tfvars" => "hcl",
        "bicep" => "bicep",
        "cue" => "cue",
        "dhall" => "dhall",
        "rego" => "rego",
        "pkl" => "pkl",
        "star" | "bzl" => "starlark",
        "nginx" | "conf" => "nginx",
        "ini" | "cfg" => "ini",
        "properties" => "properties",

        // Docs / markup
        "md" | "markdown" => "markdown",
        "adoc" | "asciidoc" => "asciidoc",
        "rst" => "rst",
        "org" => "org",
        "tex" => "latex",
        "bib" => "bibtex",
        "typst" => "typst",

        // Mobile / modern
        "dart" => "dart",
        "gd" => "gdscript",
        "jl" => "julia",
        "groovy" => "groovy",

        // Embedded / hardware
        "ino" => "arduino",
        "asm" | "s" | "S" => "asm",
        "nasm" => "nasm",
        "dts" | "dtsi" => "devicetree",
        "v" => "v",
        "vhd" | "vhdl" => "vhdl",
        "verilog" => "verilog",
        "sv" | "svh" => "systemverilog",

        // Scientific / HPC
        "f" | "f90" | "f95" | "f03" | "f08" => "fortran",

        // Web templates
        "pug" => "pug",
        "j2" | "jinja2" => "jinja2",
        "hbs" => "glimmer",
        "blade" => "blade",
        "eex" | "leex" => "eex",
        "heex" => "heex",
        "razor" | "cshtml" => "razor",

        // Graphics / shaders
        "glsl" => "glsl",
        "hlsl" => "hlsl",
        "wgsl" => "wgsl",
        "slang" => "slang",

        // Blockchain / emerging
        "sol" => "solidity",
        "cairo" => "cairo",
        "move" => "move",
        "mojo" => "mojo",
        "cr" => "crystal",
        "hx" => "haxe",
        "nim" | "nims" => "nim",
        "nu" => "nushell",

        // Data interchange
        "csv" => "csv",
        "tsv" => "tsv",
        "diff" | "patch" => "diff",

        // Nix family
        "nix" => "nix",

        // Roc
        "roc" => "roc",

        // PowerShell (parsed in-process via the airbus-cert grammar)
        "ps1" | "psm1" | "psd1" => "powershell",

        _ => return None,
    };
    Some(lang)
}

#[cfg(not(feature = "generic"))]
fn ext_to_language_pack_id(_ext: &str) -> Option<&'static str> {
    None
}

/// File extensions the generic `tree-sitter-language-pack` fallback can parse.
///
/// This list MUST stay in sync with `ext_to_language_pack_id`; the
/// `generic_extensions_all_resolve` test enforces that every entry here maps
/// to a real language id. It exists so that source-file *discovery* knows the
/// full breadth of what Aden can *parse* — keeping the compiler truly
/// language-agnostic instead of biased toward whatever language Aden itself
/// happens to be written in.
#[cfg(feature = "generic")]
pub const GENERIC_PACK_EXTENSIONS: &[&str] = &[
    "py",
    "rs",
    "go",
    "js",
    "mjs",
    "cjs",
    "ts",
    "tsx",
    "java",
    "kt",
    "kts",
    "scala",
    "cs",
    "fs",
    "fsx",
    "fsi",
    "clj",
    "cljs",
    "cljc",
    "c",
    "h",
    "cpp",
    "cc",
    "cxx",
    "hpp",
    "zig",
    "odin",
    "m",
    "mm",
    "swift",
    "d",
    "html",
    "css",
    "scss",
    "vue",
    "svelte",
    "astro",
    "graphql",
    "gql",
    "json",
    "json5",
    "yaml",
    "yml",
    "toml",
    "xml",
    "proto",
    "rb",
    "php",
    "lua",
    "pl",
    "pm",
    "r",
    "R",
    "sh",
    "bash",
    "zsh",
    "fish",
    "awk",
    "hs",
    "ml",
    "mli",
    "ex",
    "exs",
    "erl",
    "gleam",
    "elm",
    "lean",
    "lisp",
    "cl",
    "el",
    "scm",
    "rkt",
    "sql",
    "dockerfile",
    "makefile",
    "Makefile",
    "GNUmakefile",
    "mk",
    "cmake",
    "tf",
    "tfvars",
    "bicep",
    "cue",
    "dhall",
    "rego",
    "pkl",
    "star",
    "bzl",
    "nginx",
    "conf",
    "ini",
    "cfg",
    "properties",
    "md",
    "markdown",
    "adoc",
    "asciidoc",
    "rst",
    "org",
    "tex",
    "bib",
    "typst",
    "dart",
    "gd",
    "jl",
    "groovy",
    "ino",
    "asm",
    "s",
    "S",
    "nasm",
    "dts",
    "dtsi",
    "v",
    "vhd",
    "vhdl",
    "verilog",
    "sv",
    "svh",
    "f",
    "f90",
    "f95",
    "f03",
    "f08",
    "pug",
    "j2",
    "jinja2",
    "hbs",
    "blade",
    "eex",
    "leex",
    "heex",
    "razor",
    "cshtml",
    "glsl",
    "hlsl",
    "wgsl",
    "slang",
    "sol",
    "cairo",
    "move",
    "mojo",
    "cr",
    "hx",
    "nim",
    "nims",
    "nu",
    "csv",
    "tsv",
    "diff",
    "patch",
    "nix",
    "roc",
    "ps1",
    "psm1",
    "psd1",
];

#[cfg(not(feature = "generic"))]
pub const GENERIC_PACK_EXTENSIONS: &[&str] = &[];

/// Every file extension Aden can extract symbols from — the union of deep
/// extractors and the generic language-pack fallback.
///
/// This is the single source of truth used by source-file discovery so that
/// the set of files Aden *finds* always matches the set it can *parse*. Without
/// it, discovery tends to drift toward one ecosystem's conventions and silently
/// drops every other language in a polyglot repository.
pub fn supported_extensions() -> Vec<&'static str> {
    use std::collections::BTreeSet;
    let mut set: BTreeSet<&'static str> = BTreeSet::new();
    for ext in LanguageRouter::new().deep_extensions() {
        set.insert(ext);
    }
    for ext in GENERIC_PACK_EXTENSIONS {
        set.insert(ext);
    }
    set.into_iter().collect()
}

#[cfg(all(test, feature = "generic"))]
mod ext_table_tests {
    use super::*;

    #[test]
    fn generic_extensions_all_resolve() {
        for ext in GENERIC_PACK_EXTENSIONS {
            assert!(
                ext_to_language_pack_id(ext).is_some(),
                "extension '{}' is listed in GENERIC_PACK_EXTENSIONS but ext_to_language_pack_id \
                 does not resolve it — keep the two in sync",
                ext
            );
        }
    }

    #[test]
    fn supported_extensions_is_broad_and_polyglot() {
        let exts = supported_extensions();
        // A handful of languages from different ecosystems must all be present,
        // proving discovery is not biased toward any single language.
        for needle in [
            "rs", "py", "go", "ts", "java", "rb", "php", "swift", "ex", "md",
        ] {
            assert!(
                exts.contains(&needle),
                "missing expected extension '{}'",
                needle
            );
        }
    }
}
