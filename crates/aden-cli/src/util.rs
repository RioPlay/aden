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

use aden_emit::check::{collect_anchors, collect_refs};
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
    Ok(discover_file_dispositions(root)?
        .into_iter()
        .filter(|file| file.disposition.is_indexed())
        .map(|file| file.path)
        .collect())
}

/// A discovered file plus the reason it will or will not be emitted. Unlike
/// [`discover_source_files`], this deliberately retains ignored and unsupported
/// files so generation can persist a complete coverage manifest.
#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    pub path: PathBuf,
    pub disposition: aden_core::filter::FileDisposition,
}

pub fn discover_file_dispositions(
    root: &Path,
) -> Result<Vec<DiscoveredFile>, Box<dyn std::error::Error>> {
    use std::collections::HashSet;

    let supported: HashSet<&'static str> = aden_parse::supported_extensions().into_iter().collect();
    let filter = aden_core::filter::AdenFilter::from_directory(root);
    let mut files = Vec::new();
    walk_files_with_dispositions(root, root, &supported, &filter, &mut files)?;
    Ok(files)
}

fn walk_files_with_dispositions(
    dir: &Path,
    root: &Path,
    supported: &std::collections::HashSet<&'static str>,
    filter: &aden_core::filter::AdenFilter,
    out: &mut Vec<DiscoveredFile>,
) -> Result<(), Box<dyn std::error::Error>> {
    for entry in std::fs::read_dir(dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            // A directory-level ignore is recorded by policy, but walking its
            // entire contents (notably .git/, target/, and node_modules/) would
            // turn a coverage receipt into an unbounded scan. Ordinary ignored
            // files are still retained below with their exact disposition.
            if path
                .strip_prefix(root)
                .ok()
                .is_some_and(|relative| filter.should_skip(relative))
            {
                continue;
            }
            walk_files_with_dispositions(&path, root, supported, filter, out)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        let supported_file = match path.extension().and_then(|ext| ext.to_str()) {
            Some(ext) => supported.contains(ext),
            None => path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| EXTENSIONLESS_SOURCE_FILES.contains(&name)),
        };
        out.push(DiscoveredFile {
            path,
            disposition: aden_core::filter::FileDisposition::for_path(
                &rel,
                filter.should_skip(&rel),
                supported_file,
            ),
        });
    }
    Ok(())
}

