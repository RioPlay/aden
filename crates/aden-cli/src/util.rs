// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod quiet;

use std::sync::atomic::{AtomicBool, Ordering};

/// Whether store *creation* was explicitly authorized for this run — set true
/// when `-p/--project` was given or the command is `init`. Consumed by the
/// ADR-003 creation safety rail (`aden_paths::guard_creatable_root`) so reads
/// stay frictionless while creation at `$HOME`/fs-root is refused by default.
static CREATION_EXPLICIT: AtomicBool = AtomicBool::new(false);

/// Set the global creation-explicit flag. Called once during startup.
pub fn set_creation_explicit(v: bool) {
    CREATION_EXPLICIT.store(v, Ordering::Relaxed);
}

/// Query whether store creation was explicitly authorized this run.
pub fn creation_explicit() -> bool {
    CREATION_EXPLICIT.load(Ordering::Relaxed)
}

use aden_emit::check::{collect_anchors, find_refs};
use aden_graph::{GraphNode, cycles::find_cycles, integrity::check_hashes};
use serde_json::Map;
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::types::GenCache;

/// Reject project names that could traverse directories.
pub fn validate_name(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    if name.contains('/') || name.contains('\\') || name == ".." || name.starts_with("../") {
        return Err(format!(
            "Invalid project name '{}': must not contain path separators or parent references",
            name
        )
        .into());
    }
    Ok(())
}

/// Reject paths containing parent-directory references.
pub fn safe_relative(path_str: &str) -> Result<(), Box<dyn std::error::Error>> {
    if path_str.contains("..") {
        return Err(format!("Path traversal blocked: '{}' contains '..'", path_str).into());
    }
    Ok(())
}

/// Find the canonical project root containing `start`.
///
/// Single source of truth lives in [`aden_paths::resolve_root`] (ADR-003 §1):
/// `git rev-parse --show-toplevel` → root-marker walk-up → persisted
/// `.aden/project.conf` → canonical `start`. This thin wrapper keeps the
/// long-standing call sites working while every crate shares one resolver, so a
/// subdir and the repo root map to the same store key.
pub fn find_project_root(start: &Path) -> PathBuf {
    aden_paths::resolve_root(start)
}

/// One-time migration of a legacy in-tree store (`<root>/.aden/store`) to the
/// per-user central location (ADR-003). No-op when there is nothing to move or
/// a central store already exists. Called by creation paths (`gen`/`regen`)
/// before the central store is opened, so no store handle is live.
///
/// Tries an atomic `rename` first, falling back to a recursive copy + remove
/// when the two locations are on different filesystems (the common case, since
/// the data dir is typically under `$HOME` while the repo may be elsewhere).
pub fn migrate_legacy_store(root: &Path) {
    let legacy = aden_paths::legacy_store_dir(root);
    let central = aden_paths::store_dir(root);
    if !legacy.is_dir() || central.exists() {
        return;
    }
    if let Some(parent) = central.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return;
    }
    eprintln!(
        "migrating legacy in-tree store {} -> {}",
        legacy.display(),
        central.display()
    );
    if std::fs::rename(&legacy, &central).is_ok() {
        let _ = aden_paths::write_meta(root);
        return;
    }
    // Cross-filesystem: copy then remove the legacy tree.
    if copy_dir_recursive(&legacy, &central).is_ok() {
        let _ = std::fs::remove_dir_all(&legacy);
        let _ = aden_paths::write_meta(root);
    } else {
        eprintln!("warning: could not migrate legacy store; it will continue to be read in place");
    }
}

/// Recursively copy a directory tree from `src` to `dst`.
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Persist the active project root to `.aden/project.conf` so subsequent
/// commands without `--project` can find it via [`find_project_root`].
pub fn write_project_conf(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let aden_dir = root.join(".aden");
    std::fs::create_dir_all(&aden_dir)?;
    let conf = aden_dir.join("project.conf");
    let abs = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    std::fs::write(conf, format!("{}\n", abs.display()))?;
    Ok(())
}

/// Source files without an extension that Aden's parser recognizes by name
/// (the router maps these to languages such as `make`, `dockerfile`, `bzl`).
const EXTENSIONLESS_SOURCE_FILES: &[&str] = &[
    "Makefile",
    "makefile",
    "GNUmakefile",
    "Dockerfile",
    "dockerfile",
    "BUILD",
    "WORKSPACE",
];

