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

        let anchor = make_anchor(&crate_name, &file_name, "document");
        let attrs = build_code_attributes(source, "document", Some(path), None);

        // Normalize CRLF→LF first: a Windows file's `\r\n\r\n` paragraph break
        // contains no `\n\n` substring, so without this the whole file would
        // collapse into a single paragraph.
        let normalized = source.replace("\r\n", "\n");
        let paragraphs: Vec<Block> = normalized
            .split("\n\n")
            .filter(|p| !p.trim().is_empty())
            .take(10)
            .map(|p| Block::Paragraph(p.trim().to_string()))
            .collect();

        let blocks = if paragraphs.is_empty() {
            vec![Block::Paragraph(format!("Plain text file: {}", file_name))]
        } else {
            paragraphs
        };

        Ok(vec![Document {
            anchor,
            node_type: NodeType::Module,
            attributes: attrs,
            blocks,
            source_span: None,
            metadata: None,
            confidence: 0.6,
        }])
    }
}