/// Like [`discover_source_files`] but walks only `scope` (a directory at or
/// under `root`), while still resolving `.adenignore`/`.adenallow` and the
/// built-in ignore list relative to `root`. This lets `grep PATH` actually
/// narrow the search to a subtree instead of always scanning the whole project
/// (the `PATH` argument otherwise only picks the project root, so a scoped
/// search silently fanned out to every file). `scope` is expected to be a
/// directory; callers handle a single-file `PATH` themselves.
pub fn discover_source_files_scoped(
    scope: &Path,
    root: &Path,
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    use std::collections::HashSet;

    let supported: HashSet<&'static str> = aden_parse::supported_extensions().into_iter().collect();
    let filter = aden_core::filter::AdenFilter::from_directory(root);

    let mut files = Vec::new();
    walk_supported_files(scope, root, &supported, &filter, &mut files)?;

    // Prioritize files under a source-style directory so that, when a token
    // budget truncates generation, the most load-bearing code is processed
    // first. Covers: Cargo (`src/`), Go (`cmd/`, `pkg/`), Ruby/Rails (`app/`),
    // generic libraries (`lib/`, `source/`). Neutral across ecosystems.
    files.sort_by(|a, b| {
        let is_src_dir = |p: &PathBuf| {
            let s = normalize_sep(p);
            s.contains("/src/")
                || s.contains("/cmd/")
                || s.contains("/pkg/")
                || s.contains("/lib/")
                || s.contains("/app/")
                || s.contains("/source/")
        };
        is_src_dir(b).cmp(&is_src_dir(a))
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
            // Persist with forward slashes so stores built on Windows match the
            // `/`-normalized keys used by tree, grep, and impact-diff lookups.
            doc.attributes
                .insert("source_file".to_string(), rel.replace('\\', "/"));
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
#[cfg(feature = "watch")]
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

/// Impact edge types — a change to a symbol can break anything that references
/// it through one of these. The single shared SET behind every blast/impact
/// surface (`impact-diff`, `viz --mode blast|reach`, `query --impact`,
/// `understand`) so they can never drift; only the traversal DIRECTION differs
/// per consumer (blast radius walks incoming/dependents, reach walks
/// outgoing/dependencies).
///
/// Every member has a live emitter (ADR-007 §1: a filter must not name an
/// edge type that is never emitted — it reads like coverage while filtering
/// nothing). `Constrains` and `Invokes` were removed on those grounds; re-add
/// them WITH their emitter if one ever lands. `Tests` is deliberately NOT in
/// this set: every `Tests` edge is co-emitted with a `Calls` edge, so the
/// dependent traversal already reaches test symbols — they are surfaced
/// separately via `impact-diff`'s affected-tests section.
pub fn impact_edge_types() -> [aden_core::EdgeType; 4] {
    [
        aden_core::EdgeType::Uses,
        aden_core::EdgeType::Calls,
        aden_core::EdgeType::Implements,
        aden_core::EdgeType::Mutates,
    ]
}

/// Parse a single edge-type string into the corresponding enum variant (case-insensitive).
pub fn parse_single_edge_type(s: &str) -> Option<aden_core::EdgeType> {
    let lower = s.trim().to_lowercase();
    match lower.as_str() {
        "uses" => Some(aden_core::EdgeType::Uses),
        "implements" => Some(aden_core::EdgeType::Implements),
        "tests" => Some(aden_core::EdgeType::Tests),
        "documents" => Some(aden_core::EdgeType::Documents),
        "contains" => Some(aden_core::EdgeType::Contains),
        "constrains" => Some(aden_core::EdgeType::Constrains),
        "justifies" => Some(aden_core::EdgeType::Justifies),
        "invokes" => Some(aden_core::EdgeType::Invokes),
        "requires" => Some(aden_core::EdgeType::Requires),
        "mutates" => Some(aden_core::EdgeType::Mutates),
        "calls" => Some(aden_core::EdgeType::Calls),
        "supersedes" => Some(aden_core::EdgeType::Supersedes),
        "amends" => Some(aden_core::EdgeType::Amends),
        "verifies" => Some(aden_core::EdgeType::Verifies),
        "demonstrates" => Some(aden_core::EdgeType::Demonstrates),
        "mentions" => Some(aden_core::EdgeType::Mentions),
        "definesterm" => Some(aden_core::EdgeType::DefinesTerm),
        "associatedwith" => Some(aden_core::EdgeType::AssociatedWith),
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
        "contains",
        "constrains",
        "justifies",
        "invokes",
        "requires",
        "mutates",
        "calls",
        "supersedes",
        "amends",
        "verifies",
        "demonstrates",
        "mentions",
        "definesterm",
        "associatedwith",
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
/// A cache written by a different emission-logic version is discarded
/// wholesale (see [`crate::types::GEN_LOGIC_VERSION`]) so every file
/// reparses once and the store picks up newly-emitted edge kinds. The
/// returned cache is always stamped with the current version, so a
/// subsequent `save_gen_cache` persists it.
pub fn load_gen_cache(path: &Path) -> GenCache {
    let filter_fingerprint = aden_core::filter::built_in_ignore_fingerprint();
    let mut cache = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<GenCache>(&s).ok())
        .filter(|c| {
            c.version == crate::types::GEN_LOGIC_VERSION
                && c.filter_fingerprint == filter_fingerprint
        })
        .unwrap_or_default();
    cache.version = crate::types::GEN_LOGIC_VERSION;
    cache.filter_fingerprint = filter_fingerprint;
    cache
}

/// A cache logic/schema mismatch cannot be incrementally healed safely: the
/// old cache is the only ownership record for anchors emitted by files that no
/// longer exist. Callers must rebuild the store instead of silently discarding
/// that ownership map and leaving stale graph nodes behind.
pub fn gen_cache_requires_rebuild(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }
    match std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<GenCache>(&text).ok())
    {
        Some(cache) => {
            cache.version != crate::types::GEN_LOGIC_VERSION
                || cache.filter_fingerprint != aden_core::filter::built_in_ignore_fingerprint()
        }
        None => true,
    }
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
    // Phase 6 (provenance): surface the node's confidence so readers can weight
    // generated/derived content. Additive key — existing consumers are unaffected.
    if let Some(c) = serde_json::Number::from_f64(node.doc.confidence) {
        map.insert("confidence".to_string(), serde_json::Value::Number(c));
    }
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

/// Classify check messages by severity prefix.
pub fn classify_check_messages(messages: &[String]) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut info = Vec::new();
    for m in messages {
        if let Some(rest) = m.strip_prefix("ERROR: ") {
            errors.push(rest.to_string());
        } else if let Some(rest) = m.strip_prefix("WARNING: ") {
            warnings.push(rest.to_string());
        } else if let Some(rest) = m.strip_prefix("INFO: ") {
            info.push(rest.to_string());
        } else {
            info.push(m.clone());
        }
    }
    (errors, warnings, info)
}

#[derive(serde::Serialize)]
pub struct GateCounts {
    pub errors: usize,
    pub warnings: usize,
    pub info: usize,
}

#[derive(serde::Serialize)]
pub struct GateSummary {
    pub ok: bool,
    pub counts: GateCounts,
    pub top_issues: Vec<String>,
    pub truncated: bool,
}

/// Build a compact gate summary for MCP agents.
pub fn build_gate_summary(
    errors: &[String],
    warnings: &[String],
    info: &[String],
    ok: bool,
    max_issues: usize,
) -> GateSummary {
    let mut issues: Vec<String> = errors
        .iter()
        .map(|e| format!("ERROR: {e}"))
        .chain(warnings.iter().map(|w| format!("WARNING: {w}")))
        .collect();
    if issues.is_empty() {
        issues = info.iter().map(|i| format!("INFO: {i}")).collect();
    }
    let total = issues.len();
    let truncated = total > max_issues;
    let top_issues: Vec<String> = issues.into_iter().take(max_issues).collect();
    GateSummary {
        ok,
        counts: GateCounts {
            errors: errors.len(),
            warnings: warnings.len(),
            info: info.len(),
        },
        top_issues,
        truncated,
    }
}

pub fn gate_summary_line(summary: &GateSummary) -> String {
    format!(
        "Summary: ok={} | {} error(s), {} warning(s), {} info | showing {} issue(s){}",
        summary.ok,
        summary.counts.errors,
        summary.counts.warnings,
        summary.counts.info,
        summary.top_issues.len(),
        if summary.truncated {
            " (truncated)"
        } else {
            ""
        }
    )
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

    // Check for unresolved refs across every on-disk doc. `collect_refs` (not
    // a per-line `find_refs` loop) so delimited listing/literal blocks are
    // skipped — a previous version scanned line-by-line with no fence state,
    // flagging `<<x>>` examples inside ----/.... code blocks as broken refs.
    let mut unresolved = Vec::new();
    for p in &doc_files {
        let mut text = String::new();
        std::fs::File::open(p)?.read_to_string(&mut text)?;
        for r in collect_refs(&text) {
            if !all_anchors.contains(&r) {
                unresolved.push(format!("{}: unresolved <<{}>>", p.display(), r));
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

    // Documentation path references: catch prose docs that point at a repo file
    // which has MOVED or been renamed (the `aden://` anchor case is covered by
    // typed-edge/BrokenReference checks above; this covers raw path strings).
    let doc_path_issues = check_doc_path_references(&find_project_root(path));
    if doc_path_issues.is_empty() {
        messages.push("INFO: All documentation path references resolve.".to_string());
    } else {
        messages.extend(doc_path_issues);
    }

    Ok(messages)
}

/// Scan prose docs (`.md`/`.adoc`) for *link references* that point at a repo
/// file which no longer exists — i.e. documentation linking to a file that moved
/// or was renamed. (References by `aden://` anchor are covered by the typed-edge /
/// BrokenReference checks above; this covers path links in prose.)
///
/// Only genuine LINK constructs are checked — markdown `[text](path)` and adoc
/// `xref:`/`link:`/`include::` — so an illustrative backtick mention of a path
/// (e.g. a scaffolded `src/main.rs`, or a command's example `--out` target) is
/// NOT flagged. AsciiDoc passthrough (`+...+`) and monospace spans are stripped
/// before matching so syntax examples like `` `xref:file.adoc#frag` `` or
/// `+xref:other.adoc#section[label]+` do not fire. A target is resolved
/// relative to the doc's own directory, or to the repo root when it starts with a
/// real top-level directory. URLs, anchors, globs, and placeholders are
/// skipped. Lines with `aden:allow-path` are exempt.
fn check_doc_path_references(root: &Path) -> Vec<String> {
    use std::collections::HashSet;

    let top_dirs: HashSet<String> = match std::fs::read_dir(root) {
        Ok(rd) => rd
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| !n.starts_with('.') && n != "target" && n != "node_modules")
            .collect(),
        Err(_) => return Vec::new(),
    };

    let doc_exts: HashSet<&'static str> = ["md", "markdown", "adoc", "asciidoc", "asc"]
        .into_iter()
        .collect();
    let filter = aden_core::filter::AdenFilter::from_directory(root);
    let mut docs = Vec::new();
    let _ = walk_supported_files(root, root, &doc_exts, &filter, &mut docs);

    // markdown `](target)`  and  adoc `xref:`/`link:`/`include::` target.
    let md_link = regex::Regex::new(r"\]\(([^)\s]+)\)").expect("valid regex");
    let adoc_link =
        regex::Regex::new(r"(?:xref:|link:|include::)([^\[\s\]]+)").expect("valid regex");

    let mut findings: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for doc in &docs {
        let Ok(content) = std::fs::read_to_string(doc) else {
            continue;
        };
        let rel_doc = doc
            .strip_prefix(root)
            .unwrap_or(doc)
            .to_string_lossy()
            .to_string();
        let doc_dir = doc.parent().unwrap_or(root);
        let mut in_fence = false;
        for (i, line) in content.lines().enumerate() {
            let t = line.trim_start();
            if t.starts_with("```") || t == "----" || t == "...." {
                in_fence = !in_fence;
                continue;
            }
            if in_fence || line.contains("aden:allow-path") {
                continue;
            }
            let scan_line = strip_illustrative_spans(line);
            let targets = md_link
                .captures_iter(&scan_line)
                .chain(adoc_link.captures_iter(&scan_line))
                .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()));
            for target in targets {
                if let Some(resolved) = resolve_doc_link(&target, doc_dir, root, &top_dirs)
                    && !resolved.exists()
                {
                    let key = format!("{rel_doc}|{target}");
                    if seen.insert(key) {
                        findings.push(format!(
                            "ERROR: {}:{} link references missing path '{}' (moved/renamed?)",
                            rel_doc,
                            i + 1,
                            target
                        ));
                    }
                }
            }
        }
    }
    findings.sort();
    findings
}

/// Strip monospace and AsciiDoc passthrough spans so illustrative syntax
/// examples are not mistaken for live link constructs during path checking.
fn strip_illustrative_spans(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    let mut in_mono = false;
    let mut in_pass = false;
    while let Some(c) = chars.next() {
        if c == '`' && !in_pass {
            if chars.peek() == Some(&'`') {
                chars.next();
                while let Some(ch) = chars.next() {
                    if ch == '`' && chars.peek() == Some(&'`') {
                        chars.next();
                        break;
                    }
                }
                continue;
            }
            in_mono = !in_mono;
            continue;
        }
        if c == '+' && !in_mono {
            in_pass = !in_pass;
            continue;
        }
        if !in_mono && !in_pass {
            out.push(c);
        }
    }
    out
}

/// Resolve a doc link target to a filesystem path to existence-check, or `None`
/// when it isn't a repo-relative file link (URL, anchor-only, glob, placeholder,
/// extension-less). Root-relative when it starts with a top-level dir; otherwise
/// relative to the linking doc's directory.
fn resolve_doc_link(
    target: &str,
    doc_dir: &Path,
    root: &Path,
    top_dirs: &std::collections::HashSet<String>,
) -> Option<PathBuf> {
    // Drop any `#anchor` fragment; an anchor-only link has no file part.
    let path = target.split('#').next().unwrap_or(target);
    if path.is_empty()
        || path.contains("://")
        || path.starts_with("http")
        || path.starts_with("mailto:")
        || path.starts_with('~')
        || path.starts_with('/')
        || path.chars().any(|c| "{}$*?<>\"`|".contains(c))
    {
        return None;
    }
    // Must look like a file (short alphanumeric extension on the last segment).
    let last = path.rsplit('/').next().unwrap_or(path);
    let ext_ok = last
        .rsplit_once('.')
        .map(|(stem, ext)| {
            !stem.is_empty()
                && (1..=8).contains(&ext.len())
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
        })
        .unwrap_or(false);
    if !ext_ok {
        return None;
    }
    // Root-relative if it starts with a real top-level dir, else relative to the
    // doc. `..` segments are resolved by the OS during the existence check.
    let first = path.split('/').next().unwrap_or(path);
    if top_dirs.contains(first) {
        Some(root.join(path))
    } else {
        Some(doc_dir.join(path))
    }
}

/// Collect store-backed contracts as `(synthetic_path, adoc_text)` entries.
///
/// `aden gen --auto` writes symbol contracts to the fjall store (only module
/// contracts land on disk), so without this the full-text index — and therefore
/// `search` and `ask` — would never see any code symbols on any project. We
/// re-emit each stored `Document` to AsciiDoc and feed it to the index.
fn collect_store_entries(path: &Path) -> Vec<(PathBuf, String)> {
    use aden_store::{GraphStorage, Storage};

    let root = find_project_root(path);
    let (store_path, _) = aden_paths::resolve_read_store(&root);
    if !store_path.is_dir() {
        return Vec::new();
    }
    let Some(store_str) = store_path.to_str() else {
        return Vec::new();
    };

    // ADR-011: snapshot-first (via helper) so search/ask don't open fjall
    // while gen/heal/merge writers are active.
    if let Some((docs_map, _)) = aden_graph::snapshot::try_read_fresh(&root) {
        let mut docs: Vec<_> = docs_map.into_values().collect();
        docs.sort_by(|a, b| {
            a.anchor.cmp(&b.anchor).then_with(|| {
                a.attributes
                    .get("source_file")
                    .cmp(&b.attributes.get("source_file"))
            })
        });
        return docs
            .into_iter()
            .map(|doc| {
                let synthetic = doc
                    .attributes
                    .get("source_file")
                    .cloned()
                    .unwrap_or_else(|| doc.anchor.clone());
                (PathBuf::from(synthetic), index_text(&doc))
            })
            .collect();
    }

    let Ok(storage) = Storage::open_existing(store_str) else {
        return Vec::new();
    };
    let Ok(docs) = storage.get_all_documents() else {
        return Vec::new();
    };
    // `get_all_documents` returns a HashMap → `into_values()` yields a NON-deterministic
    // order. That order feeds the index build and decides anchor-collision winners (two
    // files can share a basename-anchor, e.g. `router_test.go#TestRouter`), perturbing
    // the index and making retrieval non-reproducible. Sort by (anchor, source_file) so
    // the build — and the collision winner — is identical regardless of store order.
    let mut docs: Vec<_> = docs.into_values().collect();
    docs.sort_by(|a, b| {
        a.anchor.cmp(&b.anchor).then_with(|| {
            a.attributes
                .get("source_file")
                .cmp(&b.attributes.get("source_file"))
        })
    });
    docs.into_iter()
        .map(|doc| {
            // Use the recorded source file as the synthetic path when available
            // so snippets and locate-style lookups point at the real code.
            let synthetic = doc
                .attributes
                .get("source_file")
                .cloned()
                .unwrap_or_else(|| doc.anchor.clone());
            (PathBuf::from(synthetic), index_text(&doc))
        })
        .collect()
}

/// Emit a store `Document` as AsciiDoc for the search index, with *volatile* metadata
/// stripped so the index is reproducible. `:last-verified:` carries a wall-clock
/// timestamp that differs every run; indexing it both pollutes retrieval with date
/// tokens and makes the index non-deterministic. (It stays in the on-disk/store
/// contract — this only affects what gets tokenised into the index.)
fn index_text(doc: &aden_core::Document) -> String {
    aden_emit::emit_document(doc)
        .lines()
        .filter(|l| !l.trim_start().starts_with(":last-verified:"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Load the search index from disk cache, or build and cache it.
///
/// The index merges on-disk `.adoc`/`.aden`/`.txt` files with contracts kept in
/// the fjall store, so language-agnostic `gen --auto` output (which is
/// store-first) is fully searchable.
pub fn load_or_build_index(path: &Path) -> Result<aden_index::Index, Box<dyn std::error::Error>> {
    if let Some(mut cached) = aden_index::try_load(path) {
        // A cache built before a model was available carries no embeddings; fill
        // them in now (and re-save) so hybrid retrieval activates without a full
        // rebuild. No-op when the `dense` feature is off or no model is present.
        if maybe_embed(&mut cached, path) {
            let _ = aden_index::save(&cached, path);
        }
        return Ok(cached);
    }
    let mut index = aden_index::Index::from_directory(path)?;
    // Merge store-backed contracts (disk entries already ingested take priority).
    let store_entries = collect_store_entries(path);
    if !store_entries.is_empty() {
        let mut seeded = HashSet::new();
        for (source, _) in &store_entries {
            let relative = source.strip_prefix("./").unwrap_or(source);
            if !seeded.insert(relative.to_path_buf()) {
                continue;
            }
            let full = if relative.is_absolute() {
                relative.to_path_buf()
            } else {
                path.join(relative)
            };
            if let Ok(text) = std::fs::read_to_string(full) {
                index.ingest_file_evidence(relative.to_path_buf(), &text);
            }
        }
        index.ingest(store_entries);
        index.finalize();
    }
    maybe_embed(&mut index, path);
    let _ = aden_index::save(&index, path);
    Ok(index)
}

/// Populate dense embeddings on the index when the `dense` feature is enabled and
/// a local model is available. Returns whether anything was added (so the caller
/// can persist). A no-op (returns false) otherwise — keeping the default,
/// model-free build pure BM25.
#[cfg(feature = "dense")]
fn maybe_embed(index: &mut aden_index::Index, path: &Path) -> bool {
    if index.has_embeddings() {
        return false;
    }
    match dense_embedder() {
        Some(emb) => {
            // Reuse the content-addressed cache that survives `gen`'s cache wipe,
            // so a reindex re-embeds only changed documents, not the whole corpus.
            let mut cache = aden_index::load_embedding_cache(path);
            index.embed_documents_cached(emb, &mut cache);
            let _ = aden_index::save_embedding_cache(&cache, path);
            index.has_embeddings()
        }
        None => false,
    }
}

#[cfg(not(feature = "dense"))]
fn maybe_embed(_index: &mut aden_index::Index, _path: &Path) -> bool {
    false
}

/// Per-user cache root, platform-native via `dirs::cache_dir()`:
/// `%LOCALAPPDATA%` on Windows, `~/Library/Caches` on macOS, `$XDG_CACHE_HOME`
/// (or `~/.cache`) on Linux. Falls back to `~/.cache` only if `cache_dir()` is
/// unavailable. Aden's downloadable/rebuildable per-user assets (the embedding
/// model, the OEWN lexicon store) live under here.
fn user_cache_root() -> std::path::PathBuf {
    dirs::cache_dir().unwrap_or_else(legacy_cache_root)
}

/// The pre-migration location these assets used on every OS: `~/.cache`. On
/// Linux with default XDG settings this equals `user_cache_root()` (so the
/// migration is a no-op there); when `XDG_CACHE_HOME` is customized — or on
/// Windows/macOS — the two diverge and this is read as a non-destructive fallback
/// (via `prefer_native`) so an install that already populated `~/.cache` keeps
/// working without re-downloading.
fn legacy_cache_root() -> std::path::PathBuf {
    dirs::home_dir().unwrap_or_default().join(".cache")
}

/// Choose `native` unless it is absent and `legacy` is present — a
/// non-destructive cache migration: fresh writes go to the native location, but
/// an existing legacy install is honored in place. `present` reports whether a
/// candidate is materialized; it is injected so the choice is unit-testable
/// without touching the filesystem.
fn prefer_native(
    native: std::path::PathBuf,
    legacy: std::path::PathBuf,
    present: impl Fn(&std::path::Path) -> bool,
) -> std::path::PathBuf {
    if present(&native) {
        native
    } else if present(&legacy) {
        legacy
    } else {
        native
    }
}

/// Resolve the local bge embedding model directory. `ADEN_BGE_MODEL_DIR` wins;
/// otherwise the platform-native cache location (`user_cache_root()`), with the
/// legacy `~/.cache` path honored in place if that is where the model already
/// lives (presence keyed on `model.onnx`). A fresh `aden model fetch` therefore
/// lands in the native location while existing installs are not re-downloaded.
#[cfg(any(feature = "dense", feature = "model-fetch"))]
pub fn bge_model_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("ADEN_BGE_MODEL_DIR") {
        return std::path::PathBuf::from(dir);
    }
    const REL: &str = "aden-models/bge-small-en-v1.5";
    prefer_native(
        user_cache_root().join(REL),
        legacy_cache_root().join(REL),
        |p| p.join("model.onnx").exists(),
    )
}

/// Lazily construct and cache the local embedding model (loaded once per process).
/// Reads the model from `bge_model_dir()`. Returns `None` (degrading to BM25) when
/// the model is absent or fails to load, printing a one-line hint on how to fetch
/// it so the degrade is discoverable rather than silent.
#[cfg(feature = "dense")]
fn dense_embedder() -> Option<&'static aden_index::TractEmbedder> {
    use std::sync::OnceLock;
    static EMBEDDER: OnceLock<Option<aden_index::TractEmbedder>> = OnceLock::new();
    EMBEDDER
        .get_or_init(|| {
            let dir = bge_model_dir();
            if !dir.join("model.onnx").exists() {
                eprintln!(
                    "aden: dense feature enabled but no embedding model at {}; using BM25.\n      \
                     Fetch it with `aden model fetch` (build --features model-fetch) or \
                     scripts/fetch-bge-model.sh, or set ADEN_BGE_MODEL_DIR.",
                    dir.display()
                );
                return None;
            }
            match aden_index::TractEmbedder::from_dir(&dir) {
                Ok(e) => Some(e),
                Err(e) => {
                    eprintln!("aden: dense embeddings unavailable ({e}); using BM25");
                    None
                }
            }
        })
        .as_ref()
}

/// Format a search score for the text table. BM25 scores are large (hundreds to
/// thousands) and read fine at one decimal, but hybrid RRF fused scores are tiny
/// (~`1/(60+rank)` ≈ 0.03) and a fixed `{:.1}` floored them to `0.0`. Pick the
/// precision by magnitude so both are legible.
pub fn fmt_score(score: f64) -> String {
    if score == 0.0 {
        "0".to_string()
    } else if score.abs() >= 1.0 {
        format!("{score:.1}")
    } else {
        format!("{score:.4}")
    }
}

/// Cached handle to the OEWN lexical overlay store (opened once per process). `None` if the
/// store is absent. `$ADEN_LEXICON_STORE` else the platform-native per-user cache dir
/// (`user_cache_root()/aden/lexicon`), honoring an existing legacy `~/.cache/aden/lexicon` in place.
fn lexicon_store() -> Option<&'static aden_store::Storage> {
    use std::sync::OnceLock;
    static STORE: OnceLock<Option<aden_store::Storage>> = OnceLock::new();
    STORE
        .get_or_init(|| {
            let path = std::env::var("ADEN_LEXICON_STORE").unwrap_or_else(|_| {
                // Platform-native cache dir (Win %LOCALAPPDATA%, macOS ~/Library/
                // Caches, Linux ~/.cache), with a legacy ~/.cache store honored in
                // place — non-destructive migration.
                const REL: &str = "aden/lexicon";
                prefer_native(
                    user_cache_root().join(REL),
                    legacy_cache_root().join(REL),
                    |p| p.exists(),
                )
                .to_string_lossy()
                .into_owned()
            });
            aden_store::Storage::open_existing(&path).ok()
        })
        .as_ref()
}

/// Expand `query` with grounded OEWN `SynonymOf` neighbours (the dictionary-for-prose lever).
/// Each candidate synonym is kept only if it tokenizes entirely into the corpus vocabulary, so
/// the dictionary only ever adds words the corpus actually uses (a no-op on code vocab it lacks,
/// decisive on prose). Returns `None` when the lexicon is absent or nothing grounds. Validated
/// on prose: BM25 R@1 1/42 -> 41/42, and complementary to dense (dense alone 20/42, +OEWN 41/42).
fn expand_query_with_lexicon(index: &aden_index::Index, query: &str) -> Option<String> {
    use aden_store::GraphStorage as _;
    let store = lexicon_store()?;
    // A synonym is kept only if it (a) tokenizes entirely into the corpus vocabulary
    // AND (b) at least one of its tokens is *discriminative* (document frequency in the
    // narrowing band). (b) is the fix for the external regression: grounding on mere
    // presence let common-word synonyms ("change"->"alteration", "work"->"job") in, and
    // those high-frequency terms scattered the ranking. The DF band drops them at the
    // source, so expansion can only ever add terms that actually narrow the result set.
    let grounded = |lemma: &str| {
        let toks = aden_index::tokenize(lemma);
        !toks.is_empty()
            && toks.iter().all(|t| index.knows_term(t))
            && toks.iter().any(|t| index.term_is_discriminative(t))
    };
    let mut adds: Vec<String> = Vec::new();
    for w in query
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter(|w| w.len() >= 3)
        .map(str::to_ascii_lowercase)
    {
        let Ok(edges) = store.get_outgoing_edges(&format!("aden://term/oewn/{w}")) else {
            continue;
        };
        let mut added = 0;
        for (tgt, et) in edges {
            if !matches!(et, aden_core::EdgeType::SynonymOf) {
                continue;
            }
            let lemma = tgt.rsplit('/').next().unwrap_or(&tgt).to_string();
            if lemma.contains(' ') || adds.contains(&lemma) || !grounded(&lemma) {
                continue;
            }
            adds.push(lemma);
            added += 1;
            if added >= 8 {
                break;
            }
        }
    }
    (!adds.is_empty()).then(|| format!("{query} {}", adds.join(" ")))
}

/// Heuristic: does the query look like code (identifiers/operators) rather than prose? Flags
/// snake_case, camelCase, `::`, paths, and code punctuation; plain English words do not trigger.
/// Used by auto-gating to choose between the prose lever (expansion) and the code lever (rerank).
fn query_looks_codey(query: &str) -> bool {
    query.split_whitespace().any(|t| {
        t.contains('_')
            || t.contains("::")
            || t.contains("->")
            || t.contains('(')
            || t.contains('/')
            || t.chars()
                .any(|c| matches!(c, '{' | '}' | ')' | '<' | '>' | '=' | ';' | '[' | ']'))
            || has_camel_hump(t)
    })
}

/// True if `t` has a lowercase to uppercase transition (a camelCase hump), e.g. `putEdges`.
fn has_camel_hump(t: &str) -> bool {
    let b = t.as_bytes();
    (1..b.len()).any(|i| b[i - 1].is_ascii_lowercase() && b[i].is_ascii_uppercase())
}

/// Run a search query against the index, using hybrid (dense + BM25 via RRF)
/// retrieval when embeddings are present and a model is loaded, else pure BM25.
/// This is the single entry point all `search`/`ask` paths should use so routing
/// stays consistent.
///
/// Dual-substrate retrieval levers (validated by the lexical ablations):
/// - PROSE lever: ground-and-append OEWN synonyms before retrieval (prose R@1 1/42 -> 41/42).
/// - CODE lever: PPMI rerank of the top window by corpus co-occurrence (code MRR 0.216 -> 0.289).
///
/// Auto-gating is OPT-IN (`ADEN_LEXICON_ON`), NOT on by default. The dual-substrate levers were
/// originally validated on engineered probes (single-word, zero-vocabulary-overlap queries) and an
/// in-process fixture harness, where each lever wins big. On the PRODUCT path over EXTERNAL repos
/// with NATURAL multi-word queries they do NOT win — measured neutral-to-negative on every corpus
/// (rustfmt/Go/flask/TS/prose) — because real queries already share vocabulary with the target, so
/// expansion only injects noise. The previous ON-BY-DEFAULT setting therefore shipped a net
/// regression (e.g. prose MRR 0.336->0.166, tanstack 0.104->0.007 on the same base). Default is now
/// the baseline ranking (best of everything tested); the levers are opt-in and, when enabled, run
/// behind the additive guard below so they cannot crater retrieval the way the blind-replace design
/// did. `ADEN_LEXICON_OFF` still force-disables; `ADEN_LEXICON_EXPAND` / `ADEN_PPMI_RERANK` still
/// force an individual lever on. Re-enabling by default must be gated on the EXTERNAL A/B harness
/// (`scripts/lexicon_ab_bench.py`), not the in-tree fixtures.
pub fn query_index(index: &aden_index::Index, query: &str) -> Vec<aden_index::SearchResult> {
    let auto = std::env::var_os("ADEN_LEXICON_ON").is_some()
        && std::env::var_os("ADEN_LEXICON_OFF").is_none();
    let codey = query_looks_codey(query);

    // The baseline (unexpanded) ranking is ALWAYS computed and is the safety floor:
    // the lexicon levers may only FUSE INTO it (base-weighted) or reorder its top-K,
    // never replace it. The previous design rewrote the query and ran a SINGLE pass,
    // so a misfiring expansion could evict the right result entirely — which it did on
    // every external corpus measured. Retaining the base guarantees retrieval cannot
    // drop below plain BM25/hybrid on any query.
    let base = run_base(index, query);

    // Expansion (prose lever): only when the query is non-code-shaped. Fuse the expanded
    // ranking into the base with the base up-weighted, so even a surviving noisy synonym
    // can nudge the tail but cannot displace a confident base hit.
    let do_expand = std::env::var_os("ADEN_LEXICON_EXPAND").is_some() || (auto && !codey);
    let ranked = if do_expand {
        match expand_query_with_lexicon(index, query) {
            Some(expanded) => fuse_base_weighted(base, run_base(index, &expanded)),
            None => base,
        }
    } else {
        base
    };

    // PPMI rerank (code lever): restricted to genuinely code-shaped queries (or the
    // explicit override). The old blanket `code_anchor_fraction >= 0.5` trigger fired the
    // rerank on natural-language-over-code queries too, where it regressed externally;
    // the validated win was on code-shaped queries, so gate on exactly that.
    let do_rerank = std::env::var_os("ADEN_PPMI_RERANK").is_some() || (auto && codey);
    if do_rerank {
        index.ppmi_rerank(query, ranked, 50)
    } else {
        ranked
    }
}

/// Deterministic recall backstop for prose-heavy candidate sets.
///
/// Native BM25/hybrid remains the only retrieval pass for code. When at least
/// 80% of the first ten classifiable candidates are prose documents, a cheap
/// whole-file lexical ranking participates in winner selection. Only a single
/// existing candidate can be promoted; all remaining native order is preserved,
/// so deep recall cannot be scattered. `ADEN_NAV_FUSION_OFF=1` disables the gate.
pub fn query_index_with_navigation(
    index: &aden_index::Index,
    query: &str,
    root: &Path,
) -> Vec<aden_index::SearchResult> {
    let base = query_index(index, query);
    if std::env::var_os("ADEN_NAV_FUSION_OFF").is_some()
        || !results_are_predominantly_prose(&base)
        || results_are_chronological(&base)
        || native_top_file_consensus(&base, root) >= 3
    {
        return base;
    }
    fuse_conventional_file_rank(index, base, query, root)
}

fn results_are_predominantly_prose(results: &[aden_index::SearchResult]) -> bool {
    let mut votes = Vec::new();
    for result in results.iter().take(10) {
        let ext = result
            .source_path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if result.anchor.starts_with("aden://doc/")
            || matches!(ext.as_str(), "adoc" | "aden" | "md" | "rst" | "txt")
        {
            votes.push(true);
        } else if result.anchor.starts_with("aden://module/")
            || result.anchor.starts_with("aden://symbol/")
            || matches!(
                ext.as_str(),
                "rs" | "go" | "py" | "ts" | "tsx" | "js" | "jsx" | "cs" | "c" | "h"
            )
        {
            votes.push(false);
        }
    }
    !votes.is_empty() && votes.iter().filter(|vote| **vote).count() * 5 >= votes.len() * 4
}

fn native_top_file_consensus(results: &[aden_index::SearchResult], root: &Path) -> usize {
    let Some(top) = results.first() else {
        return 0;
    };
    let top_file = source_key(&top.source_path, root);
    if top_file.is_empty() {
        return 0;
    }
    results
        .iter()
        .take(5)
        .filter(|result| source_key(&result.source_path, root) == top_file)
        .count()
}

fn results_are_chronological(results: &[aden_index::SearchResult]) -> bool {
    let mut votes = 0;
    let mut dated = 0;
    for result in results.iter().take(10) {
        let Some(stem) = result
            .source_path
            .file_stem()
            .and_then(|value| value.to_str())
        else {
            continue;
        };
        votes += 1;
        let bytes = stem.as_bytes();
        if bytes.len() == 10
            && bytes[4] == b'-'
            && bytes[7] == b'-'
            && bytes
                .iter()
                .enumerate()
                .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
        {
            dated += 1;
        }
    }
    votes > 0 && dated * 5 >= votes * 4
}

fn source_key(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .trim_start_matches("./")
        .to_ascii_lowercase()
}

fn navigation_terms(query: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "add", "allow", "and", "better", "change", "code", "does", "error", "file", "fix",
        "fixing", "for", "from", "function", "handle", "into", "make", "method", "more", "not",
        "only", "same", "should", "support", "that", "the", "this", "use", "using", "value",
        "when", "where", "which", "with",
    ];
    let mut terms: Vec<String> = query
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .map(str::to_ascii_lowercase)
        .filter(|term| term.len() >= 3 && !STOP.contains(&term.as_str()))
        .collect();
    terms.sort();
    terms.dedup();
    terms.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    terms.truncate(12);
    terms
}

fn conventional_file_rank(root: &Path, query: &str) -> Vec<String> {
    let terms = navigation_terms(query);
    if terms.is_empty() {
        return Vec::new();
    }
    let Ok(files) = discover_source_files(root) else {
        return Vec::new();
    };
    let mut scored = Vec::new();
    for file in files {
        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };
        let content = content.to_ascii_lowercase();
        let key = source_key(&file, root);
        let path_words: HashSet<&str> = key
            .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
            .collect();
        let distinct = terms
            .iter()
            .filter(|term| content.contains(term.as_str()))
            .count();
        if distinct == 0 {
            continue;
        }
        let path_hits = terms
            .iter()
            .filter(|term| path_words.contains(term.as_str()))
            .count();
        let occurrences: usize = terms
            .iter()
            .map(|term| content.match_indices(term.as_str()).count())
            .sum();
        scored.push((key, distinct, path_hits, occurrences));
    }
    scored.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| b.3.cmp(&a.3))
            .then_with(|| a.0.len().cmp(&b.0.len()))
            .then_with(|| b.0.cmp(&a.0))
    });
    scored.into_iter().map(|row| row.0).collect()
}