/// Discover source files anywhere under `root`, regardless of language.
///
/// Aden is a *language-agnostic* context compiler, so discovery must not be
/// gated on which build manifest (Cargo.toml, go.mod, package.json, …) happens
/// to be present — doing that silently drops every other language in a
/// polyglot repository and biases the tool toward whatever ecosystem it was
/// first built for. Instead we walk the whole tree and keep any file the
/// parser can actually handle (`aden_parse::supported_extensions()` is the
/// single source of truth), honoring `.adenignore`/`.adenallow` and the
/// cross-ecosystem built-in ignore list so build artifacts and vendored deps
/// are skipped for every language.
pub fn discover_source_files(root: &Path) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    use std::collections::HashSet;

    let supported: HashSet<&'static str> = aden_parse::supported_extensions().into_iter().collect();
    let filter = aden_core::filter::AdenFilter::from_directory(root);

    let mut files = Vec::new();
    walk_supported_files(root, root, &supported, &filter, &mut files)?;

    // Prioritize files under a `src/`-style directory so that, when a token
    // budget truncates generation, the most load-bearing code is processed
    // first. This is a neutral heuristic that helps every layout, not just
    // Cargo's.
    files.sort_by(|a, b| {
        let a_is_src = normalize_sep(a).contains("/src/");
        let b_is_src = normalize_sep(b).contains("/src/");
        b_is_src.cmp(&a_is_src)
    });

    Ok(files)
}

/// Recursively collect parseable source files, pruning ignored directories.
fn walk_supported_files(
    dir: &Path,
    root: &Path,
    supported: &std::collections::HashSet<&'static str>,
    filter: &aden_core::filter::AdenFilter,
    out: &mut Vec<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    for entry in std::fs::read_dir(dir).map_err(|e| format!("read_dir {}: {}", dir.display(), e))? {
        let entry = entry?;
        let p = entry.path();
        let file_type = entry.file_type()?;

        // SECURITY: never follow symlinks — they can escape the project root.
        if file_type.is_symlink() {
            continue;
        }

        // Honor .adenignore / .adenallow / built-in ignores (relative to root).
        if let Ok(rel) = p.strip_prefix(root)
            && filter.should_skip(rel)
        {
            continue;
        }

        if file_type.is_dir() {
            walk_supported_files(&p, root, supported, filter, out)?;
        } else if file_type.is_file() {
            let keep = match p.extension().and_then(|e| e.to_str()) {
                Some(ext) => supported.contains(ext),
                None => p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| EXTENSIONLESS_SOURCE_FILES.contains(&n))
                    .unwrap_or(false),
            };
            if keep {
                out.push(p);
            }
        }
    }
    Ok(())
}

/// Strip the project-root prefix from a doc's `source_file` attribute so no
/// absolute host path (and the `$HOME`/username it contains) is persisted to
/// the store or emitted into LLM context.
///
/// SECURITY (audit MEDIUM-2): this must strip against the actual discovery
/// `root`, NOT `std::env::current_dir()`. When aden indexes a repo that is not
/// under the process cwd (e.g. `aden gen /other/repo`, or any MCP call where
/// the resolved root differs from cwd), a cwd-based strip silently fails and
/// the full `/home/<user>/...` path leaks through. Stripping against `root`
/// always yields a repo-relative path. As defense-in-depth, if the value is
/// still absolute afterwards, fall back to the bare file name so no absolute
/// component can ever be stored.
pub fn sanitize_source_file(doc: &mut aden_core::Document, root: &Path) {
    if let Some(source_file) = doc.attributes.get("source_file") {
        let p = std::path::Path::new(source_file);
        // Strip the root prefix component-wise. We do NOT gate this on
        // `p.is_absolute()`: a Unix-style path like `/home/...` is not
        // "absolute" on Windows (no drive letter), but `strip_prefix` still
        // works cross-platform, so a graph built on one OS is sanitized on
        // another. The bare-filename fallback only applies to genuinely
        // host-absolute paths, so relative paths that simply don't match the
        // root are left untouched.
        let rel = p
            .strip_prefix(root)
            .ok()
            .map(|r| r.to_string_lossy().to_string())
            .or_else(|| {
                if p.is_absolute() {
                    p.file_name().map(|f| f.to_string_lossy().to_string())
                } else {
                    None
                }
            });
        if let Some(rel) = rel {
            doc.attributes.insert("source_file".to_string(), rel);
        }
    }
}

