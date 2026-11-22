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

/// Maximum cached `(canonical_start, resolved_root)` entries per request scope.
const READ_ROOT_CACHE_LIMIT: usize = 16;

/// Resolve the canonical project root containing `start` (ADR-003 §1).
///
/// Order: `git rev-parse --show-toplevel` (when in a work tree) → root-marker
/// walk-up → persisted `.aden/project.conf` → canonical `start`. Idempotent:
/// resolving an already-resolved root returns it unchanged, so callers may pass
/// either a raw scope argument or a pre-resolved root.
pub fn resolve_root(start: &Path) -> PathBuf {
    let canonicalized = start.canonicalize();
    let can_probe_git_markers = canonicalized.is_ok();
    let canon = canonicalized.unwrap_or_else(|_| start.to_path_buf());
    if let Some(hit) = READ_ROOT.with_borrow(|state| {
        state
            .as_ref()?
            .iter()
            .find(|(key, _)| key == &canon)
            .map(|(_, root)| root.clone())
    }) {
        return hit;
    }
    let root = resolve_canonical_root(&canon, can_probe_git_markers);
    READ_ROOT.with_borrow_mut(|state| {
        if let Some(entries) = state {
            if entries.len() < READ_ROOT_CACHE_LIMIT
                && !entries.iter().any(|(key, _)| key == &canon)
            {
                entries.push((canon.clone(), root.clone()));
            }
            if entries.len() < READ_ROOT_CACHE_LIMIT
                && let Ok(root_canon) = root.canonicalize()
                && root_canon != canon
                && !entries.iter().any(|(key, _)| key == &root_canon)
            {
                entries.push((root_canon, root.clone()));
            }
        }
    });
    root
}

thread_local! {
    // Outer option enables reuse only inside a synchronous read request.
    static READ_ROOT: std::cell::RefCell<Option<Vec<(PathBuf, PathBuf)>>> =
        const { std::cell::RefCell::new(None) };
}

/// Reuse root discoveries within this read and discard them on return or unwind.
pub fn with_read_root_scope<T>(read: impl FnOnce() -> T) -> T {
    struct Reset(Option<Vec<(PathBuf, PathBuf)>>);
    impl Drop for Reset {
        fn drop(&mut self) {
            READ_ROOT.replace(self.0.take());
        }
    }
    let _reset = Reset(READ_ROOT.replace(Some(Vec::new())));
    read()
}

fn resolve_canonical_root(canon: &Path, can_probe_git_markers: bool) -> PathBuf {
    if (!can_probe_git_markers || git_discovery_may_apply(canon))
        && let Some(top) = git_toplevel(canon)
    {
        return top;
    }
    let mut current = canon.to_path_buf();
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
    let mut probe = canon.to_path_buf();
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
    canon.to_path_buf()
}

