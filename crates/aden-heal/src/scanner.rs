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
use crate::drift::DriftEvent;
use crate::HealError;
use aden_core::{Block, Document};
use aden_graph::graph::AdenGraph;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub struct Scanner {
    pub repo_root: PathBuf,
}

impl Scanner {
    pub fn new(repo_root: impl AsRef<Path>) -> Self {
        Self {
            repo_root: repo_root.as_ref().to_path_buf(),
        }
    }

    pub fn scan(&self) -> Result<Vec<DriftEvent>, HealError> {
        let mut events = Vec::new();

        // a. Find and parse all source files
        let mut source_paths = Vec::new();
        self.collect_source_files(&self.repo_root, &mut source_paths)?;

        let mut source_entries: Vec<(PathBuf, Document)> = Vec::new();
        for path in &source_paths {
            if let Ok(content) = std::fs::read_to_string(path) {
                if let Ok(docs) = aden_parse::parse_file(path, &content) {
                    for doc in docs {
                        source_entries.push((path.clone(), doc));
                    }
                }
            }
        }

        let mut anchor_to_source_idx: HashMap<String, usize> = HashMap::new();
        for (i, (_, doc)) in source_entries.iter().enumerate() {
            anchor_to_source_idx.insert(doc.anchor.clone(), i);
        }

        // b. Find all contract files
        let mut contract_paths = Vec::new();
        self.collect_contract_files(&self.repo_root, &mut contract_paths)?;

        let mut contract_entries: Vec<(PathBuf, aden_graph::parser::ParsedDocument)> = Vec::new();
        for path in &contract_paths {
            let parsed = aden_graph::parser::parse_file(path)?;
            contract_entries.push((path.clone(), parsed));
        }

        // Build sets for quick lookups
        let aden_anchors: HashSet<String> = contract_entries
            .iter()
            .flat_map(|(_, pd)| pd.anchors.iter().cloned())
            .collect();

        // c. StaleHash - check source_hash against original source
        for (path, parsed) in &contract_entries {
            if let Some(expected_hash) = parsed.attributes.get("source_hash") {
                if let Some(source_path) = self.find_source_for_contract(path, parsed) {
                    if let Ok(content) = std::fs::read_to_string(&source_path) {
                        let actual_hash = aden_core::stable_hash(content.as_bytes());
                        if actual_hash != *expected_hash {
                            events.push(DriftEvent::StaleHash {
                                target_path: source_path.to_string_lossy().to_string(),
                                expected_hash: expected_hash.clone(),
                                actual_hash,
                            });
                        }
                    }
                }
            }
        }

        // d. SignatureMismatch
        for (path, parsed) in &contract_entries {
            for anchor in &parsed.anchors {
                if let Some(&idx) = anchor_to_source_idx.get(anchor) {
                    let (_, source_doc) = &source_entries[idx];
                    let current_sig = extract_sig_from_doc(source_doc);
                    let contract_sig = extract_sig_from_contract(&parsed.raw_content);
                    if contract_sig != current_sig {
                        events.push(DriftEvent::SignatureMismatch {
                            anchor: anchor.clone(),
                            contract_path: path.to_string_lossy().to_string(),
                            expected_sig: contract_sig,
                            actual_sig: current_sig,
                        });
                    }
                }
            }
        }

        // e. MissingContract - public source symbols without .aden contract
        for (path, doc) in &source_entries {
            if is_public_symbol(doc) && !aden_anchors.contains(&doc.anchor) {
                let symbol_name = doc
                    .anchor
                    .rfind('#')
                    .map(|i| doc.anchor[i + 1..].to_string())
                    .unwrap_or_else(|| doc.anchor.clone());
                events.push(DriftEvent::MissingContract {
                    source_path: path.to_string_lossy().to_string(),
                    anchor: doc.anchor.clone(),
                    symbol_name,
                });
            }
        }

        // f. OrphanAnchor - .aden anchors without corresponding source symbol
        for (path, parsed) in &contract_entries {
            if path.extension().and_then(|e| e.to_str()) == Some("aden") {
                for anchor in &parsed.anchors {
                    if !anchor_to_source_idx.contains_key(anchor) {
                        events.push(DriftEvent::OrphanAnchor {
                            anchor: anchor.clone(),
                            contract_path: path.to_string_lossy().to_string(),
                        });
                    }
                }
            }
        }

        // g. BrokenReference (best-effort; skip if graph has structural issues)
        if let Ok(graph) = AdenGraph::build_from_directory(&self.repo_root) {
        for (contract_path, ref_anchor) in graph.unresolved_refs() {
            let line = find_ref_line(&contract_path, &ref_anchor);
            events.push(DriftEvent::BrokenReference {
                contract_path,
                ref_anchor,
                line,
            });
        }
        }

        // h. DeadLink
        for (path, parsed) in &contract_entries {
            for inc in &parsed.includes {
                if let Ok(inc_path) = resolve_include(path, &inc.path) {
                    if !inc_path.exists() {
                        events.push(DriftEvent::DeadLink {
                            contract_path: path.to_string_lossy().to_string(),
                            include_path: inc.path.clone(),
                        });
                    }
                }
            }
        }

        Ok(events)
    }

