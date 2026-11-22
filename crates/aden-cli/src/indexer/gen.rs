// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use aden_store::{GraphStorage, Storage};
use rayon::prelude::*;
use std::path::Path;

#[path = "bounded.rs"]
mod bounded;

#[cfg(test)]
#[path = "bounded_diagnostic.rs"]
mod bounded_diagnostic;

use crate::indexer::link::{EdgeRecords, link_include_edges, link_store_edges};
use crate::indexer::merge::{slim_doc_for_store, write_merge_proposals};
use crate::types::{FileDispositionEntry, GenCacheEntry};
use crate::util::{
    cochange_pairs, discover_file_dispositions, extract_callees, extract_demonstrates,
    extract_doc_includes, extract_doc_mentions, extract_doc_refs, extract_doc_supersedes,
    extract_doc_terms, extract_edge_macro, extract_uses, find_project_root, load_gen_cache,
    normalize_sep, sanitize_source_file, save_gen_cache,
};

const COALESCED_DOC_BATCH_LIMIT: usize = 4096;

#[derive(Default)]
struct CoalescedDocBatch {
    docs: Vec<aden_core::Document>,
    index: std::collections::HashMap<String, usize>,
}

impl CoalescedDocBatch {
    fn push(&mut self, item: aden_core::Document) {
        if let Some(&index) = self.index.get(&item.anchor) {
            self.docs[index] = item;
        } else {
            self.index.insert(item.anchor.clone(), self.docs.len());
            self.docs.push(item);
        }
    }

    fn flush(&mut self, storage: &impl GraphStorage) -> Result<(), aden_store::StoreError> {
        if !self.docs.is_empty() {
            storage.put_documents_bulk_canonical(&self.docs)?;
            self.docs.clear();
            self.index.clear();
        }
        Ok(())
    }
}