fn git_discovery_may_apply(canon: &Path) -> bool {
    // GIT_DIR can select a repository with no on-disk marker in this ancestry.
    // Leave repository placement and discovery overrides to Git itself.
    let git_environment = [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_IMPLICIT_WORK_TREE",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    ]
    .into_iter()
    .any(|name| std::env::var_os(name).is_some());
    git_environment || canon.ancestors().any(could_have_git_marker)
}

fn could_have_git_marker(dir: &Path) -> bool {
    // Git probes .git files and directories, then looks for a bare repository
    // in the ancestor itself, which requires HEAD. A candidate of any type or
    // uncertain metadata leaves the answer to Git.
    [".git", "HEAD"].into_iter().any(|name| {
        !matches!(
            dir.join(name).symlink_metadata(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        )
    })
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

/// Strip Windows extended-length prefixes so two spellings of the same path
/// compare equal.
///
/// On Windows, `Path::canonicalize` often yields `\\?\C:\…` (or `\\?\UNC\…`),
/// while `git rev-parse --show-toplevel` returns a normal `C:/…` form. Bytewise
/// `starts_with` / `strip_prefix` then fail even when the paths are the same
/// directory — which broke `aden tree` scope checks and disabled
/// `.adenignore` during discovery (walking `target/` made `grep` take tens of
/// seconds).
///
/// Pure string transform: safe to call with non-existent paths and on non-Windows
/// hosts (no-op unless the path literally carries the prefix).
pub fn strip_verbatim(path: &Path) -> PathBuf {
    let value = path.to_string_lossy();
    if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = value.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    // Forward-slash variants sometimes appear after separator normalization.
    if let Some(rest) = value.strip_prefix("//?/") {
        if let Some(unc) = rest.strip_prefix("UNC/") {
            return PathBuf::from(format!("//{unc}"));
        }
        return PathBuf::from(rest);
    }
    path.to_path_buf()
}

/// Normalize a path for containment checks: strip verbatim prefixes and unify
/// separators. On Windows, also case-fold because NTFS is case-insensitive.
fn compare_key(path: &Path) -> PathBuf {
    let stripped = strip_verbatim(path);
    let unified = stripped
        .to_string_lossy()
        .replace(['/', '\\'], std::path::MAIN_SEPARATOR_STR);
    #[cfg(windows)]
    {
        PathBuf::from(unified.to_ascii_lowercase())
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(unified)
    }
}

/// True when `path` is `root` or a descendant.
///
/// Both sides are canonicalized when possible and compared after
/// [`strip_verbatim`], so Windows `\\?\` vs plain drive paths agree.
pub fn is_under(root: &Path, path: &Path) -> bool {
    let root_key = compare_key(&canonical(root));
    let path_key = compare_key(&path.canonicalize().unwrap_or_else(|_| path.to_path_buf()));
    path_key.starts_with(&root_key)
}

/// Project-relative path of `path` under `root`, if any.
///
/// Returns `None` when `path` is outside `root`. Prefer this over bare
/// `Path::strip_prefix` anywhere ignore rules or display keys need a relative
/// path on Windows.
pub fn relative_to(root: &Path, path: &Path) -> Option<PathBuf> {
    RelativeRoot::new(root).relative_to(path)
}

/// Reuse a normalized root when deriving keys for one traversal.
pub struct RelativeRoot(PathBuf);

impl RelativeRoot {
    pub fn new(root: &Path) -> Self {
        Self(strip_verbatim(&canonical(root)))
    }

    pub fn relative_to(&self, path: &Path) -> Option<PathBuf> {
        relative_to_normalized(&self.0, path)
    }
}

fn relative_to_normalized(root_raw: &Path, path: &Path) -> Option<PathBuf> {
    let path_raw = strip_verbatim(&path.canonicalize().unwrap_or_else(|_| path.to_path_buf()));

    if let Ok(rel) = path_raw.strip_prefix(root_raw) {
        return Some(rel.to_path_buf());
    }

    // Case-insensitive / mixed-separator fallback (Windows + odd git paths).
    let root_comps: Vec<_> = root_raw.components().collect();
    let path_comps: Vec<_> = path_raw.components().collect();
    if path_comps.len() < root_comps.len() {
        return None;
    }
    for (root_c, path_c) in root_comps.iter().zip(path_comps.iter()) {
        let a = root_c.as_os_str();
        let b = path_c.as_os_str();
        #[cfg(windows)]
        {
            if !a.eq_ignore_ascii_case(b) {
                return None;
            }
        }
        #[cfg(not(windows))]
        {
            if a != b {
                return None;
            }
        }
    }
    let mut out = PathBuf::new();
    for component in path_comps.into_iter().skip(root_comps.len()) {
        out.push(component);
    }
    Some(out)
}

/// Stable 16-hex-char project identity: `sha256(identity_root)[:8 bytes]`.
///
/// The same repo addressed from any subdirectory resolves to the same root and
/// thus the same key (the fix). Two clones at different paths get distinct keys
/// (correct — independent stores).
///
/// The hash input is [`compare_key`] of the resolved root, not the raw
/// `resolve_root` bytes. On Windows, `git rev-parse --show-toplevel` returns
/// `C:/…` while a marker walk (no git on `PATH`) returns `canonicalize`'s
/// `\\?\C:\…`. Hashing those spellings verbatim split one repo across two
/// stores — MCP children that could not see `git` rebuilt an empty graph
/// beside the shell's populated one.
pub fn project_key(root: &Path) -> String {
    key_for_resolved_root(&resolve_root(root))
}

fn key_for_resolved_root(root: &Path) -> String {
    use sha2::{Digest, Sha256};
    let identity = compare_key(&canonical(root));
    let mut hasher = Sha256::new();
    hasher.update(identity.to_string_lossy().as_bytes());
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
    try_project_dir(root)
        .unwrap_or_else(|error| panic!("invalid Aden storage configuration: {error}"))
}

/// Project-local storage policy. Environment overrides retain higher priority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    #[serde(default)]
    pub portable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dir: Option<PathBuf>,
}

pub fn storage_config_file(root: &Path) -> PathBuf {
    resolve_root(root).join(".aden/storage.toml")
}

fn storage_error(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

/// Read and strictly validate policy, including when an environment override wins.
pub fn read_storage_config(root: &Path) -> std::io::Result<Option<StorageConfig>> {
    Ok(read_storage_config_resolved(&resolve_root(root))?.map(|(config, _)| config))
}

fn read_storage_config_resolved(root: &Path) -> std::io::Result<Option<(StorageConfig, PathBuf)>> {
    use std::io::Read;
    const LIMIT: u64 = 65536;
    let path = root.join(".aden/storage.toml");
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut body = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut body)?;
    if body.len() as u64 > LIMIT {
        return Err(storage_error("storage configuration exceeds 64 KiB"));
    }
    let body = std::str::from_utf8(&body).map_err(|error| storage_error(error.to_string()))?;
    let config: StorageConfig = toml::from_str(body)
        .map_err(|error| storage_error(format!("invalid {}: {error}", path.display())))?;
    let dir = validate_storage_config_resolved(root, &config)?;
    Ok(Some((config, dir)))
}

/// Resolve the full rebuildable project directory without creating anything.
/// Custom bases must be absolute and outside the source tree.
pub fn validate_storage_config(root: &Path, config: &StorageConfig) -> std::io::Result<PathBuf> {
    let root = resolve_root(root);
    validate_storage_config_resolved(&root, config)
}

fn validate_storage_config_resolved(
    root: &Path,
    config: &StorageConfig,
) -> std::io::Result<PathBuf> {
    let dir = match (config.portable, &config.data_dir) {
        (true, None) => {
            if root.parent().is_none()
                || dirs::home_dir()
                    .is_some_and(|home| compare_key(&canonical(&home)) == compare_key(root))
            {
                return Err(storage_error(
                    "portable storage requires a project below the filesystem root and home directory",
                ));
            }
            for path in [root.join(".aden"), root.join(".aden/data")] {
                if std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink())
                {
                    return Err(storage_error(
                        "portable storage must not use symbolic links",
                    ));
                }
            }
            root.join(".aden/data")
        }
        (false, Some(base)) => {
            if !base.is_absolute()
                || base
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(storage_error(
                    "storage data_dir must be an absolute path without parent traversal",
                ));
            }
            let base = canonical_existing_ancestor(base)?;
            if base.parent().is_none()
                || dirs::home_dir()
                    .is_some_and(|home| compare_key(&canonical(&home)) == compare_key(&base))
            {
                return Err(storage_error(
                    "storage data_dir must not be a filesystem root or home directory",
                ));
            }
            if is_under(root, &base) {
                return Err(storage_error(
                    "custom storage data_dir must be outside the project; use portable = true for project-local storage",
                ));
            }
            base.join("projects").join(key_for_resolved_root(root))
        }
        _ => {
            return Err(storage_error(
                "storage configuration requires exactly one of portable = true or data_dir",
            ));
        }
    };
    if dir.exists() && !dir.is_dir() {
        return Err(storage_error(
            "configured project data path is not a directory",
        ));
    }
    // A portable checkout may move between hosts and paths. Its in-tree directory
    // is the identity; external keyed directories must retain their root owner.
    if !config.portable {
        check_meta_owner(root, &dir)?;
    }
    Ok(dir)
}