/// Make untrusted text safe to print to a terminal.
///
/// SECURITY (audit MEDIUM-3): aden's read commands echo content from arbitrary
/// untrusted source files to stdout. A crafted line containing ANSI/OSC escape
/// sequences (`ESC[2J`, OSC-52 clipboard writes, OSC-8 hyperlinks/title spoof)
/// would otherwise be interpreted by the operator's terminal and fed verbatim
/// into an agent's context. Replace ESC and all C0/C1 control bytes (except
/// `\t`) with a visible `\xNN` form. Applied unconditionally — not gated on
/// isatty — so piped/agent output is sanitized too.
pub fn sanitize_terminal(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        let cp = c as u32;
        // C0 controls (0x00–0x1F) except tab, DEL (0x7F), and C1 (0x80–0x9F).
        if (cp < 0x20 && c != '\t') || cp == 0x7f || (0x80..=0x9f).contains(&cp) {
            out.push_str(&format!("\\x{:02x}", cp.min(0xff)));
        } else {
            out.push(c);
        }
    }
    out
}

/// Normalize path separators for cross-platform skip-pattern matching.
/// On Windows, `to_string_lossy()` yields backslashes; we unify to `/`.
pub fn normalize_sep(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Sanitize an anchor into a safe filename stem.
pub fn sanitize_anchor(anchor: &str) -> String {
    let s = anchor
        .replace(['/', '#', '\\'], "-")
        .replace(":", "-")
        .replace(" ", "-");
    // Truncate to 128 characters to stay well under POSIX max-filename
    // limits while remaining human-readable.
    if s.len() > 128 {
        let hash = aden_core::stable_hash(s.as_bytes());
        format!("{}-{}", &s[..118], &hash[..8])
    } else {
        s
    }
}

/// Parse a single edge-type string into the corresponding enum variant (case-insensitive).
pub fn parse_single_edge_type(s: &str) -> Option<aden_core::EdgeType> {
    let lower = s.trim().to_lowercase();
    match lower.as_str() {
        "uses" => Some(aden_core::EdgeType::Uses),
        "implements" => Some(aden_core::EdgeType::Implements),
        "tests" => Some(aden_core::EdgeType::Tests),
        "documents" => Some(aden_core::EdgeType::Documents),
        "constrains" => Some(aden_core::EdgeType::Constrains),
        "justifies" => Some(aden_core::EdgeType::Justifies),
        "invokes" => Some(aden_core::EdgeType::Invokes),
        "requires" => Some(aden_core::EdgeType::Requires),
        "mutates" => Some(aden_core::EdgeType::Mutates),
        "calls" => Some(aden_core::EdgeType::Calls),
        "supersedes" => Some(aden_core::EdgeType::Supersedes),
        "amends" => Some(aden_core::EdgeType::Amends),
        "verifies" => Some(aden_core::EdgeType::Verifies),
        _ => None,
    }
}

/// Return list of valid edge types for error messages.
pub fn valid_edge_types() -> Vec<&'static str> {
    vec![
        "uses",
        "implements",
        "tests",
        "documents",
        "constrains",
        "justifies",
        "invokes",
        "requires",
        "mutates",
        "calls",
        "supersedes",
        "amends",
        "verifies",
    ]
}

/// Parse a comma-separated list of edge-type strings, returning `Err` on any
/// unrecognized token (and on an all-empty result).
///
/// Rather than silently `filter_map`-ing unknown tokens away (which yields an
/// empty vec the assembler treats as "follow all edges"), it rejects bad input
/// with the same message the `query --edge-type` path uses. Empty tokens from
/// trailing/double commas are skipped; a wholly empty input is an error.
pub fn parse_edge_types_validated(
    input: &str,
) -> Result<Vec<aden_core::EdgeType>, Box<dyn std::error::Error>> {
    let valid = valid_edge_types().join(", ");
    let mut out = Vec::new();
    for tok in input.split(',') {
        let trimmed = tok.trim();
        if trimmed.is_empty() {
            continue;
        }
        match parse_single_edge_type(trimmed) {
            Some(et) => out.push(et),
            None => {
                return Err(format!("invalid edge type: '{}'. Valid: {}", trimmed, valid).into());
            }
        }
    }
    if out.is_empty() {
        return Err(format!("no valid edge types in '{}'. Valid: {}", input, valid).into());
    }
    Ok(out)
}

