// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Direct source evidence. This path deliberately does not open or refresh a graph.

use super::AgentCliError;
use serde_json::json;
use std::io::Read;
use std::path::{Component, Path};

const MAX_SOURCE_BYTES: u64 = 16 * 1024 * 1024;
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
    if file.as_os_str().is_empty()
        || file
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err(read_error(
            "source_boundary",
            "File must be project-relative without parent traversal",
        )
        .into());
    }
    #[cfg(windows)]
    if file.to_string_lossy().contains(':') {
        return Err(read_error(
            "source_boundary",
            "Alternate data streams are not source files",
        )
        .into());
    }
    if !path.is_dir() {
        return Err(read_error("invalid_read_root", "Project directory is unavailable").into());
    }
    // The supplied directory is the exact read boundary. Expanding a nested
    // workspace to its enclosing Git root would grant access outside that root.
    let root = std::fs::canonicalize(path)?;
    let candidate = root.join(file);
    let real_path = std::fs::canonicalize(&candidate)
        .map_err(|_| read_error("source_unavailable", "Source file is unavailable"))?;
    if !real_path.starts_with(&root) {
        return Err(read_error("source_boundary", "Source path escapes the project").into());
    }
    // Access scope and policy scope are separate: selecting a subdirectory
    // must not bypass the enclosing project's exclusions.
    let policy_root = std::fs::canonicalize(crate::util::find_project_root(&root))?;
    let mut policy_roots = vec![root.clone()];
    if policy_root != root {
        policy_roots.push(policy_root);
    }
    let excluded = policy_roots.iter().any(|base| {
        let filter = aden_core::filter::AdenFilter::from_directory(base);
        [&candidate, &real_path].iter().any(|target| {
            target.strip_prefix(base).is_ok_and(|relative| {
                relative
                    .ancestors()
                    .any(|part| !part.as_os_str().is_empty() && filter.should_skip(part))
            })
        })
    });
    if excluded {
        return Err(read_error(
            "source_excluded",
            "Source file is excluded by project policy",
        )
        .into());
    }
    // Reject devices/FIFOs before opening; opening a FIFO can block indefinitely.
    let metadata = std::fs::metadata(&real_path)?;
    if !metadata.is_file() || metadata.len() > MAX_SOURCE_BYTES {
        return Err(read_error(
            "source_size_limit",
            "Source must be a regular file of at most 16 MiB",
        )
        .into());
    }
    let source = std::fs::File::open(&real_path)?;
    let before = source.metadata()?;
    if !before.is_file() || before.len() > MAX_SOURCE_BYTES {
        return Err(read_error(
            "source_size_limit",
            "Source must be a regular file of at most 16 MiB",
        )
        .into());
    }
    let mut bytes = Vec::new();
    (&source)
        .take(MAX_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        return Err(read_error("source_size_limit", "Source exceeds the 16 MiB read limit").into());
    }
    let after = source.metadata()?;
    if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
        return Err(read_error("source_changed", "Source changed during the read; retry").into());
    }
    let content = std::str::from_utf8(&bytes)
        .map_err(|_| read_error("source_encoding", "Source is not valid UTF-8"))?;
    if content.contains('\0') {
        return Err(read_error("source_encoding", "Source contains binary NUL bytes").into());
    }
    // Change detection, not an authenticity or cryptographic security assertion.
    // Hash literal bytes so CRLF/LF differences remain observable.
    let source_hash = aden_core::stable_hash(&bytes);
    if expected_hash
        .as_deref()
        .is_some_and(|expected| expected != source_hash)
    {
        return Err(read_error("source_changed", "Source does not match expected_hash").into());
    }
    if let Some(offset) = cursor {
        if expected_hash.is_none() || offset > content.len() || !content.is_char_boundary(offset) {
            return Err(read_error(
                "invalid_source_cursor",
                "Byte continuation requires expected_hash and a valid UTF-8 boundary",
            )
            .into());
        }
        start_line = content[..offset]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            + 1;
    }
    let end_line = end_line.unwrap_or_else(|| start_line.saturating_add(199));
    if end_line < start_line {
        return Err(read_error(
            "invalid_read_range",
            "End line precedes the byte continuation",
        )
        .into());
    }
    let total_lines = content.split_inclusive('\n').count();
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
    let selected_end = end_line.min(total_lines);
    let mut text = String::new();
    let mut actual_end = None;
    let mut truncated = false;
    let mut last_line_complete = true;
    let mut start_byte = 0;
    for (index, mut line) in content.split_inclusive('\n').enumerate() {
        let number = index + 1;
        if number < start_line {
            start_byte += line.len();
            continue;
        }
        if number > selected_end {
            break;
        }
        if number == start_line
            && let Some(offset) = cursor
        {
            let skip = offset - start_byte;
            line = &line[skip..];
            start_byte = offset;
        }
        let available = max_bytes - text.len();
        if line.len() > available {
            let mut cut = available;
            while !line.is_char_boundary(cut) {
                cut -= 1;
            }
            if cut == 0 && text.is_empty() {
                return Err(read_error(
                    "invalid_read_range",
                    "max_bytes cannot fit the next UTF-8 character; increase max_bytes to at least 4",
                )
                .into());
            }
            if cut > 0 {
                text.push_str(&line[..cut]);
                actual_end = Some(number);
                last_line_complete = false;
            }
            truncated = true;
            break;
        }
        text.push_str(line);
        actual_end = Some(number);
    }
    let file_key = crate::util::project_relative_key(&root, &real_path);
    let report = json!({
        "schema_version": 1,
        "result_state": if truncated { "truncated" } else { "complete" },
        "root": aden_paths::strip_verbatim(&root).to_string_lossy(),
        "observation": "file_bytes_read",
        "truncated": truncated,
        "next_byte": truncated.then_some(start_byte + text.len()),
        "continuation": truncated.then(|| json!({
            "file": file.to_string_lossy(),
            "path": aden_paths::strip_verbatim(&root).to_string_lossy(),
            "start_byte": start_byte + text.len(),
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
            "file_bytes": bytes.len(),
            "total_lines": total_lines,
            "requested_start_line": start_line,
            "requested_end_line": end_line,
            "start_line": actual_end.map(|_| start_line),
            "end_line": actual_end,
            "start_byte": start_byte,
            "end_byte": start_byte + text.len(),
            "returned_bytes": text.len(),
            "last_line_complete": last_line_complete,
            "text": text,
        },
    });
    if json_output {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        print!("{text}");
        if truncated {
            eprintln!(
                "Source excerpt clipped; continue with --start-byte {} --end-line {end_line} --expected-hash {source_hash}.",
                start_byte + text.len()
            );
        }
    }
    Ok(())
}