fn canonical_existing_ancestor(path: &Path) -> std::io::Result<PathBuf> {
    let mut ancestor = path;
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        suffix.push(
            ancestor
                .file_name()
                .ok_or_else(|| storage_error("invalid storage path"))?
                .to_os_string(),
        );
        ancestor = ancestor
            .parent()
            .ok_or_else(|| storage_error("invalid storage ancestor"))?;
    }
    if !ancestor.is_dir() {
        return Err(storage_error("storage path ancestor is not a directory"));
    }
    let mut result = ancestor.canonicalize()?;
    for part in suffix.into_iter().rev() {
        result.push(part);
    }
    Ok(result)
}

fn check_meta_owner(root: &Path, dir: &Path) -> std::io::Result<()> {
    let file = dir.join("meta.json");
    let body = match std::fs::read(&file) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let meta: Meta = serde_json::from_slice(&body)
        .map_err(|error| storage_error(format!("invalid {}: {error}", file.display())))?;
    if compare_key(&canonical(Path::new(&meta.root))) != compare_key(&canonical(root)) {
        return Err(storage_error(format!(
            "project data directory {} belongs to another project",
            dir.display()
        )));
    }
    Ok(())
}

/// Checked entry point for command preflight. Never falls back on malformed policy.
pub fn try_project_dir(root: &Path) -> std::io::Result<PathBuf> {
    Ok(project_storage_resolved(&resolve_root(root))?.0)
}