/// Load the generation cache from disk, returning a default on any error.
pub fn load_gen_cache(path: &Path) -> GenCache {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Persist the generation cache to disk.
pub fn save_gen_cache(path: &Path, cache: &GenCache) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(cache)?;
    std::fs::write(path, json)?;
    Ok(())
}

/// Convert a graph DocumentNode into a JSON value for CLI output.
pub fn node_to_json(node: &aden_graph::DocumentNode, depth: usize) -> serde_json::Value {
    let mut map = Map::new();
    map.insert(
        "anchor".to_string(),
        serde_json::Value::String(node.doc.anchor.clone()),
    );
    map.insert(
        "node_type".to_string(),
        serde_json::Value::String(resolve_node_type(node)),
    );
    map.insert("depth".to_string(), serde_json::Value::from(depth as u64));
    serde_json::Value::Object(map)
}

/// Resolve the human-readable type label for a graph node.
pub fn resolve_node_type(node: &aden_graph::DocumentNode) -> String {
    node.parsed
        .as_ref()
        .and_then(|p| p.attributes.get("node-type").cloned())
        .unwrap_or_else(|| format!("{:?}", node.doc.node_type))
}

/// Scan for contracts that have [must-complete] blocks that haven't been filled.
/// Returns warnings for incomplete contracts.
fn check_incomplete_contracts(path: &Path) -> Vec<String> {
    let mut incomplete = Vec::new();

    for entry in walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let p = entry.path().to_path_buf();
        if !p.is_file() {
            continue;
        }
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "adoc" && ext != "aden" {
            continue;
        }
        let mut text = String::new();
        if let Ok(mut file) = std::fs::File::open(&p) {
            let _ = file.read_to_string(&mut text);
            if text.contains("[must-complete]") {
                let has_hint = text.contains("Hint:");
                let has_content_after_marker = text
                    .match_indices("[must-complete]")
                    .last()
                    .map(|(pos, _)| text[pos..].contains("===="))
                    .unwrap_or(false);

                if has_hint || !has_content_after_marker {
                    let anchor = p.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown");
                    incomplete.push(format!(
                        "WARNING: Incomplete contract: {} - run 'aden complete' to fill missing documentation",
                        anchor
                    ));
                }
            }
        }
    }

    incomplete
}

