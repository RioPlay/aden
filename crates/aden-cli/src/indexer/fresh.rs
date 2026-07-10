// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use aden_core::receipt::{ContextReceipt, ReceiptFreshness};
use aden_store::Storage;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::indexer::r#gen::cmd_gen_silent;
use crate::util::{discover_source_files, find_project_root, load_gen_cache};

/// How aggressively a read path waits for an in-flight refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FreshPolicy {
    /// Never block on a writer. Trigger silent gen only if the write lock is free.
    /// Explore tools (`grep`, `ask`, `asm`, …).
    #[default]
    Explore,
    /// If the index is stale and a writer is busy, wait briefly for the refresh
    /// to finish (blast-radius / decision tools). Never waits longer than
    /// [`DECISION_WAIT`].
    Decision,
}

/// Cap for [`FreshPolicy::Decision`] short-wait (seconds).
pub const DECISION_WAIT: Duration = Duration::from_secs(5);

/// Machine-readable freshness for agents (JSON envelope).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // `Unavailable` reserved for hard store failures in the wire contract
pub enum Freshness {
    /// Graph matches the working tree (or was just refreshed).
    Current,
    /// Answering from last snapshot; tree may have changed or refresh is in flight.
    Snapshot,
    /// Decision tools waited; graph may still lag the tree.
    Lagging,
    /// No store / first build in progress or unavailable.
    Building,
    /// Hard failure / unusable store.
    Unavailable,
}

impl Freshness {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Snapshot => "snapshot",
            Self::Lagging => "lagging",
            Self::Building => "building",
            Self::Unavailable => "unavailable",
        }
    }
}

impl From<Freshness> for ReceiptFreshness {
    fn from(value: Freshness) -> Self {
        match value {
            Freshness::Current => Self::Current,
            Freshness::Snapshot => Self::Snapshot,
            Freshness::Lagging => Self::Lagging,
            Freshness::Building => Self::Building,
            Freshness::Unavailable => Self::Unavailable,
        }
    }
}

/// Rebuild the store from source IF (and only if) it exists on disk but is in a
/// storage-engine format this build cannot read (e.g. after a fjall upgrade).
///
/// Returns `true` when a rebuild was triggered. Unlike [`ensure_fresh`], this
/// does NOT regenerate on mere mtime staleness — it fires solely on the
/// format-mismatch signal.
pub(crate) fn recover_if_incompatible_store(path: &Path) -> bool {
    let root = find_project_root(path);
    let (store, _) = aden_paths::resolve_read_store(&root);
    if store.exists()
        && let Some(store_str) = store.to_str()
        && matches!(
            Storage::open_existing(store_str),
            Err(aden_store::StoreError::IncompatibleVersion(_))
        )
    {
        return match cmd_gen_silent(&root) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("aden: could not rebuild the incompatible store: {e}");
                false
            }
        };
    }
    false
}

/// True when callers opt out of silent incremental regen on every query.
/// Escape hatch for CI freeze / offline snapshot reads — not the MCP default.
pub(crate) fn skip_auto_gen_on_read() -> bool {
    match std::env::var("ADEN_SKIP_AUTO_GEN") {
        Ok(v) => {
            let t = v.trim();
            !t.is_empty() && !matches!(t, "0" | "false" | "no" | "off")
        }
        Err(_) => false,
    }
}

/// Whether the index lags the working tree (presence + per-file mtime).
///
/// Stale when any of:
/// - a live source is missing from the gen-cache (new file)
/// - a live source's mtime is newer than its cache entry (edit)
/// - a cache entry's source is no longer live (delete / rename / ignore)
/// - the store exists, sources exist, but the cache is empty
pub fn index_is_stale(path: &Path) -> bool {
    index_is_stale_for_root(&find_project_root(path))
}

pub(crate) fn index_is_stale_for_root(root: &Path) -> bool {
    let (existing_store, _) = aden_paths::resolve_read_store(root);
    if !existing_store.exists() {
        return false;
    }

    let cache = load_gen_cache(&aden_paths::gen_cache_file(root));
    let sources = match discover_source_files(root) {
        Ok(s) => s,
        Err(_) => return false,
    };

    let mut live: HashMap<String, u64> = HashMap::with_capacity(sources.len());
    for src in &sources {
        let rel = src
            .strip_prefix(root)
            .unwrap_or(src)
            .to_string_lossy()
            .to_string();
        if aden_core::filter::is_secret_path(Path::new(&rel)) {
            continue;
        }
        let mtime = src
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        live.insert(rel, mtime);
    }

    if cache.entries.is_empty() && !live.is_empty() {
        return true;
    }

    for (rel, mtime) in &live {
        match cache.entries.get(rel) {
            None => return true,
            Some(e) if *mtime > e.source_mtime => return true,
            _ => {}
        }
    }

    for key in cache.entries.keys() {
        if !live.contains_key(key) {
            return true;
        }
    }

    false
}