/// Resolve the data directory and whether it belongs to a portable checkout.
fn project_storage_resolved(root: &Path) -> std::io::Result<(PathBuf, bool)> {
    if let Some((config, dir)) = read_storage_config_resolved(root)?
        && std::env::var_os("ADEN_DATA_DIR").is_none()
    {
        return Ok((dir, config.portable));
    }
    Ok((project_dir_for_key(&key_for_resolved_root(root)), false))
}

/// Persist policy only. Existing data and durable intent are never migrated or removed.
/// Reset removes only storage.toml and can recover from malformed policy.
pub fn write_storage_config(root: &Path, config: Option<&StorageConfig>) -> std::io::Result<()> {
    let root = resolve_root(root);
    let file = root.join(".aden/storage.toml");
    let Some(config) = config else {
        return match std::fs::remove_file(file) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            result => result,
        };
    };
    validate_storage_config_resolved(&root, config)?;
    let body = toml::to_string_pretty(config).map_err(|error| storage_error(error.to_string()))?;
    let parent = file.parent().expect("storage config has a parent");
    std::fs::create_dir_all(parent)?;
    atomic_publish(&file, body.as_bytes())
}

fn atomic_publish(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = file.with_extension(format!(
        "pending.{}.{timestamp}.{sequence}",
        std::process::id()
    ));
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let result = (|| {
        output.write_all(bytes)?;
        output.sync_all()?;
        drop(output);
        std::fs::rename(&temp, file)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

fn project_dir_for_key(key: &str) -> PathBuf {
    data_root().join("projects").join(key)
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

/// Advisory writer lock beside the fjall store (`store.lock` for `store/`).
pub fn store_lock_file(root: &Path) -> PathBuf {
    store_dir(root).with_extension("lock")
}

/// Atomic read snapshot published at the end of every successful `gen`
/// (ADR-011). Readers load this file instead of opening the live fjall store
/// when it is present and fresh, so concurrent agents do not contend on fjall's
/// process-level database lock.
pub fn graph_snapshot_file(root: &Path) -> PathBuf {
    project_dir(root).join("graph.snapshot")
}

/// Heal drift-scan cache file.
pub fn scan_cache_file(root: &Path) -> PathBuf {
    project_dir(root).join("scan-cache.json")
}

/// Content-addressed dense-embedding cache (hybrid retrieval).
///
/// Deliberately a sibling of `cache/` (which `gen` wipes on every run) rather
/// than inside it: embeddings are expensive (bge inference) and content-keyed, so
/// they must survive an index rebuild. Cleared only on `regen`.
pub fn embeddings_cache_file(root: &Path) -> PathBuf {
    project_dir(root).join("embeddings-cache.json")
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
    if central.exists()
        || storage_config_file(root).exists()
        || std::env::var_os("ADEN_STORE").is_some()
    {
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
    let root = resolve_root(root);
    let key = key_for_resolved_root(&root);
    let (dir, portable) = project_storage_resolved(&root)?;
    if !portable {
        check_meta_owner(&root, &dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    let meta = Meta {
        root: root.display().to_string(),
        key,
    };
    let json = serde_json::to_string_pretty(&meta)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    atomic_publish(&dir.join("meta.json"), json.as_bytes())
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
    let canon = resolve_root(root);
    project_storage_resolved(&canon).map_err(|error| error.to_string())?;
    if explicit
        || std::env::var_os("ADEN_DATA_DIR").is_some()
        || std::env::var_os("ADEN_STORE").is_some()
    {
        return Ok(());
    }
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

    #[test]
    fn storage_config_size_limit_accepts_exactly_64_kib() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        std::fs::create_dir(root.join(".aden")).unwrap();
        let path = root.join(".aden/storage.toml");
        let mut body = b"portable = true\n#".to_vec();
        body.resize(65536, b' ');
        std::fs::write(&path, &body).unwrap();
        let (config, dir) = read_storage_config_resolved(root).unwrap().unwrap();
        assert!(config.portable);
        assert_eq!(dir, root.join(".aden/data"));
        body.push(b' ');
        std::fs::write(path, body).unwrap();
        let err = read_storage_config_resolved(root).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("64 KiB"));
    }

    #[test]
    fn read_scope_rechecks_root_after_request() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let inner = root.join("inner");
        std::fs::create_dir(&inner).unwrap();
        std::fs::write(root.join("Cargo.toml"), "").unwrap();
        with_read_root_scope(|| {
            assert_eq!(resolve_root(&inner), root);
            std::fs::write(inner.join("Cargo.toml"), "").unwrap();
            assert_eq!(resolve_root(&inner), root);
        });
        assert_eq!(with_read_root_scope(|| resolve_root(&inner)), inner);
    }

    #[test]
    fn read_scope_caches_subdir_and_resolved_root_and_resets_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path().canonicalize().unwrap();
        let project = outer.join("project");
        let inner = project.join("sub").join("inner");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(outer.join("Cargo.toml"), "").unwrap();
        std::fs::write(project.join("Cargo.toml"), "").unwrap();

        with_read_root_scope(|| {
            let resolved = resolve_root(&inner);
            assert_eq!(resolved, project);

            // Mutate markers after the initial subdir resolution: both the
            // subdir and its resolved root must hit the in-scope cache.
            std::fs::remove_file(project.join("Cargo.toml")).unwrap();
            std::fs::write(inner.join("Cargo.toml"), "").unwrap();
            assert_eq!(resolve_root(&resolved), project);
            assert_eq!(resolve_root(&inner), project);

            READ_ROOT.with_borrow(|state| {
                let entries = state.as_ref().expect("active read-root scope");
                assert!(entries.iter().any(|(k, v)| k == &inner && v == &project));
                assert!(entries.iter().any(|(k, v)| k == &project && v == &project));
                assert!(entries.len() <= READ_ROOT_CACHE_LIMIT);
            });
        });

        // Scope dropped: cache is cleared and fresh discovery observes disk state.
        READ_ROOT.with_borrow(|state| assert!(state.is_none()));
        assert_eq!(resolve_root(&project), outer);
        assert_eq!(resolve_root(&inner), inner);
    }

    #[test]
    fn git_marker_probe_defers_candidates_and_uncertain_paths_to_git() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert!(!could_have_git_marker(root));

        let git = root.join(".git");
        std::fs::create_dir(&git).unwrap();
        assert!(could_have_git_marker(root));
        std::fs::remove_dir(&git).unwrap();
        std::fs::write(&git, "gitdir: elsewhere\n").unwrap();
        assert!(could_have_git_marker(root));
        std::fs::remove_file(&git).unwrap();

        let head = root.join("HEAD");
        std::fs::write(&head, "ref: refs/heads/main\n").unwrap();
        assert!(could_have_git_marker(root));
        assert!(could_have_git_marker(&root.join("invalid\0path")));
    }

    #[test]
    fn atomic_metadata_publication_preserves_link_target() {
        let fixture = tempfile::tempdir().unwrap();
        let target = fixture.path().join("retained.json");
        let metadata = fixture.path().join("meta.json");
        std::fs::write(&target, br#"{"retained":true}"#).unwrap();
        std::fs::hard_link(&target, &metadata).unwrap();
        atomic_publish(&metadata, br#"{"root":"new","key":"new"}"#).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), br#"{"retained":true}"#);
        assert_eq!(read_meta(fixture.path()).unwrap().root, "new");
    }

    #[test]
    fn concurrent_metadata_readers_never_observe_partial_json() {
        let fixture = tempfile::tempdir().unwrap();
        let metadata = fixture.path().join("meta.json");
        atomic_publish(&metadata, br#"{"root":"a","key":"a"}"#).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let writer_barrier = barrier.clone();
        let writer_path = metadata.clone();
        let writer = std::thread::spawn(move || {
            writer_barrier.wait();
            for index in 0..64 {
                let meta = Meta {
                    root: format!("root-{index}"),
                    key: format!("key-{index}"),
                };
                atomic_publish(&writer_path, &serde_json::to_vec(&meta).unwrap()).unwrap();
            }
        });
        barrier.wait();
        for _ in 0..512 {
            let body = std::fs::read(&metadata).unwrap();
            let _: Meta = serde_json::from_slice(&body).unwrap();
        }
        writer.join().unwrap();
    }

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
    fn project_key_unifies_git_style_and_verbatim_spellings() {
        // These paths do not exist, so resolve_root cannot prefer git or a
        // marker and returns the input. The key must still treat the three
        // Windows spellings of one directory as one project.
        let git_style = Path::new(r"Z:/aden-does-not-exist-key-test/repo");
        let verbatim = Path::new(r"\\?\Z:\aden-does-not-exist-key-test\repo");
        let backslash = Path::new(r"Z:\aden-does-not-exist-key-test\repo");
        assert_eq!(project_key(git_style), project_key(verbatim));
        assert_eq!(project_key(git_style), project_key(backslash));
        assert_eq!(project_key(git_style).len(), KEY_HEX_LEN);
    }

    #[test]
    fn strip_verbatim_removes_windows_extended_prefix() {
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\C:\Users\me\proj")),
            PathBuf::from(r"C:\Users\me\proj")
        );
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\UNC\server\share\repo")),
            PathBuf::from(r"\\server\share\repo")
        );
        assert_eq!(
            strip_verbatim(Path::new("//?/C:/Users/me/proj")),
            PathBuf::from("C:/Users/me/proj")
        );
        // Non-verbatim paths are unchanged.
        assert_eq!(
            strip_verbatim(Path::new("/home/me/proj")),
            PathBuf::from("/home/me/proj")
        );
    }

    #[test]
    fn compare_key_unifies_verbatim_and_plain_drive_spellings() {
        // Simulate the Windows tree/grep failure: git root is plain, canonicalize
        // is verbatim. Containment must treat them as one hierarchy.
        let root = compare_key(Path::new(r"C:\Users\me\proj"));
        let scope = compare_key(Path::new(r"\\?\C:\Users\me\proj"));
        let nested = compare_key(Path::new(r"\\?\C:\Users\me\proj\crates\cli"));
        let outside = compare_key(Path::new(r"\\?\C:\Users\me\other"));
        assert_eq!(root, scope);
        assert!(nested.starts_with(&root));
        assert!(!outside.starts_with(&root));
    }

    #[test]
    fn relative_to_and_is_under_agree_on_real_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let child = root.join("src").join("lib.rs");
        std::fs::create_dir_all(child.parent().unwrap()).unwrap();
        std::fs::write(&child, b"fn x() {}").unwrap();

        assert!(is_under(root, &child));
        assert!(is_under(root, root));
        let rel = relative_to(root, &child).expect("child is under root");
        assert_eq!(rel, PathBuf::from("src").join("lib.rs"));

        let outsider = tempfile::tempdir().unwrap();
        assert!(!is_under(root, outsider.path()));
        assert!(relative_to(root, outsider.path()).is_none());
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

        // Writing from a nested scope uses the same project identity as the
        // other path helpers and records that identity in the metadata.
        let fixture = tempfile::tempdir().unwrap();
        let project = fixture.path().join("project");
        let scope = project.join("src").join("nested");
        let data = fixture.path().join("data");
        std::fs::create_dir_all(&scope).unwrap();
        std::fs::write(project.join("Makefile"), "").unwrap();
        with_env("ADEN_DATA_DIR", Some(data.to_str().unwrap()), || {
            write_meta(&scope).unwrap();
            let key = project_key(&project);
            let dir = data.join("projects").join(&key);
            assert_eq!(project_dir(&scope), dir);
            let meta = read_meta(&dir).expect("metadata at the project key directory");
            assert_eq!(meta.root, resolve_root(&project).display().to_string());
            assert_eq!(meta.key, key);
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
