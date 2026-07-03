// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Persistent token-savings ledger: `<repo_root>/.aden/savings.json`.
//!
//! Tracks cumulative savings estimates across aden read queries (`ask`, `asm`,
//! `understand`, `grep`, `locate`) so
//! `aden status` can surface both an all-time total and a "this session"
//! window. The file is NOT under `.aden/store` (which is an LSM database); it
//! lives alongside store metadata as a plain JSON sidecar, written only by
//! `record` and read by `load_summary`.
//!
//! "Session" is defined by an idle gap: if more than [`IDLE_RESET_SECS`] elapse
//! between recorded queries, the session window resets. This needs no process
//! plumbing and works identically for one-shot CLI runs and the long-lived MCP
//! server (both write the same sidecar).
//!
//! Robustness contract: a missing or corrupt file starts from defaults. Write
//! failures are reported to stderr but never propagate — the ledger is
//! best-effort telemetry, never load-bearing.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use std::collections::HashSet;

use aden_core::savings::{BASELINE_MAX_FILES, SavingsEstimate};
use aden_store::GraphStorage;
use serde::{Deserialize, Serialize};

/// Path of the savings sidecar, relative to the repo root.
const SAVINGS_FILE: &str = ".aden/savings.json";

/// Schema version tag — bump if the shape changes incompatibly.
const SCHEMA_VERSION: u32 = 2;

/// Idle gap (seconds) after which the session window resets. 30 minutes.
const IDLE_RESET_SECS: u64 = 1_800;

/// Cumulative counters. Used for both the all-time total and the session window.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SavingsLedger {
    /// Number of Aden read queries recorded.
    pub queries: u64,
    /// Total tokens Aden returned across those queries.
    pub returned_tokens: u64,
    /// Total baseline tokens (grep-read) across those queries.
    pub baseline_tokens: u64,
    /// Cumulative saved tokens (`baseline - returned`); may be negative.
    pub saved_tokens: i64,
    /// Cumulative estimated tool calls saved (sum of per-query `baseline_files`).
    #[serde(default)]
    pub tool_calls_saved: u64,
}

impl SavingsLedger {
    fn add(&mut self, est: &SavingsEstimate) {
        self.queries += 1;
        self.returned_tokens += est.returned_tokens as u64;
        self.baseline_tokens += est.baseline_tokens as u64;
        self.saved_tokens += est.saved_tokens;
        self.tool_calls_saved += est.tool_calls_saved() as u64;
    }
}

/// Session window: counters plus the unix timestamps bounding the window.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct SessionWindow {
    #[serde(default)]
    started_unix: u64,
    #[serde(default)]
    last_unix: u64,
    #[serde(default, flatten)]
    ledger: SavingsLedger,
}

/// On-disk envelope. `#[serde(default)]` on `session` lets a v1 file (which had
/// only `all_time`) load cleanly and gain a fresh session window.
#[derive(Serialize, Deserialize)]
struct SavingsFile {
    schema: u32,
    all_time: SavingsLedger,
    #[serde(default)]
    session: SessionWindow,
}

impl Default for SavingsFile {
    fn default() -> Self {
        Self {
            schema: SCHEMA_VERSION,
            all_time: SavingsLedger::default(),
            session: SessionWindow::default(),
        }
    }
}

/// All-time total plus the current session window, for the status surface.
pub struct SavingsSummary {
    pub all_time: SavingsLedger,
    pub session: SavingsLedger,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn read_file(path: &Path) -> SavingsFile {
    match std::fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice::<SavingsFile>(&bytes).unwrap_or_else(|_| SavingsFile::default())
        }
        Err(_) => SavingsFile::default(),
    }
}

/// Load the all-time total and the current session window. Returns defaults when
/// the file is absent or corrupt — never errors. A session whose `last_unix` is
/// older than [`IDLE_RESET_SECS`] is reported as empty (it has lapsed).
pub fn load_summary(repo_root: &Path) -> SavingsSummary {
    let file = read_file(&repo_root.join(SAVINGS_FILE));
    let now = now_unix();
    let session = if file.session.last_unix != 0
        && now.saturating_sub(file.session.last_unix) > IDLE_RESET_SECS
    {
        SavingsLedger::default()
    } else {
        file.session.ledger
    };
    SavingsSummary {
        all_time: file.all_time,
        session,
    }
}