    fn is_excluded_dir(path: &Path) -> bool {
        path.file_name()
            .and_then(|n| n.to_str())
            .map(|n| matches!(n, "target" | ".git" | "node_modules" | ".cargo" | ".rustup"))
            .unwrap_or(false)
    }

    fn collect_source_files(&self, dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), HealError> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                if !Self::is_excluded_dir(&path) {
                    self.collect_source_files(&path, files)?;
                }
            } else if path.is_file() {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if matches!(ext, "rs" | "ps1" | "psm1") {
                    files.push(path);
                }
            }
        }
        Ok(())
    }

    fn collect_contract_files(
        &self,
        dir: &Path,
        files: &mut Vec<PathBuf>,
    ) -> Result<(), HealError> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                if !Self::is_excluded_dir(&path) {
                    self.collect_contract_files(&path, files)?;
                }
            } else if path.is_file() {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if ext == "adoc" || ext == "aden" {
                    files.push(path);
                }
            }
        }
        Ok(())
    }

    fn find_source_for_contract(
        &self,
        _contract_path: &Path,
        parsed: &aden_graph::parser::ParsedDocument,
    ) -> Option<PathBuf> {
        // Try explicit :source_file: attribute
        if let Some(source_file) = parsed.attributes.get("source_file") {
            let candidate = self.repo_root.join(source_file);
            if candidate.exists() {
                return Some(candidate);
            }
        }

        // Infer from anchor pattern: aden://module/{crate}/{file}#{symbol}
        for anchor in &parsed.anchors {
            if let Some(hash_pos) = anchor.rfind('#') {
                let prefix = &anchor[..hash_pos];
                if let Some(rest) = prefix.strip_prefix("aden://module/") {
                    let parts: Vec<&str> = rest.splitn(2, '/').collect();
                    if parts.len() == 2 {
                        let crate_name = parts[0];
                        let file_name = parts[1];
                        let candidates = [
                            format!("crates/{}/src/{}", crate_name, file_name),
                            format!("crates/{}/{}", crate_name, file_name),
                            format!("{}/src/{}", crate_name, file_name),
                            format!("{}/{}", crate_name, file_name),
                        ];
                        for candidate in &candidates {
                            let path = self.repo_root.join(candidate);
                            if path.exists() {
                                return Some(path);
                            }
                        }
                    }
                }
            }
        }

        None
    }
}

fn extract_sig_from_doc(doc: &Document) -> Vec<String> {
    let mut sig = Vec::new();
    for block in &doc.blocks {
        if let Block::Table(table) = block {
            for row in &table.rows {
                if row.len() >= 2 && row[0].starts_with("param ") {
                    sig.push(row[1].clone());
                }
            }
        }
    }
    sig
}

fn extract_sig_from_contract(content: &str) -> Vec<String> {
    let mut sig = Vec::new();
    let mut in_table = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "|===" {
            in_table = !in_table;
            continue;
        }
        if in_table && trimmed.starts_with("|param ") {
            let after_prefix = &trimmed[1..]; // remove leading |
            let cells: Vec<&str> = after_prefix.split('|').collect();
            if cells.len() >= 2 {
                sig.push(cells[1].trim().to_string());
            }
        }
    }

    sig
}

fn is_public_symbol(doc: &Document) -> bool {
    for block in &doc.blocks {
        if let Block::Table(table) = block {
            for row in &table.rows {
                if row.len() >= 2 && row[0] == "Visibility" {
                    return row[1] == "Public" || row[1] == "Crate";
                }
            }
        }
    }
    // Default to true for languages where visibility is not tracked
    true
}

fn find_ref_line(content: &str, ref_anchor: &str) -> usize {
    for (i, line) in content.lines().enumerate() {
        if line.contains(&format!("<<{}", ref_anchor)) {
            return i + 1;
        }
    }
    0
}

/// Resolve an include path, preventing directory traversal attacks.
fn resolve_include(current: &Path, include: &str) -> std::io::Result<PathBuf> {
    let base = current.parent().unwrap_or(Path::new("."));
    let candidate = base.join(include);
    
    // Prevent traversal outside the base directory
    if candidate.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("Include path '{}' contains parent-dir traversal (..). Denied for security.", include)
        ));
    }
    
    Ok(candidate)
}
