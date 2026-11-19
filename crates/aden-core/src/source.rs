// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Bounded project source access for direct reads and graph-backed evidence.

use crate::filter::{AdenFilter, content_has_high_confidence_secret, is_secret_path};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

/// Maximum allowed size of a stored source file, in bytes.
pub const STORED_SOURCE_MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_SOURCE_BYTES: u64 = STORED_SOURCE_MAX_BYTES as u64;

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("{message}")]
    Rejected {
        code: &'static str,
        message: &'static str,
    },
}

fn reject(code: &'static str, message: &'static str) -> SourceError {
    SourceError::Rejected { code, message }
}

/// Read an explicitly named project-relative file. Returns the canonical
/// access boundary, canonical source path, and text with literal line endings.
pub fn read_direct_source(
    root: &Path,
    file: &Path,
) -> Result<(PathBuf, PathBuf, String), SourceError> {
    validate_relative_file(file)?;
    if !root.is_dir() {
        return Err(reject(
            "invalid_read_root",
            "Project directory is unavailable",
        ));
    }
    let reader = ProjectSourceReader::new(root.canonicalize()?)?;
    let (real_path, content) = reader.read(file, false)?;
    Ok((reader.root, real_path, content))
}

/// A request-scoped reader for stored source paths. The caller's root bounds
/// all reads. The enclosing project can supply additional exclusions.
pub struct StoredSourceReader {
    reader: ProjectSourceReader,
    canonical_spelling: PathBuf,
    supplied_spelling: PathBuf,
}

impl StoredSourceReader {
    pub fn new(root: &Path) -> io::Result<Self> {
        let canonical = root.canonicalize()?;
        let canonical_spelling = aden_paths::strip_verbatim(&canonical);
        let supplied_spelling = aden_paths::strip_verbatim(&std::path::absolute(root)?);
        Ok(Self {
            reader: ProjectSourceReader::new(canonical)?,
            canonical_spelling,
            supplied_spelling,
        })
    }

    /// Read a relative or legacy absolute path within the caller's root.
    /// Stored-source access also rejects likely secret names and contents.
    pub fn read(&self, file: &Path) -> Result<String, SourceError> {
        let source = aden_paths::strip_verbatim(file);
        let relative = if source.is_absolute() {
            // Keep the suffix lexical so policy also checks the names of links.
            relative_spelling(&source, &self.canonical_spelling)
                .or_else(|| relative_spelling(&source, &self.supplied_spelling))
                .ok_or_else(|| {
                    reject("source_boundary", "Stored source path escapes the project")
                })?
        } else {
            source.as_path()
        };
        validate_relative_file(relative)?;
        self.reader.read(relative, true).map(|(_, content)| content)
    }
}

struct ProjectSourceReader {
    root: PathBuf,
    filter: AdenFilter,
    project_policy: Option<(PathBuf, AdenFilter)>,
}

impl ProjectSourceReader {
    fn new(root: PathBuf) -> io::Result<Self> {
        let policy_root = aden_paths::resolve_root(&root).canonicalize()?;
        let project_policy = (policy_root != root).then(|| {
            let filter = AdenFilter::from_directory(&policy_root);
            (policy_root, filter)
        });
        let filter = AdenFilter::from_directory(&root);
        Ok(Self {
            root,
            filter,
            project_policy,
        })
    }

    fn read(
        &self,
        relative: &Path,
        reject_secrets: bool,
    ) -> Result<(PathBuf, String), SourceError> {
        let candidate = self.root.join(relative);
        let real_path = candidate
            .canonicalize()
            .map_err(|_| reject("source_unavailable", "Source file is unavailable"))?;
        if !real_path.starts_with(&self.root) {
            return Err(reject("source_boundary", "Source path escapes the project"));
        }
        let is_excluded = |base: &Path, filter: &AdenFilter| {
            [&candidate, &real_path].iter().any(|target| {
                target.strip_prefix(base).is_ok_and(|relative| {
                    (reject_secrets && is_secret_path(relative))
                        || relative
                            .ancestors()
                            .any(|part| !part.as_os_str().is_empty() && filter.should_skip(part))
                })
            })
        };
        if is_excluded(&self.root, &self.filter)
            || self
                .project_policy
                .as_ref()
                .is_some_and(|(base, filter)| is_excluded(base, filter))
        {
            return Err(excluded());
        }
        let content = read_text(&real_path)?;
        if reject_secrets && content_has_high_confidence_secret(&content) {
            return Err(excluded());
        }
        Ok((real_path, content))
    }
}

