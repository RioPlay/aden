// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use aden_graph::graph::AdenGraph;
use aden_store::{GraphStorage, Storage};
use rayon::prelude::*;
use std::path::Path;

use crate::indexer::link::{CalleeStats, EdgeRecords, link_include_edges, link_store_edges};
use crate::indexer::merge::{slim_doc_for_store, write_merge_proposals};
use crate::types::GenCacheEntry;
use crate::util::{
    cochange_pairs, discover_source_files, extract_callees, extract_demonstrates,
    extract_doc_includes, extract_doc_mentions, extract_doc_refs, extract_doc_supersedes,
    extract_doc_terms, extract_edge_macro, extract_uses, find_project_root, load_gen_cache,
    sanitize_source_file, save_gen_cache,
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
    /// Prose cross-references (`ref:<fragment>` entries from the parser-filled
    /// `doc_refs` attribute — AsciiDoc `<<target>>`/`xref:`, markdown
    /// `[text](#frag)`). Linked as bidirectional `RelatesTo` edges against doc
    /// anchor fragments only.
    refs: Vec<String>,
    /// `include::` document-composition targets (raw file paths from the
    /// `doc_includes` attribute). Linked as directional `Requires` edges.
    includes: Vec<String>,
    /// `edge::implements[Trait::method]` references (trait impls) — linked as
    /// `Implements` edges so blast radius reaches implementors (Wave 1).
    implements: Vec<String>,
    /// `edge::mutates[Type]` references (`&mut self` receivers) — linked as
    /// `Mutates` edges from a method to the type whose state it writes.
    mutates: Vec<String>,
    /// Backtick symbol names from the parser-filled `doc_mentions` attribute
    /// (prose only — the parsers' fence state keeps listings out). Linked as
    /// `Mentions` edges when the name resolves to exactly one code symbol
    /// (Wave 2).
    mentions: Vec<String>,
    /// Supersede-context refs (`<by|of>:ref:<frag>` entries from the
    /// parser-filled `doc_supersedes` attribute — a cross-reference on a line
    /// with supersede language). Linked as directed NEW —Supersedes→ OLD
    /// edges against doc anchor fragments only (Wave 3).
    supersedes: Vec<String>,
    /// `kind:name` entries from the parser-filled `symbol_references`
    /// attribute on doc code listings. Linked as `Demonstrates` edges under
    /// the same unambiguous-only rule (Wave 2).
    demonstrates: Vec<String>,
    /// Full `aden://term/…` anchors from the parser-filled `doc_terms`
    /// attribute on glossary sections. Linked as `DefinesTerm` edges by exact
    /// anchor match (the parser constructed both ends, so no fuzzing).
    defines_terms: Vec<String>,
    /// Whether this symbol's generated document was actually written to the
    /// store. False when the merge gate held it back (overlay conflict) or on a
    /// dry-run — so the summary count reflects real writes, not just processing.
    wrote: bool,
}

/// Work item returned from parallel file processing.
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
        /// Slimmed documents to write to the store. Carried out of the parallel
        /// pass so the actual `put_document` happens *sequentially in sorted
        /// source order* in Phase 2 — otherwise two files sharing a basename
        /// anchor (e.g. `openapi2/helpers.go` + `openapi3/helpers.go` →
        /// `helpers.go#copyURI`) race, and the collision winner (hence the index)
        /// is non-deterministic. Empty on a `--propose` dry-run.
        docs: Vec<aden_core::Document>,
        /// Anchors whose freshly-generated content collided with durable
        /// `[human]`/`[agent]` overlay intent. Surfaced as proposals; the
        /// stored document is left untouched for these.
        conflicts: Vec<(String, aden_core::contract::MergeProposal)>,
    },
}
/// Emit a progress line unless quiet mode is on.
macro_rules! progress {
    ($quiet:expr, $($arg:tt)*) => {
        if !$quiet { println!($($arg)*); }
    };
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
    cmd_gen_inner(path, quiet, false, false, false)
}