/// One stored symbol plus the compact data the linker needs. Carrying callee
/// names out of the parse phase means linking never has to reload the (huge)
/// document store to rebuild the call graph.
struct EmittedSymbol {
    anchor: String,
    is_rust_test: bool,
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
    /// `edge::member_of[Type]` references (impl methods) — linked REVERSED as
    /// `Type —Contains→ method`, so a type reaches its impl-block methods.
    member_of: Vec<String>,
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
/// symbols. `Skip` is reserved for files whose parsed bytes are unchanged.
enum WorkItem {
    Skip,
    Excluded {
        cache_key: String,
        source_mtime: u64,
        source_path: String,
        disposition: aden_core::filter::FileDisposition,
    },
    Reindexed {
        cache_key: String,
        source_mtime: u64,
        source_path: String,
        source_digest: String,
        symbols: Vec<EmittedSymbol>,
        /// Slimmed documents to write to the store (their canonical merge base
        /// is derived lazily from the stored document on read/CoW). Carried out
        /// of the parallel pass so the actual write happens *sequentially in
        /// sorted source order* in Phase 2 — otherwise two files sharing a
        /// basename anchor (e.g. `openapi2/helpers.go` + `openapi3/helpers.go` →
        /// `helpers.go#copyURI`) race, and the collision winner (hence the
        /// index) is non-deterministic. Empty on a `--propose` dry-run.
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
/// Internal generation callers share a durable worker and wait at most 30s.
/// A pending result leaves the worker running. Explicit CLI generation uses
/// `cmd_gen_opts` below and has no overall time cap.
pub fn cmd_gen(path: &Path, quiet: bool) -> Result<(), Box<dyn std::error::Error>> {
    super::generation_job::wait_for_completion(path, std::time::Duration::from_secs(30))?;
    if !quiet {
        println!("Generation completed.");
    }
    Ok(())
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
/// it — parallel-agent driving makes that concurrency real. Explicit `gen`
/// waits up to 10 minutes for a live holder; a dead holder is reclaimed
/// immediately (process liveness on Linux). Silent auto-refresh never waits:
/// if the lock is held it fail-opens so reads serve the last snapshot instead
/// of blocking the agent (zero-friction / single-flight join).
const WRITE_QUEUE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// Silent read-path refresh: try-lock only (no multi-minute queue).
const SILENT_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(0);

fn acquire_store_lock(
    store_path: &Path,
    silent: bool,
) -> std::io::Result<aden_core::lock::FileLock> {
    let lock_path = aden_core::lock::store_lock_path(store_path);
    if silent && super::generation_job::is_worker() {
        loop {
            match aden_core::lock::FileLock::acquire_timeout(&lock_path, SILENT_LOCK_TIMEOUT) {
                Ok(lease) => return Ok(lease),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    super::generation_job::checkpoint("waiting_for_writer", None, None)
                        .map_err(|e| std::io::Error::other(e.to_string()))?;
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(e) => return Err(e),
            }
        }
    }
    if silent {
        // Fail-open immediately when another writer owns the lock.
        aden_core::lock::FileLock::acquire_timeout(lock_path, SILENT_LOCK_TIMEOUT)
    } else {
        aden_core::lock::FileLock::acquire_timeout_verbose(lock_path, WRITE_QUEUE_TIMEOUT)
    }
}

/// A project can legitimately be huge. Above this count, say so once, because
/// the usual cause is a vendored or mirrored tree inside the checkout; naming
/// the count and both exclusion routes lets an operator decide before a long
/// build instead of discovering the scope afterwards.
const LARGE_SCOPE_WARN_FILES: usize = 20_000;

fn warn_large_scope(discovered: usize, silent: bool) {
    if silent || discovered < LARGE_SCOPE_WARN_FILES {
        return;
    }
    eprintln!(
        "aden: {discovered} source files selected. If a vendored or mirrored tree is included, exclude it with a pattern in {} or in {} (comma separated).",
        aden_paths::data_root()
            .join(aden_core::filter::MACHINE_IGNORE_FILE)
            .display(),
        aden_core::filter::EXCLUDES_ENV,
    );
}

fn cmd_gen_inner(
    path: &Path,
    quiet: bool,
    silent: bool,
    propose: bool,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut trace = super::gen_trace::GenTrace::new(quiet, silent);
    if !path.exists() {
        return Err("Path does not exist or is not a file/directory".into());
    }

    // Project root: for a single file, search upward from its directory.
    let search_start = if path.is_file() {
        // A bare relative filename has an empty parent, which cannot be
        // canonicalized and would select a different store from `gen .`.
        path.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
    } else {
        path
    };
    let root = find_project_root(search_start);
    super::generation_job::checkpoint("initial_fingerprint", None, None)?;
    trace.stage("initial_fingerprint");
    // Generation must account for a source that becomes unreadable instead of
    // aborting before the per-file pass can record `io_failed`.  Its snapshot
    // deliberately has no freshness manifest: an unreadable source can never
    // authoritatively bind the graph to the working tree, so reads fail closed
    // until a later successful generation fingerprints every source.
    let mut indexed_source_fingerprint = match super::fresh::source_fingerprint(&root) {
        Ok(fingerprint) => Some(fingerprint),
        Err(e) => {
            if !silent {
                eprintln!(
                    "WARN: Cannot establish authoritative freshness before generation: {e}; \
                     unreadable sources will be recorded and reads remain stale until recovery."
                );
            }
            super::fresh::clear_freshness_manifest(&root);
            None
        }
    };

    // Deterministic integration-test seam for the otherwise timing-sensitive
    // "tree changes while a read-triggered generation is running" case.  The
    // manifest must retain the pre-generation fingerprint, causing the caller
    // to report lag rather than a false `current`.  Release builds omit this.
    #[cfg(debug_assertions)]
    if let Some(relative) = std::env::var_os("ADEN_TEST_MUTATE_DURING_GEN") {
        let target = root.join(relative);
        let mut bytes = std::fs::read(&target)?;
        bytes.extend_from_slice(b"\n// aden freshness test mutation\n");
        std::fs::write(target, bytes)?;
    }

    // Store-first: `gen` writes ONLY to .aden/store. Module hub nodes
    // (mod-project, mod-<crate>) are synthesized into the store by
    // link_store_edges — no .adoc files or contracts/ directory are emitted.
    let pending_snapshot: Option<std::path::PathBuf>;
    let writer_lease;
    let journal;
    {
        super::generation_job::checkpoint("discovery", None, None)?;
        trace.stage("discovery");
        // A single file re-indexes just itself; a directory indexes the project.
        let mut full_tree_scope = !path.is_file();
        let mut discovered = if path.is_file() {
            vec![crate::util::DiscoveredFile {
                path: path.to_path_buf(),
                disposition: aden_core::filter::FileDisposition::Indexed,
            }]
        } else {
            discover_file_dispositions(&root)?
        };
        let mut sources: Vec<_> = discovered
            .iter()
            .filter(|file| file.disposition.is_indexed())
            .map(|file| file.path.clone())
            .collect();
        // An empty checkout may have just lost its final source. Reconcile
        // its cached owners or interrupted writes before certifying the graph.
        if discovered.is_empty() {
            if propose {
                eprintln!("No files discovered in {}.", root.display());
                return Ok(());
            }
            if std::env::var_os("ADEN_STORE").is_some() {
                eprintln!(
                    "No files discovered in {}. Shared store preserved.",
                    root.display()
                );
                return Ok(());
            }
            if !aden_paths::gen_cache_file(&root).try_exists()?
                && !aden_paths::store_dir(&root)
                    .with_extension("generation")
                    .join("recovery.json")
                    .try_exists()?
            {
                eprintln!("No files discovered in {}.", root.display());
                return Ok(());
            }
        }
        trace.count("discovered_files", discovered.len());
        trace.count("source_files", sources.len());
        warn_large_scope(discovered.len(), silent);
        super::generation_job::checkpoint("discovery", Some(0), Some(discovered.len()))?;
        trace.count("reindexed_files", 0);
        trace.count("excluded_source_files", 0);

        // ADR-003: the store now lives in the per-user data dir, keyed per
        // project. Refuse to create one at $HOME / fs-root unless explicit, then
        // migrate any legacy in-tree store before opening the central one.
        super::generation_job::checkpoint("store_setup", None, None)?;
        trace.stage("store_setup");
        aden_paths::guard_creatable_root(&root, crate::util::creation_explicit())?;
        crate::util::migrate_legacy_store(&root);
        let store_path = aden_paths::store_dir(&root);
        if let Some(parent) = store_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create store dir {}: {}", parent.display(), e))?;
        }
        // G4: serialize concurrent writers. Held until this function returns,
        // covering the whole open/index/flush write phase.
        // Silent path: WouldBlock → Ok (fail-open; another gen is in flight).
        super::generation_job::checkpoint("store_lock", None, None)?;
        trace.stage("store_lock");
        writer_lease = match acquire_store_lock(&store_path, silent) {
            Ok(l) => l,
            Err(e) if silent && e.kind() == std::io::ErrorKind::WouldBlock => {
                return Ok(());
            }
            Err(e) => {
                return Err(if e.kind() == std::io::ErrorKind::WouldBlock {
                    e.to_string().into()
                } else {
                    format!(
                        "failed to acquire store lock at {}: {e}",
                        aden_core::lock::store_lock_path(&store_path).display()
                    )
                    .into()
                });
            }
        };

        if aden_paths::store_dir(&root) != store_path {
            return Err(
                "Storage configuration changed while waiting for the writer; retry the command"
                    .into(),
            );
        }
        journal = super::generation_job::Paths::for_store(root.clone(), &store_path);
        super::generation_job::checkpoint("coalesce_check", None, None)?;

        trace.stage("coalesce_check");
        // Single-flight join: after waiting (explicit) or acquiring (silent), if
        // another writer already refreshed the tree and we are not forcing a
        // full rebuild, skip the expensive reparse.
        if silent
            && !force
            && store_path.exists()
            // Unchanged source bytes do not establish engine compatibility.
            // An unknown/old marker must reach the real engine open below,
            // including its guarded per-project format-recovery path.
            && Storage::has_current_format_marker(&store_path)
            && !crate::indexer::fresh::index_is_stale_for_root(&root)
        {
            return Ok(());
        }
        // Explicit gen that waited for a concurrent writer: still run unless
        // the user is on a dry-run path and the tree is already current — keep
        // explicit gen as "do the work" so operators can force a reindex.
        // (silent join above is the agent-facing coalesce.)

        super::generation_job::checkpoint("cache_load", None, None)?;
        trace.stage("cache_load");
        let cache_path = aden_paths::gen_cache_file(&root);
        let (mut cache, cache_requires_rebuild) = load_gen_cache(&cache_path).map_err(|error| {
            format!(
                "Failed to read generation cache {}: {error}",
                cache_path.display()
            )
        })?;

        super::generation_job::checkpoint("store_open", None, None)?;

        trace.stage("store_open");
        let store_str = store_path
            .to_str()
            .expect("Store path should be valid UTF-8");
        // A store that does not exist yet — first gen, a manual `rm -rf store/`,
        // or a `regen` wipe — is empty: its incremental gen-cache, if any, is now
        // stale and must be ignored, or every file would be skipped as
        // "unchanged" against an empty store and the rebuild would silently
        // produce nothing (the recovery-from-deletion trap).
        let mut full_rebuild = force || !store_path.exists() || cache_requires_rebuild;
        if cache_requires_rebuild && !propose && store_path.exists() {
            if std::env::var_os("ADEN_STORE").is_some() {
                return Err("generation-cache schema changed while $ADEN_STORE is pinned/shared; run `aden regen` with a per-project store to rebuild safely".into());
            }
            progress!(
                silent,
                "Generation cache schema changed — rebuilding store from source."
            );
            std::fs::remove_dir_all(&store_path).map_err(|error| {
                format!(
                    "failed to remove stale store {} for generation-cache migration: {error}",
                    store_path.display()
                )
            })?;
            if path.is_file() {
                full_tree_scope = true;
                discovered = discover_file_dispositions(&root)?;
                sources = discovered
                    .iter()
                    .filter(|file| file.disposition.is_indexed())
                    .map(|file| file.path.clone())
                    .collect();
            }
        }
        let existing_store = store_path.exists();
        let storage = match Storage::new_with_retry(store_str, WRITE_QUEUE_TIMEOUT) {
            Ok(s) => s,
            Err(aden_store::StoreError::IncompatibleVersion(_)) if propose => {
                return Err("The existing store must be upgraded before proposals can be compared; run aden gen first".into());
            }
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
                    full_tree_scope = true;
                    discovered = discover_file_dispositions(&root)?;
                    sources = discovered
                        .iter()
                        .filter(|file| file.disposition.is_indexed())
                        .map(|file| file.path.clone())
                        .collect();
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
        // Recovery may expand a single-file request into a full-tree rebuild.
        trace.count("discovered_files", discovered.len());
        trace.count("source_files", sources.len());
        super::generation_job::checkpoint("discovery", Some(0), Some(discovered.len()))?;
        let _ = aden_paths::write_meta(&root);

        super::generation_job::checkpoint("cache_recovery", None, None)?;
        trace.stage("cache_recovery");
        if !propose {
            super::fresh::clear_freshness_manifest(&root);
        }
        let pending_recovery =
            super::generation_job::recover_cache(&journal, &mut cache, &storage)?;
        if pending_recovery && !full_tree_scope {
            // A partial request cannot settle a whole-generation journal. In
            // particular an interrupted owner may have reverted to cached
            // bytes while its transient documents remain in the store.
            discovered = discover_file_dispositions(&root)?;
            sources = discovered
                .iter()
                .filter(|file| file.disposition.is_indexed())
                .map(|file| file.path.clone())
                .collect();
            full_tree_scope = true;
            trace.count("discovered_files", discovered.len());
            trace.count("source_files", sources.len());
            super::generation_job::checkpoint("discovery", Some(0), Some(discovered.len()))?;
        }
        if !full_tree_scope {
            // The initial fingerprint covers every source, but a normal file
            // request has only reconciled one. Publish its useful snapshot
            // without certifying unrelated source changes as current.
            indexed_source_fingerprint = None;
        }
        let live_keys: Option<std::collections::HashSet<String>> = full_tree_scope.then(|| {
            discovered
                .iter()
                .map(|file| crate::util::project_relative_key(&root, &file.path))
                .collect()
        });
        let mut generated = 0usize;
        let mut skipped = 0usize;
        let mut pre_prune_anchors = Vec::new();
        let mut pre_recovery = std::collections::BTreeMap::new();

        super::generation_job::checkpoint("persist_prune", None, None)?;

        trace.stage("persist_prune");
        // Persist every path-level disposition before parsing. This keeps
        // ignored, unsupported, and secret-path files visible to coverage and
        // prunes symbols that were indexed before a rule changed.
        if full_tree_scope {
            for file in &discovered {
                if file.disposition.is_indexed() {
                    continue;
                }
                let key = crate::util::project_relative_key(&root, &file.path);
                if let Some(previous) = cache.entries.remove(&key) {
                    pre_prune_anchors.extend(previous.anchors.iter().cloned());
                    pre_recovery.insert(key.clone(), previous.anchors);
                }
                let mtime = file
                    .path
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_secs())
                    .unwrap_or(0);
                cache.dispositions.insert(
                    key,
                    FileDispositionEntry {
                        disposition: file.disposition,
                        source_mtime: mtime,
                        source_path: file.path.to_string_lossy().to_string(),
                    },
                );
            }
        }

        // Anchors that have an intent overlay on disk. Computed once: only these
        // symbols can produce a merge conflict, so the gate skips the per-symbol
        // store read + overlay parse for every other symbol. Empty when there is
        // no `.aden/overlays/` directory (the common case → zero overhead).
        let overlay_slugs = crate::commands::overlay::overlay_slugs(&root);
        // Only overlaid anchors need their original generated layer retained.
        // Later batches may collide with an earlier anchor, so merge notices
        // must use its pre-generation baseline rather than a batch's new write.
        // Arc avoids cloning this exception to the bounded document lifetime.
        let overlay_baselines = std::sync::Mutex::new(std::collections::HashMap::<
            String,
            Option<std::sync::Arc<aden_core::Document>>,
        >::new());

        // Bind incremental reuse to inert extraction configuration as well as
        // source bytes. Read before parallel parsing and propagate I/O failures.
        let macro_config_fingerprints: std::collections::HashMap<_, _> = sources
            .iter()
            .filter(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("c" | "h")))
            .map(|p| {
                aden_parse::macro_declarations::config_fingerprint(p).map(|hash| (p.clone(), hash))
            })
            .collect::<aden_core::Result<_>>()?;

        sources.sort();
        // Retain compact link records across batches, but drain full documents
        // before parsing the next bounded source batch. Collect compact
        // (anchor, callees) link records so the linker never reloads documents.
        let mut link_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut use_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut ref_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut include_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut impl_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut mutates_records: Vec<(String, Vec<String>)> = Vec::new();
        let mut member_of_records: Vec<(String, Vec<String>)> = Vec::new();
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
        let mut stale_anchors: Vec<String> = pre_prune_anchors;
        // Merge conflicts surfaced by the reconcile gate, written as proposals.
        let mut merge_conflicts: Vec<(String, aden_core::contract::MergeProposal)> = Vec::new();
        // Deterministic store writes: order work by source path so that when two
        // files share a basename anchor, the collision winner is the same every run
        // (last sorted source wins) — making the store, and the index built from it,
        // reproducible. The parallel pass deferred every `put_document` to here.
        fn work_key(w: &WorkItem) -> &str {
            match w {
                WorkItem::Reindexed { source_path, .. } => source_path.as_str(),
                WorkItem::Excluded { source_path, .. } => source_path.as_str(),
                WorkItem::Skip => "",
            }
        }
        if !propose {
            // Resolution must never trust a lexicon from a partially completed
            // multi-batch generation. Readers use the last snapshot while this
            // writer holds the store; a crash leaves the marker absent and
            // selective resolution falls back to the canonical doc keys.
            storage
                .invalidate_symbol_lexicon()
                .map_err(|e| format!("Failed to invalidate symbol lexicon: {e}"))?;
        }
        let mut recovery = pre_recovery;
        let mut recovery_journal_dirty = !recovery.is_empty();
        trace.count("recovery_journal_writes", 0);
        // Carry deleted and interrupted owners forward before the next batch
        // can replace the journal. Both are independent of the batch boundary.
        if !propose {
            for (key, entry) in &cache.entries {
                let deleted = live_keys.as_ref().is_some_and(|live| !live.contains(key));
                if deleted || entry.source_digest.is_empty() {
                    recovery.insert(key.clone(), entry.anchors.clone());
                    recovery_journal_dirty |= deleted;
                }
            }
        }
        let mut batch_start = 0;
        let mut batch_count = 0;
        let mut peak_batch_files = 0;
        let mut peak_batch_bytes = 0;
        let mut peak_batch_snapshot_bytes = 0;
        let mut peak_batch_documents = 0;
        let batch_limits = bounded::limits();
        let mut coalesced = CoalescedDocBatch::default();
        #[cfg(debug_assertions)]
        let per_file = std::env::var_os("ADEN_TEST_FAIL_AFTER_FILE_WRITE").is_some()
            || std::env::var_os("ADEN_TEST_HOLD_AFTER_FILE_WRITE").is_some();
        #[cfg(not(debug_assertions))]
        let per_file = false;
        while batch_start < sources.len() {
            let (batch_len, batch_bytes) = bounded::batch_len_with_limits(
                sources[batch_start..].iter().map(|source| {
                    // Metadata is a scheduling estimate. Sources can change
                    // concurrently, and parsed output can exceed input size.
                    source
                        .metadata()
                        .map(|m| m.len())
                        .unwrap_or(bounded::SOURCE_BYTES)
                }),
                batch_limits.0,
                batch_limits.1,
            );
            let batch = &sources[batch_start..batch_start + batch_len];
            batch_count += 1;
            peak_batch_files = peak_batch_files.max(batch_len);
            peak_batch_bytes = peak_batch_bytes.max(batch_bytes);
            // Phase 1: Parallel file processing — read, parse, write to store
            super::generation_job::checkpoint("parse_prepare", None, None)?;
            trace.stage("parse_prepare");
            let mut work_items: Vec<_> = batch
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

                    let src_rel = crate::util::project_relative(&root, src_path);
                    // Security floor: never index credential material into the store
                    // (where ask/asm would assemble it into LLM context). Search
                    // (grep/locate/audit) can still see these files to fix them.
                    let path_disposition = aden_core::filter::FileDisposition::for_path(
                        &src_rel,
                        false,
                        true,
                    );
                    if !path_disposition.is_indexed() {
                        return Some(WorkItem::Excluded {
                            // `/`-normalized so Windows cache keys match store source_file.
                            cache_key: normalize_sep(&src_rel),
                            source_mtime: mtime_secs,
                            source_path: src_path.to_string_lossy().to_string(),
                            disposition: path_disposition,
                        });
                    }
                    let cache_key = normalize_sep(&src_rel);
                    // Read before deciding reuse. Timestamps can be preserved by
                    // editors/copy tools and have coarse resolution on some filesystems.
                    // The digest must describe this same buffer passed to the parser.
                    let source = match std::fs::read_to_string(src_path) {
                        Ok(s) => s,
                        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                            return Some(WorkItem::Excluded {
                                cache_key,
                                source_mtime: mtime_secs,
                                source_path: src_path.to_string_lossy().to_string(),
                                disposition: aden_core::filter::FileDisposition::InvalidEncoding,
                            });
                        }
                        Err(e) => {
                            if !quiet {
                                eprintln!("WARN: Failed to read {}: {}", src_path.display(), e);
                            }
                            return Some(WorkItem::Excluded {
                                cache_key,
                                source_mtime: mtime_secs,
                                source_path: src_path.to_string_lossy().to_string(),
                                disposition: aden_core::filter::FileDisposition::IoFailed,
                            });
                        }
                    };

                    let source_digest = if let Some(config) = macro_config_fingerprints.get(src_path) {
                        aden_core::hash_source(&format!("{}:{config}", aden_core::hash_source(&source)))
                    } else {
                        aden_core::stable_hash(source.as_bytes())
                    };
                    // Force/propose and a full rebuild must re-examine every file:
                    // force overwrites, propose audits overlays, and a new/recovered
                    // store has no contracts corresponding to old cache entries.
                    // Empty legacy digests never match a freshly computed digest.
                    if !full_rebuild
                        && !propose
                        && let Some(entry) = cache.entries.get(&cache_key)
                        && entry.source_digest == source_digest
                    {
                        return Some(WorkItem::Skip);
                    }

                    // Security floor (content): the filename-based is_secret_path
                    // check above misses a credential embedded in an ordinary source
                    // or config file. Scan content for high-confidence secret tokens
                    // (AWS/GitHub/OpenAI/Slack keys, PEM private keys) and refuse to
                    // index such a file into the store, where ask/asm would otherwise
                    // assemble it into LLM context (CWE-798/CWE-200).
                    if let Some(secret_match) =
                        aden_core::filter::detect_high_confidence_secret(&source)
                    {
                        if !silent {
                            eprintln!(
                                "WARN: Skipping {}:{} — file content matches credential rule '{}' (not indexed). Add to .adenignore if intentional.",
                                src_rel.display(),
                                secret_match.line,
                                secret_match.rule
                            );
                        }
                        return Some(WorkItem::Excluded {
                            cache_key,
                            source_mtime: mtime_secs,
                            source_path: src_path.to_string_lossy().to_string(),
                            disposition: aden_core::filter::FileDisposition::SecretContent,
                        });
                    }

                    // Parse
                    // Deterministic integration-test seam for the otherwise rare
                    // parser-error branch. It is compiled out of release builds
                    // and requires an exact root-relative path, so production
                    // classification remains entirely parser-driven.
                    #[cfg(debug_assertions)]
                    if std::env::var_os("ADEN_TEST_FORCE_PARSE_FAILED")
                        .is_some_and(|value| value == src_rel.as_os_str())
                    {
                        return Some(WorkItem::Excluded {
                            cache_key,
                            source_mtime: mtime_secs,
                            source_path: src_path.to_string_lossy().to_string(),
                            disposition: aden_core::filter::FileDisposition::ParseFailed,
                        });
                    }
                    let docs = match aden_parse::parse_file(src_path, &source) {
                        Ok(d) => d,
                        Err(aden_core::Error::UnsupportedLanguage(_)) => return Some(WorkItem::Excluded {
                            cache_key,
                            source_mtime: mtime_secs,
                            source_path: src_path.to_string_lossy().to_string(),
                            disposition: aden_core::filter::FileDisposition::Unsupported,
                        }),
                        Err(e) => {
                            if !silent {
                                eprintln!("WARN: Parse failed for {}: {}", src_path.display(), e);
                            }
                            return Some(WorkItem::Excluded {
                                cache_key,
                                source_mtime: mtime_secs,
                                source_path: src_path.to_string_lossy().to_string(),
                                disposition: aden_core::filter::FileDisposition::ParseFailed,
                            });
                        }
                    };

                    // Parse documents; the actual store write is deferred to Phase 2
                    // (sequential, sorted) so basename-anchor collisions resolve
                    // deterministically instead of racing across worker threads.
                    let mut emitted = Vec::new();
                    let mut conflicts: Vec<(String, aden_core::contract::MergeProposal)> = Vec::new();
                    let mut docs_local: Vec<aden_core::Document> = Vec::new();
                    for mut doc in docs {
                        sanitize_source_file(&mut doc, &root);

                        // Capture call sites for graph linking before slimming, then
                        // drop the redundant edge:: listing so the store stays compact.
                        // Real containment/Calls edges are built in link_store_edges,
                        // so the old parent-module relationship boilerplate is gone.
                        let callees = extract_callees(&doc);
                        let uses = extract_uses(&doc);
                        let refs = extract_doc_refs(&doc);
                        let includes = extract_doc_includes(&doc);
                        let implements = extract_edge_macro(&doc, "implements");
                        let mutates = extract_edge_macro(&doc, "mutates");
                        let member_of = extract_edge_macro(&doc, "member_of");
                        let mentions = extract_doc_mentions(&doc);
                        let supersedes = extract_doc_supersedes(&doc);
                        let demonstrates = extract_demonstrates(&doc);
                        let defines_terms = extract_doc_terms(&doc);
                        slim_doc_for_store(&mut doc);

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
                                &crate::commands::overlay::sanitize_anchor_filename(&doc.anchor),
                            )
                        {
                            let stored = overlay_baselines
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                .entry(doc.anchor.clone())
                                .or_insert_with(|| {
                                    storage
                                        .get_document(&doc.anchor)
                                        .ok()
                                        .flatten()
                                        .map(std::sync::Arc::new)
                                })
                                .clone();
                            let overlay = crate::commands::overlay::load_overlay(
                                &root,
                                &doc.anchor,
                            )
                            .ok()
                            .flatten();
                            if let Ok(p) = aden_core::contract::reconcile_anchor(
                                &doc,
                                stored.as_deref(),
                                overlay.as_ref(),
                            ) && !p.is_clean()
                            {
                                conflicts.push((doc.anchor.clone(), p));
                            }
                        }

                        emitted.push(EmittedSymbol {
                            anchor: doc.anchor.clone(),
                            is_rust_test: doc
                                .attributes
                                .get("rust_test")
                                .is_some_and(|value| value == "true"),
                            callees,
                            uses,
                            refs,
                            includes,
                            implements,
                            mutates,
                            member_of,
                            mentions,
                            supersedes,
                            demonstrates,
                            defines_terms,
                            wrote: write,
                        });
                        // Defer the write — Phase 2 stores these in sorted source
                        // order. Canonical merge bases are tracked via the compact
                        // `has_canonical_base` flag rather than storing redundant
                        // contract copies on disk.
                        if write {
                            docs_local.push(doc);
                        }
                    }

                    // Always report a reindexed file — even with zero symbols — so
                    // the prune step can drop anchors a now-empty file used to own.
                    Some(WorkItem::Reindexed {
                        cache_key: cache_key.clone(),
                        source_mtime: mtime_secs,
                        source_path: src_path.to_string_lossy().to_string(),
                        source_digest,
                        symbols: emitted,
                        docs: docs_local,
                        conflicts,
                    })
                })
                .collect();

