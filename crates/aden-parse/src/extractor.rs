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
    if let Some(name) = path
        .ancestors()
        .find(|p| {
            p.join("Cargo.toml").exists()
                || p.join("package.json").exists()
                || p.join("pyproject.toml").exists()
                || p.join("setup.py").exists()
                || p.join("setup.cfg").exists()
                || p.join("go.mod").exists()
                || p.join("tsconfig.json").exists()
                || p.join("jsconfig.json").exists()
                || p.join("pom.xml").exists()
                || p.join("build.gradle").exists()
                || p.join("build.gradle.kts").exists()
                || p.join("Gemfile").exists()
                || p.read_dir()
                    .ok()
                    .map(|mut d| {
                        d.any(|e| {
                            e.ok()
                                .and_then(|e| {
                                    let n = e.file_name();
                                    let s = n.to_string_lossy();
                                    (s.ends_with(".gemspec")).then(|| ())
                                })
                                .is_some()
                        })
                    })
                    .unwrap_or(false)
        })
        .and_then(dir_name)
    {
        return name;
    }
    // Manifest-less fallback (C, Makefile-built trees like the Linux kernel):
    // the top-level directory under the VCS root is the subsystem the file
    // belongs to (`mm/page_alloc.c` → `mm`, `net/core/sock.c` → `net`).
    // Without this, EVERY anchor in such a tree collapses into one
    // `aden://module/unknown/…` group and community labels become useless at
    // scale. Files directly at the root fall through to the parent-dir name.
    if let Some(repo) = path.ancestors().find(|p| p.join(".git").exists())
        && let Ok(rel) = path.strip_prefix(repo)
        && rel.components().count() > 1
        && let Some(std::path::Component::Normal(top)) = rel.components().next()
    {
        return top.to_string_lossy().to_string();
    }
    // No VCS root either (e.g. a tarball checkout): the file's own directory
    // is still a more honest group than a global "unknown" bucket.
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .and_then(dir_name)
        .filter(|n| n != "." && n != "..")
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