/// `gen` with the three-way-merge flags exposed on the CLI.
///
/// * `propose` — dry-run: reconcile and write conflict proposals, but never
///   mutate the store.
/// * `force` — bypass the merge gate and overwrite the store unconditionally
///   (emergency escape hatch; can clobber `[human]`/`[agent]` overlay collisions).
pub fn cmd_gen_opts(
    path: &Path,
    quiet: bool,
    propose: bool,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    cmd_gen_inner(path, quiet, false, propose, force)
}

/// Fully-silent variant for the auto-refresh path (see `ensure_fresh`).
pub fn cmd_gen_silent(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    cmd_gen_inner(path, true, true, false, false)
}
/// Lock the store for a write phase (G4). Held for the whole of `gen` so two
/// concurrent runs against one store cannot interleave their writes and corrupt
/// it — parallel-agent driving makes that concurrency real. Waits up to 10
/// minutes for a live holder; a dead holder is reclaimed immediately (process
/// liveness on Linux), so this blocks only on a genuinely active gen. The
/// lockfile is a sibling of the store directory.
fn acquire_store_lock(store_path: &Path) -> std::io::Result<aden_core::lock::FileLock> {
    aden_core::lock::FileLock::acquire_timeout(
        store_path.with_extension("lock"),
        std::time::Duration::from_secs(600),
    )
}
#[cfg(test)]
mod store_lock_tests {
    use super::*;

