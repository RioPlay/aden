// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Thin wrapper around the indexer — public API for commands and heal/query.

#![allow(unused_imports)]

pub use crate::indexer::fresh::{
    DECISION_WAIT, FreshPolicy, Freshness, STALE_HINT, StaleHintGuard, augment_read_json,
    ensure_fresh, ensure_fresh_decision, ensure_fresh_with_policy, index_is_stale,
    maybe_print_stale_hint, read_index_stale,
};
pub use crate::indexer::r#gen::{cmd_gen, cmd_gen_opts, cmd_gen_silent};

pub(crate) use crate::indexer::fresh::{recover_if_incompatible_store, skip_auto_gen_on_read};
pub(crate) use crate::indexer::link::doc_anchor_file;
pub(crate) use crate::indexer::merge::slim_doc_for_store;

/// A bounded refresh can leave the first graph build running. Without an older
/// snapshot, graph commands must report that state before opening the live store.
/// Source-only commands do not need this gate and remain usable during indexing.
pub(crate) fn require_graph_ready(
    path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let root = crate::util::find_project_root(path);
    let snapshot = aden_paths::graph_snapshot_file(&root);
    if snapshot.is_file() {
        return Ok(());
    }
    if let Some(job) = crate::indexer::generation_job::status(&root)?
        && job.active()
        && !snapshot.is_file()
    {
        return Err(super::AgentCliError::new(
            "index_generation_pending",
            format!(
                "Index generation job {} is still active; the first graph snapshot is not available yet.",
                job.job_id
            ),
            "Check `aden jobs status` (MCP: generation_status), then retry this read after indexing completes. Source reads remain available.",
        ).into());
    }
    Ok(())
}