/// Per-language declaration references (`fn:`/`struct:`/`class:`/`type:`/
/// `use:`/`mod:` prefixed) extracted from a doc code listing — the typed half
/// of the `symbol_references` attribute (the language-neutral half is
/// `listing_call_tokens`). One shared implementation: this used to live as an
/// identical copy in BOTH the markdown and asciidoc parsers.
pub(crate) fn extract_code_references(code: &str, lang: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let lang_lower = lang.to_lowercase();
    match lang_lower.as_str() {
        "rust" => {
            for line in code.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("fn ") {
                    if let Some(name) = trimmed.strip_prefix("fn ") {
                        let name = name
                            .split('(')
                            .next()
                            .unwrap_or(name)
                            .split('{')
                            .next()
                            .unwrap_or(name);
                        refs.push(format!("fn:{}", name.trim()));
                    }
                } else if trimmed.starts_with("struct ") {
                    if let Some(name) = trimmed.strip_prefix("struct ") {
                        let name = name.split_whitespace().next().unwrap_or(name);
                        refs.push(format!("struct:{}", name));
                    }
                } else if trimmed.starts_with("enum ") {
                    if let Some(name) = trimmed.strip_prefix("enum ") {
                        let name = name.split_whitespace().next().unwrap_or(name);
                        refs.push(format!("enum:{}", name));
                    }
                } else if trimmed.starts_with("impl ") || trimmed.starts_with("trait ") {
                    if let Some(name) = trimmed.split_whitespace().nth(1) {
                        refs.push(format!("type:{}", name));
                    }
                } else if trimmed.starts_with("use ") {
                    if let Some(name) = trimmed.strip_prefix("use ") {
                        let name = name.split_whitespace().next().unwrap_or(name);
                        let name = name.trim_end_matches(';');
                        refs.push(format!("use:{}", name));
                    }
                } else if trimmed.contains("::") {
                    let parts: Vec<&str> = trimmed.split("::").collect();
                    if parts.len() >= 2 {
                        refs.push(format!("mod:{}", parts[0]));
                    }
                }
            }
        }
        "python" => {
            for line in code.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("def ") {
                    if let Some(name) = trimmed.strip_prefix("def ") {
                        let name = name.split('(').next().unwrap_or(name);
                        refs.push(format!("fn:{}", name.trim()));
                    }
                } else if trimmed.starts_with("class ") {
                    if let Some(name) = trimmed.strip_prefix("class ") {
                        let name = name.split('(').next().unwrap_or(name);
                        refs.push(format!("class:{}", name.trim()));
                    }
                } else if trimmed.starts_with("import ") || trimmed.starts_with("from ") {
                    let name = trimmed.split_whitespace().nth(1).unwrap_or(trimmed);
                    refs.push(format!("use:{}", name));
                }
            }
        }
        "javascript" | "typescript" | "js" | "ts" => {
            for line in code.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("function ") {
                    if let Some(name) = trimmed.strip_prefix("function ") {
                        let name = name.split('(').next().unwrap_or(name);
                        refs.push(format!("fn:{}", name.trim()));
                    }
                } else if trimmed.starts_with("const ") && trimmed.contains("=>") {
                    if let Some(name) = trimmed.strip_prefix("const ") {
                        let name = name.split('=').next().unwrap_or(name);
                        refs.push(format!("fn:{}", name.trim()));
                    }
                } else if trimmed.starts_with("class ") {
                    if let Some(name) = trimmed.strip_prefix("class ") {
                        let name = name.split('{').next().unwrap_or(name);
                        refs.push(format!("class:{}", name.trim()));
                    }
                } else if (trimmed.starts_with("interface ") || trimmed.starts_with("type "))
                    && let Some(name) = trimmed.split_whitespace().nth(1)
                {
                    let name = name.split('{').next().unwrap_or(name);
                    refs.push(format!("type:{}", name));
                }
            }
        }
        "go" => {
            for line in code.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("func ") {
                    if let Some(name) = trimmed.strip_prefix("func ") {
                        let name = name.split('(').next().unwrap_or(name);
                        refs.push(format!("fn:{}", name.trim()));
                    }
                } else if trimmed.starts_with("type ") {
                    if let Some(name) = trimmed.strip_prefix("type ") {
                        let name = name.split_whitespace().next().unwrap_or(name);
                        refs.push(format!("type:{}", name));
                    }
                } else if trimmed.starts_with("import ")
                    && let Some(name) = trimmed.strip_prefix("import ")
                {
                    let name = name.trim_matches('"');
                    refs.push(format!("use:{}", name));
                }
            }
        }
        _ => {}
    }
    refs
}

/// Floor on mention/call-token name length: below this, prose words and
/// generic identifiers (`new`, `get`, `run`) flood the channel with noise.
pub(crate) const MENTION_MIN_LEN: usize = 4;

/// Collect backtick-span symbol mentions from one PROSE line, as
/// (0-based line index, name) pairs — the per-format half of the Wave-2
/// `Mentions` channel (graph-type roadmap). Callers are the doc parsers'
/// line loops, which know listing/literal fence state, so a backtick span
/// inside a code example never reaches here (the same division of labor as
/// `doc_refs`: extraction per-format, resolution format-neutral in the
/// linker).
///
/// Precision guards live on both sides. Here: the span must be
/// identifier-shaped (starts `[A-Za-z_]`, only `[A-Za-z0-9_:.]` after an
/// optional trailing `()`), at least `MENTION_MIN_LEN` chars, and a single
/// token (no spaces — `aden gen .` is a command, not a symbol). At link
/// time: the name must resolve to exactly ONE code anchor.
pub(crate) fn collect_backtick_mentions(line: &str, idx: usize, out: &mut Vec<(usize, String)>) {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'`' {
            i += 1;
            continue;
        }
        let Some(close) = line[i + 1..].find('`') else {
            return;
        };
        let span = &line[i + 1..i + 1 + close];
        i += close + 2;
        if let Some(name) = mention_candidate(span) {
            out.push((idx, name.to_string()));
        }
    }
}