    #[test]
    fn gen_store_lock_is_exclusive_and_releases() {
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("store");

        let held = acquire_store_lock(&store_path).expect("first gen takes the lock");
        // A second writer on the same store's sibling lockfile is blocked.
        let blocked = aden_core::lock::FileLock::acquire_timeout(
            store_path.with_extension("lock"),
            std::time::Duration::from_millis(100),
        );
        assert_eq!(
            blocked.expect_err("second writer must block").kind(),
            std::io::ErrorKind::WouldBlock
        );

        drop(held);
        // Once the first writer finishes, the store is acquirable again.
        aden_core::lock::FileLock::acquire_timeout(
            store_path.with_extension("lock"),
            std::time::Duration::from_millis(100),
        )
        .expect("lock is free after release");
    }
}
fn cmd_gen_inner(
    path: &Path,
    quiet: bool,
    silent: bool,
    propose: bool,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
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
        let mut sources = if path.is_file() {
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

        // ADR-003: the store now lives in the per-user data dir, keyed per
        // project. Refuse to create one at $HOME / fs-root unless explicit, then
        // migrate any legacy in-tree store before opening the central one.
        aden_paths::guard_creatable_root(&root, crate::util::creation_explicit())?;
        crate::util::migrate_legacy_store(&root);
        let store_path = aden_paths::store_dir(&root);
        if let Some(parent) = store_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create store dir {}: {}", parent.display(), e))?;
        }
        // G4: serialize concurrent writers. Held until this function returns,
        // covering the whole open/index/flush write phase.
        let _store_lock = acquire_store_lock(&store_path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::WouldBlock {
                format!(
                    "store locked — another aden process holds the lock at {}. \
                     Wait for it to finish or stop the other process.",
                    store_path.with_extension("lock").display()
                )
            } else {
                format!(
                    "failed to acquire store lock at {}: {e}",
                    store_path.display()
                )
            }
        })?;
        let store_str = store_path
            .to_str()
            .expect("Store path should be valid UTF-8");
        // A store that does not exist yet — first gen, a manual `rm -rf store/`,
        // or a `regen` wipe — is empty: its incremental gen-cache, if any, is now
        // stale and must be ignored, or every file would be skipped as
        // "unchanged" against an empty store and the rebuild would silently
        // produce nothing (the recovery-from-deletion trap).
        let mut full_rebuild = force || !store_path.exists();
        let storage = match Storage::new(store_str) {
            Ok(s) => s,
            Err(aden_store::StoreError::IncompatibleVersion(_)) => {
                // The on-disk store was written in an engine format this build
                // cannot read (e.g. a fjall major upgrade). The store is a
                // rebuildable cache (ADR-003), so wipe it and rebuild from
                // source. Scoped strictly to this signal — a generic Io error
                // falls through to the hard-failure arm below and never wipes.
                //
                // SAFETY: a pinned/shared `$ADEN_STORE` may hold several projects'
                // data; never auto-wipe shared state. Surface an actionable error
                // and let the user decide (mirrors `regen`'s pinned-store
                // caution). The default per-project store is safe to wipe.
                if std::env::var_os("ADEN_STORE").is_some() {
                    return Err(format!(
                        "Store at {} is in an incompatible engine format, and $ADEN_STORE \
                         is pinned/shared — refusing to auto-wipe shared state. Unset \
                         $ADEN_STORE for a per-project store, or run `aden regen` to rebuild.",
                        store_path.display()
                    )
                    .into());
                }
                progress!(
                    silent,
                    "Store format changed (engine upgrade) — rebuilding from source."
                );
                let _ = std::fs::remove_dir_all(&store_path);
                full_rebuild = true;
                // The wipe destroyed the WHOLE store, so a single-file `gen` must
                // now repopulate the whole project — otherwise the rebuild would
                // re-index just that one file and leave a near-empty graph.
                if path.is_file() {
                    sources = discover_source_files(&root)?;
                }
                Storage::new(store_str).map_err(|e| {
                    format!("Failed to rebuild store at {}: {}", store_path.display(), e)
                })?
            }
            Err(e) => {
                return Err(
                    format!("Failed to open store at {}: {}", store_path.display(), e).into(),
                );
            }
        };
        let _ = aden_paths::write_meta(&root);

        let cache_path = aden_paths::gen_cache_file(&root);
        let mut cache = load_gen_cache(&cache_path);
        let mut generated = Vec::new();
        let mut skipped = 0usize;

        // Anchors that have an intent overlay on disk. Computed once: only these
        // symbols can produce a merge conflict, so the gate skips the per-symbol
        // store read + overlay parse for every other symbol. Empty when there is
        // no `.aden/overlays/` directory (the common case → zero overhead).
        let overlay_slugs = crate::commands::overlay::overlay_slugs(&root);

        // Phase 1: Parallel file processing — read, parse, write to store
        let mut work_items: Vec<_> = sources
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
                // `--force-regen`/`--propose` and a full rebuild (new/empty/
                // recovered store) must re-examine every file even when its mtime
                // is unchanged: force needs to overwrite, the dry-run needs to
                // audit current state against overlays, and a full rebuild has an
                // empty store whose "unchanged" cache entries no longer reflect
                // any stored contract.
                if !full_rebuild
                    && !propose
                    && let Some(e) = cache.entries.get(&cache_key)
                    && e.source_mtime == mtime_secs
                {
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

                // Parse documents; the actual store write is deferred to Phase 2
                // (sequential, sorted) so basename-anchor collisions resolve
                // deterministically instead of racing across worker threads.
                let mut emitted = Vec::new();
                let mut conflicts: Vec<(String, aden_core::contract::MergeProposal)> = Vec::new();
                let mut docs_local: Vec<aden_core::Document> = Vec::new();
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
                    let includes = extract_doc_includes(&doc_clone);
                    let implements = extract_edge_macro(&doc_clone, "implements");
                    let mutates = extract_edge_macro(&doc_clone, "mutates");
                    let mentions = extract_doc_mentions(&doc_clone);
                    let supersedes = extract_doc_supersedes(&doc_clone);
                    let demonstrates = extract_demonstrates(&doc_clone);
                    let defines_terms = extract_doc_terms(&doc_clone);
                    slim_doc_for_store(&mut doc_clone);

                    // Three-way merge gate. A conflict can only arise when the
                    // symbol has an overlay, so for everything else we skip the
                    // store read + overlay parse entirely (zero overhead in the
                    // common case). `propose` is a dry-run that never writes;
                    // `force` skips the gate (no notices).
                    //
                    // Semantics are *notify*, not block: the generated layer
                    // always updates so the store never drifts from source, and
                    // the overlay's durable intent is preserved (separate file)
                    // and delivered to readers (folded into the read graph). When
                    // a guarded generated unit changes we record a *notice* so the
                    // author re-reviews their overlay. The notice self-clears on
                    // the next run once the generated content settles.
                    let write = !propose;
                    if !force
                        && overlay_slugs.contains(
                            &crate::commands::overlay::sanitize_anchor_filename(&doc_clone.anchor),
                        )
                    {
                        let stored = storage.get_document(&doc_clone.anchor).ok().flatten();
                        let overlay = crate::commands::overlay::load_overlay(
                            &root,
                            &doc_clone.anchor,
                        )
                        .ok()
                        .flatten();
                        if let Ok(p) = aden_core::contract::reconcile_anchor(
                            &doc_clone,
                            stored.as_ref(),
                            overlay.as_ref(),
                        ) && !p.is_clean()
                        {
                            conflicts.push((doc_clone.anchor.clone(), p));
                        }
                    }

                    emitted.push(EmittedSymbol {
                        anchor: doc_clone.anchor.clone(),
                        callees,
                        uses,
                        refs,
                        includes,
                        implements,
                        mutates,
                        mentions,
                        supersedes,
                        demonstrates,
                        defines_terms,
                        wrote: write,
                    });
                    // Defer the write — Phase 2 stores these in sorted source order.
                    if write {
                        docs_local.push(doc_clone);
                    }
                }

                // Always report a reindexed file — even with zero symbols — so
                // the prune step can drop anchors a now-empty file used to own.
                Some(WorkItem::Reindexed {
                    cache_key: cache_key.clone(),
                    source_mtime: mtime_secs,
                    source_path: src_path.to_string_lossy().to_string(),
                    symbols: emitted,
                    docs: docs_local,
                    conflicts,
                })
            })
            .collect();

        // Phase 2: Merge parallel results into shared state. Collect compact
        // (anchor, callees) link records so the linker never reloads documents.
        let mut link_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut use_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut ref_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut include_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut impl_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut mutates_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut mention_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut supersede_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut demo_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut term_records: Vec<(String, Vec<String>)> = Vec::new();
        // Anchors whose SOURCE FILE is a test/spec file (conventional path
        // markers, shared with ask-routing's `is_test_result`). Their resolved
        // calls are additionally emitted as `Tests` edges. Lookup-only — never
        // iterated — so it cannot perturb emission order (determinism).
        let mut test_anchors: std::collections::HashSet<String> = std::collections::HashSet::new();
        // Anchors to prune: symbols a reindexed file no longer defines.
        let mut stale_anchors: Vec<String> = Vec::new();
        // Merge conflicts surfaced by the reconcile gate, written as proposals.
        let mut merge_conflicts: Vec<(String, aden_core::contract::MergeProposal)> = Vec::new();
        // Deterministic store writes: order work by source path so that when two
        // files share a basename anchor, the collision winner is the same every run
        // (last sorted source wins) — making the store, and the index built from it,
        // reproducible. The parallel pass deferred every `put_document` to here.
        fn work_key(w: &WorkItem) -> &str {
            match w {
                WorkItem::Reindexed { source_path, .. } => source_path.as_str(),
                WorkItem::Skip => "",
            }
        }
        work_items.sort_by(|a, b| work_key(a).cmp(work_key(b)));
        for item in work_items {
            match item {
                WorkItem::Skip => skipped += 1,
                WorkItem::Reindexed {
                    cache_key,
                    source_mtime,
                    source_path,
                    symbols,
                    docs,
                    conflicts,
                } => {
                    for d in &docs {
                        if let Err(e) = storage.put_document(d) {
                            eprintln!("WARN: Failed to store {}: {}", d.anchor, e);
                            continue;
                        }
                        // Record the canonical contract text as the base snapshot
                        // for three-way merges.  The snapshot is the
                        // `emit_contract_document` output for the exact document
                        // written above (already slimmed by slim_doc_for_store);
                        // `parse_contract` is its exact inverse so the round-trip
                        // is lossless.
                        let snapshot = aden_emit::emit_contract_document(
                            &aden_core::contract::ContractDocument::from_document(d),
                        );
                        if let Err(e) = storage.put_base_snapshot(&d.anchor, &snapshot) {
                            eprintln!(
                                "WARN: Failed to record base snapshot for {}: {}",
                                d.anchor, e
                            );
                        }
                    }
                    merge_conflicts.extend(conflicts);
                    // The module-form anchor flattens the directory, so
                    // test-ness comes from the real (root-relative) source
                    // path — the same rule ask-routing applies at query time.
                    let from_test_file = crate::commands::query::is_test_source_path(&cache_key);
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
                        // Count only symbols actually written to the store, so the
                        // summary never claims to have stored a conflict-held doc.
                        if sym.wrote {
                            generated.push(sym.anchor.clone());
                        }
                        if !sym.refs.is_empty() {
                            ref_records.push((sym.anchor.clone(), sym.refs));
                        }
                        if !sym.includes.is_empty() {
                            include_records.push((sym.anchor.clone(), sym.includes));
                        }
                        if !sym.uses.is_empty() {
                            use_records.push((sym.anchor.clone(), sym.uses));
                        }
                        if !sym.implements.is_empty() {
                            impl_records.push((sym.anchor.clone(), sym.implements));
                        }
                        if !sym.mutates.is_empty() {
                            mutates_records.push((sym.anchor.clone(), sym.mutates));
                        }
                        if !sym.mentions.is_empty() {
                            mention_records.push((sym.anchor.clone(), sym.mentions));
                        }
                        if !sym.supersedes.is_empty() {
                            supersede_records.push((sym.anchor.clone(), sym.supersedes));
                        }
                        if !sym.demonstrates.is_empty() {
                            demo_records.push((sym.anchor.clone(), sym.demonstrates));
                        }
                        if !sym.defines_terms.is_empty() {
                            term_records.push((sym.anchor.clone(), sym.defines_terms));
                        }
                        if from_test_file {
                            test_anchors.insert(sym.anchor.clone());
                        }
                        if !sym.callees.is_empty() {
                            link_records.push((sym.anchor, sym.callees));
                        }
                    }
                }
            }
        }

        // Dry-run: never mutate the store (no prune, link, flush, or cache
        // save). Write conflict proposals and report, then stop.
        if propose {
            let written = write_merge_proposals(&root, &merge_conflicts);
            progress!(
                silent,
                "gen --propose: {} annotated symbol(s) would change → {} review notice(s) in .aden/proposals/. No store changes written.",
                merge_conflicts.len(),
                written
            );
            return Ok(());
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
        let cochange = cochange_pairs(&root, &cache);
        let callee_stats = match link_store_edges(
            &storage,
            EdgeRecords {
                calls: &link_records,
                uses: &use_records,
                refs: &ref_records,
                implements: &impl_records,
                mutates: &mutates_records,
                mentions: &mention_records,
                supersedes: &supersede_records,
                demonstrates: &demo_records,
                terms: &term_records,
                cochange: &cochange,
                test_anchors: &test_anchors,
            },
        ) {
            Ok(stats) => stats,
            Err(e) => {
                eprintln!("WARN: Failed to link graph edges: {}", e);
                CalleeStats::default()
            }
        };

        // Additive second pass: include:: directives -> Requires edges. Kept
        // separate from link_store_edges (which works from anchors only); this
        // resolves file-wise and put_edges_bulk appends, so prior edges persist.
        if let Err(e) = link_include_edges(&storage, &include_records) {
            eprintln!("WARN: Failed to link include edges: {}", e);
        }

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

        // Notices: a guarded symbol's generated content changed while a durable
        // overlay annotates it. The store was updated (no drift) and the overlay
        // is preserved + delivered; the notice asks the author to re-review.
        if !merge_conflicts.is_empty() {
            let written = write_merge_proposals(&root, &merge_conflicts);
            progress!(
                silent,
                "{} annotated symbol(s) changed → {} review notice(s) in .aden/proposals/ (your overlay intent is preserved; re-check it in .aden/overlays/).",
                merge_conflicts.len(),
                written
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
                "Call graph: {} internal calls linked, {} external (stdlib/other crate — no \
                 project symbol), {} polymorphic (name defined in several places, e.g. \
                 new/from/trait methods — left unlinked to avoid false edges).",
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
    let cache_dir = aden_paths::cache_dir(path);
    if cache_dir.exists() {
        let _ = std::fs::remove_dir_all(&cache_dir);
        let _ = std::fs::create_dir_all(&cache_dir);
    }

    Ok(())
}
