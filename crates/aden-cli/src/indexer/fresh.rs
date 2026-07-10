// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use aden_core::receipt::{ContextReceipt, ReceiptFreshness};
use aden_store::Storage;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use crate::indexer::r#gen::cmd_gen_silent;
use crate::util::{discover_source_files, find_project_root};

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

static REQUIRE_FRESH: AtomicBool = AtomicBool::new(false);
static REFRESH_CAUSE: AtomicU8 = AtomicU8::new(0);
static NEXT_AUTHORITATIVE_DEADLINE: AtomicU64 = AtomicU64::new(1);
static ACTIVE_AUTHORITATIVE_DEADLINE: AtomicU64 = AtomicU64::new(0);

pub fn set_require_fresh(value: bool) {
    REQUIRE_FRESH.store(value, Ordering::Relaxed);
}

fn require_fresh() -> bool {
    REQUIRE_FRESH.load(Ordering::Relaxed)
}

/// Start the one wall-clock budget for an authoritative read.  It deliberately
/// spans source fingerprinting, silent generation, writer waits, and retries:
/// checking a deadline only between those operations would still let a blocked
/// generation exceed the contract.  This runs in the command process, so
/// terminating it cannot leave a child writer behind.
fn begin_authoritative_deadline() -> Option<u64> {
    if !require_fresh() {
        return None;
    }
    let token = NEXT_AUTHORITATIVE_DEADLINE.fetch_add(1, Ordering::Relaxed);
    ACTIVE_AUTHORITATIVE_DEADLINE.store(token, Ordering::Release);
    std::thread::spawn(move || {
        std::thread::sleep(DECISION_WAIT);
        if ACTIVE_AUTHORITATIVE_DEADLINE.load(Ordering::Acquire) == token {
            eprintln!(
                "aden: authoritative freshness required, but refresh did not complete within {}s; retry after the active writer finishes or unset ADEN_SKIP_AUTO_GEN",
                DECISION_WAIT.as_secs()
            );
            std::process::exit(2);
        }
    });
    Some(token)
}

