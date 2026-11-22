// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Direct source evidence. This path deliberately does not open or refresh a graph.

use super::AgentCliError;
use aden_core::source::{SourceError, StoredSourceReader, read_direct_source};
use memchr::memchr_iter;
use serde_json::json;
use std::path::Path;

// Even JSON's worst-case escaping fits comfortably inside MCP's 64 KiB ceiling.
const MAX_TEXT_BYTES: usize = 8192;

fn read_error(code: &'static str, message: &str) -> AgentCliError {
    AgentCliError::new(
        code,
        message,
        "Use an allowed project-relative UTF-8 file, a valid 1-based line range, and max_bytes between 1 and 8192.",
    )
}

pub struct ReadOptions {
    pub start_line: usize,
    pub end_line: Option<usize>,
    pub all_lines: bool,
    pub max_bytes: usize,
    pub expected_hash: Option<String>,
    pub start_byte: Option<usize>,
}

pub fn cmd_read(
    file: &Path,
    path: &Path,
    options: ReadOptions,
    json_output: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let ReadOptions {
        mut start_line,
        end_line,
        all_lines,
        max_bytes,
        expected_hash,
        start_byte: cursor,
    } = options;
    if start_line == 0
        || end_line.is_some_and(|end| end < start_line)
        || !(1..=MAX_TEXT_BYTES).contains(&max_bytes)
    {
        return Err(read_error("invalid_read_range", "Invalid source range or byte limit").into());
    }
    let (root, real_path, content) = read_direct_source(path, file).map_err(source_error)?;
    // Change detection, not an authenticity or cryptographic security assertion.
    // Hash literal bytes so CRLF/LF differences remain observable.
    let bytes = content.as_bytes();
    let compute_source_hash = || aden_core::stable_hash(bytes);
    let checked_hash = expected_hash.as_ref().map(|_| compute_source_hash());
    if expected_hash != checked_hash {
        return Err(read_error("source_changed", "Source does not match expected_hash").into());
    }
    let line_breaks = if let Some(offset) = cursor {
        if expected_hash.is_none() || offset > content.len() || !content.is_char_boundary(offset) {
            return Err(read_error(
                "invalid_source_cursor",
                "Byte continuation requires expected_hash and a valid UTF-8 boundary",
            )
            .into());
        }
        let (prefix, suffix) = bytes.split_at(offset);
        let preceding_breaks = memchr_iter(b'\n', prefix).count();
        start_line = preceding_breaks + 1;
        preceding_breaks + memchr_iter(b'\n', suffix).count()
    } else {
        memchr_iter(b'\n', bytes).count()
    };
    if end_line.is_some_and(|end| end < start_line) {
        return Err(read_error(
            "invalid_read_range",
            "End line precedes the byte continuation",
        )
        .into());
    }
    let total_lines = line_breaks + usize::from(!bytes.is_empty() && !bytes.ends_with(b"\n"));
    if start_line > total_lines
        && !(total_lines == 0 && start_line == 1)
        && cursor != Some(content.len())
    {
        return Err(read_error(
            "source_range_unavailable",
            "Start line is beyond the end of the file",
        )
        .into());
    }
    let end_line = end_line.unwrap_or_else(|| {
        if all_lines {
            total_lines.max(start_line)
        } else {
            start_line.saturating_add(199)
        }
    });
    let selected_end = end_line.min(total_lines);
    let start_byte = cursor.unwrap_or_else(|| line_start(bytes, start_line));
    let mut end_byte = start_byte;
    let mut actual_end =
        (cursor == Some(content.len()) && start_line <= selected_end).then_some(start_line);
    let mut truncated = false;
    let mut last_line_complete = true;
    for (index, line) in content[start_byte..].split_inclusive('\n').enumerate() {
        let number = start_line + index;
        if number > selected_end {
            break;
        }
        let available = max_bytes - (end_byte - start_byte);
        if line.len() > available {
            let mut cut = available;
            while !line.is_char_boundary(cut) {
                cut -= 1;
            }
            if cut == 0 && end_byte == start_byte {
                return Err(read_error(
                    "invalid_read_range",
                    "max_bytes cannot fit the next UTF-8 character; increase max_bytes to at least 4",
                )
                .into());
            }
            if cut > 0 {
                end_byte += cut;
                actual_end = Some(number);
                last_line_complete = false;
            }
            truncated = true;
            break;
        }
        end_byte += line.len();
        actual_end = Some(number);
    }
    let text = &content[start_byte..end_byte];
    if !json_output {
        print!("{text}");
        if truncated {
            let source_hash = checked_hash.unwrap_or_else(compute_source_hash);
            eprintln!(
                "Source excerpt clipped; use --json for hash-pinned continuation details (next byte {end_byte}, end line {end_line}, expected hash {source_hash}).",
            );
        }
        return Ok(());
    }

    let source_hash = checked_hash.unwrap_or_else(compute_source_hash);
    let file_key = crate::util::project_relative_key(&root, &real_path);
    let root = aden_paths::strip_verbatim(&root);
    let root = root.to_string_lossy();
    let report = json!({
        "schema_version": 1,
        "result_state": if truncated { "truncated" } else { "complete" },
        "root": root,
        "observation": "file_bytes_read",
        "truncated": truncated,
        "next_byte": truncated.then_some(end_byte),
        "continuation": truncated.then(|| json!({
            "file": file.to_string_lossy(),
            "path": root,
            "start_byte": end_byte,
            "end_line": end_line,
            "expected_hash": source_hash,
            "max_bytes": max_bytes,
        })),
        "source": {
            "state": if truncated { "clipped" } else { "complete" },
            "file": file_key,
            "source_hash": source_hash,
            "hash_algorithm": "aden-core-stable-v1",
            "expected_hash_matched": expected_hash.as_ref().map(|_| true),
            "file_bytes": content.len(),
            "total_lines": total_lines,
            "requested_start_line": start_line,
            "requested_end_line": end_line,
            "start_line": actual_end.map(|_| start_line),
            "end_line": actual_end,
            "start_byte": start_byte,
            "end_byte": end_byte,
            "returned_bytes": text.len(),
            "last_line_complete": last_line_complete,
            "text": text,
        },
    });
    crate::read_worker::emit_machine_body(serde_json::to_string(&report)?);
    Ok(())
}