/// Perform all integrity checks on a project directory.
/// Returns a list of human-readable messages ("INFO: ...", "ERROR: ...", "WARNING: ...").
pub fn perform_check(path: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut messages = Vec::new();

    let graph = aden_graph::cache::build_from_directory_cached(path)?;

    // Collect all anchors from the graph (store-first)
    let mut all_anchors: HashSet<String> = HashSet::new();
    for node_idx in graph.graph.node_indices() {
        let node = &graph.graph[node_idx];
        all_anchors.insert(node.anchor().to_string());
        if let Some(parsed) = &node.parsed {
            for anchor in &parsed.anchors {
                all_anchors.insert(anchor.clone());
            }
        }
    }

    // Walk the WHOLE tree for on-disk .adoc/.aden files, honoring
    // .adenignore/.adenallow plus aden's own scaffolding dirs. A previous version
    // scanned only `read_dir(path)` (the top level), so `check .` never saw refs
    // in subdirectories (e.g. docs/architecture.adoc) and reported "All <<refs>>
    // resolve" while a narrower `check docs` and `aden diagnose` flagged the same
    // broken refs — root-scope under-reporting. `.aden/` (store, overlays,
    // proposals) and `.agent/` (templates/scaffolding) are aden's own artifacts,
    // not project content to validate, so they are always skipped.
    let filter = aden_core::filter::AdenFilter::from_directory(path);
    let is_aden_artifact = |rel: &Path| -> bool {
        rel.components()
            .any(|c| matches!(c.as_os_str().to_str(), Some(".aden") | Some(".agent")))
    };
    let mut doc_files: Vec<PathBuf> = Vec::new();
    for entry in walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            e.path()
                .strip_prefix(path)
                .map(|rel| {
                    rel.as_os_str().is_empty()
                        || (!filter.should_skip(rel) && !is_aden_artifact(rel))
                })
                .unwrap_or(true)
        })
        .filter_map(|e| e.ok())
    {
        let p = entry.path();
        if p.is_file() {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext == "adoc" || ext == "aden" {
                doc_files.push(p.to_path_buf());
            }
        }
    }

    // Collect anchors defined in on-disk docs not yet in the store.
    for p in &doc_files {
        let mut text = String::new();
        std::fs::File::open(p)?.read_to_string(&mut text)?;
        all_anchors.extend(collect_anchors(&text));
    }

    // Check for unresolved refs across every on-disk doc.
    let mut unresolved = Vec::new();
    for p in &doc_files {
        let mut text = String::new();
        std::fs::File::open(p)?.read_to_string(&mut text)?;
        for line in text.lines() {
            for r in find_refs(line) {
                if !all_anchors.contains(&r) {
                    unresolved.push(format!("{}: unresolved <<{}>>", p.display(), r));
                }
            }
        }
    }

    if unresolved.is_empty() {
        messages.push("INFO: All <<refs>> resolve.".to_string());
    } else {
        // Cap at 5 per check to avoid flooding agent context; total count always shown.
        const UNRESOLVED_CAP: usize = 5;
        messages.push(format!(
            "ERROR: {} unresolved <<ref>>(s) found:",
            unresolved.len()
        ));
        for issue in unresolved.iter().take(UNRESOLVED_CAP) {
            messages.push(format!("ERROR: {}", issue));
        }
        if unresolved.len() > UNRESOLVED_CAP {
            messages.push(format!(
                "  ... and {} more unresolved ref(s)",
                unresolved.len() - UNRESOLVED_CAP
            ));
        }
    }

    let cycles = find_cycles(&graph);
    if cycles.is_empty() {
        messages.push("INFO: No include cycles detected.".to_string());
    } else {
        for cycle in &cycles {
            messages.push(format!("ERROR: Cycle detected: {}", cycle.join(" -> ")));
        }
    }

    // Many orphans are EXPECTED: doc-heading anchors (aden://doc/...) and
    // standalone metadata docs (ADR/plan/use-case/readme/agent) legitimately
    // have no graph edges — they are reference material, not dead code. Only
    // flag *actionable* orphans (code symbols/modules that should be connected)
    // as a WARNING, so the real signal is not buried under hundreds of expected
    // metadata nodes. The rest are reported as a quiet count. Classification is
    // shared with `status`/`quick_health_score` via `classify_orphans`.
    let (expected, actionable) = classify_orphans(&graph);

    if actionable.is_empty() {
        if expected.is_empty() {
            messages.push("INFO: No orphan documents.".to_string());
        } else {
            messages.push(format!(
                "INFO: No actionable orphans ({} expected metadata doc(s) have no edges, which is normal).",
                expected.len()
            ));
        }
    } else {
        // Summarize rather than emit one warning per orphan — a large repo can
        // have hundreds, which buries the rest of the check output.
        const ORPHAN_SAMPLE: usize = 10;
        messages.push(format!(
            "WARNING: {} actionable orphan symbol(s) with no edges (run 'aden heal . --gc' to remove if deleted):",
            actionable.len()
        ));
        for o in actionable.iter().take(ORPHAN_SAMPLE) {
            messages.push(format!("  - {}", o));
        }
        if actionable.len() > ORPHAN_SAMPLE {
            messages.push(format!(
                "  ... and {} more",
                actionable.len() - ORPHAN_SAMPLE
            ));
        }
        if !expected.is_empty() {
            messages.push(format!(
                "INFO: (plus {} expected metadata doc(s) with no edges — normal)",
                expected.len()
            ));
        }
    }

    let hash_issues = check_hashes(&graph, &find_project_root(path));
    if hash_issues.is_empty() {
        messages.push("INFO: All source_hash values valid.".to_string());
    } else {
        for (anchor, msg) in &hash_issues {
            messages.push(format!("{} (anchor: {})", msg, anchor));
        }
    }

    let edge_issues = graph.validate_typed_edges();
    if edge_issues.is_empty() {
        messages.push("INFO: All typed edges valid.".to_string());
    } else {
        for issue in edge_issues {
            messages.push(format!("ERROR: {}", issue));
        }
    }

    let incomplete_contracts = check_incomplete_contracts(path);
    if incomplete_contracts.is_empty() {
        messages.push("INFO: All contracts complete.".to_string());
    } else {
        for msg in incomplete_contracts {
            messages.push(msg);
        }
    }

    Ok(messages)
}