fn fuse_conventional_file_rank(
    index: &aden_index::Index,
    mut base: Vec<aden_index::SearchResult>,
    query: &str,
    root: &Path,
) -> Vec<aden_index::SearchResult> {
    use std::collections::HashMap;
    const K: f64 = 60.0;
    const CONVENTIONAL_WEIGHT: f64 = 1.5;
    let conventional: Vec<String> = if std::env::var_os("ADEN_NAV_FILESYSTEM").is_none() {
        let mut seen = HashSet::new();
        index
            .lexical_file_rank(query)
            .iter()
            .map(|path| source_key(path, root))
            .filter(|path| seen.insert(path.clone()))
            .collect()
    } else {
        conventional_file_rank(root, query)
    };
    if conventional.is_empty() {
        return base;
    }
    let mut score: HashMap<String, f64> = HashMap::new();
    // Match the measured candidate window. Aggregating every anchor lets a
    // large file win merely because it was split into many indexed sections.
    for (rank, result) in base.iter().take(20).enumerate() {
        *score
            .entry(source_key(&result.source_path, root))
            .or_default() += 1.0 / (K + (rank + 1) as f64);
    }
    for (rank, path) in conventional.iter().take(20).enumerate() {
        *score.entry(path.clone()).or_default() += CONVENTIONAL_WEIGHT / (K + (rank + 1) as f64);
    }
    let mut original_scores: Vec<f64> = base.iter().map(|result| result.score).collect();
    original_scores.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let winner = base
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| {
            let a_score = score
                .get(&source_key(&a.source_path, root))
                .copied()
                .unwrap_or(0.0);
            let b_score = score
                .get(&source_key(&b.source_path, root))
                .copied()
                .unwrap_or(0.0);
            a_score
                .partial_cmp(&b_score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    a.score
                        .partial_cmp(&b.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| b.anchor.cmp(&a.anchor))
        })
        .map(|(index, _)| index)
        .unwrap_or(0);
    if winner > 0 {
        let promoted = base.remove(winner);
        base.insert(0, promoted);
    }
    // Downstream routing uses score bands and assumes descending scores. Preserve
    // the native score distribution while assigning it to the fused ordering.
    for (result, score) in base.iter_mut().zip(original_scores) {
        result.score = score;
    }
    base
}

