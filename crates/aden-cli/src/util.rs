pub mod quiet;

use aden_emit::check::{collect_anchors, find_refs};
use aden_graph::{cycles::find_cycles, integrity::check_hashes, GraphNode};
use serde_json::Map;
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::types::GenCache;

/// Infer the parent module anchor from a source file path.
///
/// Supports all common monorepo workspace layouts:
///   - `crates/<name>/src/...`   (Cargo workspaces)
///   - `packages/<name>/src/...` (npm/pnpm workspaces, some Cargo)
///   - `modules/<name>/src/...`  (Go, generic)
///   - `libs/<name>/src/...`     (Nx, Bazel)
///   - `services/<name>/src/...` (microservice repos)
///   - `apps/<name>/src/...`     (monorepo apps)
///   - `src/<name>/...`          (flat layout — returns mod-<name>)
///
/// Returns `None` for single-package repos (no subdirectory workspace).
fn infer_parent_module_from_source(source_path: &std::path::Path) -> Option<String> {
    let path_str = source_path.to_string_lossy();
    // Ordered by specificity: longer segment names first to avoid false matches
    const WORKSPACE_DIRS: &[(&str, usize)] = &[
        ("/crates/",   8),
        ("/packages/", 10),
        ("/modules/",  9),
        ("/libs/",     6),
        ("/services/", 10),
        ("/apps/",     6),
        ("crates/",    7),
        ("packages/",  9),
        ("modules/",   8),
        ("libs/",      5),
        ("services/",  9),
        ("apps/",      5),
        ("/src/",      5), // flat layout: /src/<name>/...
        ("src/",       4),
    ];
    for (segment, skip) in WORKSPACE_DIRS {
        if let Some(start) = path_str.find(segment) {
            let after = &path_str[start + skip..];
            if let Some(end) = after.find('/') {
                let name = &after[..end];
                if !name.is_empty() && name != "src" && name != "lib" && name != "main" {
                    return Some(format!("mod-{}", name));
                }
            }
        }
    }
    None
}

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

/// Walk up from `start` looking for a directory containing `.aden/`.
pub fn find_aden_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    loop {
        if current.join(".aden").is_dir() {
            return Some(current);
        }
        if let Some(parent) = current.parent() {
            current = parent.to_path_buf();
        } else {
            return None;
        }
    }
}

/// Compute the base-cache path for a given contract output path.
pub fn base_cache_path(contract_path: &Path) -> Option<PathBuf> {
    let file_name = contract_path.file_name()?.to_str()?;
    let root = if contract_path.is_absolute() {
        find_aden_root(contract_path.parent()?)?
    } else {
        find_aden_root(std::env::current_dir().ok()?.as_path())?
    };
    Some(root.join(".aden").join("contract-base").join(file_name))
}

/// Project-root markers across ecosystems. Ordered roughly by how strongly each
/// signals a repository root. Includes VCS and Aden's own workspace dir so that
/// root detection works for *any* language — not just the ones whose build
/// manifests happen to be listed first.
const ROOT_MARKER_FILES: &[&str] = &[
    // Aden / VCS — strongest signal of the true repo root
    "aden.toml",
    ".git",
    ".aden",
    ".hg",
    ".svn",
    // Rust
    "Cargo.toml",
    // Go
    "go.mod",
    // Node / JS / TS
    "package.json",
    // Python
    "pyproject.toml",
    "setup.py",
    "setup.cfg",
    "requirements.txt",
    "Pipfile",
    // JVM
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    // Ruby
    "Gemfile",
    // PHP
    "composer.json",
    // C / C++
    "CMakeLists.txt",
    // Generic
    "Makefile",
];