pub const STALE_HINT: &str = "NOTE: index may lag the working tree — refresh in progress or last snapshot served. Blast-radius tools wait briefly; explore tools fail open.";

/// Whether the index is stale for agent-facing read output.
///
/// When auto-gen is on (default), a successful ensure_fresh leaves the graph
/// current; we only report lag when auto-gen was skipped *or* the tree is still
/// dirty after a non-blocking refresh attempt (writer busy).
pub fn read_index_stale(path: &Path) -> bool {
    index_is_stale(path)
}

/// Best-effort freshness classification for JSON envelopes.
pub fn classify_freshness(path: &Path) -> Freshness {
    let root = find_project_root(path);
    let (store, _) = aden_paths::resolve_read_store(&root);
    if !store.exists() {
        return Freshness::Building;
    }
    if index_is_stale_for_root(&root) {
        if aden_paths::graph_snapshot_file(&root).is_file() {
            return Freshness::Lagging;
        }
        return Freshness::Snapshot;
    }
    Freshness::Current
}

/// Add legacy freshness fields and the versioned Context Receipt to read JSON.
///
/// Migration: legacy `freshness`, `index_stale`, `stale_hint`, and `items`
/// fields remain unchanged for the compatibility window. New consumers read the
/// nested `context_receipt` object. The receipt is deliberately nested so its
/// future fields cannot collide with payload fields.
/// Bare arrays retain the established `items` wrapper.
pub fn augment_read_json(path: &Path, value: serde_json::Value) -> serde_json::Value {
    let freshness = classify_freshness(path);
    let stale = freshness != Freshness::Current;
    augment_read_json_with_freshness(stale, freshness, value)
}

fn augment_read_json_with_freshness(
    stale: bool,
    freshness: Freshness,
    value: serde_json::Value,
) -> serde_json::Value {
    let insert = |map: &mut serde_json::Map<String, serde_json::Value>| {
        map.insert(
            "freshness".into(),
            serde_json::Value::String(freshness.as_str().into()),
        );
        map.insert("index_stale".into(), serde_json::Value::Bool(stale));
        if stale {
            map.insert(
                "stale_hint".into(),
                serde_json::Value::String(STALE_HINT.into()),
            );
        }
        // A legacy producer may already own this field. Preserve it exactly;
        // changing its shape would be a breaking migration. Aden-generated
        // read envelopes reserve this namespace and therefore receive v1.
        map.entry("context_receipt").or_insert_with(|| {
            serde_json::to_value(ContextReceipt::new().with_freshness(freshness.into()))
                .expect("ContextReceipt always serializes")
        });
    };
    match value {
        serde_json::Value::Object(mut map) => {
            insert(&mut map);
            serde_json::Value::Object(map)
        }
        other => {
            let mut map = serde_json::Map::new();
            insert(&mut map);
            map.insert("items".into(), other);
            serde_json::Value::Object(map)
        }
    }
}

/// Print a stale-index hint when the tree lags (JSON uses envelope fields).
pub fn maybe_print_stale_hint(path: &Path, json: bool) {
    if !json && read_index_stale(path) {
        println!("{STALE_HINT}");
    }
}

/// RAII guard: prints [`maybe_print_stale_hint`] when the read command completes.
pub struct StaleHintGuard {
    root: PathBuf,
    json: bool,
}

impl StaleHintGuard {
    pub fn new(path: &Path, json: bool) -> Self {
        Self {
            root: find_project_root(path),
            json,
        }
    }
}

impl Drop for StaleHintGuard {
    fn drop(&mut self) {
        maybe_print_stale_hint(&self.root, self.json);
    }
}

/// Ensure the store is up to date before a read (explore policy: non-blocking).
pub fn ensure_fresh(path: &Path) {
    ensure_fresh_with_policy(path, FreshPolicy::Explore);
}

/// Decision-grade tools: short-wait for an in-flight gen when the index is stale.
pub fn ensure_fresh_decision(path: &Path) {
    ensure_fresh_with_policy(path, FreshPolicy::Decision);
}

