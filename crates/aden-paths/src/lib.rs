// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-user, per-project path resolution and project identity for Aden (ADR-003).
//!
//! Aden splits its on-disk data into three classes:
//!
//! - **Rebuildable** (`store/`, `cache/`, `gen-cache.json`, `scan-cache.json`,
//!   `meta.json`) lives in a per-user data directory, keyed per project, so it
//!   is *never* written into the project tree. This eliminates "subfolder
//!   pollution" — stray `.aden/store` directories materializing wherever a read
//!   command happened to run — by construction.
//! - **Durable, repo-coupled intent** (`overlays/`, `constitution.adoc`,
//!   `hooks/`, `staging/`, `proposals/`, `project.conf`) stays in-tree under
//!   `<root>/.aden/` and is git-tracked, so it travels with the repository.
//! - **Ephemeral scratch** lives in the OS temp dir.
//!
//! Resolution always returns *something* (reads stay frictionless). *Creation*
//! is strict: [`guard_creatable_root`] refuses to materialize a store at `$HOME`
//! or a filesystem root unless the caller was explicit.
//!
//! Environment overrides:
//! - `ADEN_DATA_DIR` replaces the per-user data base (CI / containers / monorepos).
//! - `ADEN_STORE` pins the store directory verbatim and *disables* per-project
//!   keying (power users / pinned CI). When set, every project shares it.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Number of hex chars in a project key (`sha256(root)[:8 bytes]`).
const KEY_HEX_LEN: usize = 16;

/// Project-root markers, highest-priority ecosystems first. Mirrors the CLI's
/// historical marker set; kept here so root resolution is self-contained and
/// every crate maps any in-project path to the *same* root (and thus the same
/// store key), eliminating the "bypass" defect class where library code built
/// `.aden/store` from a raw, unresolved argument.
const ROOT_MARKERS: &[&str] = &[
    // Aden / VCS — strongest signal of the true repo root
    "aden.toml",
    ".git",
    ".aden",
    ".hg",
    ".svn",
    // Rust
    "Cargo.toml",
    // Go
    "go.mod",
    // Node / JS / TS
    "package.json",
    // Python
    "pyproject.toml",
    "setup.py",
    "setup.cfg",
    "requirements.txt",
    "Pipfile",
    // JVM
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    // Ruby
    "Gemfile",
    // PHP
    "composer.json",
    // C / C++
    "CMakeLists.txt",
    // Generic
    "Makefile",
];

/// Resolve the canonical project root containing `start` (ADR-003 §1).
///
/// Order: `git rev-parse --show-toplevel` (when in a work tree) → root-marker
/// walk-up → persisted `.aden/project.conf` → canonical `start`. Idempotent:
/// resolving an already-resolved root returns it unchanged, so callers may pass
/// either a raw scope argument or a pre-resolved root.
pub fn resolve_root(start: &Path) -> PathBuf {
    let canon = canonical(start);
    if let Some(top) = git_toplevel(&canon) {
        return top;
    }
    let mut current = canon.clone();
    loop {
        if ROOT_MARKERS.iter().any(|m| current.join(m).exists()) {
            return current;
        }
        match current.parent() {
            Some(p) => current = p.to_path_buf(),
            None => break,
        }
    }
    // Persisted project.conf fallback (written by `--project`).
    let mut probe = canon.clone();
    loop {
        let conf = probe.join(".aden").join("project.conf");
        if conf.is_file()
            && let Ok(s) = std::fs::read_to_string(&conf)
        {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                let p = PathBuf::from(trimmed);
                if p.is_dir() {
                    return p;
                }
            }
        }
        match probe.parent() {
            Some(p) => probe = p.to_path_buf(),
            None => break,
        }
    }
    canon
}

/// Resolve the git work-tree root via `git rev-parse --show-toplevel`. `None`
/// when git is unavailable, `dir` isn't in a work tree, or the command fails.
fn git_toplevel(dir: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let trimmed = String::from_utf8(output.stdout).ok()?;
    let trimmed = trimmed.trim();
    if trimmed.is_empty() {
        return None;
    }
    let p = PathBuf::from(trimmed);
    p.is_dir().then_some(p)
}

/// Per-user data root: `$ADEN_DATA_DIR` if set, else `dirs::data_dir()/aden`.
///
/// Falls back to `~/.local/share/aden` semantics via the `dirs` crate; if even
/// that is unavailable (no home dir), falls back to the OS temp dir so reads
/// never hard-fail on path resolution.
pub fn data_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("ADEN_DATA_DIR") {
        return PathBuf::from(dir);
    }
    match dirs::data_dir() {
        Some(d) => d.join("aden"),
        None => std::env::temp_dir().join("aden"),
    }
}