/// Supersede-context detection for prose cross-references (Wave 3
/// `Supersedes`). A ref on a line with supersede language becomes a directed
/// NEW —Supersedes→ OLD edge; this returns which side the enclosing doc is on:
/// - `Some("by")` — passive ("Superseded … by <<X>>"): the REFERENCED doc
///   supersedes the enclosing one;
/// - `Some("of")` — active ("supersedes <<X>>"): the enclosing doc supersedes
///   the referenced one;
/// - `None` — no supersede language; the ref stays an ordinary cross-reference.
///
/// `"superseded" + "by"` is checked first so the passive form wins when a line
/// somehow carries both phrasings (the rarer, more specific pattern).
pub(crate) fn supersede_direction(line: &str) -> Option<&'static str> {
    let l = line.to_lowercase();
    if !l.contains("supersed") {
        return None;
    }
    if l.contains("superseded") && l.contains("by") {
        Some("by")
    } else {
        Some("of")
    }
}

/// A glossary entry found by a format parser: the per-format half of Term
/// extraction. `slug` is the explicit `[[anchor]]` when the author declared
/// one, else `term_slug(name)`.
pub(crate) struct GlossaryEntry {
    pub name: String,
    pub slug: String,
    pub definition: String,
}

/// Build the Term node for a glossary entry — the format-neutral half. The
/// term's own `doc_mentions` attribute carries the term name (when
/// identifier-shaped) plus any backticked names in the definition, so terms
/// link to the code they define through the ordinary Mentions channel with
/// its unambiguous-only guard — no new resolution machinery.
pub(crate) fn build_term_document(project: &str, path: &Path, entry: &GlossaryEntry) -> Document {
    let mut attrs = build_code_attributes(&entry.definition, "term", Some(path), None);
    attrs.insert("term_name".to_string(), entry.name.clone());
    let mut mentions: Vec<(usize, String)> = Vec::new();
    if let Some(n) = mention_candidate(&entry.name) {
        mentions.push((0, n.to_string()));
    }
    for (i, line) in entry.definition.lines().enumerate() {
        collect_backtick_mentions(line, i, &mut mentions);
    }
    let mut names: Vec<String> = mentions.into_iter().map(|(_, n)| n).collect();
    names.sort();
    names.dedup();
    if !names.is_empty() {
        attrs.insert("doc_mentions".to_string(), names.join(","));
    }
    Document {
        anchor: make_term_anchor(project, &entry.slug),
        node_type: aden_core::NodeType::Term,
        attributes: attrs,
        blocks: vec![
            aden_core::Block::Paragraph(entry.name.clone()),
            aden_core::Block::Paragraph(entry.definition.clone()),
        ],
        source_span: None,
        metadata: None,
        confidence: 0.9,
    }
}

/// True when a title marks glossary content — the gate for Term-node
/// extraction (Wave 2 remainder). Ordinary description lists are prose; only
/// a glossary-titled section, or any section of a glossary-titled document,
/// emits `aden://term/` nodes.
pub(crate) fn is_glossary_title(title: &str) -> bool {
    title.to_lowercase().contains("glossary")
}

/// Slug for a term anchor: lowercase, alphanumerics kept, every other run
/// collapsed to one `-` (matches the doc-heading slug discipline).
pub(crate) fn term_slug(name: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    out.trim_end_matches('-').to_string()
}

/// Anchor for a glossary term node.
pub(crate) fn make_term_anchor(project: &str, slug: &str) -> String {
    format!("aden://term/{project}/{slug}")
}

/// The identifier-shaped core of a backtick span, or None if the span is not
/// a plausible symbol mention (commands, flags, paths, prose).
pub(crate) fn mention_candidate(span: &str) -> Option<&str> {
    let name = span.trim().trim_end_matches("()");
    if name.len() < MENTION_MIN_LEN {
        return None;
    }
    let mut chars = name.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '.')) {
        return None;
    }
    Some(name)
}

