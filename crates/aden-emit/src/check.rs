// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// All rights reserved.
//
// Aden: A Dense Referential Context Compiler
// Original author and maintainer: RioPlay
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
use std::collections::HashSet;
use std::fmt::Write;

/// Find all `<<reference>>` patterns in a line and return the inner reference texts.
/// Ignores anything inside backticks (`) to avoid flagging literal examples.
pub fn find_refs(line: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut in_backticks = false;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'`' {
            in_backticks = !in_backticks;
            i += 1;
            continue;
        }
        if in_backticks {
            i += 1;
            continue;
        }
        if c == b'<' && i + 1 < bytes.len() && bytes[i + 1] == b'<' {
            if let Some(end) = line[i + 2..].find(">>") {
                let abs_end = i + 2 + end;
                let inner = &line[i + 2..abs_end];
                let anchor_name = inner.split(',').next().unwrap_or(inner).trim();
                if !anchor_name.is_empty() && !anchor_name.contains(' ') {
                    refs.push(anchor_name.to_string());
                }
                i = abs_end + 2;
                continue;
            }
        }
        i += 1;
    }
    refs
}

/// Collect all `[[anchor]]` declarations from emitted text.
pub fn collect_anchors(output: &str) -> HashSet<String> {
    let mut anchors = HashSet::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("[[") && trimmed.ends_with("]]") {
            let inner = &trimmed[2..trimmed.len() - 2];
            anchors.insert(inner.to_string());
        }
    }
    anchors
}

/// Verify that `output` contains no unresolved `<<refs>>`.
pub fn verify(output: &str) -> Result<(), String> {
    let anchors = collect_anchors(output);
    let mut issues = Vec::new();
    for line in output.lines() {
        for r in find_refs(line) {
            if !anchors.contains(&r) {
                issues.push(format!("Unresolved reference: <<{r}>>"));
            }
        }
    }
    if issues.is_empty() {
        Ok(())
    } else {
        let mut msg = String::new();
        for issue in &issues {
            writeln!(msg, "{issue}").unwrap();
        }
        Err(msg)
    }
}
