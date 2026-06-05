// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Plain text extractor for Aden.
//!
//! Treats plain text files as single documents.

use crate::extractor::{build_code_attributes, make_anchor};
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
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let crate_name = infer_project_name(path);

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

fn infer_project_name(path: &Path) -> String {
    path.ancestors()
        .find(|p| {
            p.join("Cargo.toml").exists()
                || p.join("package.json").exists()
                || p.join("pyproject.toml").exists()
                || p.join("setup.py").exists()
                || p.join("go.mod").exists()
                || p.join("tsconfig.json").exists()
        })
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}
