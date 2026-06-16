// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Plain text extractor for Aden.
//!
//! Treats plain text files as single documents.

use crate::extractor::{
    build_code_attributes, infer_project_name, infer_project_root, make_anchor,
    project_relative_file,
};
use aden_core::{Block, Document, NodeType, Result};
use std::path::Path;

pub struct PlainTextExtractor;

impl Default for PlainTextExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl PlainTextExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl crate::extractor::LanguageExtractor for PlainTextExtractor {
    fn language_id(&self) -> &'static str {
        "plaintext"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &["txt", "text"]
    }

    fn extract_documents(&self, source: &str, path: &Path) -> Result<Vec<Document>> {
        let crate_name = infer_project_name(path);
        let project_root = infer_project_root(path);
        let file_name = project_relative_file(path, &project_root);

        // Normalize CRLF→LF first so a Windows file's `\r\n\r\n` paragraph break
        // is seen as a blank line, and so line/byte offsets are computed against
        // one consistent representation.
        let normalized = source.replace("\r\n", "\n");

        // One `Note` node per blank-line-delimited paragraph, each carrying its
        // own line span. A notepad-style file of distinct thoughts is then
        // independently retrievable, citable, and linkable — not collapsed into
        // a single blob. Paragraph anchors are ordinal (`#p0`, `#p1`, …); a
        // content-addressed scheme is the durability refinement to add before
        // prose↔prose linking, when anchor stability first matters.
        let docs: Vec<Document> = split_paragraphs(&normalized)
            .into_iter()
            .enumerate()
            .map(|(i, para)| {
                let span = aden_core::SourceSpan {
                    file: path.to_string_lossy().to_string(),
                    start_line: para.start_line,
                    end_line: para.end_line,
                    start_byte: para.start_byte,
                    end_byte: para.end_byte,
                };
                let attrs = build_code_attributes(source, "note", Some(path), Some(&span));
                Document {
                    anchor: make_anchor(&crate_name, &file_name, &format!("p{i}")),
                    node_type: NodeType::Note,
                    attributes: attrs,
                    blocks: vec![Block::Paragraph(para.text)],
                    source_span: Some(span),
                    metadata: None,
                    confidence: 0.6,
                }
            })
            .collect();

        if !docs.is_empty() {
            return Ok(docs);
        }

        // Empty / whitespace-only file: keep one placeholder node so the file
        // still appears in the graph and stays locatable.
        let span = crate::extractor::whole_file_span(source, path);
        let attrs = build_code_attributes(source, "note", Some(path), Some(&span));
        Ok(vec![Document {
            anchor: make_anchor(&crate_name, &file_name, "p0"),
            node_type: NodeType::Note,
            attributes: attrs,
            blocks: vec![Block::Paragraph(format!("Plain text file: {file_name}"))],
            source_span: Some(span),
            metadata: None,
            confidence: 0.6,
        }])
    }
}

/// A blank-line-delimited paragraph with its 1-based line span and byte range
/// (offsets into the CRLF-normalized source).
struct Paragraph {
    start_line: usize,
    end_line: usize,
    start_byte: usize,
    end_byte: usize,
    text: String,
}

/// Split normalized text into paragraphs separated by blank lines, tracking each
/// paragraph's 1-based line span and byte range. Blank separator lines and any
/// trailing newline are excluded from the spans.
fn split_paragraphs(normalized: &str) -> Vec<Paragraph> {
    let mut out: Vec<Paragraph> = Vec::new();
    let mut byte_pos = 0usize;
    let mut start: Option<(usize, usize)> = None; // (start_line, start_byte)
    let mut end_line = 0usize;
    let mut end_byte = 0usize;
    let mut lines: Vec<&str> = Vec::new();

    for (idx, line) in normalized.split('\n').enumerate() {
        let line_start = byte_pos;
        let line_end = byte_pos + line.len();
        if line.trim().is_empty() {
            if let Some((s_line, s_byte)) = start.take() {
                out.push(Paragraph {
                    start_line: s_line,
                    end_line,
                    start_byte: s_byte,
                    end_byte,
                    text: lines.join("\n"),
                });
                lines.clear();
            }
        } else {
            if start.is_none() {
                start = Some((idx + 1, line_start));
            }
            end_line = idx + 1;
            end_byte = line_end;
            lines.push(line);
        }
        byte_pos = line_end + 1; // +1 for the '\n' that `split` consumed
    }
    if let Some((s_line, s_byte)) = start {
        out.push(Paragraph {
            start_line: s_line,
            end_line,
            start_byte: s_byte,
            end_byte,
            text: lines.join("\n"),
        });
    }
    out
}