/// Canonicalize `root`, falling back to the path as given when canonicalization
/// fails (read-only checkouts, not-yet-created dirs). Mirrors the lexical
/// fallback in `find_project_root` so the project key stays stable across runs.
fn canonical(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

/// Stable 16-hex-char project identity: `sha256(canonical_root)[:8 bytes]`.
///
/// The same repo addressed from any subdirectory resolves to the same root and
/// thus the same key (the fix). Two clones at different paths get distinct keys
/// (correct — independent stores).
pub fn project_key(root: &Path) -> String {
    use sha2::{Digest, Sha256};
    let canon = resolve_root(root);
    let mut hasher = Sha256::new();
    hasher.update(canon.as_os_str().as_encoded_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(KEY_HEX_LEN);
    for byte in digest.iter().take(KEY_HEX_LEN / 2) {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// `<data_root>/projects/<key>` — the per-project key directory holding all
/// rebuildable artifacts for `root`.
pub fn project_dir(root: &Path) -> PathBuf {
    data_root().join("projects").join(project_key(root))
}

/// Rebuildable graph store directory.
///
/// `$ADEN_STORE` (when set) overrides this verbatim and bypasses per-project
/// keying — every project then shares the one pinned store.
pub fn store_dir(root: &Path) -> PathBuf {
    if let Some(dir) = std::env::var_os("ADEN_STORE") {
        return PathBuf::from(dir);
    }
    project_dir(root).join("store")
}

/// Rebuildable graph/index cache directory.
pub fn cache_dir(root: &Path) -> PathBuf {
    project_dir(root).join("cache")
}

/// Incremental `gen` cache file.
pub fn gen_cache_file(root: &Path) -> PathBuf {
    project_dir(root).join("gen-cache.json")
}

/// Heal drift-scan cache file.
pub fn scan_cache_file(root: &Path) -> PathBuf {
    project_dir(root).join("scan-cache.json")
}

/// Ephemeral per-run scratch dir (may be wiped on reboot). Not durable.
pub fn temp_dir() -> PathBuf {
    std::env::temp_dir().join("aden")
}

// --- Durable, in-tree, git-tracked intent. These STAY under <root>/.aden/. ---

/// In-tree `.aden/` directory holding durable repo-coupled intent.
pub fn intent_dir(root: &Path) -> PathBuf {
    root.join(".aden")
}

/// In-tree per-symbol intent overlays (`.aden/overlays/`).
pub fn overlays_dir(root: &Path) -> PathBuf {
    intent_dir(root).join("overlays")
}

/// In-tree project constitution (`.aden/constitution.adoc`).
pub fn constitution_file(root: &Path) -> PathBuf {
    intent_dir(root).join("constitution.adoc")
}

/// In-tree sample git hooks (`.aden/hooks/`).
pub fn hooks_dir(root: &Path) -> PathBuf {
    intent_dir(root).join("hooks")
}

/// In-tree agent proposal staging audit trail (`.aden/staging/`).
pub fn staging_dir(root: &Path) -> PathBuf {
    intent_dir(root).join("staging")
}

/// In-tree approved-proposal records (`.aden/proposals/`).
pub fn proposals_dir(root: &Path) -> PathBuf {
    intent_dir(root).join("proposals")
}

/// In-tree persisted project root (`.aden/project.conf`).
pub fn project_conf_file(root: &Path) -> PathBuf {
    intent_dir(root).join("project.conf")
}

// --- Legacy back-compat -----------------------------------------------------

/// The pre-ADR-003 in-tree store location (`<root>/.aden/store`). Used only to
/// detect and migrate legacy stores; new stores are never written here.
/// Resolves to the project root first so a subdir argument still points at the
/// repo-level legacy store.
pub fn legacy_store_dir(root: &Path) -> PathBuf {
    resolve_root(root).join(".aden").join("store")
}

/// Resolve which store a *read* command should open, preferring the central
/// (per-user) store and falling back to a legacy in-tree store if one exists.
///
/// Returns `(path, is_legacy)`. When `is_legacy` is true the caller should emit
/// a one-time deprecation notice (see [`legacy_notice`]). When neither exists,
/// returns the central path (the subsequent open will fail with a clear error).
pub fn resolve_read_store(root: &Path) -> (PathBuf, bool) {
    let central = store_dir(root);
    if central.exists() {
        return (central, false);
    }
    let legacy = legacy_store_dir(root);
    if legacy.exists() {
        return (legacy, true);
    }
    (central, false)
}

/// One-line deprecation notice for a legacy in-tree store still being read.
pub fn legacy_notice(root: &Path) -> String {
    format!(
        "note: reading legacy in-tree store at {}; run 'aden store migrate' to move it to {}",
        legacy_store_dir(root).display(),
        store_dir(root).display(),
    )
}

// --- Project metadata (meta.json) -------------------------------------------

/// Per-project metadata recorded in `<project_dir>/meta.json`. Enables
/// `aden store list` / `aden store prune` and detects the (astronomically rare)
/// key collision by recording the real root.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    /// The canonical project root this store belongs to.
    pub root: String,
    /// The project key (matches the containing directory name).
    pub key: String,
}

/// Path to a project's `meta.json`.
pub fn meta_file(root: &Path) -> PathBuf {
    project_dir(root).join("meta.json")
}

/// Write/refresh `meta.json` for `root`. Called by creation paths (init/gen/regen)
/// after the project directory exists.
pub fn write_meta(root: &Path) -> std::io::Result<()> {
    let dir = project_dir(root);
    std::fs::create_dir_all(&dir)?;
    let meta = Meta {
        root: resolve_root(root).display().to_string(),
        key: project_key(root),
    };
    let json = serde_json::to_string_pretty(&meta)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(dir.join("meta.json"), json)
}

/// Read a `meta.json` from a project key directory (used by `store list/prune`).
pub fn read_meta(key_dir: &Path) -> Option<Meta> {
    let bytes = std::fs::read(key_dir.join("meta.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

// --- Creation safety rails --------------------------------------------------

/// Refuse to *create* a store when the resolved root is a dangerous location
/// ($HOME or a filesystem root) unless the caller was explicit.
///
/// `explicit` should be true when `-p`/`--project` was given, the command is
/// `init`, or `$ADEN_DATA_DIR`/`$ADEN_STORE` is set. Resolution (reads) never
/// calls this — only creation does.
pub fn guard_creatable_root(root: &Path, explicit: bool) -> Result<(), String> {
    if explicit
        || std::env::var_os("ADEN_DATA_DIR").is_some()
        || std::env::var_os("ADEN_STORE").is_some()
    {
        return Ok(());
    }
    let canon = resolve_root(root);
    // Filesystem root (no parent).
    if canon.parent().is_none() {
        return Err(format!(
            "refusing to create a store at filesystem root {}; pass -p <project> or run 'aden init'",
            canon.display()
        ));
    }
    if let Some(home) = dirs::home_dir()
        && canonical(&home) == canon
    {
        return Err(format!(
            "refusing to create a store at your home directory {}; pass -p <project> or run 'aden init'",
            canon.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run `f` with `var` set to `val`, restoring the prior value afterward.
    /// Tests touching process-global env must not run concurrently; the harness
    /// here keeps each override scoped and serialized via a single test.
    fn with_env<T>(var: &str, val: Option<&str>, f: impl FnOnce() -> T) -> T {
        let prev = std::env::var_os(var);
        match val {
            Some(v) => unsafe { std::env::set_var(var, v) },
            None => unsafe { std::env::remove_var(var) },
        }
        let out = f();
        match prev {
            Some(p) => unsafe { std::env::set_var(var, p) },
            None => unsafe { std::env::remove_var(var) },
        }
        out
    }

    #[test]
    fn key_is_stable_and_16_hex() {
        let dir = tempfile::tempdir().unwrap();
        let k1 = project_key(dir.path());
        let k2 = project_key(dir.path());
        assert_eq!(k1, k2, "key must be deterministic");
        assert_eq!(k1.len(), KEY_HEX_LEN);
        assert!(k1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn key_stable_when_canonicalize_fails() {
        // A nonexistent path can't canonicalize; key must still be deterministic.
        let p = Path::new("/no/such/path/aden-test-xyz");
        assert_eq!(project_key(p), project_key(p));
    }

    #[test]
    fn distinct_roots_distinct_keys() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        assert_ne!(project_key(a.path()), project_key(b.path()));
    }

    #[test]
    fn env_and_overrides_and_guard() {
        let root_buf = std::env::temp_dir().join("aden-fixture");
        let root = root_buf.as_path();

        with_env("ADEN_DATA_DIR", Some("/custom/data"), || {
            assert_eq!(data_root(), PathBuf::from("/custom/data"));
            assert!(store_dir(root).starts_with("/custom/data/projects"));
        });

        with_env("ADEN_STORE", Some("/pinned/store"), || {
            assert_eq!(store_dir(root), PathBuf::from("/pinned/store"));
        });

        // Guard: refuse fs root without explicit, allow with explicit.
        with_env("ADEN_DATA_DIR", None, || {
            with_env("ADEN_STORE", None, || {
                assert!(guard_creatable_root(Path::new("/"), false).is_err());
                assert!(guard_creatable_root(Path::new("/"), true).is_ok());
                if let Some(home) = dirs::home_dir() {
                    assert!(guard_creatable_root(&home, false).is_err());
                }
            });
        });
    }

    #[test]
    fn intent_paths_stay_in_tree() {
        let root = Path::new("/proj");
        assert_eq!(overlays_dir(root), Path::new("/proj/.aden/overlays"));
        assert_eq!(
            constitution_file(root),
            Path::new("/proj/.aden/constitution.adoc")
        );
        assert_eq!(legacy_store_dir(root), Path::new("/proj/.aden/store"));
    }
}