fn line_start(bytes: &[u8], line: usize) -> usize {
    let mut remaining = line - 1;
    if remaining == 0 {
        return 0;
    }

    // Skip blocks using bulk newline counts and locate a single break in the target block.
    const BLOCK_BYTES: usize = 4096;
    for (index, block) in bytes.chunks(BLOCK_BYTES).enumerate() {
        let breaks = memchr_iter(b'\n', block).count();
        if remaining <= breaks {
            let newline = memchr_iter(b'\n', block)
                .nth(remaining - 1)
                .expect("counted newline");
            return index * BLOCK_BYTES + newline + 1;
        }
        remaining -= breaks;
    }
    unreachable!("requested line must be within the source")
}

fn source_error(error: SourceError) -> Box<dyn std::error::Error> {
    match error {
        SourceError::Io(error) => error.into(),
        SourceError::Rejected { code, message } => read_error(code, message).into(),
    }
}

pub(super) fn read_stored_source(
    path: &Path,
    file: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    StoredSourceReader::new(path)?
        .read(file)
        .map_err(source_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_passage_sources_obey_project_scope_and_read_policy() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let doc = root.join("guide.adoc");
        std::fs::write(&doc, "Crème\r\n").unwrap();
        for path in [Path::new("guide.adoc"), doc.as_path()] {
            assert_eq!(read_stored_source(&root, path).unwrap(), "Crème\r\n");
        }
        #[cfg(windows)]
        {
            let alternate = fixture.path().join("PROJECT").join("guide.adoc");
            if alternate.is_file() {
                assert_eq!(read_stored_source(&root, &alternate).unwrap(), "Crème\r\n");
            }
        }
        let outside = fixture.path().join("other.adoc");
        std::fs::write(&outside, "synthetic-private-marker").unwrap();
        std::fs::write(root.join(".env"), "synthetic-private-marker").unwrap();
        std::fs::write(
            root.join("changed.adoc"),
            concat!(
                "-----BEGIN ",
                "PRIVATE ",
                "KEY-----\nsynthetic-private-marker"
            ),
        )
        .unwrap();
        std::fs::write(root.join("binary.adoc"), [0xff]).unwrap();
        for path in [
            Path::new("../other.adoc"),
            outside.as_path(),
            Path::new(".env"),
            Path::new("changed.adoc"),
            Path::new("binary.adoc"),
            Path::new("missing.adoc"),
        ] {
            let err = read_stored_source(&root, path).unwrap_err();
            assert!(!err.to_string().contains("synthetic-private-marker"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let alias = fixture.path().join("root-alias");
            symlink(&root, &alias).unwrap();
            assert_eq!(
                read_stored_source(&alias, &alias.join("guide.adoc")).unwrap(),
                "Crème\r\n"
            );
            symlink(&doc, root.join("allowed.adoc")).unwrap();
            symlink(&outside, root.join("outside.adoc")).unwrap();
            assert_eq!(
                read_stored_source(&root, Path::new("allowed.adoc")).unwrap(),
                "Crème\r\n"
            );
            assert!(read_stored_source(&root, Path::new("outside.adoc")).is_err());
        }
    }
}
