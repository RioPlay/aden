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
use crate::ProposeError;
use aden_core::{Document, Table};
use std::fmt::Write;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub enum DriftEvent {
    SignatureMismatch {
        anchor: String,
        current_table: Table,
        proposed_table: Table,
    },
    BrokenReference {
        anchor: String,
        line: String,
        broken_ref: String,
        suggested_ref: String,
    },
    StaleHash {
        anchor: String,
        old_hash: String,
        new_hash: String,
    },
}

pub fn generate_patch(
    target_path: &Path,
    drift: &DriftEvent,
    _document: &Document,
) -> Result<String, ProposeError> {
    let (id, drift_type, anchor, confidence, rationale, current_cell, proposed_cell) = match drift {
        DriftEvent::SignatureMismatch {
            anchor,
            current_table,
            proposed_table,
        } => {
            let id = format!("proposal-sig-{}", anchor);
            let confidence = 0.85;
            let rationale = format!(
                "The signature block for `{}` no longer matches the extracted AST. \
                 The stored contract table is out of date.",
                anchor
            );
            let current = table_to_asciidoc(current_table);
            let proposed = table_to_asciidoc(proposed_table);
            (
                id,
                "SignatureMismatch",
                anchor.clone(),
                confidence,
                rationale,
                current,
                proposed,
            )
        }
        DriftEvent::BrokenReference {
            anchor,
            line,
            broken_ref,
            suggested_ref,
        } => {
            let id = format!("proposal-ref-{}", anchor);
            let confidence = 0.92;
            let rationale = format!(
                "The reference `{}` in `{}` could not be resolved. \
                 Replacing it with `{}` restores the link.",
                broken_ref, anchor, suggested_ref
            );
            let proposed_line = line.replace(broken_ref, suggested_ref);
            (
                id,
                "BrokenReference",
                anchor.clone(),
                confidence,
                rationale,
                line.clone(),
                proposed_line,
            )
        }
        DriftEvent::StaleHash {
            anchor,
            old_hash,
            new_hash,
        } => {
            let id = format!("proposal-hash-{}", anchor);
            let confidence = 0.99;
            let rationale = format!(
                "The stored source hash for `{}` is stale. \
                 The contract should reference the current source hash.",
                anchor
            );
            let current = format!(":source_hash: {}", old_hash);
            let proposed = format!(":source_hash: {}", new_hash);
            (
                id,
                "StaleHash",
                anchor.clone(),
                confidence,
                rationale,
                current,
                proposed,
            )
        }
    };

    let warning = if confidence < 0.9 {
        format!(
            "\nagent-note::WARNING[Confidence is {:.2}; manual review recommended.]\n",
            confidence
        )
    } else {
        String::new()
    };

    let mut patch = String::new();
    writeln!(patch, "[[{}]]", id).unwrap();
    writeln!(patch, "= Proposal: {} for {}", drift_type, anchor).unwrap();
    writeln!(patch).unwrap();
    writeln!(patch, ":status: PENDING_REVIEW").unwrap();
    writeln!(patch, ":confidence: {}", confidence).unwrap();
    writeln!(patch, ":target: {}", target_path.display()).unwrap();
    writeln!(patch, ":drift_type: {}", drift_type).unwrap();
    writeln!(patch).unwrap();
    writeln!(patch, "== Rationale").unwrap();
    writeln!(patch, "{}", rationale).unwrap();
    writeln!(patch).unwrap();
    writeln!(patch, "== Proposed Changes").unwrap();
    writeln!(patch, "[cols=\"1,1\"]").unwrap();
    writeln!(patch, "|===").unwrap();
    writeln!(patch, "|Current |Proposed").unwrap();
    writeln!(patch).unwrap();

    match drift {
        DriftEvent::SignatureMismatch { .. } => {
            writeln!(patch, "a|").unwrap();
            writeln!(patch, "----").unwrap();
            patch.push_str(&current_cell);
            if !current_cell.ends_with('\n') {
                writeln!(patch).unwrap();
            }
            writeln!(patch, "----").unwrap();
            writeln!(patch, "a|").unwrap();
            writeln!(patch, "----").unwrap();
            patch.push_str(&proposed_cell);
            if !proposed_cell.ends_with('\n') {
                writeln!(patch).unwrap();
            }
            writeln!(patch, "----").unwrap();
        }
        _ => {
            writeln!(
                patch,
                "|{} |{}",
                current_cell.trim_end(),
                proposed_cell.trim_end()
            )
            .unwrap();
        }
    }

    writeln!(patch, "|===").unwrap();
    patch.push_str(&warning);

    Ok(patch)
}

fn table_to_asciidoc(table: &Table) -> String {
    let mut s = String::new();
    writeln!(s, "|===").unwrap();
    if !table.headers.is_empty() {
        for h in &table.headers {
            write!(s, "|{}", h).unwrap();
        }
        writeln!(s).unwrap();
    }
    for row in &table.rows {
        for cell in row {
            write!(s, "|{}", cell).unwrap();
        }
        writeln!(s).unwrap();
    }
    writeln!(s, "|===").unwrap();
    s
}