/// Call-shaped identifiers in a doc code listing — the language-NEUTRAL half
/// of the Wave-2 `Demonstrates` channel: any `[A-Za-z_][A-Za-z0-9_]*` token
/// immediately followed by `(`, at least `MENTION_MIN_LEN` chars, minus
/// universal keywords. Works for every language the listing might hold (the
/// per-language `extract_code_references` declarations remain a parallel,
/// richer signal where the language is known). Deduped + sorted for
/// deterministic attribute output.
pub(crate) fn listing_call_tokens(code: &str) -> Vec<String> {
    const KEYWORDS: &[&str] = &[
        "if", "for", "while", "match", "switch", "return", "catch", "print", "println", "assert",
        "panic", "format", "main",
    ];
    let mut out: Vec<String> = Vec::new();
    for line in code.lines() {
        let bytes = line.as_bytes();
        let mut start: Option<usize> = None;
        for (i, &b) in bytes.iter().enumerate() {
            let is_ident = b.is_ascii_alphanumeric() || b == b'_';
            match (start, is_ident) {
                (None, true) if b.is_ascii_alphabetic() || b == b'_' => start = Some(i),
                (Some(s), false) => {
                    if b == b'(' {
                        let tok = &line[s..i];
                        if tok.len() >= MENTION_MIN_LEN && !KEYWORDS.contains(&tok) {
                            out.push(tok.to_string());
                        }
                    }
                    start = None;
                }
                _ => {}
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::infer_project_name;
    use super::{collect_backtick_mentions, listing_call_tokens};
    use std::fs;

    #[test]
    fn backtick_mentions_extract_identifiers_only() {
        let mut out = Vec::new();
        collect_backtick_mentions(
            "The `helper_fn` routine (also `Type::method()` and `pkg.attr`) beats `aden gen .`, `--fix`, and `ab`.",
            7,
            &mut out,
        );
        let names: Vec<&str> = out.iter().map(|(_, n)| n.as_str()).collect();
        assert_eq!(names, vec!["helper_fn", "Type::method", "pkg.attr"]);
        assert!(out.iter().all(|(i, _)| *i == 7));
    }

    #[test]
    fn call_tokens_are_neutral_and_guarded() {
        let toks = listing_call_tokens(
            "let s = helper_fn(\"hi\");\nif check(x) { obj.method_name(1) }\nfor(;;) {}\nab(1);",
        );
        // `check` passes (>=4, not a keyword); `if`/`for` keywords and the
        // short `ab` do not; method calls extract their bare name.
        assert_eq!(toks, vec!["check", "helper_fn", "method_name"]);
    }

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

    /// Manifest-less tree with a VCS root (the Linux-kernel layout): the
    /// module name is the top-level directory under the repo root, NOT a
    /// global "unknown" bucket — `mm/page_alloc.c` → `mm`,
    /// `net/core/sock.c` → `net`.
    #[test]
    fn manifestless_repo_uses_top_level_dir_as_module() {
        let base = std::env::temp_dir().join("aden_infer_test_kernel");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join(".git")).unwrap();
        fs::create_dir_all(base.join("mm")).unwrap();
        fs::create_dir_all(base.join("net/core")).unwrap();
        fs::write(base.join("mm/page_alloc.c"), "int x;\n").unwrap();
        fs::write(base.join("net/core/sock.c"), "int y;\n").unwrap();
        fs::write(base.join("main.c"), "int z;\n").unwrap();

        assert_eq!(infer_project_name(&base.join("mm/page_alloc.c")), "mm");
        assert_eq!(
            infer_project_name(&base.join("net/core/sock.c")),
            "net",
            "nested files group by TOP-level subsystem, not their leaf dir"
        );
        // A root-level file has no top-level subdir — its parent (the repo
        // dir itself) is the honest group.
        assert_eq!(
            infer_project_name(&base.join("main.c")),
            "aden_infer_test_kernel"
        );

        let _ = fs::remove_dir_all(&base);
    }

    /// No manifest AND no VCS root (tarball checkout): the file's own
    /// directory still beats a global "unknown".
    #[test]
    fn manifestless_no_vcs_falls_back_to_parent_dir() {
        let base = std::env::temp_dir().join("aden_infer_test_tarball");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("drivers")).unwrap();
        fs::write(base.join("drivers/thing.c"), "int x;\n").unwrap();

        assert_eq!(infer_project_name(&base.join("drivers/thing.c")), "drivers");

        let _ = fs::remove_dir_all(&base);
    }

    /// Git-only repo (no manifest): Python and generic (PowerShell) files under
    /// src/ must resolve to the SAME project component — both use the
    /// VCS-root top-level-dir fallback, which is "src" for src/foo.py.
    #[test]
    fn git_only_repo_python_and_generic_same_project_component() {
        let base = std::env::temp_dir().join("aden_infer_test_gitonly");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join(".git")).unwrap();
        fs::create_dir_all(base.join("src")).unwrap();
        fs::write(
            base.join("src/module.py"),
            "def compute_checksum(data): pass\n",
        )
        .unwrap();
        fs::write(base.join("src/utils.ps1"), "function Get-Foo {}\n").unwrap();

        let py = infer_project_name(&base.join("src/module.py"));
        let ps1 = infer_project_name(&base.join("src/utils.ps1"));
        assert_eq!(
            py, ps1,
            "Python and PowerShell files under src/ must share the same project component; \
             got py={py:?} ps1={ps1:?}"
        );
        assert_eq!(py, "src", "git-only src/ files must resolve to 'src'");

        let _ = fs::remove_dir_all(&base);
    }

    /// pom.xml manifest: project name = directory containing pom.xml.
    #[test]
    fn pom_xml_inferred_as_project_name() {
        let base = std::env::temp_dir().join("aden_infer_test_pom");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("src/main/java")).unwrap();
        fs::write(base.join("pom.xml"), "<project/>\n").unwrap();
        fs::write(base.join("src/main/java/Foo.java"), "public class Foo {}\n").unwrap();

        assert_eq!(
            infer_project_name(&base.join("src/main/java/Foo.java")),
            "aden_infer_test_pom",
            "pom.xml ancestor must be used as project root"
        );

        let _ = fs::remove_dir_all(&base);
    }

    /// build.gradle manifest: project name = directory containing build.gradle.
    #[test]
    fn build_gradle_inferred_as_project_name() {
        let base = std::env::temp_dir().join("aden_infer_test_gradle");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("src/main")).unwrap();
        fs::write(base.join("build.gradle"), "// gradle\n").unwrap();
        fs::write(base.join("src/main/Foo.kt"), "fun foo() {}\n").unwrap();

        assert_eq!(
            infer_project_name(&base.join("src/main/Foo.kt")),
            "aden_infer_test_gradle",
            "build.gradle ancestor must be used as project root"
        );

        let _ = fs::remove_dir_all(&base);
    }

    /// build.gradle.kts manifest: project name = directory containing build.gradle.kts.
    #[test]
    fn build_gradle_kts_inferred_as_project_name() {
        let base = std::env::temp_dir().join("aden_infer_test_gradle_kts");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("src")).unwrap();
        fs::write(base.join("build.gradle.kts"), "// gradle kts\n").unwrap();
        fs::write(base.join("src/Bar.kt"), "fun bar() {}\n").unwrap();

        assert_eq!(
            infer_project_name(&base.join("src/Bar.kt")),
            "aden_infer_test_gradle_kts",
            "build.gradle.kts ancestor must be used as project root"
        );

        let _ = fs::remove_dir_all(&base);
    }

    /// Gemfile manifest: project name = directory containing Gemfile,
    /// NOT a lib/-derived dotted path.
    #[test]
    fn gemfile_inferred_as_project_name_not_lib_path() {
        let base = std::env::temp_dir().join("aden_infer_test_gemfile");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("lib/my_gem")).unwrap();
        fs::write(base.join("Gemfile"), "source 'https://rubygems.org'\n").unwrap();
        fs::write(base.join("lib/my_gem/client.rb"), "class Client; end\n").unwrap();

        assert_eq!(
            infer_project_name(&base.join("lib/my_gem/client.rb")),
            "aden_infer_test_gemfile",
            "Gemfile ancestor must be used as project root (not lib/-derived path)"
        );

        let _ = fs::remove_dir_all(&base);
    }

    /// setup.cfg manifest: project name = directory containing setup.cfg.
    #[test]
    fn setup_cfg_inferred_as_project_name() {
        let base = std::env::temp_dir().join("aden_infer_test_setup_cfg");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("src/mypackage")).unwrap();
        fs::write(base.join("setup.cfg"), "[metadata]\nname = mypackage\n").unwrap();
        fs::write(base.join("src/mypackage/mod.py"), "def foo(): pass\n").unwrap();

        assert_eq!(
            infer_project_name(&base.join("src/mypackage/mod.py")),
            "aden_infer_test_setup_cfg",
            "setup.cfg ancestor must be used as project root"
        );

        let _ = fs::remove_dir_all(&base);
    }
}