/// Run the base ranking for `q`: hybrid (BM25+dense) when embeddings are present, else BM25.
fn run_base(index: &aden_index::Index, q: &str) -> Vec<aden_index::SearchResult> {
    #[cfg(feature = "dense")]
    if index.has_embeddings()
        && let Some(emb) = dense_embedder()
    {
        return index.hybrid_query(q, emb);
    }
    index.query(q)
}

/// Fuse a `base` ranking with an `exp`(anded) ranking via Reciprocal Rank Fusion, with the
/// base up-weighted (`BASE_WEIGHT`). RRF combines by rank, not score, so the two passes
/// (run over different query strings, hence different score scales) merge without
/// normalization. Up-weighting the base means expansion can only lift a result the base
/// ranked low or missed — it can never push a confidently-ranked base hit down. Fully
/// deterministic (ties broken by anchor).
fn fuse_base_weighted(
    base: Vec<aden_index::SearchResult>,
    exp: Vec<aden_index::SearchResult>,
) -> Vec<aden_index::SearchResult> {
    use std::collections::HashMap;
    const K: f64 = 60.0;
    const BASE_WEIGHT: f64 = 2.0;
    let mut score: HashMap<String, f64> = HashMap::new();
    for (i, r) in base.iter().enumerate() {
        *score.entry(r.anchor.clone()).or_insert(0.0) += BASE_WEIGHT / (K + (i + 1) as f64);
    }
    for (i, r) in exp.iter().enumerate() {
        *score.entry(r.anchor.clone()).or_insert(0.0) += 1.0 / (K + (i + 1) as f64);
    }
    // Keep one SearchResult per anchor, preferring the base object (its snippet/score
    // reflect the user's literal query) over the expanded one.
    let mut by_anchor: HashMap<String, aden_index::SearchResult> = HashMap::new();
    for r in exp.into_iter() {
        by_anchor.insert(r.anchor.clone(), r);
    }
    for r in base.into_iter() {
        by_anchor.insert(r.anchor.clone(), r);
    }
    let mut out: Vec<aden_index::SearchResult> = by_anchor.into_values().collect();
    out.sort_by(|a, b| {
        let sa = score.get(&a.anchor).copied().unwrap_or(0.0);
        let sb = score.get(&b.anchor).copied().unwrap_or(0.0);
        sb.partial_cmp(&sa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.anchor.cmp(&b.anchor))
    });
    out
}