/// Find project root by walking up from `start` looking for any known
/// project-root marker (a build manifest, a VCS directory, or `.aden/`).
/// Language-agnostic: every ecosystem's manifest is recognized equally.
pub fn find_project_root(start: &Path) -> PathBuf {
    let mut current = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    loop {
        if ROOT_MARKER_FILES.iter().any(|m| current.join(m).exists()) {
            return current;
        }
        if let Some(parent) = current.parent() {
            current = parent.to_path_buf();
        } else {
            return start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
        }
    }
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

    let supported: HashSet<&'static str> =
        aden_parse::supported_extensions().into_iter().collect();
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

/// Strip absolute prefix from source_file attributes to prevent
/// username / home-directory leakage in emitted contracts.
pub fn sanitize_source_file(doc: &mut aden_core::Document) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    if let Some(source_file) = doc.attributes.get("source_file") {
        let p = std::path::Path::new(source_file);
        if p.is_absolute()
            && let Ok(rel) = p.strip_prefix(&cwd)
        {
            doc.attributes
                .insert("source_file".to_string(), rel.to_string_lossy().to_string());
        }
    }
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

/// Parse a comma-separated list of edge-type strings.
pub fn parse_edge_types(input: &str) -> Vec<aden_core::EdgeType> {
    input
        .split(',')
        .filter_map(parse_single_edge_type)
        .collect()
}

/// Emit documents to files or stdout.
pub fn emit_docs(
    mut docs: Vec<aden_core::Document>,
    out_dir: Option<&Path>,
    source: &Path,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if docs.is_empty() {
        return Ok(());
    }
    // SECURITY: Strip absolute paths from source_file attributes before emitting
    for doc in &mut docs {
        sanitize_source_file(doc);
    }

    // Auto-link to parent module (infer from source path)
    let parent_module = infer_parent_module_from_source(source);
    if parent_module.is_some() {
        for doc in &mut docs {
            doc.blocks.push(aden_core::Block::Paragraph("== Relationships".to_string()));
            doc.blocks.push(aden_core::Block::DescriptionList(vec![
                (format!("<<{},module>>", parent_module.as_ref().unwrap()), 
                 "This symbol is part of the parent module.".to_string())
            ]));
        }
    }

    let is_markdown = format.eq_ignore_ascii_case("md");
    let output = if is_markdown {
        aden_emit::emit_md(&docs)
    } else {
        aden_emit::emit(&docs)
    };

    if let Some(out) = out_dir {
        std::fs::create_dir_all(out)?;
        for doc in &docs {
            let ext = if is_markdown { "md" } else { "adoc" };
            let file_name = format!("{}.{}", sanitize_anchor(&doc.anchor), ext);
            let file_path = out.join(&file_name);
            let content = if is_markdown {
                aden_emit::emit_document_md(doc)
            } else {
                aden_emit::emit_document(doc)
            };
            std::fs::write(&file_path, content)?;
            println!("Emitted {}", file_path.display());
        }
    } else {
        println!("{output}");
    }

    Ok(())
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

    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
                if ext == "adoc" || ext == "aden" {
                    let mut text = String::new();
                    if let Ok(mut file) = std::fs::File::open(&p) {
                        let _ = file.read_to_string(&mut text);
                        // Check for [must-complete] marker
                        if text.contains("[must-complete]") {
                            // Check if it's been filled (has non-empty required fields)
                            // A filled contract won't have the hint line or will have content after ====
                            let has_hint = text.contains("Hint:");
                            let has_content_after_marker = text.match_indices("[must-complete]")
                                .last()
                                .map(|(pos, _)| text[pos..].contains("===="))
                                .unwrap_or(false);

                            if has_hint || !has_content_after_marker {
                                let anchor = p.file_stem()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or("unknown");
                                incomplete.push(format!(
                                    "WARNING: Incomplete contract: {} - run 'aden complete' to fill missing documentation",
                                    anchor
                                ));
                            }
                        }
                    }
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

    // Also check on-disk .adoc/.aden files for anchors not yet in store
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let p = entry.path();
        if p.is_file() {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext == "adoc" || ext == "aden" {
                let mut text = String::new();
                std::fs::File::open(&p)?.read_to_string(&mut text)?;
                all_anchors.extend(collect_anchors(&text));
            }
        }
    }

    // Check for unresolved refs in on-disk files
    let mut unresolved = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let p = entry.path();
        if p.is_file() {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext == "adoc" || ext == "aden" {
                let mut text = String::new();
                std::fs::File::open(&p)?.read_to_string(&mut text)?;
                for line in text.lines() {
                    for r in find_refs(line) {
                        if !all_anchors.contains(&r) {
                            unresolved.push(format!("{}: unresolved <<{}>>", p.display(), r));
                        }
                    }
                }
            }
        }
    }

    if unresolved.is_empty() {
        messages.push("INFO: All <<refs>> resolve.".to_string());
    } else {
        for issue in unresolved {
            messages.push(format!("ERROR: {}", issue));
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

    let orphans = graph.orphans();
    if orphans.is_empty() {
        messages.push("INFO: No orphan documents.".to_string());
    } else {
        // Summarize rather than emit one warning per orphan — a large repo can
        // have hundreds, which buries the rest of the check output (and the
        // agent's context). Show a count and a sample.
        const ORPHAN_SAMPLE: usize = 10;
        messages.push(format!(
            "WARNING: {} orphan document(s) (run 'aden heal . --gc' to link or remove):",
            orphans.len()
        ));
        for o in orphans.iter().take(ORPHAN_SAMPLE) {
            messages.push(format!("  - {}", o));
        }
        if orphans.len() > ORPHAN_SAMPLE {
            messages.push(format!("  ... and {} more", orphans.len() - ORPHAN_SAMPLE));
        }
    }

    let hash_issues = check_hashes(&graph);
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
/// `aden gen --auto` writes symbol contracts to the sled store (only module
/// contracts land on disk), so without this the full-text index — and therefore
/// `search` and `ask` — would never see any code symbols on any project. We
/// re-emit each stored `Document` to AsciiDoc and feed it to the index.
fn collect_store_entries(path: &Path) -> Vec<(PathBuf, String)> {
    use aden_store::{GraphStorage, Storage};

    let store_path = find_project_root(path).join(".aden").join("store");
    if !store_path.is_dir() {
        return Vec::new();
    }
    let Some(store_str) = store_path.to_str() else {
        return Vec::new();
    };
    let Ok(storage) = Storage::new(store_str) else {
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
/// the sled store, so language-agnostic `gen --auto` output (which is
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
pub fn quick_health_score(path: &Path) -> Result<f64, Box<dyn std::error::Error>> {
    use aden_heal::{Scanner, generate};

    let scanner = Scanner::new(path);
    let events = scanner.scan()?;

    // Use same weighting as heal report: severity-weighted events / total contracts
    let report = generate(events, path);
    Ok(report.overall_score)
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