fn complete_authoritative_deadline(token: Option<u64>) {
    if let Some(token) = token {
        let _ = ACTIVE_AUTHORITATIVE_DEADLINE.compare_exchange(
            token,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

fn set_refresh_cause(cause: u8) {
    REFRESH_CAUSE.store(cause, Ordering::Relaxed);
}

fn refresh_cause() -> &'static str {
    match REFRESH_CAUSE.load(Ordering::Relaxed) {
        1 => "source_changed",
        2 => "store_missing",
        3 => "store_incompatible",
        4 => "refresh_in_flight",
        5 => "frozen",
        6 => "refresh_failed",
        _ => "none",
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct FreshnessManifest {
    pub graph_revision: String,
    pub source_fingerprint: String,
}

fn manifest_path(root: &Path) -> PathBuf {
    aden_paths::project_dir(root).join("freshness.json")
}

fn stable_hex(parts: impl IntoIterator<Item = impl Hash>) -> String {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    for part in parts {
        part.hash(&mut hash);
    }
    format!("{:016x}", hash.finish())
}

/// Content-addressed tree fingerprint. Paths and bytes are both included, so
/// same-second edits, rename/delete, and equal-sized rewrites cannot look fresh.
pub(crate) fn source_fingerprint(root: &Path) -> std::io::Result<String> {
    let mut sources = discover_source_files(root)
        .map_err(|e| std::io::Error::other(format!("source discovery failed: {e}")))?;
    sources.sort();
    let mut records = Vec::with_capacity(sources.len());
    for source in sources {
        let relative = source.strip_prefix(root).unwrap_or(&source);
        // A source we cannot read cannot authoritatively match an old manifest.
        // Propagate the error so every caller fails closed rather than hashing a
        // stable synthetic error marker as if it were indexed content.
        let bytes = std::fs::read(&source)?;
        records.push((relative.to_string_lossy().into_owned(), bytes));
    }
    Ok(stable_hex(records))
}

pub(crate) fn publish_freshness_manifest(
    root: &Path,
    snapshot: &[u8],
    indexed_source_fingerprint: String,
) -> std::io::Result<()> {
    let manifest = FreshnessManifest {
        graph_revision: stable_hex([snapshot]),
        // Capture this before generation begins. If a file mutates mid-gen,
        // the post-gen observation differs and the next read re-arms refresh.
        source_fingerprint: indexed_source_fingerprint,
    };
    let path = manifest_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&manifest)?)?;
    std::fs::rename(tmp, path)
}

fn load_manifest(root: &Path) -> Option<FreshnessManifest> {
    serde_json::from_slice(&std::fs::read(manifest_path(root)).ok()?).ok()
}

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

    // A valid manifest is the sole authoritative source-to-graph binding.
    // The legacy mtime/cache fallback cannot distinguish same-second,
    // equal-sized rewrites, so absent, corrupt, or unwriteable manifests must
    // never yield a `current` claim.
    let Some(manifest) = load_manifest(root) else {
        return true;
    };
    source_fingerprint(root)
        .map(|observed| observed != manifest.source_fingerprint)
        .unwrap_or(true)
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
    augment_read_json_for_root(Some(path), stale, freshness, value)
}

#[cfg(test)]
fn augment_read_json_with_freshness(
    stale: bool,
    freshness: Freshness,
    value: serde_json::Value,
) -> serde_json::Value {
    augment_read_json_for_root(None, stale, freshness, value)
}

fn augment_read_json_for_root(
    path: Option<&Path>,
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
            let root = path.map(find_project_root);
            let manifest = root.as_deref().and_then(load_manifest);
            let observed = root
                .as_deref()
                .and_then(|root| source_fingerprint(root).ok());
            serde_json::to_value(
                ContextReceipt::new()
                    .with_freshness(freshness.into())
                    .with_revision(
                        manifest.map(|m| m.graph_revision),
                        observed,
                        refresh_cause(),
                    ),
            )
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
    set_refresh_cause(0);
    let authoritative_deadline = begin_authoritative_deadline();
    let root = find_project_root(path);
    let skip_auto = skip_auto_gen_on_read();
    let (existing_store, _) = aden_paths::resolve_read_store(&root);
    if !existing_store.exists() {
        set_refresh_cause(2);
        if !skip_auto {
            let _ = cmd_gen_silent(&root);
        }
        enforce_authoritative(&root);
        complete_authoritative_deadline(authoritative_deadline);
        return;
    }

    if !skip_auto && recover_if_incompatible_store(&root) {
        set_refresh_cause(3);
        enforce_authoritative(&root);
        complete_authoritative_deadline(authoritative_deadline);
        return;
    }

    if skip_auto {
        if index_is_stale_for_root(&root) {
            set_refresh_cause(5);
            enforce_authoritative(&root);
        }
        complete_authoritative_deadline(authoritative_deadline);
        return;
    }
    if !index_is_stale_for_root(&root) {
        complete_authoritative_deadline(authoritative_deadline);
        return;
    }
    set_refresh_cause(1);

    // Silent incremental regen (single-flight inside gen; fail-open if locked).
    // Dirty re-arm: up to two more silent gens when we successfully refreshed but
    // the tree changed again under us.
    for _ in 0..3 {
        let _ = cmd_gen_silent(&root);
        if !index_is_stale_for_root(&root) {
            complete_authoritative_deadline(authoritative_deadline);
            return;
        }
        // Still stale: either lock was contended or more edits landed mid-gen.
        if policy == FreshPolicy::Decision || require_fresh() {
            set_refresh_cause(4);
            wait_for_refresh_or_timeout(&root, DECISION_WAIT);
            if !index_is_stale_for_root(&root) {
                complete_authoritative_deadline(authoritative_deadline);
                return;
            }
            // Continue loop for another silent attempt after the wait.
        } else {
            // Explore: fail-open immediately (serve last snapshot).
            enforce_authoritative(&root);
            complete_authoritative_deadline(authoritative_deadline);
            return;
        }
    }
    set_refresh_cause(6);
    enforce_authoritative(&root);
    complete_authoritative_deadline(authoritative_deadline);
}

fn enforce_authoritative(root: &Path) {
    if require_fresh() && index_is_stale_for_root(root) {
        eprintln!(
            "aden: authoritative freshness required, but refresh did not complete within {}s; retry after the active writer finishes or unset ADEN_SKIP_AUTO_GEN",
            DECISION_WAIT.as_secs()
        );
        std::process::exit(2);
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