/// Cross-query CALIBRATED confidence that `query` has a genuinely good match in
/// `index` (∈[0,1]), the off-topic safety signal consumed by gather-then-select's
/// `relevance_confidence`. The production relevance map is RRF-fused (magnitude
/// discarded), so this reaches past it to the raw dense COSINE of the best match —
/// the only cross-query-comparable signal — and maps it through the embedder's
/// semantic band via [`aden_index::semantic_match_confidence`]. Returns `None` when
/// dense is unavailable (no embeddings / no model): then no calibration is possible
/// and the gate runs at full strength, exactly as before.
#[allow(unused_variables)]
pub fn query_relevance_confidence(index: &aden_index::Index, query: &str) -> Option<f32> {
    #[cfg(feature = "dense")]
    {
        if index.has_embeddings()
            && let Some(emb) = dense_embedder()
        {
            let best = index
                .dense_query(query, emb)
                .first()
                .map(|r| r.score as f32)?;
            return Some(aden_index::semantic_match_confidence(best));
        }
    }
    None
}

/// Classify an anchor as *expected* metadata that legitimately has no graph
/// edges. Doc-heading anchors (`aden://doc/...`) and standalone metadata docs
/// (ADR/plan/use-case/readme/agent) are reference material, not dead code, so
/// they are NOT actionable orphans.
///
/// Single source of truth for orphan classification — `status` and `check`
/// (`perform_check`) both route through it so they can never disagree on what
/// counts as a real orphan.
pub fn is_expected_metadata(anchor: &str) -> bool {
    // Delegate to the shared predicate in aden-heal so the heal scanner and the
    // CLI's orphan classification can never diverge on what counts as a real,
    // actionable orphan.
    aden_heal::drift::is_expected_metadata(anchor)
}

