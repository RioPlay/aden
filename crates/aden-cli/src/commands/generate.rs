// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use aden_graph::graph::AdenGraph;
use aden_store::{GraphStorage, Storage};
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::Path;

use crate::types::GenCacheEntry;
use crate::util::{
    discover_source_files, find_project_root, load_gen_cache, sanitize_source_file, save_gen_cache,
};

/// One stored symbol plus the compact data the linker needs. Carrying callee
/// names out of the parse phase means linking never has to reload the (huge)
/// document store to rebuild the call graph.
struct EmittedSymbol {
    anchor: String,
    callees: Vec<String>,
    /// `edge::uses[Type]` references — types named in a signature/fields, linked
    /// as `Uses` edges so a type that is used but never *called* is not a false
    /// dead-code candidate.
    uses: Vec<String>,
    /// `<<target>>` cross-references found in the document body (docs link to
    /// other docs / code via these).
    refs: Vec<String>,
}

/// Work item returned from parallel file processing.
///
/// `Reindexed` is emitted whenever a file was (re)parsed — even if it produced
/// ZERO symbols (e.g. every function was deleted). That is deliberate: the
/// prune step diffs each reindexed file's fresh anchor set against the set the
/// cache recorded last time, so an emptied file correctly drops its stale
/// symbols. `Skip` is reserved for files whose mtime is unchanged.
enum WorkItem {
    Skip,
    Reindexed {
        cache_key: String,
        source_mtime: u64,
        source_path: String,
        symbols: Vec<EmittedSymbol>,
    },
}

