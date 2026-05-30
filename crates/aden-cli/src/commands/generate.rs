use aden_graph::graph::AdenGraph;
use aden_store::{GraphStorage, Storage};
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::types::GenCacheEntry;
use crate::util::{
    base_cache_path, discover_source_files, emit_docs, find_project_root, load_gen_cache,
    sanitize_anchor, sanitize_source_file, save_gen_cache,
};

/// One stored symbol plus the compact data the linker needs. Carrying callee
/// names out of the parse phase means linking never has to reload the (huge)
/// document store to rebuild the call graph.
struct EmittedSymbol {
    anchor: String,
    cache_key: String,
    cache_val: GenCacheEntry,
    callees: Vec<String>,
    /// `<<target>>` cross-references found in the document body (docs link to
    /// other docs / code via these).
    refs: Vec<String>,
}

/// Work item returned from parallel file processing.
enum WorkItem {
    Skip,
    Emitted(Vec<EmittedSymbol>),
}

/// Emit a progress line unless quiet mode is on.
macro_rules! progress {
    ($quiet:expr, $($arg:tt)*) => {
        if !$quiet { println!($($arg)*); }
    };
}

/// Automatically generate module contracts for directories in the workspace.
/// This ensures deterministic module anchors exist before symbol contracts.
/// Language-agnostic: works for any project with src/ directories.
fn generate_module_contracts(root: &Path, out_dir: &Path, quiet: bool) -> Result<(), Box<dyn std::error::Error>> {
    // Ensure the output directory exists before writing any contracts.
    std::fs::create_dir_all(out_dir)?;

    // Common source directory names across languages
    let src_dirs = ["src", "lib", "app", "modules", "source"];
    
    // Find all directories that could be modules (contain source files)
    let mut modules: Vec<(String, PathBuf)> = Vec::new();
    
    // Check for common workspace structures
    let workspace_dirs = [
        root.join("crates"),
        root.join("packages"),
        root.join("modules"),
        root.join("src"),
    ];
    
    for ws_dir in workspace_dirs.iter() {
        if !ws_dir.is_dir() {
            continue;
        }
        
        for entry in std::fs::read_dir(ws_dir)? {
            let entry = entry?;
            let mod_path = entry.path();
            if !mod_path.is_dir() {
                continue;
            }
            
            // Check if this module has source files
            for src_name in &src_dirs {
                let src_path = mod_path.join(src_name);
                if src_path.is_dir() {
                    let mod_name = mod_path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("");
                    if !mod_name.is_empty() && !mod_name.starts_with('.') {
                        modules.push((mod_name.to_string(), src_path));
                    }
                    break;
                }
            }
        }
    }
    
    // Also check root src/ directly
    for src_name in &src_dirs {
        let src_path = root.join(src_name);
        if src_path.is_dir() {
            let mod_name = root.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("project")
                .to_string();
            modules.push((mod_name, src_path));
            break;
        }
    }
    
    // ── Generate root mod-project (once, before individual modules) ───────
    let project_anchor = "mod-project";
    let project_path = out_dir.join(format!("{}.adoc", project_anchor));
    if !project_path.exists() {
        let mut module_lines = Vec::new();
        for (mod_name, _) in &modules {
            module_lines.push(format!(
                "| <<mod-{name}>> | {name}",
                name = mod_name
            ));
        }
        let modules_table = if module_lines.is_empty() {
            String::new()
        } else {
            format!(
                "\n|===\n| Module | Description\n\n{}\n|===",
                module_lines.join("\n")
            )
        };
        let project_content = format!(
            r#":source_file: .
:node-type: module
:last-verified: {date}T00:00:00Z

[[mod-project]]
= Project Root

Root module for the project. All submodules reference this.

== Modules
{modules_table}
"#,
            date = chrono::Utc::now().format("%Y-%m-%d"),
            modules_table = modules_table
        );
        std::fs::write(&project_path, &project_content)?;
        progress!(quiet, "Generated: {}", project_path.display());
    }

    // ── Generate individual module contracts ──────────────────────────────
    for (mod_name, src_path) in modules {
        let module_anchor = format!("mod-{}", mod_name);
        let contract_file = format!("{}.adoc", module_anchor);
        let out_path = out_dir.join(&contract_file);

        // Preserve any existing module contract that already declares the right
        // anchor — it may contain human or agent edits. We intentionally do NOT
        // pattern-match Aden's own historical boilerplate here: baking the
        // tool's own identity into regeneration logic is exactly what makes a
        // context compiler "self-centered" and breaks it on other codebases.
        if out_path.exists()
            && let Ok(existing) = std::fs::read_to_string(&out_path)
            && existing.contains(&format!("[[{}]]", module_anchor))
        {
            continue;
        }

        let content = format!(
            r#":source_file: {src}
:node-type: module
:last-verified: {date}T00:00:00Z

[[{anchor}]]
= {name}

Part of: <<mod-project>>
"#,
            src = src_path.display(),
            date = chrono::Utc::now().format("%Y-%m-%d"),
            anchor = module_anchor,
            name = mod_name
        );

        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&out_path, &content)?;
        progress!(quiet, "Generated module: {} ({})", out_path.display(), src_path.display());
    }

    Ok(())
}