/// Increment the ledger with one query's estimate.
///
/// All-time counters always accrue. The session window accrues too, unless more
/// than [`IDLE_RESET_SECS`] elapsed since the last query — in which case it
/// resets first, so the window reflects only the current burst of work. Any I/O
/// or serialization failure is logged to stderr but not propagated.
pub fn record(repo_root: &Path, est: &SavingsEstimate) {
    let path = repo_root.join(SAVINGS_FILE);
    let mut file = read_file(&path);
    let now = now_unix();

    // Roll the session window if it has gone idle (or was never started).
    let lapsed =
        file.session.last_unix == 0 || now.saturating_sub(file.session.last_unix) > IDLE_RESET_SECS;
    if lapsed {
        file.session = SessionWindow {
            started_unix: now,
            last_unix: now,
            ledger: SavingsLedger::default(),
        };
    }

    file.all_time.add(est);
    file.session.ledger.add(est);
    file.session.last_unix = now;
    file.schema = SCHEMA_VERSION;

    // Ensure the .aden/ directory exists.
    if let Some(parent) = path.parent()
        && !parent.exists()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        eprintln!(
            "aden: savings: could not create {}: {}",
            parent.display(),
            e
        );
        return;
    }

    match serde_json::to_string_pretty(&file) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                eprintln!("aden: savings: write failed ({}): {}", path.display(), e);
            }
        }
        Err(e) => eprintln!("aden: savings: serialize failed: {}", e),
    }
}

fn anchor_source_file(path: &Path, anchor: &str) -> Option<String> {
    // ADR-011: prefer graph.snapshot via the standard helper.
    if let Some((docs, _)) = aden_graph::snapshot::try_read_fresh(path)
        && let Some(d) = docs.get(anchor)
    {
        return d.attributes.get("source_file").cloned();
    }
    let (store_path, _) = aden_paths::resolve_read_store(path);
    let storage = aden_store::Storage::open_existing(store_path.to_str()?).ok()?;
    storage
        .get_document(anchor)
        .ok()
        .flatten()
        .and_then(|d| d.attributes.get("source_file").cloned())
}

/// Estimate tokens saved vs. a grep-read baseline of distinct source files touched.
pub fn estimate_read_savings(
    path: &Path,
    primary_anchors: &[String],
    returned_bytes: usize,
) -> SavingsEstimate {
    let hydrate_root = crate::util::find_project_root(path);
    let mut seen_files: HashSet<String> = HashSet::new();
    let mut baseline_bytes: usize = 0;
    for anchor in primary_anchors {
        if seen_files.len() >= BASELINE_MAX_FILES {
            break;
        }
        if let Some(src) = anchor_source_file(path, anchor) {
            if src.is_empty() || seen_files.contains(&src) {
                continue;
            }
            let abs = hydrate_root.join(&src);
            if let Ok(meta) = std::fs::metadata(&abs) {
                seen_files.insert(src);
                baseline_bytes += meta.len() as usize;
            }
        }
    }
    SavingsEstimate::from_bytes(returned_bytes, baseline_bytes, seen_files.len())
}

/// Record one completed read query into the ledger (best-effort).
pub fn record_read_query(
    repo_root: &Path,
    store_path: &Path,
    primary_anchors: &[String],
    returned_bytes: usize,
) {
    let est = estimate_read_savings(store_path, primary_anchors, returned_bytes);
    record(repo_root, &est);
}

/// Collect unique anchor strings in first-seen order.
pub fn unique_anchors(anchors: impl IntoIterator<Item = impl AsRef<str>>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for a in anchors {
        let s = a.as_ref();
        if seen.insert(s.to_string()) {
            out.push(s.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_saved_tokens_matches_baseline_minus_returned() {
        let ledger = SavingsLedger {
            queries: 3,
            returned_tokens: 1_000,
            baseline_tokens: 10_000,
            saved_tokens: 9_000,
            tool_calls_saved: 5,
        };
        let expected = ledger.baseline_tokens as i64 - ledger.returned_tokens as i64;
        assert_eq!(ledger.saved_tokens, expected);
    }
}