/// Emit a progress line unless quiet mode is on.
macro_rules! progress {
    ($quiet:expr, $($arg:tt)*) => {
        if !$quiet { println!($($arg)*); }
    };
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
        1 => segs[0],     // file at module root — use it
        n => segs[n - 2], // directory containing the file
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
                    if let Some(rest) = line.trim().strip_prefix("edge::calls[")
                        && let Some(callee) = rest.strip_suffix(']')
                            && !callee.is_empty() {
                                callees.push(callee.to_string());
                            }
                }
            }
            Block::Table(t)
                if t.headers.first().map(|h| h.eq_ignore_ascii_case("callee")) == Some(true) =>
            {
                for row in &t.rows {
                    if let Some(c) = row.first()
                        && !c.is_empty() {
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
fn extract_uses(doc: &aden_core::Document) -> Vec<String> {
    use aden_core::Block;
    let mut uses = Vec::new();
    for block in &doc.blocks {
        if let Block::Listing { code, .. } = block {
            for line in code.lines() {
                if let Some(rest) = line.trim().strip_prefix("edge::uses[")
                    && let Some(t) = rest.strip_suffix(']')
                        && !t.is_empty() {
                            uses.push(t.to_string());
                        }
            }
        }
    }
    uses.sort();
    uses.dedup();
    uses
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

/// Resolve a callee string to a single target anchor, or None if unknown.
/// Tries the full callee, then the trailing segment after the last `.`/`:` so
/// receiver/qualified calls link (`c.ExecuteC` → `ExecuteC`, `click.echo` →
/// `echo`, `Path::new` → `new`). When a name is ambiguous (defined in several
/// places) we disambiguate by locality: prefer a candidate in the caller's own
/// FILE, then in its crate. Most calls are intra-file/intra-crate, so this
/// resolves the common case (e.g. a private `node_text` helper copied into every
/// extractor file) instead of dropping the edge — without guessing across
/// modules, which would forge false edges.
fn resolve_callee<'a>(
    callee: &str,
    caller: &str,
    name_index: &HashMap<&str, Vec<&'a str>>,
) -> Option<&'a str> {
    let caller_file = anchor_file(caller);
    let caller_crate = crate_from_anchor(caller);
    let pick = |cands: &[&'a str]| -> Option<&'a str> {
        match cands {
            [] => None,
            [one] => Some(*one),
            many => {
                // Ambiguous: prefer the caller's own file, then its crate.
                if let Some(cf) = caller_file {
                    let same: Vec<&'a str> = many
                        .iter()
                        .copied()
                        .filter(|a| anchor_file(a) == Some(cf))
                        .collect();
                    if same.len() == 1 {
                        return Some(same[0]);
                    }
                }
                let cc = caller_crate.as_deref()?;
                let same: Vec<&'a str> = many
                    .iter()
                    .copied()
                    .filter(|a| crate_from_anchor(a).as_deref() == Some(cc))
                    .collect();
                if same.len() == 1 { Some(same[0]) } else { None }
            }
        }
    };
    if let Some(t) = name_index.get(callee)
        && let Some(r) = pick(t) {
            return Some(r);
        }
    let base = callee.rsplit(['.', ':']).next().unwrap_or(callee);
    if base != callee && !base.is_empty()
        && let Some(t) = name_index.get(base)
            && let Some(r) = pick(t) {
                return Some(r);
            }
    None
}

/// The file portion of a module anchor: `aden://module/<file>#<sym>` → `<file>`
/// (e.g. `aden-parse/rust.rs`). Used to scope ambiguous-callee resolution to the
/// caller's own file. Returns `None` for non-module anchors (docs, etc.).
fn anchor_file(anchor: &str) -> Option<&str> {
    anchor.strip_prefix("aden://module/")?.split('#').next()
}

/// Tally of how the call-site callee references fared during resolution. Counts
/// are per callee reference, not per built edge — self-calls and several callee
/// strings collapsing onto one target are each counted once here, yet build zero
/// or deduped edges. Pure diagnostics: it never changes which edges are built, it
/// just explains where the call graph thins out (a dropped call site is either an
/// `Unresolved` name that matches no stored symbol, or an `Ambiguous` one that
/// matches several and couldn't be disambiguated by locality).
#[derive(Default)]
struct CalleeStats {
    resolved: usize,
    unresolved: usize,
    ambiguous: usize,
}

/// Why a callee did not produce a `Calls` edge — used to attribute drops to the
/// messiness they signal (no such symbol vs. a name shared across modules).
enum DropReason {
    Unresolved,
    Ambiguous,
}

/// Classify a callee that `resolve_callee` declined to link. Mirrors that
/// function's lookup order (full name, then trailing segment) but only inspects
/// candidate *counts*: zero candidates → `Unresolved`, otherwise the locality
/// heuristic gave up on multiple → `Ambiguous`. Cheap (HashMap lookups, no
/// allocation) and never alters resolution.
fn classify_drop(callee: &str, name_index: &HashMap<&str, Vec<&str>>) -> DropReason {
    let count = |name: &str| name_index.get(name).map(|c| c.len()).unwrap_or(0);
    let mut n = count(callee);
    if n == 0 {
        let base = callee.rsplit(['.', ':']).next().unwrap_or(callee);
        if base != callee && !base.is_empty() {
            n = count(base);
        }
    }
    if n == 0 {
        DropReason::Unresolved
    } else {
        DropReason::Ambiguous
    }
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
///
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
    use_records: &[(String, Vec<String>)],
    ref_records: &[(String, Vec<String>)],
) -> Result<CalleeStats, Box<dyn std::error::Error>> {
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

    // Call edges from the compact per-symbol records. Tally each callee so the
    // gen summary can flag where the call graph silently thins out.
    let mut callee_stats = CalleeStats::default();
    for (anchor, callees) in link_records {
        for callee in callees {
            match resolve_callee(callee, anchor, &name_index) {
                Some(target) if target != anchor.as_str() => {
                    callee_stats.resolved += 1;
                    edges.push((anchor.clone(), target.to_string(), EdgeType::Calls));
                }
                // A self-call resolves but builds no edge (we skip self-loops);
                // count it as resolved so it isn't mistaken for a dropped edge.
                Some(_) => callee_stats.resolved += 1,
                None => match classify_drop(callee, &name_index) {
                    DropReason::Unresolved => callee_stats.unresolved += 1,
                    DropReason::Ambiguous => callee_stats.ambiguous += 1,
                },
            }
        }
    }

    // Type-usage edges: a symbol whose signature/fields name a stored type
    // `Uses` it. Keeps a type that is used (but never *called*) from looking like
    // dead code in graph-wide queries like `where callers=0`.
    for (anchor, used_types) in use_records {
        for used in used_types {
            if let Some(target) = resolve_callee(used, anchor, &name_index)
                && target != anchor.as_str() {
                    edges.push((anchor.clone(), target.to_string(), EdgeType::Uses));
                }
        }
    }

    // Cross-reference edges from document `<<target>>` macros. Bidirectional so
    // backlinks work (a doc and what it references are mutually reachable).
    for (anchor, refs) in ref_records {
        for r in refs {
            if let Some(target) = resolve_callee(r, anchor, &name_index)
                && target != anchor.as_str() {
                    edges.push((anchor.clone(), target.to_string(), EdgeType::RelatesTo));
                    edges.push((target.to_string(), anchor.clone(), EdgeType::RelatesTo));
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
            edges.push((
                project.to_string(),
                module_anchor.clone(),
                EdgeType::Documents,
            ));
            edges.push((module_anchor, project.to_string(), EdgeType::PartOf));
        }
    }

    storage.put_edges_bulk(&edges)?;
    storage.flush()?;
    Ok(callee_stats)
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
        let _ = cmd_gen_silent(&root);
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
    let newest_known = cache
        .entries
        .values()
        .map(|e| e.source_mtime)
        .max()
        .unwrap_or(0);

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
        // Silent incremental regen: re-parses only changed files and re-links
        // edges, without printing anything (this runs transparently on reads).
        let _ = cmd_gen_silent(&root);
    }
}

/// Auto-document a codebase: discover source files, skip unchanged,
/// emit structured contracts to store, and optionally to disk.
/// Compile source into the store.
///
/// Two independent verbosity axes:
/// - `quiet`  — suppress the per-file "Stored <anchor>" progress lines but
///   still print the one-line summary. This is what `--quiet`/`regen` want
///   ("summary only").
/// - `silent` — suppress EVERYTHING, including the summary and parse warnings.
///   This is the transparent refresh-on-read path (`ensure_fresh`), which must
///   never write to stdout/stderr during `ask`/`query`/`grep`/etc.
pub fn cmd_gen(path: &Path, quiet: bool) -> Result<(), Box<dyn std::error::Error>> {
    cmd_gen_inner(path, quiet, false)
}

/// Fully-silent variant for the auto-refresh path (see `ensure_fresh`).
pub fn cmd_gen_silent(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    cmd_gen_inner(path, true, true)
}

fn cmd_gen_inner(path: &Path, quiet: bool, silent: bool) -> Result<(), Box<dyn std::error::Error>> {
    if !path.exists() {
        return Err("Path does not exist or is not a file/directory".into());
    }

    // Project root: for a single file, search upward from its directory.
    let search_start = if path.is_file() {
        path.parent().unwrap_or(path)
    } else {
        path
    };
    let root = find_project_root(search_start);

    // Store-first: `gen` writes ONLY to .aden/store. Module hub nodes
    // (mod-project, mod-<crate>) are synthesized into the store by
    // link_store_edges — no .adoc files or contracts/ directory are emitted.
    {
        // A single file re-indexes just itself; a directory indexes the project.
        let sources = if path.is_file() {
            vec![path.to_path_buf()]
        } else {
            discover_source_files(&root)?
        };
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
                // Security floor: never index credential material into the store
                // (where ask/asm would assemble it into LLM context). Search
                // (grep/locate/audit) can still see these files to fix them.
                if aden_core::filter::is_secret_path(src_rel) {
                    return None;
                }
                let cache_key = src_rel.to_string_lossy().to_string();
                if let Some(e) = cache.entries.get(&cache_key)
                    && e.source_mtime == mtime_secs {
                        return Some(WorkItem::Skip);
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

                // Security floor (content): the filename-based is_secret_path
                // check above misses a credential embedded in an ordinary source
                // or config file. Scan content for high-confidence secret tokens
                // (AWS/GitHub/OpenAI/Slack keys, PEM private keys) and refuse to
                // index such a file into the store, where ask/asm would otherwise
                // assemble it into LLM context (CWE-798/CWE-200).
                if aden_core::filter::content_has_high_confidence_secret(&source) {
                    if !silent {
                        eprintln!(
                            "WARN: Skipping {} — file content matches a credential pattern (not indexed). Add to .adenignore if intentional.",
                            src_rel.display()
                        );
                    }
                    return None;
                }

                // Parse
                let docs = match aden_parse::parse_file(src_path, &source) {
                    Ok(d) => d,
                    Err(aden_core::Error::UnsupportedLanguage(_)) => return None,
                    Err(e) => {
                        if !silent {
                            eprintln!("WARN: Parse failed for {}: {}", src_path.display(), e);
                        }
                        return None;
                    }
                };

                // Write each document to store
                let mut emitted = Vec::new();
                for doc in docs {
                    let mut doc_clone = doc.clone();
                    sanitize_source_file(&mut doc_clone, &root);

                    // Capture call sites for graph linking before slimming, then
                    // drop the redundant edge:: listing so the store stays compact.
                    // Real containment/Calls edges are built in link_store_edges,
                    // so the old parent-module relationship boilerplate is gone.
                    let callees = extract_callees(&doc_clone);
                    let uses = extract_uses(&doc_clone);
                    let refs = extract_doc_refs(&doc_clone);
                    slim_doc_for_store(&mut doc_clone);

                    if let Err(e) = storage.put_document(&doc_clone) {
                        eprintln!("WARN: Failed to store {}: {}", doc_clone.anchor, e);
                        continue;
                    }

                    if !quiet {
                        progress!(quiet, "Stored {}", doc_clone.anchor);
                    }

                    emitted.push(EmittedSymbol {
                        anchor: doc_clone.anchor.clone(),
                        callees,
                        uses,
                        refs,
                    });
                }

                // Always report a reindexed file — even with zero symbols — so
                // the prune step can drop anchors a now-empty file used to own.
                Some(WorkItem::Reindexed {
                    cache_key: cache_key.clone(),
                    source_mtime: mtime_secs,
                    source_path: src_path.to_string_lossy().to_string(),
                    symbols: emitted,
                })
            })
            .collect();

        // Phase 2: Merge parallel results into shared state. Collect compact
        // (anchor, callees) link records so the linker never reloads documents.
        let mut link_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut use_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut ref_records: Vec<(String, Vec<String>)> = Vec::new();
        // Anchors to prune: symbols a reindexed file no longer defines.
        let mut stale_anchors: Vec<String> = Vec::new();
        for item in work_items {
            match item {
                WorkItem::Skip => skipped += 1,
                WorkItem::Reindexed {
                    cache_key,
                    source_mtime,
                    source_path,
                    symbols,
                } => {
                    let fresh: Vec<String> = symbols.iter().map(|s| s.anchor.clone()).collect();
                    // Diff against what this file contributed last time: any
                    // previously-recorded anchor not in the fresh set is a
                    // symbol that was deleted/renamed and must be pruned.
                    if let Some(prev) = cache.entries.get(&cache_key) {
                        for old in &prev.anchors {
                            if !fresh.contains(old) {
                                stale_anchors.push(old.clone());
                            }
                        }
                    }
                    cache.entries.insert(
                        cache_key,
                        GenCacheEntry {
                            source_mtime,
                            source_path,
                            anchors: fresh,
                        },
                    );
                    for sym in symbols {
                        generated.push(sym.anchor.clone());
                        if !sym.refs.is_empty() {
                            ref_records.push((sym.anchor.clone(), sym.refs));
                        }
                        if !sym.uses.is_empty() {
                            use_records.push((sym.anchor.clone(), sym.uses));
                        }
                        if !sym.callees.is_empty() {
                            link_records.push((sym.anchor, sym.callees));
                        }
                    }
                }
            }
        }

        // Case (b): whole-file deletion. On a full-tree gen (NOT a single-file
        // re-index, which only knows about one file), any cache entry whose
        // source file is no longer in the discovered set is gone — prune all
        // anchors it owned and drop the entry.
        if !path.is_file() {
            let live: std::collections::HashSet<String> = sources
                .iter()
                .map(|p| {
                    p.strip_prefix(&root)
                        .unwrap_or(p)
                        .to_string_lossy()
                        .to_string()
                })
                .collect();
            let dead_keys: Vec<String> = cache
                .entries
                .keys()
                .filter(|k| !live.contains(*k))
                .cloned()
                .collect();
            for k in dead_keys {
                if let Some(entry) = cache.entries.remove(&k) {
                    stale_anchors.extend(entry.anchors);
                }
            }
        }

        // Prune stale nodes (deleted symbols / deleted files). delete_node
        // cascades edges in both directions so no dangling reference survives.
        // Guard: never touch synthesized hub nodes (mod-*) — they carry no
        // source_file and are rebuilt by link_store_edges below.
        let mut pruned = 0usize;
        for anchor in &stale_anchors {
            if anchor.starts_with("mod-") {
                continue;
            }
            match storage.delete_node(anchor) {
                Ok(()) => pruned += 1,
                Err(e) => {
                    if !silent {
                        eprintln!("WARN: Failed to prune {}: {}", anchor, e);
                    }
                }
            }
        }

        // Flush store to persist all documents
        storage
            .flush()
            .map_err(|e| format!("Store flush failed: {}", e))?;

        // Connect the graph: persist module<->symbol containment and call edges
        // so the store-first graph used by asm/ask/query is actually traversable.
        let callee_stats = match link_store_edges(&storage, &link_records, &use_records, &ref_records)
        {
            Ok(stats) => stats,
            Err(e) => {
                eprintln!("WARN: Failed to link graph edges: {}", e);
                CalleeStats::default()
            }
        };

        save_gen_cache(&cache_path, &cache)?;

        // The summary is "summary only" output: shown under --quiet/regen, but
        // suppressed entirely on the silent refresh-on-read path.
        if pruned > 0 {
            progress!(
                silent,
                "\nStored {} contracts. Skipped {} unchanged files. Pruned {} stale symbol(s).",
                generated.len(),
                skipped,
                pruned
            );
        } else {
            progress!(
                silent,
                "\nStored {} contracts. Skipped {} unchanged files.",
                generated.len(),
                skipped
            );
        }
        if skipped == 0 && generated.len() == sources.len() {
            progress!(
                silent,
                "(All files were skipped — nothing changed since last run)"
            );
        }

        // Call-graph resolution health. Dropped call sites (unresolved/ambiguous
        // callees) are exactly where the graph thins out, so surface the counts
        // on the same summary channel — quiet/regen still shows it, the silent
        // refresh-on-read path stays silent. Only emit when something dropped to
        // keep the clean case uncluttered. Counts are per callee reference, not
        // per built edge (self-calls / collapsed targets inflate `resolved`).
        if callee_stats.unresolved > 0 || callee_stats.ambiguous > 0 {
            progress!(
                silent,
                "Call sites: {} resolved, {} unresolved, {} ambiguous.",
                callee_stats.resolved,
                callee_stats.unresolved,
                callee_stats.ambiguous
            );
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
    }

    // Invalidate caches after generating so the next query rebuilds
    let cache_dir = path.join(".aden/cache");
    if cache_dir.exists() {
        let _ = std::fs::remove_dir_all(&cache_dir);
        let _ = std::fs::create_dir_all(&cache_dir);
    }

    Ok(())
}

#[cfg(test)]
mod link_tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn anchor_file_extracts_path() {
        assert_eq!(
            anchor_file("aden://module/aden-parse/rust.rs#node_text"),
            Some("aden-parse/rust.rs")
        );
        assert_eq!(anchor_file("aden://doc/x/y.md/h1foo"), None);
    }

    #[test]
    fn resolve_unique_callee_links() {
        let mut idx: HashMap<&str, Vec<&str>> = HashMap::new();
        idx.insert("foo", vec!["aden://module/c/a.rs#foo"]);
        assert_eq!(
            resolve_callee("foo", "aden://module/c/b.rs#bar", &idx),
            Some("aden://module/c/a.rs#foo")
        );
    }

    #[test]
    fn ambiguous_callee_prefers_same_file() {
        let mut idx: HashMap<&str, Vec<&str>> = HashMap::new();
        idx.insert(
            "node_text",
            vec![
                "aden://module/aden-parse/rust.rs#node_text",
                "aden://module/aden-parse/tree_sitter_common.rs#node_text",
                "aden://module/aden-cli/x.rs#node_text",
            ],
        );
        // Caller in rust.rs → the rust.rs copy wins (same file), not the shared one.
        assert_eq!(
            resolve_callee(
                "node_text",
                "aden://module/aden-parse/rust.rs#extract_struct",
                &idx
            ),
            Some("aden://module/aden-parse/rust.rs#node_text")
        );
    }

    #[test]
    fn ambiguous_callee_falls_back_to_same_crate_then_gives_up() {
        let mut idx: HashMap<&str, Vec<&str>> = HashMap::new();
        idx.insert(
            "helper",
            vec![
                "aden://module/crate-a/x.rs#helper",
                "aden://module/crate-b/y.rs#helper",
            ],
        );
        // Different file but same crate → same-crate wins.
        assert_eq!(
            resolve_callee("helper", "aden://module/crate-a/z.rs#caller", &idx),
            Some("aden://module/crate-a/x.rs#helper")
        );
        // Caller in a third crate → genuinely ambiguous, do not guess.
        assert_eq!(
            resolve_callee("helper", "aden://module/crate-c/z.rs#caller", &idx),
            None
        );
    }

    #[test]
    fn classify_drop_zero_candidates_is_unresolved() {
        // Empty index: nothing matches the callee at all.
        let idx: HashMap<&str, Vec<&str>> = HashMap::new();
        assert!(
            matches!(classify_drop("nonexistent", &idx), DropReason::Unresolved),
            "a callee with zero candidates must be Unresolved"
        );
        // A name present but unrelated to the callee is still zero candidates.
        let mut idx2: HashMap<&str, Vec<&str>> = HashMap::new();
        idx2.insert("something_else", vec!["aden://module/c/a.rs#something_else"]);
        assert!(matches!(classify_drop("missing", &idx2), DropReason::Unresolved));
    }

    #[test]
    fn classify_drop_multiple_full_name_candidates_is_ambiguous() {
        // The full callee name itself resolves to >= 2 candidates.
        let mut idx: HashMap<&str, Vec<&str>> = HashMap::new();
        idx.insert(
            "helper",
            vec![
                "aden://module/crate-a/x.rs#helper",
                "aden://module/crate-b/y.rs#helper",
            ],
        );
        assert!(
            matches!(classify_drop("helper", &idx), DropReason::Ambiguous),
            ">=2 candidates for the full name must be Ambiguous"
        );
    }

    #[test]
    fn classify_drop_trailing_segment_with_multiple_candidates_is_ambiguous() {
        // Full qualified name has no candidates, but its trailing segment
        // (after '.' / ':') matches >= 2 — mirrors resolve_callee's fallback.
        let mut idx: HashMap<&str, Vec<&str>> = HashMap::new();
        idx.insert(
            "node_text",
            vec![
                "aden://module/aden-parse/rust.rs#node_text",
                "aden://module/aden-parse/python.rs#node_text",
            ],
        );
        // Dotted receiver call: `self.node_text` → base `node_text`.
        assert!(
            matches!(classify_drop("self.node_text", &idx), DropReason::Ambiguous),
            "trailing segment after '.' with >=2 candidates must be Ambiguous"
        );
        // Path-qualified call: `Parser::node_text` → base `node_text`.
        assert!(
            matches!(classify_drop("Parser::node_text", &idx), DropReason::Ambiguous),
            "trailing segment after ':' with >=2 candidates must be Ambiguous"
        );
    }

    #[test]
    fn extract_uses_reads_edge_uses_listings() {
        use aden_core::{Block, Document, NodeType};
        let doc = Document {
            anchor: "x".into(),
            node_type: NodeType::Function,
            attributes: Default::default(),
            blocks: vec![Block::Listing {
                language: None,
                code: "edge::uses[EmittedSymbol]\nedge::uses[DocumentNode]".into(),
            }],
            source_span: None,
            metadata: None,
            confidence: 1.0,
        };
        // sorted + deduped
        assert_eq!(
            extract_uses(&doc),
            vec!["DocumentNode".to_string(), "EmittedSymbol".to_string()]
        );
    }
}