            let mut batch_snapshot_bytes = 0;
            let mut batch_documents = 0;
            let trace_enabled = trace.is_enabled();
            for item in &work_items {
                if let WorkItem::Reindexed { docs, .. } = item {
                    batch_documents += docs.len();
                    if trace_enabled {
                        batch_snapshot_bytes += docs
                            .iter()
                            .map(|doc| {
                                aden_store::serialize_document(doc).map_or(0, |bytes| bytes.len())
                            })
                            .sum::<usize>();
                    }
                }
            }
            peak_batch_documents = peak_batch_documents.max(batch_documents);
            peak_batch_snapshot_bytes = peak_batch_snapshot_bytes.max(batch_snapshot_bytes);
            super::generation_job::checkpoint("persist_prune", None, None)?;
            trace.stage("persist_prune");
            work_items.sort_by(|a, b| work_key(a).cmp(work_key(b)));
            if !propose {
                for item in &work_items {
                    match item {
                        WorkItem::Reindexed {
                            cache_key, symbols, ..
                        } => {
                            recovery_journal_dirty = true;
                            let anchors =
                                recovery.entry(cache_key.clone()).or_insert_with(Vec::new);
                            if let Some(old) = cache.entries.get(cache_key) {
                                anchors.extend(old.anchors.clone());
                            }
                            anchors.extend(symbols.iter().map(|s| s.anchor.clone()));
                        }
                        WorkItem::Excluded { cache_key, .. } => {
                            if let Some(old) = cache.entries.get(cache_key) {
                                recovery.insert(cache_key.clone(), old.anchors.clone());
                                recovery_journal_dirty = true;
                            }
                        }
                        WorkItem::Skip => {}
                    }
                }
                if recovery_journal_dirty {
                    super::generation_job::prepare_recovery(&journal, &recovery)?;
                    trace.increment("recovery_journal_writes");
                    recovery_journal_dirty = false;
                }
            }
            for (completed, item) in work_items.into_iter().enumerate() {
                super::generation_job::checkpoint(
                    "persist_prune",
                    Some(batch_start + completed),
                    Some(sources.len()),
                )?;
                match item {
                    WorkItem::Skip => skipped += 1,
                    WorkItem::Excluded {
                        cache_key,
                        source_mtime,
                        source_path,
                        disposition,
                    } => {
                        trace.increment("excluded_source_files");
                        // A file that was once indexed may now be redacted or fail
                        // parsing. Remove its old symbols and persist the reason so
                        // it is never a silent omission.
                        if let Some(previous) = cache.entries.remove(&cache_key) {
                            stale_anchors.extend(previous.anchors);
                        }
                        cache.dispositions.insert(
                            cache_key,
                            FileDispositionEntry {
                                disposition,
                                source_mtime,
                                source_path,
                            },
                        );
                    }
                    WorkItem::Reindexed {
                        cache_key,
                        source_mtime,
                        source_path,
                        source_digest,
                        symbols,
                        docs,
                        conflicts,
                    } => {
                        trace.increment("reindexed_files");
                        if per_file {
                            storage.put_documents_bulk_canonical(&docs).map_err(|e| {
                                format!("Failed to store documents for {source_path}: {e}")
                            })?;
                        } else if docs.len() >= COALESCED_DOC_BATCH_LIMIT {
                            // Keep large single-file stores atomic instead of splitting one file
                            // across multiple partial WAL commits.
                            coalesced.flush(&storage)?;
                            storage.put_documents_bulk_canonical(&docs).map_err(|e| {
                                format!("Failed to store documents for {source_path}: {e}")
                            })?;
                        } else {
                            for doc in docs {
                                coalesced.push(doc);
                            }
                            if coalesced.docs.len() >= COALESCED_DOC_BATCH_LIMIT {
                                coalesced.flush(&storage)?;
                            }
                        }
                        #[cfg(debug_assertions)]
                        if std::env::var("ADEN_TEST_FAIL_AFTER_FILE_WRITE")
                            .ok()
                            .as_deref()
                            == Some(cache_key.as_str())
                        {
                            return Err("injected failure after atomic file write".into());
                        }
                        #[cfg(debug_assertions)]
                        if !propose
                            && std::env::var_os("ADEN_TEST_HOLD_AFTER_FILE_WRITE").as_deref()
                                == Some(std::ffi::OsStr::new("1"))
                        {
                            // Deterministic abrupt-death proof: the intent is already
                            // staged, and the atomic document batch is durable before
                            // the test harness terminates its owned fixture worker.
                            storage
                                .flush()
                                .map_err(|e| format!("Failed to flush test batch: {e}"))?;
                            while root.join(".aden-test-generation-hold").exists() {
                                super::generation_job::checkpoint(
                                    "test_hold_after_write",
                                    None,
                                    None,
                                )?;
                                std::thread::sleep(std::time::Duration::from_millis(50));
                            }
                        }
                        merge_conflicts.extend(conflicts);
                        // The module-form anchor flattens the directory, so
                        // test-ness comes from the real (root-relative) source
                        // path — the same rule ask-routing applies at query time.
                        let from_test_file =
                            crate::commands::query::is_test_source_path(&cache_key);
                        let fresh: Vec<String> = symbols.iter().map(|s| s.anchor.clone()).collect();
                        // Diff against what this file contributed last time: any
                        // previously-recorded anchor not in the fresh set is a
                        // symbol that was deleted/renamed and must be pruned.
                        if let Some(prev) = cache.entries.get(&cache_key)
                            && prev.anchors != fresh
                        {
                            let kept: std::collections::HashSet<_> = fresh.iter().collect();
                            stale_anchors.extend(
                                prev.anchors
                                    .iter()
                                    .filter(|old| !kept.contains(*old))
                                    .cloned(),
                            );
                        }
                        cache.dispositions.remove(&cache_key);
                        cache.entries.insert(
                            cache_key,
                            GenCacheEntry {
                                source_mtime,
                                source_path,
                                source_digest,
                                anchors: fresh,
                            },
                        );
                        for sym in symbols {
                            // Count only symbols actually written to the store, so the
                            // summary never claims to have stored a conflict-held doc.
                            if sym.wrote {
                                generated += 1;
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
                            if !sym.member_of.is_empty() {
                                member_of_records.push((sym.anchor.clone(), sym.member_of));
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
                            if from_test_file || sym.is_rust_test {
                                test_anchors.insert(sym.anchor.clone());
                            }
                            if !sym.callees.is_empty() {
                                link_records.push((sym.anchor, sym.callees));
                            }
                        }
                    }
                }
            }

            batch_start += batch_len;
        }
        coalesced.flush(&storage)?;
        trace.count("parse_batches", batch_count);
        trace.count("peak_parse_batch_files", peak_batch_files);
        trace.count("peak_parse_batch_source_bytes", peak_batch_bytes as usize);
        trace.count("peak_parse_batch_documents", peak_batch_documents);
        trace.count("peak_parse_batch_snapshot_bytes", peak_batch_snapshot_bytes);

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

        // With no parse batches (or no dirty batch), deletions may still be
        // pending. Durably journal their owners immediately before any edge
        // clearing or stale-node pruning mutates the store.
        if recovery_journal_dirty {
            super::generation_job::prepare_recovery(&journal, &recovery)?;
            trace.increment("recovery_journal_writes");
        }

        // Case (b): whole-file deletion. On a full-tree gen (NOT a single-file
        // re-index, which only knows about one file), any cache entry whose
        // source file is no longer in the discovered set is gone — prune all
        // anchors it owned and drop the entry.
        if let Some(live) = live_keys {
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
            cache.dispositions.retain(|key, _| live.contains(key));
        }

        // The journal identifies the reindexed files. A shared anchor can carry
        // calls from several files; clearing it is safe only if every owner
        // participated and the linker can replay all their contributions.
        // A pinned store may also contain owners outside this project's cache.
        if existing_store && !recovery.is_empty() && std::env::var_os("ADEN_STORE").is_none() {
            let mut refreshed: std::collections::HashSet<&str> = recovery
                .keys()
                .filter_map(|key| cache.entries.get(key))
                .flat_map(|entry| entry.anchors.iter().map(String::as_str))
                .collect();
            if !refreshed.is_empty() {
                for (key, entry) in &cache.entries {
                    if !recovery.contains_key(key) {
                        for anchor in &entry.anchors {
                            refreshed.remove(anchor.as_str());
                        }
                    }
                }
                // Keep each storage transaction bounded even on a forced reparse.
                let mut batch = Vec::with_capacity(256);
                for anchor in refreshed {
                    batch.push(anchor);
                    if batch.len() == 256 {
                        storage.clear_generated_calls(&batch)?;
                        batch.clear();
                    }
                }
                storage.clear_generated_calls(&batch)?;
            }
        }

        // Its durable journal remains on disk until publication succeeds.
        drop(recovery);

        // Prune stale nodes (deleted symbols / deleted files). delete_node
        // cascades edges in both directions so no dangling reference survives.
        // Guard: never touch synthesized hub nodes (mod-*) — they carry no
        // source_file and are rebuilt by link_store_edges below.
        let mut pruned = 0usize;
        // A removed or renamed source can share a canonical symbol with a
        // surviving owner. Prune only anchors absent from every live entry.
        let mut stale: std::collections::BTreeSet<_> = stale_anchors.into_iter().collect();
        if !stale.is_empty() {
            for entry in cache.entries.values() {
                for anchor in &entry.anchors {
                    stale.remove(anchor);
                }
            }
        }
        let mut prune_batch = Vec::with_capacity(256);
        for anchor in &stale {
            if anchor.starts_with("mod-") {
                continue;
            }
            prune_batch.push(anchor.as_str());
            if prune_batch.len() == 256 {
                storage.delete_nodes_bulk(&prune_batch)?;
                pruned += prune_batch.len();
                prune_batch.clear();
            }
        }
        if !prune_batch.is_empty() {
            storage.delete_nodes_bulk(&prune_batch)?;
            pruned += prune_batch.len();
        }

        // Connect the graph: persist module<->symbol containment and call edges
        // so the store-first graph used by asm/ask/query is actually traversable.
        trace.count("generated_contracts", generated);
        trace.count("skipped_files", skipped);
        trace.count("pruned_symbols", pruned);
        super::generation_job::checkpoint("git_history", None, None)?;
        trace.stage("git_history");
        let cochange = cochange_pairs(&root, &cache);
        trace.count("cochange_pairs", cochange.len());
        super::generation_job::checkpoint("linking", None, None)?;
        trace.stage("linking");
        let callee_stats = match link_store_edges(
            &storage,
            EdgeRecords {
                calls: &link_records,
                uses: &use_records,
                refs: &ref_records,
                implements: &impl_records,
                mutates: &mutates_records,
                member_of: &member_of_records,
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
                return Err(format!("Failed to link graph edges: {e}").into());
            }
        };

        // Additive second pass: include:: directives -> Requires edges. Kept
        // separate from link_store_edges (which works from anchors only); this
        // resolves file-wise and put_edges_bulk appends, so prior edges persist.
        super::generation_job::checkpoint("include_linking", None, None)?;
        trace.stage("include_linking");
        if let Err(e) = link_include_edges(&storage, &include_records) {
            return Err(format!("Failed to link include edges: {e}").into());
        }

        // Every document write, stale-node prune, and synthesized hub write is
        // now complete. Synchronize discovered file metadata into SQLite `files`,
        // publish the lexicon marker last, and durably flush before preparing a
        // snapshot.
        super::generation_job::checkpoint("store_flush", None, None)?;
        trace.stage("store_flush");
        let mut file_records = Vec::with_capacity(cache.entries.len() + cache.dispositions.len());
        for (rel_path, entry) in &cache.entries {
            file_records.push(aden_store::FileRecord {
                rel_path: rel_path.clone(),
                mtime: Some(entry.source_mtime),
                digest: (!entry.source_digest.is_empty()).then(|| entry.source_digest.clone()),
                disposition: Some("indexed".to_string()),
            });
        }
        for (rel_path, entry) in &cache.dispositions {
            if cache.entries.contains_key(rel_path) {
                continue;
            }
            file_records.push(aden_store::FileRecord {
                rel_path: rel_path.clone(),
                mtime: Some(entry.source_mtime),
                digest: None,
                disposition: Some(format!("{:?}", entry.disposition)),
            });
        }
        storage
            .sync_file_records(&file_records)
            .map_err(|e| format!("Failed to sync file metadata: {e}"))?;
        storage
            .finalize_symbol_lexicon()
            .map_err(|e| format!("Failed to finalize symbol lexicon: {e}"))?;
        storage
            .flush()
            .map_err(|e| format!("Store flush failed: {}", e))?;

        super::generation_job::checkpoint("extraction_cleanup", None, None)?;

        trace.stage("extraction_cleanup");
        // Linking is complete. Release extraction-only strings before loading
        // docs + edges for the read snapshot; retaining both sets made cold
        // kernel-scale indexing carry the call/reference corpus twice.
        drop((
            link_records,
            use_records,
            ref_records,
            include_records,
            impl_records,
            mutates_records,
            member_of_records,
            mention_records,
            supersede_records,
            demo_records,
            term_records,
            test_anchors,
        ));
        drop(cochange);

        // ADR-011: serialize the read snapshot to its temporary file while
        // storage is open; publish after the store handle drops so a final fjall
        // flush cannot age the store past the snapshot file.
        super::generation_job::checkpoint("snapshot_prepare", None, None)?;
        trace.stage("snapshot_prepare");
        let snapshot_path = aden_paths::graph_snapshot_file(&root);
        trace.count("snapshot_prepared", 0);
        let orphans = if quiet {
            aden_graph::snapshot::prepare_from_storage(&snapshot_path, &storage)
                .map(|()| aden_graph::snapshot::OrphanSummary::default())
        } else {
            aden_graph::snapshot::prepare_from_storage_with_orphans(&snapshot_path, &storage)
        }
        .map_err(|e| format!("Failed to prepare read snapshot: {e}"))?;
        trace.count("snapshot_prepared", 1);
        pending_snapshot = Some(snapshot_path);

        super::generation_job::checkpoint("cache_write", None, None)?;

        trace.stage("cache_write");
        save_gen_cache(&cache_path, &cache)?;
        super::generation_job::checkpoint("summary_and_notices", None, None)?;
        trace.stage("summary_and_notices");

        // The summary is "summary only" output: shown under --quiet/regen, but
        // suppressed entirely on the silent refresh-on-read path.
        if pruned > 0 {
            progress!(
                silent,
                "\nStored {} contracts. Skipped {} unchanged files. Pruned {} stale symbol(s).",
                generated,
                skipped,
                pruned
            );
        } else {
            progress!(
                silent,
                "\nStored {} contracts. Skipped {} unchanged files.",
                generated,
                skipped
            );
        }
        if skipped == 0 && generated == sources.len() {
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

        super::generation_job::checkpoint("orphan_diagnostics", None, None)?;
        trace.stage("orphan_diagnostics");
        if orphans.total > 0 {
            eprintln!("\nWARNING: {} orphan symbol(s) detected:", orphans.total);
            for orphan in &orphans.examples {
                eprintln!("  - {}", orphan);
            }
            if orphans.total > orphans.examples.len() {
                eprintln!("  ... and {} more", orphans.total - orphans.examples.len());
            }
            eprintln!("  Run 'aden heal . --gc' to auto-link or remove orphans");
        }
        // Keep the writer lock through DB close, as before, and publish only
        // after the final engine flush performed by the storage destructor.
        super::generation_job::checkpoint("store_close", None, None)?;
        trace.stage("store_close");
        drop(storage);
        super::generation_job::checkpoint("scope_cleanup", None, None)?;
        trace.stage("scope_cleanup");
    }

    super::generation_job::checkpoint("snapshot_publish", None, None)?;

    trace.stage("snapshot_publish");
    trace.count("snapshot_published", 0);
    trace.count("freshness_manifest_published", 0);
    if let Some(snapshot_path) = pending_snapshot {
        match aden_graph::snapshot::publish_prepared(&snapshot_path) {
            Ok(()) => {
                trace.count("snapshot_published", 1);
                if let Some(indexed_source_fingerprint) = indexed_source_fingerprint {
                    let graph_revision = super::fresh::snapshot_revision(&snapshot_path)?;
                    super::fresh::publish_freshness_manifest(
                        &root,
                        graph_revision,
                        indexed_source_fingerprint,
                    )?;
                    trace.count("freshness_manifest_published", 1);
                }
            }
            Err(e) => return Err(format!("Failed to publish read snapshot: {e}").into()),
        }
    }

    // Invalidate caches after generating so the next query rebuilds
    super::generation_job::checkpoint("cache_invalidation", None, None)?;
    trace.stage("cache_invalidation");
    let cache_dir = aden_paths::cache_dir(&root);
    if cache_dir.exists() {
        let _ = std::fs::remove_dir_all(&cache_dir);
        let _ = std::fs::create_dir_all(&cache_dir);
    }

    journal.clear_recovery()?;
    drop(writer_lease);
    trace.finish();
    if !propose {
        crate::commands::store::housekeeping::maybe_run_automatic_housekeeping(
            aden_paths::try_project_dir(&root).ok().as_deref(),
            silent,
        );
    }
    Ok(())
}

#[cfg(test)]
mod store_lock_tests {
    use super::*;

