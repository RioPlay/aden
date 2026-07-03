// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use aden_store::Storage;
use std::path::{Path, PathBuf};

use crate::indexer::r#gen::cmd_gen_silent;
use crate::util::{discover_source_files, find_project_root, load_gen_cache};

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
/// Rebuild the store from source IF (and only if) it exists on disk but is in a
/// storage-engine format this build cannot read (e.g. after a fjall upgrade).
///
/// Returns `true` when a rebuild was triggered. Unlike [`ensure_fresh`], this
/// does NOT regenerate on mere mtime staleness — it fires solely on the
/// format-mismatch signal. That distinction matters for `heal`, whose whole job
/// is to observe drift between source and the *current* store: a staleness-gen
/// before a heal scan would reconcile the very drift heal is meant to surface,
/// but a store in an unreadable format carries no usable baseline to drift from,
/// so rebuilding it first is correct (and leaves heal a readable store).
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
        // Recovery is best-effort, but its OUTCOME must be honest: return `true`
        // only when the rebuild actually succeeded. A failure here means the
        // store is still unreadable — most concretely a pinned/shared
        // `$ADEN_STORE`, which `cmd_gen_inner` refuses to auto-wipe and so
        // returns `Err`. Returning `true` regardless would make callers
        // (`ensure_fresh`, heal) treat the store as recovered, skip their own
        // logic, and silently degrade to empty results. Surface the error so the
        // user understands why, and return `false` so callers do not assume
        // success.
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

/// True when read-path callers (MCP, coxn) own explicit `gen` and must not
/// trigger a silent incremental regen on every query.
pub(crate) fn skip_auto_gen_on_read() -> bool {
    match std::env::var("ADEN_SKIP_AUTO_GEN") {
        Ok(v) => {
            let t = v.trim();
            !t.is_empty() && !matches!(t, "0" | "false" | "no" | "off")
        }
        Err(_) => false,
    }
}

/// Whether any indexed source file is newer than the last `gen` cache entry.
pub fn index_is_stale(path: &Path) -> bool {
    index_is_stale_for_root(&find_project_root(path))
}

fn index_is_stale_for_root(root: &Path) -> bool {
    use std::time::UNIX_EPOCH;

    let (existing_store, _) = aden_paths::resolve_read_store(root);
    if !existing_store.exists() {
        return false;
    }

    let cache = load_gen_cache(&aden_paths::gen_cache_file(root));
    let sources = match discover_source_files(root) {
        Ok(s) => s,
        Err(_) => return false,
    };

    let newest_known = cache
        .entries
        .values()
        .map(|e| e.source_mtime)
        .max()
        .unwrap_or(0);

    sources.iter().any(|src| {
        let mtime = src
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        mtime > newest_known
    })
}

pub const STALE_HINT: &str = "NOTE: index_stale=true — working tree changed since last `gen`; run `gen` or `sync` for a fresh graph.";

/// Whether the index is stale for agent-facing read output (auto-gen suppressed).
pub fn read_index_stale(path: &Path) -> bool {
    skip_auto_gen_on_read() && index_is_stale(path)
}

/// Add `index_stale` (and `stale_hint` when true) to MCP read-tool JSON output.
/// Bare arrays are wrapped as `{"index_stale": …, "items": …}` so agents always
/// receive an object envelope.
pub fn augment_read_json(path: &Path, value: serde_json::Value) -> serde_json::Value {
    augment_read_json_with_stale(read_index_stale(path), value)
}

fn augment_read_json_with_stale(stale: bool, value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(mut map) => {
            map.insert("index_stale".into(), serde_json::Value::Bool(stale));
            if stale {
                map.insert(
                    "stale_hint".into(),
                    serde_json::Value::String(STALE_HINT.into()),
                );
            }
            serde_json::Value::Object(map)
        }
        other => {
            let mut map = serde_json::Map::new();
            map.insert("index_stale".into(), serde_json::Value::Bool(stale));
            if stale {
                map.insert(
                    "stale_hint".into(),
                    serde_json::Value::String(STALE_HINT.into()),
                );
            }
            map.insert("items".into(), other);
            serde_json::Value::Object(map)
        }
    }
}

/// Print a stale-index hint when auto-gen is suppressed (MCP read tools).
/// JSON mode carries `index_stale` via [`augment_read_json`] instead.
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

pub fn ensure_fresh(path: &Path) {
    let root = find_project_root(path);
    let skip_auto = skip_auto_gen_on_read();
    // No store yet → build it now. Read commands are store-first, so a fresh
    // project must be indexed on first query (this is what makes asm/ask/locate
    // work without an explicit `aden gen`). When auto-gen is suppressed the
    // caller must run `aden gen` explicitly (coxn does this at boot).
    let (existing_store, _) = aden_paths::resolve_read_store(&root);
    if !existing_store.exists() {
        if !skip_auto {
            let _ = cmd_gen_silent(&root);
        }
        return;
    }

    // The store exists but may be unreadable by this build — e.g. it was written
    // by an older storage-engine format and this binary was just upgraded. The
    // mtime freshness check below would see an up-to-date tree and skip the
    // rebuild, leaving the read to open an incompatible store and degrade to
    // empty results. Recover on the format-mismatch signal (and ONLY that), so
    // the read auto-recovers with zero user action.
    if !skip_auto && recover_if_incompatible_store(&root) {
        return;
    }

    if index_is_stale_for_root(&root) && !skip_auto {
        // Silent incremental regen: re-parses only changed files and re-links
        // edges, without printing anything (this runs transparently on reads).
        let _ = cmd_gen_silent(&root);
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
        let obj =
            augment_read_json_with_stale(false, serde_json::json!({"total": 0, "matches": []}));
        assert_eq!(obj["total"], 0);
        assert_eq!(obj["index_stale"].as_bool(), Some(false));
        assert!(obj.get("stale_hint").is_none());

        let arr = augment_read_json_with_stale(false, serde_json::json!(["a"]));
        assert_eq!(arr["items"][0], "a");
        assert_eq!(arr["index_stale"].as_bool(), Some(false));

        let stale = augment_read_json_with_stale(true, serde_json::json!({"total": 1}));
        assert_eq!(stale["index_stale"].as_bool(), Some(true));
        assert_eq!(stale["stale_hint"].as_str(), Some(STALE_HINT));
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
}