/// Collect store-backed contracts as `(synthetic_path, adoc_text)` entries.
///
/// `aden gen --auto` writes symbol contracts to the fjall store (only module
/// contracts land on disk), so without this the full-text index — and therefore
/// `search` and `ask` — would never see any code symbols on any project. We
/// re-emit each stored `Document` to AsciiDoc and feed it to the index.
fn collect_store_entries(path: &Path) -> Vec<(PathBuf, String)> {
    use aden_store::{GraphStorage, Storage};

    let (store_path, _) = aden_paths::resolve_read_store(&find_project_root(path));
    if !store_path.is_dir() {
        return Vec::new();
    }
    let Some(store_str) = store_path.to_str() else {
        return Vec::new();
    };
    let Ok(storage) = Storage::open_existing(store_str) else {
        return Vec::new();
    };
    let Ok(docs) = storage.get_all_documents() else {
        return Vec::new();
    };
    docs.into_values()
        .map(|doc| {
            // Use the recorded source file as the synthetic path when available
            // so snippets and locate-style lookups point at the real code.
            let synthetic = doc
                .attributes
                .get("source_file")
                .cloned()
                .unwrap_or_else(|| doc.anchor.clone());
            (PathBuf::from(synthetic), aden_emit::emit_document(&doc))
        })
        .collect()
}

/// Load the search index from disk cache, or build and cache it.
///
/// The index merges on-disk `.adoc`/`.aden`/`.txt` files with contracts kept in
/// the fjall store, so language-agnostic `gen --auto` output (which is
/// store-first) is fully searchable.
pub fn load_or_build_index(path: &Path) -> Result<aden_index::Index, Box<dyn std::error::Error>> {
    if let Some(cached) = aden_index::try_load(path) {
        return Ok(cached);
    }
    let mut index = aden_index::Index::from_directory(path)?;
    // Merge store-backed contracts (disk entries already ingested take priority).
    let store_entries = collect_store_entries(path);
    if !store_entries.is_empty() {
        index.ingest(store_entries);
        index.finalize();
    }
    let _ = aden_index::save(&index, path);
    Ok(index)
}

/// Compute a quick health score from drift events (consistent with heal report).
/// Classify an anchor as *expected* metadata that legitimately has no graph
/// edges. Doc-heading anchors (`aden://doc/...`) and standalone metadata docs
/// (ADR/plan/use-case/readme/agent) are reference material, not dead code, so
/// they are NOT actionable orphans.
///
/// Single source of truth for orphan classification — `status` and `check`
/// (`perform_check`) both route through it so they can never disagree on what
/// counts as a real orphan.
pub fn is_expected_metadata(anchor: &str) -> bool {
    anchor.starts_with("aden://doc/")
        || anchor.starts_with("adr-")
        || anchor.starts_with("plan-")
        || anchor.starts_with("use-case-")
        || anchor.starts_with("agent-")
        || anchor == "readme"
}

/// Partition a graph's orphans into `(expected_metadata, actionable)`.
/// Actionable orphans are code symbols/modules that should be connected but
/// aren't; expected ones are reference docs that normally have no edges.
pub fn classify_orphans(
    graph: &aden_graph::AdenGraph<aden_graph::DocumentNode, aden_graph::AdenEdge>,
) -> (Vec<String>, Vec<String>) {
    graph
        .orphans()
        .into_iter()
        .partition(|a| is_expected_metadata(a))
}

/// Health = fraction of nodes that are NOT actionable orphans.
pub fn health_score_from_graph(
    graph: &aden_graph::AdenGraph<aden_graph::DocumentNode, aden_graph::AdenEdge>,
) -> f64 {
    let total = graph.graph.node_count();
    if total == 0 {
        return 1.0;
    }
    let (_expected, actionable) = classify_orphans(graph);
    let connected = total.saturating_sub(actionable.len());
    connected as f64 / total as f64
}