    #[test]
    fn gen_store_lock_is_exclusive_and_releases() {
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("store");

        let held = acquire_store_lock(&store_path, false).expect("first gen takes the lock");
        // A second writer on the same store's sibling lockfile is blocked.
        let blocked = aden_core::lock::FileLock::acquire_timeout(
            store_path.with_extension("lock"),
            std::time::Duration::from_millis(100),
        );
        assert_eq!(
            blocked.expect_err("second writer must block").kind(),
            std::io::ErrorKind::WouldBlock
        );

        // Silent path fail-opens immediately (WouldBlock) instead of waiting.
        let silent = acquire_store_lock(&store_path, true);
        assert_eq!(
            silent.expect_err("silent gen must not queue").kind(),
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

#[cfg(test)]
mod coalesced_doc_tests {
    use super::*;
    fn doc(anchor: &str, tag: &str) -> aden_core::Document {
        let mut attributes = std::collections::HashMap::new();
        attributes.insert("source_hash".into(), tag.into());
        aden_core::Document {
            anchor: anchor.into(),
            node_type: aden_core::NodeType::Note,
            attributes,
            blocks: vec![],
            source_span: None,
            metadata: None,
            confidence: 1.0,
        }
    }
    fn expected_base(doc: &aden_core::Document) -> String {
        aden_emit::emit_contract_document(&aden_core::contract::ContractDocument::from_document(
            doc,
        ))
    }
    #[test]
    fn collision_and_flush_across_chunk_boundaries() {
        let path = std::env::temp_dir().join(format!("aden-coalesced-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        let storage = Storage::new(&path.to_string_lossy()).unwrap();
        let mut batch = CoalescedDocBatch::default();
        batch.push(doc("same", "first"));
        let last = doc("same", "last");
        batch.push(last.clone());
        assert_eq!(batch.docs.len(), 1);
        for i in 1..COALESCED_DOC_BATCH_LIMIT {
            batch.push(doc(&format!("n{i}"), "base"));
        }
        batch.flush(&storage).unwrap();
        assert!(batch.docs.is_empty() && batch.index.is_empty());
        assert_eq!(
            storage
                .get_document("same")
                .unwrap()
                .unwrap()
                .attributes
                .get("source_hash")
                .map(String::as_str),
            Some("last")
        );
        assert_eq!(
            storage.get_base_snapshot("same").unwrap().as_deref(),
            Some(expected_base(&last).as_str())
        );
        let later = doc("same", "later chunk");
        batch.push(later.clone());
        batch.flush(&storage).unwrap();
        assert_eq!(
            storage
                .get_document("same")
                .unwrap()
                .unwrap()
                .attributes
                .get("source_hash")
                .map(String::as_str),
            Some("later chunk")
        );
        assert_eq!(
            storage.get_base_snapshot("same").unwrap().as_deref(),
            Some(expected_base(&later).as_str())
        );
        drop(storage);
        std::fs::remove_dir_all(path).ok();
    }
}
