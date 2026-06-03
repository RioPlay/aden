// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! CSV extractor for Aden.
//!
//! Extracts column headers and table structure from CSV files.

use crate::extractor::{build_code_attributes, make_anchor};
use aden_core::{Block, Document, NodeType, Result, Table};
use std::path::Path;

pub struct CsvExtractor;

impl Default for CsvExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl CsvExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl crate::extractor::LanguageExtractor for CsvExtractor {
    fn language_id(&self) -> &'static str {
        "csv"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &["csv", "tsv"]
    }

    fn extract_documents(&self, source: &str, path: &Path) -> Result<Vec<Document>> {
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let crate_name = infer_project_name(path);
        let delimiter = if file_name.ends_with(".tsv") {
            '\t'
        } else {
            ','
        };

        let lines: Vec<&str> = source.lines().collect();
        if lines.is_empty() {
            return Ok(Vec::new());
        }

        let headers: Vec<String> = lines[0]
            .split(delimiter)
            .map(|h| h.trim().trim_matches('"').to_string())
            .collect();

        let rows: Vec<Vec<String>> = lines[1..]
            .iter()
            .take(100)
            .map(|line| {
                line.split(delimiter)
                    .map(|c| c.trim().trim_matches('"').to_string())
                    .collect()
            })
            .collect();

        let anchor = make_anchor(&crate_name, &file_name, "data");
        let attrs = build_code_attributes(source, "table", Some(path), None);

        let table = Table {
            headers: headers.clone(),
            rows,
        };

        let description = if headers.len() > 3 {
            format!(
                "CSV with {} columns: {}",
                headers.len(),
                headers.iter().take(3).map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            )
        } else {
            format!("CSV with columns: {}", headers.join(", "))
        };

        Ok(vec![Document {
            anchor,
            node_type: NodeType::Module,
            attributes: attrs,
            blocks: vec![
                Block::Paragraph(description),
                Block::Table(table),
            ],
            source_span: None,
            metadata: None,
            confidence: 0.7,
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