pub fn quick_health_score(path: &Path) -> Result<f64, Box<dyn std::error::Error>> {
    // Use the graph-based approach so the health score agrees with `check` and
    // `status`: expected metadata docs (doc headings, ADRs, plans, etc.) are
    // NOT counted as unhealthy orphans. The heal-scanner approach counted all
    // OrphanAnchor events including the ~5000 expected metadata docs, producing
    // a permanently 0/100 score even on a fully-synced project.
    let graph = aden_graph::cache::build_from_directory_cached(path)?;
    Ok(health_score_from_graph(&graph))
}

/// Escape text for safe insertion into an AsciiDoc table cell.
/// Prevents injection of directives, includes, block terminators, and formatting.
pub fn escape_adoc_cell(text: &str) -> String {
    let mut out = text.replace('|', "{vbar}").replace(['\n', '\r'], " ");
    // Neutralize AsciiDoc directives and block terminators
    out = out.replace("include::", "[include blocked]");
    out = out.replace("ifdef::", "[ifdef blocked]");
    out = out.replace("ifndef::", "[ifndef blocked]");
    out = out.replace("----", "[---- blocked]");
    out = out.replace("++++", "[++++ blocked]");
    out = out.replace("|===", "[table blocked]");
    out
}

/// Validate that an identifier is safe for filesystem and URL usage.
/// Rejects empty strings, paths with path separators, dots (directory traversal), and non-ASCII.
pub fn is_safe_id(id: &str) -> bool {
    if id.len() < 3 || id.len() > 128 {
        return false;
    }
    id.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Generate a unique proposal ID from PID and nanosecond timestamp.
pub fn generate_proposal_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id();
    format!("{pid}-{ts}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_terminal_neutralizes_escapes_keeps_text() {
        // ESC, BEL, and an OSC-52 clipboard sequence must be rendered visible.
        let evil = "\x1b[2Jclear\x07bell\x1b]52;c;cG93d2540\x07";
        let out = sanitize_terminal(evil);
        assert!(!out.contains('\x1b'), "raw ESC must be gone: {out:?}");
        assert!(!out.contains('\x07'), "raw BEL must be gone: {out:?}");
        assert!(out.contains("\\x1b"), "ESC shown as \\x1b: {out:?}");
        assert!(
            out.contains("clear") && out.contains("bell"),
            "real text kept"
        );
        // Tabs are preserved; ordinary unicode passes through.
        assert_eq!(sanitize_terminal("a\tb→c"), "a\tb→c");
    }

    #[test]
    fn parse_edge_types_validated_accepts_valid_list() {
        let out = parse_edge_types_validated("uses, calls").expect("valid input");
        assert_eq!(
            out,
            vec![aden_core::EdgeType::Uses, aden_core::EdgeType::Calls]
        );
    }

    #[test]
    fn parse_edge_types_validated_rejects_unknown_token() {
        let err = parse_edge_types_validated("uses,garbage").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("invalid edge type: 'garbage'"), "got: {msg}");
        assert!(msg.contains("Valid:"), "must list valid types: {msg}");
    }

    #[test]
    fn parse_edge_types_validated_rejects_all_empty() {
        // The previous silent-swallow path returned an empty vec here, which the
        // assembler treats as "follow all edges". Now it must be an error.
        assert!(parse_edge_types_validated("garbage,nonsense").is_err());
        assert!(parse_edge_types_validated("").is_err());
        assert!(parse_edge_types_validated(",,").is_err());
    }

    #[test]
    fn sanitize_source_file_strips_against_root_not_cwd() {
        use std::collections::HashMap;
        let mut attrs = HashMap::new();
        attrs.insert(
            "source_file".to_string(),
            "/home/someone/projects/widget/src/lib.rs".to_string(),
        );
        let mut doc = aden_core::Document {
            anchor: "x".into(),
            node_type: aden_core::NodeType::Function,
            attributes: attrs,
            blocks: vec![],
            source_span: None,
            metadata: None,
            confidence: 1.0,
        };
        sanitize_source_file(&mut doc, Path::new("/home/someone/projects/widget"));
        assert_eq!(
            doc.attributes.get("source_file").map(|s| s.as_str()),
            Some("src/lib.rs"),
            "must strip against the given root, leaking no /home/ prefix"
        );
    }
}