pub fn ensure_fresh_with_policy(path: &Path, policy: FreshPolicy) {
    let root = find_project_root(path);
    let skip_auto = skip_auto_gen_on_read();
    let (existing_store, _) = aden_paths::resolve_read_store(&root);
    if !existing_store.exists() {
        if !skip_auto {
            let _ = cmd_gen_silent(&root);
        }
        return;
    }

    if !skip_auto && recover_if_incompatible_store(&root) {
        return;
    }

    if skip_auto || !index_is_stale_for_root(&root) {
        return;
    }

    // Silent incremental regen (single-flight inside gen; fail-open if locked).
    // Dirty re-arm: up to two more silent gens when we successfully refreshed but
    // the tree changed again under us.
    for _ in 0..3 {
        let _ = cmd_gen_silent(&root);
        if !index_is_stale_for_root(&root) {
            return;
        }
        // Still stale: either lock was contended or more edits landed mid-gen.
        if policy == FreshPolicy::Decision {
            wait_for_refresh_or_timeout(&root, DECISION_WAIT);
            if !index_is_stale_for_root(&root) {
                return;
            }
            // Continue loop for another silent attempt after the wait.
        } else {
            // Explore: fail-open immediately (serve last snapshot).
            return;
        }
    }
}

/// Poll until the index is fresh or the deadline passes (another process holds gen).
fn wait_for_refresh_or_timeout(root: &Path, budget: Duration) {
    let deadline = SystemTime::now() + budget;
    let lock_path = aden_paths::store_lock_file(root);
    while SystemTime::now() < deadline {
        if !index_is_stale_for_root(root) {
            return;
        }
        // If no writer is active, another attempt will be made by the caller.
        if aden_core::lock::read_holder(&lock_path).is_none() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod skip_auto_gen_tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_env(key: &str, value: Option<&str>, f: impl FnOnce()) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var(key).ok();
        match value {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
        f();
        match prev {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
    }

    #[test]
    fn augment_read_json_wraps_arrays_and_tags_objects() {
        let obj = augment_read_json_with_freshness(
            false,
            Freshness::Current,
            serde_json::json!({"total": 0, "matches": []}),
        );
        assert_eq!(obj["total"], 0);
        assert_eq!(obj["index_stale"].as_bool(), Some(false));
        assert_eq!(obj["freshness"].as_str(), Some("current"));
        assert!(obj.get("stale_hint").is_none());
        assert_eq!(obj["context_receipt"]["schema_version"], 1);
        assert_eq!(obj["context_receipt"]["freshness"], "current");

        let arr =
            augment_read_json_with_freshness(false, Freshness::Current, serde_json::json!(["a"]));
        assert_eq!(arr["items"][0], "a");
        assert_eq!(arr["index_stale"].as_bool(), Some(false));

        let stale = augment_read_json_with_freshness(
            true,
            Freshness::Lagging,
            serde_json::json!({"total": 1}),
        );
        assert_eq!(stale["index_stale"].as_bool(), Some(true));
        assert_eq!(stale["freshness"].as_str(), Some("lagging"));
        assert_eq!(stale["stale_hint"].as_str(), Some(STALE_HINT));
        assert_eq!(stale["context_receipt"]["freshness"], "lagging");

        let legacy = augment_read_json_with_freshness(
            false,
            Freshness::Current,
            serde_json::json!({"context_receipt":{"legacy":true}}),
        );
        assert_eq!(
            legacy["context_receipt"],
            serde_json::json!({"legacy":true})
        );

        let array = augment_read_json_with_freshness(
            false,
            Freshness::Current,
            serde_json::json!([{"context_receipt":{"payload":true}}]),
        );
        assert_eq!(
            array["items"][0]["context_receipt"],
            serde_json::json!({"payload":true})
        );
        assert_eq!(array["context_receipt"]["schema_version"], 1);
    }

    #[test]
    fn skip_auto_gen_env_is_truthy_except_off_values() {
        with_env("ADEN_SKIP_AUTO_GEN", None, || {
            assert!(!skip_auto_gen_on_read());
        });
        with_env("ADEN_SKIP_AUTO_GEN", Some("1"), || {
            assert!(skip_auto_gen_on_read());
        });
        with_env("ADEN_SKIP_AUTO_GEN", Some("true"), || {
            assert!(skip_auto_gen_on_read());
        });
        with_env("ADEN_SKIP_AUTO_GEN", Some("0"), || {
            assert!(!skip_auto_gen_on_read());
        });
        with_env("ADEN_SKIP_AUTO_GEN", Some("false"), || {
            assert!(!skip_auto_gen_on_read());
        });
    }

    #[test]
    fn freshness_as_str_labels() {
        assert_eq!(Freshness::Current.as_str(), "current");
        assert_eq!(Freshness::Lagging.as_str(), "lagging");
        assert_eq!(Freshness::Snapshot.as_str(), "snapshot");
    }
}
