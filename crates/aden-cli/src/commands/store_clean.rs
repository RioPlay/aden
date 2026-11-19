// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Explicit operator cleanup. Never remove the project directory or lock inodes.
use std::io;
use std::path::{Path, PathBuf};

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn redirected(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

/// Reject redirects, including junctions, before measuring or deleting anything.
fn validate_ancestors(path: &Path) -> io::Result<()> {
    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(meta) if redirected(&meta) => {
                return Err(invalid("cleanup refuses symbolic links and reparse points"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn tree_size(path: &Path) -> io::Result<u64> {
    let mut pending = vec![path.to_path_buf()];
    let mut bytes = 0u64;
    while let Some(path) = pending.pop() {
        let meta = std::fs::symlink_metadata(&path)?;
        if redirected(&meta) {
            return Err(invalid("cleanup refuses symbolic links and reparse points"));
        }
        if meta.is_dir() {
            for entry in std::fs::read_dir(path)? {
                pending.push(entry?.path());
            }
        } else if meta.is_file() {
            bytes = bytes.saturating_add(meta.len());
        } else {
            return Err(invalid("cleanup refuses special files"));
        }
    }
    Ok(bytes)
}

fn candidates(directory: &Path, scope: &str) -> io::Result<Vec<PathBuf>> {
    let names: &[&str] = match scope {
        "cache" => &["cache", "scan-cache.json", "embeddings-cache.json"],
        "graph" => &["store", "graph.snapshot", "gen-cache.json"],
        "all" => &[
            "cache",
            "scan-cache.json",
            "embeddings-cache.json",
            "store",
            "graph.snapshot",
            "gen-cache.json",
        ],
        _ => return Err(invalid("cleanup scope must be cache, graph, or all")),
    };
    validate_ancestors(directory)?;
    let mut paths = Vec::new();
    for name in names {
        let path = directory.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                tree_size(&path)?;
                paths.push(path);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(paths)
}

pub fn cmd_store_clean(
    path: &Path,
    scope: &str,
    apply: bool,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !path.is_dir() {
        return Err(invalid("cleanup requires an existing project directory").into());
    }
    if std::env::var_os("ADEN_STORE").is_some() {
        return Err(
            invalid("cleanup refuses ADEN_STORE because it may be shared; unset it first").into(),
        );
    }
    let root = crate::util::find_project_root(path);
    let directory = aden_paths::try_project_dir(&root)?;
    // Environment-keyed layouts also need owner validation before destruction.
    validate_ancestors(&directory)?;
    let meta_path = directory.join("meta.json");
    let meta = match std::fs::symlink_metadata(&meta_path) {
        Ok(metadata) => {
            if redirected(&metadata) || !metadata.is_file() || metadata.len() > 65536 {
                return Err(invalid("cleanup refuses unsafe project metadata").into());
            }
            Some(serde_json::from_slice::<aden_paths::Meta>(&std::fs::read(
                &meta_path,
            )?)?)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let portable = std::env::var_os("ADEN_DATA_DIR").is_none()
        && aden_paths::read_storage_config(&root)?.is_some_and(|config| config.portable);
    if let Some(meta) = meta
        && !portable
        && !(aden_paths::is_under(&root, Path::new(&meta.root))
            && aden_paths::is_under(Path::new(&meta.root), &root))
    {
        return Err(invalid("cleanup directory belongs to another project").into());
    }
    candidates(&directory, scope)?;
    // Preview creates no directories, leases or metadata. Apply shares the launch
    // gate and writer exclusion used by storage configuration, including recovery.
    let _guard = if apply {
        Some(crate::indexer::generation_job::configuration_guard(&root)?)
    } else {
        None
    };
    if aden_paths::try_project_dir(&root)? != directory {
        return Err(invalid("storage configuration changed; retry cleanup").into());
    }
    let paths = candidates(&directory, scope)?;
    let mut bytes = 0u64;
    let mut entries = Vec::new();
    for path in paths {
        let size = tree_size(&path)?;
        if apply {
            // Revalidate the full tree immediately before each removal.
            validate_ancestors(&path)?;
            if std::fs::symlink_metadata(&path)?.is_dir() {
                std::fs::remove_dir_all(&path)?;
            } else {
                std::fs::remove_file(&path)?;
            }
            match std::fs::symlink_metadata(&path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                _ => return Err(io::Error::other("cleanup removal could not be verified").into()),
            }
        }
        bytes = bytes.saturating_add(size);
        entries.push(serde_json::json!({"path": path, "bytes": size}));
    }
    if json {
        println!(
            "{}",
            serde_json::json!({
                "schema_version": 1, "project": root, "scope": scope,
                "applied": apply, "bytes": bytes, "entries": entries,
            })
        );
    } else {
        for entry in entries {
            println!(
                "{} {} ({} bytes)",
                if apply { "removed" } else { "would remove" },
                entry["path"].as_str().unwrap_or_default(),
                entry["bytes"]
            );
        }
        println!(
            "{} {bytes} bytes.{}",
            if apply { "Reclaimed" } else { "Would reclaim" },
            if apply {
                " Graph reads rebuild generated data as needed."
            } else {
                " Use --apply to remove these generated files."
            }
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_allowlist_preserves_intent_locks_and_unknown_files() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        for name in ["store", "cache", "overlays", "store.generation"] {
            std::fs::create_dir(dir.join(name)).unwrap();
            std::fs::write(dir.join(name).join("data"), b"keep or remove").unwrap();
        }
        for name in ["meta.json", "store.lock", "constitution.adoc", "unknown"] {
            std::fs::write(dir.join(name), b"preserve").unwrap();
        }
        let paths = candidates(dir, "all").unwrap();
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&dir.join("store")));
        assert!(paths.contains(&dir.join("cache")));
        assert_eq!(candidates(dir, "cache").unwrap(), vec![dir.join("cache")]);
        assert!(candidates(dir, "../").is_err());
        assert!(
            dir.join("store/data").exists(),
            "preview must not remove files"
        );
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_rejects_redirects_before_any_removal() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), temp.path().join("cache")).unwrap();
        assert!(candidates(temp.path(), "all").is_err());
        assert!(outside.path().exists());
    }
}