/// Detect existing contract structure and return matching output directory.
/// Supports any workspace layout: crates/, packages/, modules/, or flat src/.
fn detect_contract_structure(path: &Path, source_path: &Path) -> Option<std::path::PathBuf> {
    let root = find_project_root(path);
    let rel = source_path.strip_prefix(&root).ok()?;

    let components: Vec<_> = rel.components().collect();
    if components.len() >= 3 {
        let first = components[0].as_os_str().to_str()?;
        let second = components[1].as_os_str().to_str()?;

        // Support any named workspace directory, not just "crates"
        for workspace_dir in &["crates", "packages", "modules"] {
            if first == *workspace_dir {
                let contract_dir = root
                    .join("contracts")
                    .join(workspace_dir)
                    .join(second)
                    .join("src");
                if contract_dir.exists() && contract_dir.is_dir() {
                    return Some(contract_dir);
                }
            }
        }
    }

    None
}

/// Module name for a symbol anchor of the form
/// `aden://module/<path>/<file>#<symbol>`.
///
/// The module is the directory that immediately contains the file — i.e. the
/// package. Using the *first* path segment was wrong for path-based ecosystems:
/// Go anchors like `aden://module/github.com/spf13/cobra/command.go#Execute`
/// collapsed the entire repo into `mod-github.com`. The last path segment is
/// always the file (`make_anchor` appends `/<file>#<sym>`), so the directory
/// before it is the real package/crate. For aden's own `crate/file.rs` layout
/// this still yields the crate name, so nothing regresses.
fn crate_from_anchor(anchor: &str) -> Option<String> {
    let rest = anchor.strip_prefix("aden://module/")?;
    let path = rest.split('#').next()?;
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let name = match segs.len() {
        0 => return None,
        1 => segs[0],                 // file at module root — use it
        n => segs[n - 2],             // directory containing the file
    };
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Callee names referenced by a symbol document, for call-graph linking.
/// Reads both the `edge::calls[...]` listing and the `Callee` table so it works
/// regardless of which an extractor emits.
fn extract_callees(doc: &aden_core::Document) -> Vec<String> {
    use aden_core::Block;
    let mut callees = Vec::new();
    for block in &doc.blocks {
        match block {
            Block::Listing { code, .. } => {
                for line in code.lines() {
                    if let Some(rest) = line.trim().strip_prefix("edge::calls[") {
                        if let Some(callee) = rest.strip_suffix(']') {
                            if !callee.is_empty() {
                                callees.push(callee.to_string());
                            }
                        }
                    }
                }
            }
            Block::Table(t) if t.headers.first().map(|h| h.eq_ignore_ascii_case("callee")) == Some(true) => {
                for row in &t.rows {
                    if let Some(c) = row.first() {
                        if !c.is_empty() {
                            callees.push(c.clone());
                        }
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

/// Append `<<target>>` cross-reference targets found in `text` to `out`.
fn collect_xrefs(text: &str, out: &mut Vec<String>) {
    let mut rest = text;
    while let Some(s) = rest.find("<<") {
        let after = &rest[s + 2..];
        let Some(e) = after.find(">>") else { break };
        let inner = &after[..e];
        let target = inner.split(',').next().unwrap_or(inner).trim();
        if !target.is_empty() && !target.contains('{') {
            out.push(target.to_string());
        }
        rest = &after[e + 2..];
    }
}

/// Cross-references a document makes via `<<target>>` macros in its prose. These
/// become graph edges so documentation is connected to what it references (docs
/// were previously hollow, unlinked islands).
fn extract_doc_refs(doc: &aden_core::Document) -> Vec<String> {
    use aden_core::Block;
    let mut refs = Vec::new();
    for block in &doc.blocks {
        match block {
            Block::Paragraph(t) => collect_xrefs(t, &mut refs),
            Block::Listing { code, .. } => collect_xrefs(code, &mut refs),
            _ => {}
        }
    }
    refs.sort();
    refs.dedup();
    refs
}

/// Slim a document before storing it. Drops the `edge::calls[...]` listing
/// block — it is redundant with the `Callee` table for display and is no longer
/// needed for linking (callees are carried out of the parse phase directly), so
/// storing it just bloats the (already large) store on big repos.
fn slim_doc_for_store(doc: &mut aden_core::Document) {
    use aden_core::Block;
    doc.blocks.retain(|b| {
        let Block::Listing { code, .. } = b else {
            return true;
        };
        // Drop only listings that are purely `edge::` macros.
        !code
            .lines()
            .filter(|l| !l.trim().is_empty())
            .all(|l| l.trim().starts_with("edge::"))
    });
}

/// Resolve a callee string to a single target anchor, or None if unknown or
/// ambiguous. Tries the full callee, then the trailing segment after the last
/// `.`/`:` so receiver/qualified calls link (`c.ExecuteC` → `ExecuteC`,
/// `click.echo` → `echo`, `Path::new` → `new`). Ambiguous names are left
/// unlinked rather than guessed, keeping the call graph precise.
fn resolve_callee<'a>(callee: &str, name_index: &HashMap<&str, Vec<&'a str>>) -> Option<&'a str> {
    if let Some(t) = name_index.get(callee) {
        return if t.len() == 1 { Some(t[0]) } else { None };
    }
    let base = callee.rsplit(['.', ':']).next().unwrap_or(callee);
    if base != callee && !base.is_empty() {
        if let Some(t) = name_index.get(base) {
            if t.len() == 1 {
                return Some(t[0]);
            }
        }
    }
    None
}

/// Connect the stored symbols into a traversable graph by persisting edges,
/// with bounded memory so it scales to large repositories.
///
/// Critically this never calls `get_all_documents()` — loading every full
/// document into RAM is what made linking the Linux kernel (a 17 GB store)
/// OOM. Instead it:
/// 1. reads only the anchor *keys* (`get_all_anchors`) to build the name index
///    and the module containment edges, and
/// 2. takes call-site data as compact `(anchor, callees)` records collected
///    during the parse phase.
/// All edges are then written with a single `put_edges_bulk` pass (O(E), not the
/// O(N^2) that per-edge writes incur on high-degree module nodes).
///
/// Edges built:
/// - Containment: `mod-<crate>` --Documents--> symbol, symbol --PartOf-->
///   `mod-<crate>`, `mod-project` --Documents--> each module. Module nodes are
///   synthesized here (they otherwise live only in ignored `.adoc` files).
/// - Calls: each resolved callee becomes a `Calls` edge.
fn link_store_edges<S: GraphStorage>(
    storage: &S,
    link_records: &[(String, Vec<String>)],
    ref_records: &[(String, Vec<String>)],
) -> Result<(), Box<dyn std::error::Error>> {
    use aden_core::{Block, Document, EdgeType, NodeType};
    use std::collections::HashSet;

    // Anchor keys only — cheap relative to full documents.
    let anchors = storage.get_all_anchors()?;

    // Short symbol name -> anchors that define it (borrows from `anchors`).
    let mut name_index: HashMap<&str, Vec<&str>> = HashMap::new();
    for anchor in &anchors {
        if let Some(hash) = anchor.rfind('#') {
            let name = &anchor[hash + 1..];
            if !name.is_empty() {
                name_index.entry(name).or_default().push(anchor.as_str());
            }
        }
    }

    let mut edges: Vec<(String, String, EdgeType)> = Vec::new();
    let mut modules: HashSet<String> = HashSet::new();

    // Containment for every anchor.
    for anchor in &anchors {
        if let Some(krate) = crate_from_anchor(anchor) {
            let module_anchor = format!("mod-{}", krate);
            modules.insert(krate);
            edges.push((module_anchor.clone(), anchor.clone(), EdgeType::Documents));
            edges.push((anchor.clone(), module_anchor, EdgeType::PartOf));
        }
    }

    // Call edges from the compact per-symbol records.
    for (anchor, callees) in link_records {
        for callee in callees {
            if let Some(target) = resolve_callee(callee, &name_index) {
                if target != anchor.as_str() {
                    edges.push((anchor.clone(), target.to_string(), EdgeType::Calls));
                }
            }
        }
    }

    // Cross-reference edges from document `<<target>>` macros. Bidirectional so
    // backlinks work (a doc and what it references are mutually reachable).
    for (anchor, refs) in ref_records {
        for r in refs {
            if let Some(target) = resolve_callee(r, &name_index) {
                if target != anchor.as_str() {
                    edges.push((anchor.clone(), target.to_string(), EdgeType::RelatesTo));
                    edges.push((target.to_string(), anchor.clone(), EdgeType::RelatesTo));
                }
            }
        }
    }

    // Synthesize module nodes + project root, and connect the project to each.
    if !modules.is_empty() {
        let make_module_doc = |anchor: &str, body: &str| Document {
            anchor: anchor.to_string(),
            node_type: NodeType::Module,
            attributes: HashMap::new(),
            blocks: vec![Block::Paragraph(body.to_string())],
            source_span: None,
            metadata: None,
            confidence: 1.0,
        };
        let project = "mod-project";
        if !anchors.contains(project) {
            let _ = storage.put_document(&make_module_doc(
                project,
                "Project root. Links to every crate/module in the project.",
            ));
        }
        for krate in &modules {
            let module_anchor = format!("mod-{}", krate);
            if !anchors.contains(&module_anchor) {
                let _ = storage.put_document(&make_module_doc(
                    &module_anchor,
                    &format!(
                        "Module {}. Contains the symbols extracted from its source.",
                        krate
                    ),
                ));
            }
            edges.push((project.to_string(), module_anchor.clone(), EdgeType::Documents));
            edges.push((module_anchor, project.to_string(), EdgeType::PartOf));
        }
    }

    storage.put_edges_bulk(&edges)?;
    storage.flush()?;
    Ok(())
}

/// Ensure the store is up to date with the source before a read command serves
/// from it. This is the "fresh by construction" path: a cheap mtime sweep over
/// the gen-cache, and — only if a source file is new or modified — a quiet
/// incremental `gen` (which skips unchanged files and re-links edges). When
/// nothing changed it is just stat calls, so queries stay fast while never
/// serving stale context. Deletions are intentionally ignored here (they only
/// leave harmless orphans); `aden heal . --gc` reclaims those.
///
/// Best-effort: any error degrades to serving the existing store rather than
/// failing the read.
pub fn ensure_fresh(path: &Path) {
    use std::time::UNIX_EPOCH;

    let root = find_project_root(path);
    // No store yet → build it now. Read commands are store-first, so a fresh
    // project must be indexed on first query (this is what makes asm/ask/locate
    // work without an explicit `aden gen`).
    if !root.join(".aden").join("store").exists() {
        let _ = cmd_gen(&root, None, false, true, false, false, "adoc", true);
        return;
    }

    let cache = load_gen_cache(&root.join(".aden").join("gen-cache.json"));
    let sources = match discover_source_files(&root) {
        Ok(s) => s,
        Err(_) => return,
    };

    // The newest source mtime gen has already seen. Comparing against this —
    // rather than requiring every discovered file to be present in the cache —
    // avoids perpetual staleness from files that are discovered but never
    // cached (e.g. unsupported languages that fail to parse). A file newer than
    // anything gen knew about is genuinely new or modified.
    let newest_known = cache.entries.values().map(|e| e.source_mtime).max().unwrap_or(0);

    let stale = sources.iter().any(|src| {
        let mtime = src
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        mtime > newest_known
    });

    if stale {
        // Quiet incremental regen: re-parses only changed files and re-links edges.
        let _ = cmd_gen(&root, None, false, true, false, false, "adoc", true);
    }
}

/// Auto-document a codebase: discover source files, skip unchanged,
/// emit structured contracts to store, and optionally to disk.
pub fn cmd_gen(
    path: &Path,
    out_dir: Option<&Path>,
    detect_out_dir: bool,
    auto: bool,
    merge: bool,
    propose: bool,
    format: &str,
    quiet: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if path.is_file() {
        let source = std::fs::read_to_string(path)?;
        let docs = aden_parse::parse_file(path, &source)?;

        // Resolve output dir relative to the file's parent, not the CWD.
        let file_root = path.parent().unwrap_or(path);
        let effective_out = if detect_out_dir {
            detect_contract_structure(path, path)
                .unwrap_or_else(|| file_root.join(".aden/contracts"))
        } else {
            out_dir
                .map(|d| d.to_path_buf())
                .unwrap_or_else(|| file_root.join(".aden/contracts"))
        };

        if merge || propose {
            return cmd_gen_contract(path, &source, docs, Some(&effective_out), propose);
        }
        return emit_docs(docs, Some(&effective_out), path, format);
    }

    if !path.is_dir() {
        return Err("Path does not exist or is not a file/directory".into());
    }

    let root = find_project_root(path);
    // Default output dir is <target>/.aden/contracts — inside .aden, never pollutes project
    let default_out = root.join(".aden").join("contracts");
    let effective_out_buf;
    let effective_out = match out_dir {
        Some(d) => d,
        None => {
            effective_out_buf = default_out;
            &effective_out_buf
        }
    };

    // Auto-generate module contracts for each crate (deterministic)
    generate_module_contracts(&root, effective_out, quiet)?;

    if detect_out_dir && out_dir.is_none() {
        let mut auto_detected = false;
        if root.join(".aden").join("contracts").join("crates").exists() {
            let contracts_crates = root.join(".aden").join("contracts").join("crates");
            if contracts_crates.is_dir() {
                progress!(quiet, "INFO: Detected .aden/contracts/crates/ structure. Using --detect-out-dir.");
                progress!(quiet, "      Contracts will be placed in .aden/contracts/crates/<crate>/src/");
                auto_detected = true;
            }
        }
        if !auto_detected {
            progress!(quiet, "INFO: No existing contract structure detected. Using default .aden/contracts/");
        }
    }

    if merge || propose {
        return cmd_gen_merge(&root, effective_out, propose);
    }

    // Default to auto mode for directories (backward compatible with single files)
    let auto_by_default = path.is_dir();
    if auto || auto_by_default {
        // ── AUTO MODE: workspace-aware incremental generation ────────────────
        let sources = discover_source_files(&root)?;
        if sources.is_empty() {
            eprintln!(
                "No source files discovered in {}. Is this a supported project?",
                root.display()
            );
            return Ok(());
        }

        // Open store for writing contracts
        let store_path = root.join(".aden").join("store");
        let storage = Storage::new(
            store_path
                .to_str()
                .expect("Store path should be valid UTF-8"),
        )
        .map_err(|e| format!("Failed to open store at {}: {}", store_path.display(), e))?;

        let cache_path = root.join(".aden").join("gen-cache.json");
        let mut cache = load_gen_cache(&cache_path);
        let mut generated = Vec::new();
        let mut skipped = 0usize;

        // Phase 1: Parallel file processing — read, parse, write to store
        let work_items: Vec<_> = sources
            .par_iter()
            .filter_map(|src_path| {
                let src_mtime = src_path
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                let mtime_secs = src_mtime
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();

                // Check mtime cache — if match, return Skip
                let src_rel = src_path.strip_prefix(&root).unwrap_or(src_path);
                let cache_key = src_rel.to_string_lossy().to_string();
                if let Some(e) = cache.entries.get(&cache_key) {
                    if e.source_mtime == mtime_secs {
                        return Some(WorkItem::Skip);
                    }
                }

                // Read source
                let source = match std::fs::read_to_string(src_path) {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::InvalidData => return None,
                    Err(e) => {
                        if !quiet {
                            eprintln!("WARN: Failed to read {}: {}", src_path.display(), e);
                        }
                        return None;
                    }
                };

                // Parse
                let docs = match aden_parse::parse_file(src_path, &source) {
                    Ok(d) => d,
                    Err(aden_core::Error::UnsupportedLanguage(_)) => return None,
                    Err(e) => {
                        eprintln!("WARN: Parse failed for {}: {}", src_path.display(), e);
                        return None;
                    }
                };

                // Write each document to store
                let mut emitted = Vec::new();
                for doc in docs {
                    let mut doc_clone = doc.clone();
                    sanitize_source_file(&mut doc_clone);

                    // Capture call sites for graph linking before slimming, then
                    // drop the redundant edge:: listing so the store stays compact.
                    // Real containment/Calls edges are built in link_store_edges,
                    // so the old parent-module relationship boilerplate is gone.
                    let callees = extract_callees(&doc_clone);
                    let refs = extract_doc_refs(&doc_clone);
                    slim_doc_for_store(&mut doc_clone);

                    if let Err(e) = storage.put_document(&doc_clone) {
                        eprintln!("WARN: Failed to store {}: {}", doc_clone.anchor, e);
                        continue;
                    }

                    if !quiet {
                        progress!(quiet, "Stored {}", doc_clone.anchor);
                    }

                    let cache_val = GenCacheEntry {
                        source_mtime: mtime_secs,
                        source_path: src_path.to_string_lossy().to_string(),
                    };

                    emitted.push(EmittedSymbol {
                        anchor: doc_clone.anchor.clone(),
                        cache_key: cache_key.clone(),
                        cache_val,
                        callees,
                        refs,
                    });
                }

                if emitted.is_empty() {
                    Some(WorkItem::Skip)
                } else {
                    Some(WorkItem::Emitted(emitted))
                }
            })
            .collect();

        // Phase 2: Merge parallel results into shared state. Collect compact
        // (anchor, callees) link records so the linker never reloads documents.
        let mut link_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut ref_records: Vec<(String, Vec<String>)> = Vec::new();
        for item in work_items {
            match item {
                WorkItem::Skip => skipped += 1,
                WorkItem::Emitted(emitted) => {
                    for sym in emitted {
                        generated.push(sym.anchor.clone());
                        cache.entries.insert(sym.cache_key, sym.cache_val);
                        if !sym.refs.is_empty() {
                            ref_records.push((sym.anchor.clone(), sym.refs));
                        }
                        if !sym.callees.is_empty() {
                            link_records.push((sym.anchor, sym.callees));
                        }
                    }
                }
            }
        }

        // Flush store to persist all documents
        storage.flush().map_err(|e| format!("Store flush failed: {}", e))?;

        // Connect the graph: persist module<->symbol containment and call edges
        // so the store-first graph used by asm/ask/query is actually traversable.
        if let Err(e) = link_store_edges(&storage, &link_records, &ref_records) {
            eprintln!("WARN: Failed to link graph edges: {}", e);
        }

        save_gen_cache(&cache_path, &cache)?;

        progress!(quiet, "\nStored {} contracts. Skipped {} unchanged files.", generated.len(), skipped);
        if skipped == 0 && generated.len() == sources.len() {
            progress!(quiet, "(All files were skipped — nothing changed since last run)");
        }

        // Report orphan symbols using store-first graph build. Suppressed in
        // quiet mode so the transparent refresh-on-read path stays silent.
        if !quiet {
            match AdenGraph::build_from_storage(&storage) {
                Ok(graph) => {
                    let orphans = graph.orphans();
                    if !orphans.is_empty() {
                        eprintln!("\nWARNING: {} orphan symbol(s) detected:", orphans.len());
                        for orphan in orphans.iter().take(5) {
                            eprintln!("  - {}", orphan);
                        }
                        if orphans.len() > 5 {
                            eprintln!("  ... and {} more", orphans.len() - 5);
                        }
                        eprintln!("  Run 'aden heal . --gc' to auto-link or remove orphans");
                    }
                }
                Err(e) => {
                    eprintln!("Note: Could not check for orphans: {}", e);
                }
            }
        }
    } else {
        // ── LEGACY MODE: flat parse_directory output ────────────────────────
        let docs = aden_parse::parse_directory(path)?;
        return emit_docs(docs, out_dir, path, format);
    }

    // Invalidate caches after generating contracts so next query rebuilds
    let cache_dir = path.join(".aden/cache");
    if cache_dir.exists() {
        let _ = std::fs::remove_dir_all(&cache_dir);
        let _ = std::fs::create_dir_all(&cache_dir);
    }

    Ok(())
}

/// Single-file contract generation with three-way merge support.
pub fn cmd_gen_contract(
    _path: &Path,
    _source: &str,
    docs: Vec<aden_core::Document>,
    out_dir: Option<&Path>,
    propose: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use aden_core::contract::{
        ContractDocument, ContractState, MergeAction, ParseMode, parse_contract,
    };

    if docs.is_empty() {
        return Ok(());
    }

    // NOTE: callers must pass a resolved out_dir. The fallback "contracts" is
    // intentionally relative to the CWD as a last resort; callers should never
    // hit this branch.
    let fallback = std::path::PathBuf::from("contracts");
    let effective_out = out_dir.unwrap_or(&fallback);
    std::fs::create_dir_all(effective_out)?;

    let contract_path = effective_out.join(format!("{}.adoc", sanitize_anchor(&docs[0].anchor)));

    // Ground: freshly generated contract from AST
    let ground_doc = ContractDocument::from_document(&docs[0]);

    // Base: last pure generated content (from .aden/contract-base/)
    let base_doc = if let Some(base_path) = base_cache_path(&contract_path) {
        if base_path.exists() {
            let existing = std::fs::read_to_string(&base_path)?;
            parse_contract(&existing, ParseMode::Permissive).unwrap_or_else(|_| ground_doc.clone())
        } else {
            ground_doc.clone()
        }
    } else {
        ground_doc.clone()
    };

    // Working: current contract file on disk (with possible human edits)
    let working_doc = if contract_path.exists() {
        let existing = std::fs::read_to_string(&contract_path)?;
        parse_contract(&existing, ParseMode::Permissive).unwrap_or_else(|e| {
            eprintln!(
                "WARN: Failed to parse existing contract {}: {}. Treating as fresh.",
                contract_path.display(),
                e
            );
            ground_doc.clone()
        })
    } else {
        ground_doc.clone()
    };

    let state = ContractState::new(ground_doc.clone(), base_doc, working_doc);
    let proposal = state.propose()?;

    if propose {
        println!("// Merge Proposal for {}", contract_path.display());
        println!(
            "//   Preserved: {} | Updated: {} | Conflicts: {} | Inserted: {} | Deleted: {}",
            proposal.preserved_count,
            proposal.updated_count,
            proposal.conflict_count,
            proposal.inserted_count,
            proposal.deleted_count
        );
        for action in &proposal.actions {
            match action {
                MergeAction::UpdateGenerated { index, .. } => {
                    println!("  UPDATE [generated] @ block {}", index);
                }
                MergeAction::PreserveHuman { index } => {
                    println!("  PRESERVE human/agent block @ {}", index);
                }
                MergeAction::Conflict { index, reason } => {
                    println!("  CONFLICT @ block {}: {}", index, reason);
                }
                MergeAction::InsertGenerated { after_index, .. } => {
                    println!("  INSERT [generated] after block {}", after_index);
                }
                MergeAction::DeleteGenerated { index, reason } => {
                    println!("  DELETE [generated] @ block {}: {}", index, reason);
                }
            }
        }
        return Ok(());
    }

    // Merge mode: apply and write
    let merged = state.apply(&proposal)?;
    let output = aden_emit::emit_contract_document(&merged);
    std::fs::write(&contract_path, output)?;
    println!("Merged contract: {}", contract_path.display());

    // Update base cache so next run has clean generated snapshot
    if let Some(base_path) = base_cache_path(&contract_path) {
        if let Some(parent) = base_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&base_path, aden_emit::emit_contract_document(&ground_doc))?;
    }

    Ok(())
}

/// Directory-mode contract generation with merge support.
pub fn cmd_gen_merge(
    root: &Path,
    effective_out: &Path,
    propose: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use aden_core::contract::{ContractDocument, ContractState, ParseMode, parse_contract};

    let sources = discover_source_files(root)?;
    if sources.is_empty() {
        eprintln!("No source files discovered in {}.", root.display());
        return Ok(());
    }

    std::fs::create_dir_all(effective_out)?;

    for src_path in &sources {
        let source = match std::fs::read_to_string(src_path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => continue,
            Err(e) => {
                eprintln!("WARN: Failed to read {}: {}", src_path.display(), e);
                continue;
            }
        };

        let docs = match aden_parse::parse_file(src_path, &source) {
            Ok(d) => d,
            Err(aden_core::Error::UnsupportedLanguage(_)) => continue,
            Err(e) => {
                eprintln!("WARN: Parse failed for {}: {}", src_path.display(), e);
                continue;
            }
        };

        for doc in &docs {
            let file_name = format!("{}.adoc", sanitize_anchor(&doc.anchor));
            let contract_path = effective_out.join(&file_name);

            let ground_doc = ContractDocument::from_document(doc);

            // Base: last pure generated content
            let base_doc = if let Some(base_path) = base_cache_path(&contract_path) {
                if base_path.exists() {
                    let existing = std::fs::read_to_string(&base_path)?;
                    parse_contract(&existing, ParseMode::Permissive)
                        .unwrap_or_else(|_| ground_doc.clone())
                } else {
                    ground_doc.clone()
                }
            } else {
                ground_doc.clone()
            };

            // Working: current contract file on disk
            let working_doc = if contract_path.exists() {
                let existing = std::fs::read_to_string(&contract_path)?;
                parse_contract(&existing, ParseMode::Permissive)
                    .unwrap_or_else(|_| ground_doc.clone())
            } else {
                ground_doc.clone()
            };

            let state = ContractState::new(ground_doc.clone(), base_doc, working_doc);
            let proposal = state.propose()?;

            if propose {
                println!("// Proposal: {}", contract_path.display());
                println!(
                    "//   Preserved: {} | Updated: {} | Conflicts: {} | Inserted: {} | Deleted: {}",
                    proposal.preserved_count,
                    proposal.updated_count,
                    proposal.conflict_count,
                    proposal.inserted_count,
                    proposal.deleted_count
                );
            } else {
                let merged = state.apply(&proposal)?;
                let output = aden_emit::emit_contract_document(&merged);
                std::fs::write(&contract_path, output)?;
                println!("Merged contract: {}", contract_path.display());

                // Update base cache
                if let Some(base_path) = base_cache_path(&contract_path) {
                    if let Some(parent) = base_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&base_path, aden_emit::emit_contract_document(&ground_doc))?;
                }
            }
        }
    }

    Ok(())
}
