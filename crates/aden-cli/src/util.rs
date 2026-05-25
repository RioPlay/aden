use aden_emit::check::{collect_anchors, find_refs};
use aden_graph::{cycles::find_cycles, integrity::check_hashes};
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

/// Recursively walk a directory and collect files matching any of `exts`.
/// Skips paths that contain any substring in `skip_patterns`.
pub fn walk_src_files(
    dir: &Path,
    exts: &[&str],
    out: &mut Vec<PathBuf>,
    skip_patterns: &[&str],
) -> Result<(), Box<dyn std::error::Error>> {
    for entry in std::fs::read_dir(dir).map_err(|e| format!("read_dir: {}", e))? {
        let entry = entry.map_err(|e| format!("entry: {}", e))?;
        let p = entry.path();
        let p_str = normalize_sep(&p);
        if skip_patterns.iter().any(|pat| p_str.contains(pat)) {
            continue;
        }
        if entry.file_type()?.is_symlink() {
            continue;
        }
        if entry.file_type()?.is_dir() {
            walk_src_files(&p, exts, out, skip_patterns)?;
        } else if entry.file_type()?.is_file()
            && let Some(ext) = p.extension().and_then(|e| e.to_str())
            && exts.contains(&ext)
        {
            out.push(p);
        }
    }
    Ok(())
}

/// Find project root by walking up from `start` looking for Cargo.toml,
/// aden.toml, go.mod, or package.json.
pub fn find_project_root(start: &Path) -> PathBuf {
    let mut current = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    loop {
        if current.join("Cargo.toml").exists()
            || current.join("aden.toml").exists()
            || current.join("go.mod").exists()
            || current.join("package.json").exists()
        {
            return current;
        }
        if let Some(parent) = current.parent() {
            current = parent.to_path_buf();
        } else {
            return start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
        }
    }
}

/// Discover source files based on build system detected at `root`.
pub fn discover_source_files(root: &Path) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut files = Vec::new();

    if root.join("Cargo.toml").exists() {
        walk_src_files(root, &["rs"], &mut files, &["/.git/", "/target/"])?;
        files.sort_by(|a, b| {
            let a_is_src = normalize_sep(a).contains("/src/");
            let b_is_src = normalize_sep(b).contains("/src/");
            b_is_src.cmp(&a_is_src)
        });
    } else if root.join("go.mod").exists() {
        walk_src_files(root, &["go"], &mut files, &["/vendor/", " /.git/"])?;
    } else if root.join("package.json").exists() {
        walk_src_files(
            root,
            &["ts", "tsx", "js", "jsx", "mjs", "cjs"],
            &mut files,
            &["/node_modules/", " /.git/"],
        )?;
    } else {
        walk_src_files(
            root,
            &["rs", "py", "js", "ts", "go", "c", "cpp", "h"],
            &mut files,
            &["/.git/", "/target/"],
        )?;
    }

    Ok(files)
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
    _source: &Path,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if docs.is_empty() {
        return Ok(());
    }
    // SECURITY: Strip absolute paths from source_file attributes before emitting
    for doc in &mut docs {
        sanitize_source_file(doc);
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
        serde_json::Value::String(node.anchor.clone()),
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
        .attributes
        .get("node-type")
        .cloned()
        .unwrap_or_else(|| format!("{:?}", node.doc.node_type))
}

/// Perform all integrity checks on a project directory.
/// Returns a list of human-readable messages ("INFO: ...", "ERROR: ...", "WARNING: ...").
pub fn perform_check(path: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut messages = Vec::new();
    let mut all_anchors: HashSet<String> = HashSet::new();

    let graph = aden_graph::cache::build_from_directory_cached(path)?;

    // Collect local anchors
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

    for node in graph.graph.node_indices() {
        for anchor in &graph.graph[node].parsed.anchors {
            all_anchors.insert(anchor.clone());
        }
    }

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
        for o in &orphans {
            messages.push(format!("WARNING: Orphan document: {}", o));
        }
    }

    let hash_issues = check_hashes(&graph);
    if hash_issues.is_empty() {
        messages.push("INFO: All source_hash values valid.".to_string());
    } else {
        for (anchor, msg) in &hash_issues {
            messages.push(format!("ERROR: {} (anchor: {})", msg, anchor));
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

    Ok(messages)
}

/// Load the search index from disk cache, or build and cache it.
pub fn load_or_build_index(path: &Path) -> Result<aden_index::Index, Box<dyn std::error::Error>> {
    if let Some(cached) = aden_index::try_load(path) {
        return Ok(cached);
    }
    let index = aden_index::Index::from_directory(path)?;
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