/// Partition a graph's orphans into `(expected_metadata, actionable)`.
/// Actionable orphans are code symbols/modules that should be connected but
/// aren't; expected ones are reference docs that normally have no edges.
pub fn classify_orphans(
    graph: &aden_graph::AdenGraph<aden_graph::DocumentNode, aden_graph::AdenEdge>,
) -> (Vec<String>, Vec<String>) {
    // Dedup by anchor first. `orphans()` iterates by node, so when several files
    // collapse to one anchor (e.g. many `README.md` → `[[README]]`) it repeats
    // that anchor — which would otherwise emit duplicate orphan lines/counts in
    // `check`/`status` and over-penalize the health score. Report each distinct
    // orphan anchor once. (Mirrors aden-diagnose's `scan_orphans`.)
    let mut seen = std::collections::HashSet::new();
    graph
        .orphans()
        .into_iter()
        .filter(|a| seen.insert(a.clone()))
        .partition(|a| is_expected_metadata(a))
}

/// Health = fraction of distinct anchors that are NOT actionable orphans.
pub fn health_score_from_graph(
    graph: &aden_graph::AdenGraph<aden_graph::DocumentNode, aden_graph::AdenEdge>,
) -> f64 {
    // Count distinct anchors, not raw nodes, so the denominator matches the
    // anchor-deduped `actionable` numerator from `classify_orphans`. Without
    // this, a duplicate-anchor collision (itself reported separately) would
    // inflate the node count and skew the score.
    let total = graph.anchor_to_index.len();
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
    // Allow '.' so that proposal IDs derived from anchors that include a file
    // extension (e.g. `merge-aden---module-src-lib.rs-alpha`) are accepted.
    // Matches the set allowed by `aden_propose::store::is_safe_id`.
    id.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
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

/// Callee names referenced by a symbol document, for call-graph linking.
/// Reads both the `edge::calls[...]` listing and the `Callee` table so it works
/// regardless of which an extractor emits.
pub fn extract_callees(doc: &aden_core::Document) -> Vec<String> {
    use aden_core::Block;
    let mut callees = Vec::new();
    for block in &doc.blocks {
        match block {
            Block::Listing { code, .. } => {
                for line in code.lines() {
                    if let Some(rest) = line.trim().strip_prefix("edge::calls[")
                        && let Some(callee) = rest.strip_suffix(']')
                        && !callee.is_empty()
                    {
                        callees.push(callee.to_string());
                    }
                }
            }
            Block::Table(t)
                if t.headers.first().map(|h| h.eq_ignore_ascii_case("callee")) == Some(true) =>
            {
                for row in &t.rows {
                    if let Some(c) = row.first()
                        && !c.is_empty()
                    {
                        callees.push(c.clone());
                    }
                }
            }
            _ => {}
        }
    }
    callees.sort();
    callees.dedup();
    callees
}

/// Type names a symbol `Uses`, read from `edge::uses[...]` listings (emitted by
/// extractors for the types referenced in a signature/fields). Kept separate
/// from callees so they link as `Uses` edges, not `Calls`.
pub fn extract_uses(doc: &aden_core::Document) -> Vec<String> {
    use aden_core::Block;
    let mut uses = Vec::new();
    for block in &doc.blocks {
        if let Block::Listing { code, .. } = block {
            for line in code.lines() {
                if let Some(rest) = line.trim().strip_prefix("edge::uses[")
                    && let Some(t) = rest.strip_suffix(']')
                    && !t.is_empty()
                {
                    uses.push(t.to_string());
                }
            }
        }
    }
    uses.sort();
    uses.dedup();
    uses
}

/// Targets of one `edge::<kind>[...]` macro family in a document's listing
/// blocks, sorted + deduped. Shared reader for the Wave-1 edge macros
/// (`implements`, `mutates`) — same format `extract_uses` reads for `uses`.
pub fn extract_edge_macro(doc: &aden_core::Document, kind: &str) -> Vec<String> {
    use aden_core::Block;
    let prefix = format!("edge::{kind}[");
    let mut out = Vec::new();
    for block in &doc.blocks {
        if let Block::Listing { code, .. } = block {
            for line in code.lines() {
                if let Some(rest) = line.trim().strip_prefix(prefix.as_str())
                    && let Some(t) = rest.strip_suffix(']')
                    && !t.is_empty()
                {
                    out.push(t.to_string());
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Prose cross-references a doc node makes, read from the `doc_refs` attribute
/// the format parsers fill (`ref:<fragment>` entries — AsciiDoc `<<target>>`/
/// `xref:file#frag` and markdown `[text](#frag)` forms). Extraction is
/// per-format and lives in the parser, which knows listing-fence and backtick
/// state (a `<<x>>` inside a code example is not a reference); resolution in
/// [`link_store_edges`] is format-neutral. A previous version scanned the
/// document BLOCKS here with no fence/backtick awareness, which also picked up
/// `a << b >> c` shift expressions from embedded code listings.
pub fn extract_doc_refs(doc: &aden_core::Document) -> Vec<String> {
    extract_joined_attribute(doc, "doc_refs")
}

/// Document-composition targets, read from the `doc_includes` attribute the
/// AsciiDoc parser fills for `include::` directives. Resolved file-wise (by
/// stem) to directional `Requires` edges in [`link_include_edges`].
pub fn extract_doc_includes(doc: &aden_core::Document) -> Vec<String> {
    extract_joined_attribute(doc, "doc_includes")
}

/// Backtick prose mentions, read from the `doc_mentions` attribute the format
/// parsers fill (Wave 2). Same division of labor as `doc_refs`: the parser
/// knows fence/backtick state; resolution in [`link_store_edges`] is
/// format-neutral and links only unambiguous names.
pub fn extract_doc_mentions(doc: &aden_core::Document) -> Vec<String> {
    extract_joined_attribute(doc, "doc_mentions")
}

/// Supersede-context refs, read from the `doc_supersedes` attribute the format
/// parsers fill (Wave 3). Entries are `<by|of>:ref:<frag>` — a direction
/// prefix plus the same `ref:` form the `doc_refs` channel uses.
pub fn extract_doc_supersedes(doc: &aden_core::Document) -> Vec<String> {
    extract_joined_attribute(doc, "doc_supersedes")
}

/// `kind:name` references a doc code listing makes, read from the
/// `symbol_references` attribute the format parsers fill on `code_block_*`
/// docs (declaration scan + language-neutral call-token scan). Linked as
/// `Demonstrates` edges (Wave 2).
pub fn extract_demonstrates(doc: &aden_core::Document) -> Vec<String> {
    extract_joined_attribute(doc, "symbol_references")
}

/// Term anchors a glossary section defines, read from the `doc_terms`
/// attribute the format parsers fill (Wave 2 remainder). Values are full
/// `aden://term/…` anchors, so linking is exact-match only.
pub fn extract_doc_terms(doc: &aden_core::Document) -> Vec<String> {
    extract_joined_attribute(doc, "doc_terms")
}

/// A comma-joined doc attribute as a sorted, deduped list.
pub fn extract_joined_attribute(doc: &aden_core::Document, key: &str) -> Vec<String> {
    let Some(joined) = doc.attributes.get(key) else {
        return Vec::new();
    };
    let mut vals: Vec<String> = joined
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    vals.sort();
    vals.dedup();
    vals
}

/// Module-level co-change pairs from git history (Wave 3 `AssociatedWith` —
/// the Hebbian episodic signal: things that fire together wire together).
/// Deterministic from repo state: the last 1000 non-merge commits, skipping
/// bulk commits (>20 files — rename sweeps and reformats say nothing about
/// functional coupling), pair-counting each commit's files at module level
/// and keeping pairs that co-changed ≥3 times. Files map to module anchors
/// via the gen cache (file → anchors recorded by gen itself), so the whole
/// pass costs one `git log` — no store scan. Non-repos and git failures
/// degrade to no edges.
pub fn cochange_pairs(
    root: &std::path::Path,
    cache: &crate::types::GenCache,
) -> Vec<crate::types::CochangePair> {
    use std::collections::BTreeMap;
    const COCHANGE_COMMITS: &str = "1000";
    const COCHANGE_MAX_FILES: usize = 20;
    const COCHANGE_THRESHOLD: u32 = 3;

    // Repo-relative file → file-level anchor: the `#`-stripped prefix of the
    // file's first symbol anchor (code files have exactly one). Files with no
    // symbol anchors (prose, empty) drop out here.
    let mut file_anchor: BTreeMap<&str, String> = BTreeMap::new();
    for (key, entry) in &cache.entries {
        for a in &entry.anchors {
            if let Some(h) = a.find('#') {
                file_anchor.insert(key.as_str(), a[..h].to_string());
                break;
            }
        }
    }
    if file_anchor.is_empty() {
        return Vec::new();
    }
    // Reverse map for attaching the source file to each emitted anchor
    // (BTreeMap iteration order makes the first-file-wins pick deterministic).
    let mut anchor_file: BTreeMap<&str, &str> = BTreeMap::new();
    for (f, a) in &file_anchor {
        anchor_file.entry(a.as_str()).or_insert(f);
    }

    let Ok(out) = std::process::Command::new("git")
        .args([
            "log",
            "--no-merges",
            "-n",
            COCHANGE_COMMITS,
            "--pretty=format:@@",
            "--name-only",
        ])
        .current_dir(root)
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let log = String::from_utf8_lossy(&out.stdout);

    let mut counts: BTreeMap<(String, String), u32> = BTreeMap::new();
    for block in log.split("@@") {
        let files: Vec<&str> = block
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        // Bulk-commit gate on raw files touched, BEFORE anchor mapping — a
        // 200-file reformat is noise even if only 5 of those files are indexed.
        if files.len() < 2 || files.len() > COCHANGE_MAX_FILES {
            continue;
        }
        let mut anchors: Vec<&str> = files
            .iter()
            .filter_map(|f| file_anchor.get(f).map(String::as_str))
            .collect();
        anchors.sort_unstable();
        anchors.dedup();
        for i in 0..anchors.len() {
            for j in i + 1..anchors.len() {
                *counts
                    .entry((anchors[i].to_string(), anchors[j].to_string()))
                    .or_default() += 1;
            }
        }
    }
    counts
        .into_iter()
        .filter(|(_, c)| *c >= COCHANGE_THRESHOLD)
        .map(|((a, b), _)| {
            let fa = anchor_file
                .get(a.as_str())
                .copied()
                .unwrap_or("")
                .to_string();
            let fb = anchor_file
                .get(b.as_str())
                .copied()
                .unwrap_or("")
                .to_string();
            ((a, fa), (b, fb))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn retrieval_result(anchor: &str, path: &str, score: f64) -> aden_index::SearchResult {
        aden_index::SearchResult {
            anchor: anchor.to_string(),
            source_path: PathBuf::from(path),
            score,
            snippet: String::new(),
        }
    }

    #[test]
    fn navigation_gate_requires_four_fifths_prose_votes() {
        let mut results: Vec<_> = (0..8)
            .map(|i| retrieval_result(&format!("aden://doc/repo/{i}"), "guide.adoc", 1.0))
            .collect();
        results.extend(
            (0..2).map(|i| retrieval_result(&format!("aden://module/repo/{i}"), "main.rs", 1.0)),
        );
        assert!(results_are_predominantly_prose(&results));
        results[0] = retrieval_result("aden://module/repo/extra", "lib.rs", 1.0);
        assert!(!results_are_predominantly_prose(&results));
    }

    #[test]
    fn navigation_terms_are_bounded_and_drop_common_noise() {
        let terms = navigation_terms(
            "remember my username and password so I stop retyping them with the tool",
        );
        assert!(terms.contains(&"username".to_string()));
        assert!(terms.contains(&"password".to_string()));
        assert!(!terms.contains(&"with".to_string()));
        assert!(terms.len() <= 12);
    }

    #[test]
    fn navigation_consensus_counts_top_file_only_within_top_five() {
        let root = Path::new("/repo");
        let results = vec![
            retrieval_result("a", "/repo/guide.adoc", 5.0),
            retrieval_result("b", "/repo/other.adoc", 4.0),
            retrieval_result("c", "/repo/guide.adoc", 3.0),
            retrieval_result("d", "/repo/guide.adoc", 2.0),
            retrieval_result("e", "/repo/third.adoc", 1.0),
            retrieval_result("f", "/repo/guide.adoc", 0.5),
        ];
        assert_eq!(native_top_file_consensus(&results, root), 3);
    }

    #[test]
    fn navigation_detects_date_named_timeline_candidates() {
        let dated: Vec<_> =
            (1..=8)
                .map(|day| {
                    retrieval_result(
                        &format!("aden://doc/log/{day}"),
                        &format!("log/2026-06-{day:02}.adoc"),
                        1.0,
                    )
                })
                .chain((0..2).map(|i| {
                    retrieval_result(&format!("aden://doc/roadmap/{i}"), "roadmap.adoc", 1.0)
                }))
                .collect();
        assert!(results_are_chronological(&dated));
        let mut mixed = dated;
        mixed[0] = retrieval_result("a", "guide.adoc", 1.0);
        assert!(!results_are_chronological(&mixed));
    }

    #[test]
    fn prefer_native_picks_native_or_falls_back_to_existing_legacy() {
        let native = std::path::PathBuf::from("/native/aden/x");
        let legacy = std::path::PathBuf::from("/legacy/aden/x");

        // Native present -> native (even if legacy also present).
        let r = prefer_native(native.clone(), legacy.clone(), |_| true);
        assert_eq!(r, native);

        // Native absent, legacy present -> legacy (non-destructive migration).
        let r = prefer_native(native.clone(), legacy.clone(), |p| p == legacy);
        assert_eq!(r, legacy);

        // Neither present -> native (fresh writes land at the native location).
        let r = prefer_native(native.clone(), legacy.clone(), |_| false);
        assert_eq!(r, native);
    }

    #[test]
    fn discover_source_files_scoped_narrows_to_subtree() {
        // A repo with source under two crates; a scoped walk must see only the
        // subtree it was pointed at, while the unscoped walk sees the whole tree.
        let dir = std::env::temp_dir().join(format!("aden-scope-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = dir.join("crate-a/src");
        let b = dir.join("crate-b/src");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("lib.rs"), "fn a() {}\n").unwrap();
        std::fs::write(b.join("lib.rs"), "fn b() {}\n").unwrap();

        let all = discover_source_files(&dir).unwrap();
        assert_eq!(all.len(), 2, "unscoped walk sees both crates");

        let scoped = discover_source_files_scoped(&dir.join("crate-a"), &dir).unwrap();
        assert_eq!(scoped.len(), 1, "scoped walk sees only crate-a");
        assert!(scoped[0].ends_with("crate-a/src/lib.rs"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_doc_link_classifies_targets() {
        let root = Path::new("/repo");
        let docdir = Path::new("/repo/docs");
        let tops: std::collections::HashSet<String> = ["docs".to_string(), "crates".to_string()]
            .into_iter()
            .collect();

        // Root-relative (starts with a top dir).
        assert_eq!(
            resolve_doc_link("docs/a.adoc", docdir, root, &tops),
            Some(PathBuf::from("/repo/docs/a.adoc"))
        );
        // Doc-relative sibling (not a top dir).
        assert_eq!(
            resolve_doc_link("sibling.adoc", docdir, root, &tops),
            Some(PathBuf::from("/repo/docs/sibling.adoc"))
        );
        // Anchor fragment is dropped; file part still resolves.
        assert_eq!(
            resolve_doc_link("crates/x/y.rs#frag", docdir, root, &tops),
            Some(PathBuf::from("/repo/crates/x/y.rs"))
        );
        // Not file links: URLs, anchors-only, extensionless, globs.
        assert_eq!(
            resolve_doc_link("https://x.com/a.html", docdir, root, &tops),
            None
        );
        assert_eq!(resolve_doc_link("#section", docdir, root, &tops), None);
        assert_eq!(
            resolve_doc_link("crates/aden-cli", docdir, root, &tops),
            None
        );
        assert_eq!(resolve_doc_link("src/*.rs", docdir, root, &tops), None);
    }

    #[test]
    fn doc_path_gate_flags_broken_link_not_examples() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs/real.adoc"), "= Real\n").unwrap();
        std::fs::write(
            root.join("docs/guide.adoc"),
            // A valid link, a BROKEN link, an illustrative backtick mention, and a
            // link inside a code fence — only the broken link must be flagged.
            "= Guide\n\
             See xref:docs/real.adoc[real].\n\
             See xref:docs/missing.adoc[gone].\n\
             Scaffolding produces `src/main.rs` for you.\n\
             ----\n\
             xref:docs/also-not-real.adoc[in a fence]\n\
             ----\n",
        )
        .unwrap();

        let findings = check_doc_path_references(root);
        assert_eq!(
            findings.len(),
            1,
            "exactly one broken link expected: {findings:?}"
        );
        assert!(
            findings[0].contains("docs/missing.adoc"),
            "got {findings:?}"
        );
        assert!(
            !findings.iter().any(|f| f.contains("src/main.rs")),
            "backtick mention must not flag"
        );
        assert!(
            !findings.iter().any(|f| f.contains("also-not-real")),
            "fenced link must not flag"
        );
    }

    #[test]
    fn doc_path_gate_ignores_monospace_and_passthrough_xref_examples() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(
            root.join("docs/guide.adoc"),
            "= Guide\n\
             The illustrative `xref:file.adoc#fragment` example must not flag.\n\
             Passthrough +xref:other-doc.adoc#section[label]+ is also illustrative.\n\
             Live xref:docs/missing.adoc[broken] must still flag.\n",
        )
        .unwrap();

        let findings = check_doc_path_references(root);
        assert_eq!(
            findings.len(),
            1,
            "only the live broken link must flag: {findings:?}"
        );
        assert!(
            findings[0].contains("docs/missing.adoc"),
            "got {findings:?}"
        );
    }

    #[test]
    fn classify_orphans_dedups_colliding_anchors() {
        use aden_graph::{AdenEdge, AdenGraph, DocumentNode};
        let orphan = |src: &str| DocumentNode {
            doc: aden_core::Document {
                anchor: "dup".into(),
                node_type: aden_core::NodeType::Function,
                attributes: std::collections::HashMap::new(),
                blocks: vec![],
                source_span: None,
                metadata: None,
                confidence: 1.0,
            },
            source_path: std::path::PathBuf::from(src),
            parsed: None,
        };
        // M3: add_node rejects duplicate anchors, so only one node lands. The
        // classify_orphans dedup still matters when orphans() repeats an anchor
        // (legacy cached graphs); here we verify the single-anchor orphan path.
        let mut graph: AdenGraph<DocumentNode, AdenEdge> = AdenGraph::new();
        graph.add_node(orphan("a.rs")).expect("first node");
        assert!(
            graph.add_node(orphan("b.rs")).is_err(),
            "duplicate anchor must be rejected at insert"
        );
        assert_eq!(graph.orphans(), vec!["dup"]);

        // classify_orphans must report the distinct orphan anchor ONCE.
        let (_expected, actionable) = classify_orphans(&graph);
        assert_eq!(
            actionable,
            vec!["dup".to_string()],
            "colliding orphan anchor must be deduped to a single entry"
        );

        // The health score is anchor-based: one distinct anchor, and it is an
        // orphan → 0% healthy (node-based counting would wrongly read 0.5).
        assert_eq!(
            health_score_from_graph(&graph),
            0.0,
            "score must count the collision as one orphan anchor, not two nodes"
        );
    }

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

    #[test]
    fn disposition_discovery_accounts_for_ignored_and_unsupported_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join("notes.xyz"), "unparsed\n").unwrap();
        std::fs::write(dir.path().join(".adenignore"), "hidden.rs\n").unwrap();
        std::fs::write(dir.path().join("hidden.rs"), "fn hidden() {}\n").unwrap();

        let entries = discover_file_dispositions(dir.path()).unwrap();
        let by_name: std::collections::HashMap<_, _> = entries
            .iter()
            .map(|entry| {
                (
                    entry
                        .path
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .to_string(),
                    entry.disposition,
                )
            })
            .collect();
        assert_eq!(
            by_name.get("main.rs"),
            Some(&aden_core::filter::FileDisposition::Indexed)
        );
        assert_eq!(
            by_name.get("hidden.rs"),
            Some(&aden_core::filter::FileDisposition::Ignored)
        );
        assert_eq!(
            by_name.get("notes.xyz"),
            Some(&aden_core::filter::FileDisposition::Unsupported)
        );
    }

    #[test]
    fn old_generation_cache_requires_a_store_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("gen-cache.json");
        std::fs::write(
            &cache_path,
            r#"{"version":5,"entries":{"old.rs":{"source_mtime":0,"source_path":"old.rs","anchors":["old"]}}}"#,
        )
        .unwrap();
        assert!(gen_cache_requires_rebuild(&cache_path));
        assert!(load_gen_cache(&cache_path).entries.is_empty());

        let wrong_policy = serde_json::json!({
            "version": crate::types::GEN_LOGIC_VERSION,
            "filter_fingerprint": 1,
            "entries": {
                "old.rs": {"source_mtime": 0, "source_path": "old.rs", "anchors": ["old"]}
            }
        });
        std::fs::write(&cache_path, serde_json::to_vec(&wrong_policy).unwrap()).unwrap();
        assert!(gen_cache_requires_rebuild(&cache_path));
        assert!(load_gen_cache(&cache_path).entries.is_empty());

        std::fs::write(&cache_path, "{not-json").unwrap();
        assert!(gen_cache_requires_rebuild(&cache_path));
        std::fs::write(&cache_path, [0xff, 0xfe]).unwrap();
        assert!(gen_cache_requires_rebuild(&cache_path));
    }

    #[test]
    fn cache_persists_explicit_dispositions() {
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("gen-cache.json");
        let mut cache = GenCache {
            version: crate::types::GEN_LOGIC_VERSION,
            filter_fingerprint: aden_core::filter::built_in_ignore_fingerprint(),
            ..GenCache::default()
        };
        cache.dispositions.insert(
            "secret.rs".into(),
            crate::types::FileDispositionEntry {
                disposition: aden_core::filter::FileDisposition::SecretContent,
                source_mtime: 1,
                source_path: "secret.rs".into(),
            },
        );
        save_gen_cache(&cache_path, &cache).unwrap();
        let reloaded = load_gen_cache(&cache_path);
        assert_eq!(
            reloaded.dispositions["secret.rs"].disposition,
            aden_core::filter::FileDisposition::SecretContent
        );
    }
}