fn excluded() -> SourceError {
    reject(
        "source_excluded",
        "Source file is excluded by project policy",
    )
}

fn validate_relative_file(file: &Path) -> Result<(), SourceError> {
    if file.as_os_str().is_empty()
        || file
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err(reject(
            "source_boundary",
            "File must be project-relative without parent traversal",
        ));
    }
    #[cfg(windows)]
    if file.to_string_lossy().contains(':') {
        return Err(reject(
            "source_boundary",
            "Alternate data streams are not source files",
        ));
    }
    Ok(())
}

fn relative_spelling<'a>(source: &'a Path, root: &Path) -> Option<&'a Path> {
    #[cfg(not(windows))]
    return source.strip_prefix(root).ok();
    #[cfg(windows)]
    {
        let mut parts = source.components();
        for part in root.components() {
            if !parts
                .next()?
                .as_os_str()
                .eq_ignore_ascii_case(part.as_os_str())
            {
                return None;
            }
        }
        Some(parts.as_path())
    }
}

fn read_text(real_path: &Path) -> Result<String, SourceError> {
    let bounded_regular_file = |metadata: std::fs::Metadata| {
        if !metadata.is_file() || metadata.len() > MAX_SOURCE_BYTES {
            return Err(reject(
                "source_size_limit",
                "Source must be a regular file of at most 16 MiB",
            ));
        }
        Ok(metadata)
    };
    // Reject devices and FIFOs before opening, which could block indefinitely.
    bounded_regular_file(real_path.metadata()?)?;
    let source = std::fs::File::open(real_path)?;
    let before = bounded_regular_file(source.metadata()?)?;
    let mut bytes = Vec::new();
    (&source)
        .take(MAX_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        return Err(reject(
            "source_size_limit",
            "Source exceeds the 16 MiB read limit",
        ));
    }
    let after = source.metadata()?;
    if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
        return Err(reject(
            "source_changed",
            "Source changed during the read; retry",
        ));
    }
    let content = String::from_utf8(bytes)
        .map_err(|_| reject("source_encoding", "Source is not valid UTF-8"))?;
    if content.contains('\0') {
        return Err(reject(
            "source_encoding",
            "Source contains binary NUL bytes",
        ));
    }
    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_reader_confines_repeated_reads_to_the_requested_subdirectory() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path().join("nested");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(project.path().join(".git")).unwrap();
        std::fs::write(project.path().join(".adenignore"), "nested/hidden.rs\n").unwrap();
        std::fs::write(project.path().join("outside.rs"), "outside").unwrap();
        std::fs::write(root.join("hidden.rs"), "excluded").unwrap();
        let file = root.join("main.rs");
        std::fs::write(&file, "// café\r\nfn main() {}\r\n").unwrap();

        let reader = StoredSourceReader::new(&root).unwrap();
        assert_eq!(
            reader.read(Path::new("main.rs")).unwrap(),
            "// café\r\nfn main() {}\r\n"
        );
        assert_eq!(reader.read(&file).unwrap(), "// café\r\nfn main() {}\r\n");
        assert!(matches!(
            reader.read(Path::new("hidden.rs")),
            Err(SourceError::Rejected {
                code: "source_excluded",
                ..
            })
        ));
        assert!(matches!(
            reader.read(&project.path().join("outside.rs")),
            Err(SourceError::Rejected {
                code: "source_boundary",
                ..
            })
        ));

        std::fs::File::options()
            .write(true)
            .open(file)
            .unwrap()
            .set_len(MAX_SOURCE_BYTES + 1)
            .unwrap();
        assert!(matches!(
            reader.read(Path::new("main.rs")),
            Err(SourceError::Rejected {
                code: "source_size_limit",
                ..
            })
        ));
    }
}
