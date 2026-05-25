// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// All rights reserved.
//
// Aden: A Dense Referential Context Compiler
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Subagent staging protocol skeleton (Phase 0.8).
//!
//! Staging files are write-only from the agent perspective and are **never**
//! read by `aden check`. They exist as an append-only audit trail of agent
//! proposals before promotion to `[agent]` or `[proposed]` blocks.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A single staging entry representing an agent's proposed work.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StagingEntry {
    /// ISO-8601 timestamp of the proposal.
    pub timestamp: String,
    /// Agent role / identity.
    pub role: String,
    /// Session or task identifier.
    pub task_id: String,
    /// Files touched by this proposal.
    pub files: Vec<String>,
    /// Constitutional confidence score (0.0–1.0).
    pub confidence: f64,
    /// Raw AsciiDoc content of the proposal.
    pub content: String,
    /// Parsed directives (if any) extracted from the proposal.
    pub directives: Vec<String>,
}

/// Staging manager: handles the `.aden/staging/` directory.
#[derive(Debug, Clone)]
pub struct StagingManager {
    root: PathBuf,
}

impl StagingManager {
    /// Create a staging manager for the given project root.
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.join(".aden").join("staging"),
        }
    }

    /// Ensure the staging directory exists.
    pub fn ensure_dir(&self) -> crate::Result<()> {
        std::fs::create_dir_all(&self.root).map_err(|e| crate::Error::Io(e.to_string()))
    }

    /// Write a new staging file.
    ///
    /// Files are named `{role}-{timestamp}.adoc` and are append-only.
    pub fn stage(&self, entry: &StagingEntry) -> crate::Result<PathBuf> {
        self.ensure_dir()?;

        let sanitized_role = entry
            .role
            .replace(|c: char| !c.is_alphanumeric() && c != '-', "");
        let file_name = format!("{}-{}.adoc", sanitized_role, entry.timestamp);
        let path = self.root.join(&file_name);

        // If collision, append counter
        let final_path = if path.exists() {
            let mut counter = 1u32;
            loop {
                let candidate = self.root.join(format!(
                    "{}-{timestamp}-{counter}.adoc",
                    sanitized_role,
                    timestamp = entry.timestamp
                ));
                if !candidate.exists() {
                    break candidate;
                }
                counter += 1;
                if counter > 999 {
                    return Err(crate::Error::Generic(
                        "Too many staging collisions — possible runaway agent loop".to_string(),
                    ));
                }
            }
        } else {
            path
        };

        let output = format_staging_entry(entry);
        std::fs::write(&final_path, output).map_err(|e| crate::Error::Io(e.to_string()))?;

        Ok(final_path)
    }

    /// List all staging files (sorted by modification time, oldest first).
    pub fn list(&self) -> crate::Result<Vec<PathBuf>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut files: Vec<PathBuf> = std::fs::read_dir(&self.root)
            .map_err(|e| crate::Error::Io(e.to_string()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e == "adoc")
                    .unwrap_or(false)
            })
            .collect();

        // Sort by mtime
        files.sort_by(|a, b| {
            let ma = std::fs::metadata(a)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            let mb = std::fs::metadata(b)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            ma.cmp(&mb)
        });

        Ok(files)
    }

    /// Load a staging entry from disk.
    pub fn load(&self, path: &Path) -> crate::Result<StagingEntry> {
        let text = std::fs::read_to_string(path).map_err(|e| crate::Error::Io(e.to_string()))?;
        parse_staging_entry(&text)
    }

    /// Return the staging directory path.
    pub fn dir(&self) -> &Path {
        &self.root
    }
}

fn format_staging_entry(entry: &StagingEntry) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "[[staging-{role}-{ts}]]\n",
        role = entry.role,
        ts = entry.timestamp
    ));
    out.push_str(&format!(
        "= Staging Proposal: {role}\n\n",
        role = entry.role
    ));

    out.push_str("|===\n");
    out.push_str("|Field|Value\n");
    out.push_str(&format!("|Timestamp|{}\n", entry.timestamp));
    out.push_str(&format!("|Role|{}\n", entry.role));
    out.push_str(&format!("|Task ID|{}\n", entry.task_id));
    out.push_str(&format!("|Confidence|{:.2}\n", entry.confidence));
    out.push_str(&format!("|Files|{}\n", entry.files.join(", ")));
    out.push_str(&format!("|Directives|{}\n", entry.directives.join(", ")));
    out.push_str("|===\n\n");

    out.push_str("[proposed]\n");
    out.push_str("----\n");
    out.push_str(&entry.content);
    if !entry.content.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("----\n");
    out
}

fn parse_staging_entry(text: &str) -> crate::Result<StagingEntry> {
    let mut entry = StagingEntry {
        timestamp: String::new(),
        role: String::new(),
        task_id: String::new(),
        files: Vec::new(),
        confidence: 0.0,
        directives: Vec::new(),
        content: String::new(),
    };

    let mut in_table = false;
    let mut in_proposed = false;
    let mut proposed_lines: Vec<String> = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("[proposed]") {
            in_table = false;
            in_proposed = true;
            continue;
        }
        if trimmed == "----" && in_proposed {
            if !entry.content.is_empty() {
                // End of proposed block
                in_proposed = false;
            }
            continue;
        }
        if trimmed == "|===" {
            in_table = !in_table;
            continue;
        }
        if in_table && trimmed.starts_with('|') {
            let cells: Vec<&str> = trimmed.split('|').skip(1).collect();
            if cells.len() >= 2 {
                let key = cells[0].trim();
                let value = cells[1].trim();
                match key {
                    "Timestamp" => entry.timestamp = value.to_string(),
                    "Role" => entry.role = value.to_string(),
                    "Task ID" => entry.task_id = value.to_string(),
                    "Confidence" => entry.confidence = value.parse().unwrap_or(0.0),
                    "Files" => {
                        entry.files = value.split(',').map(|s| s.trim().to_string()).collect()
                    }
                    "Directives" => {
                        entry.directives = value.split(',').map(|s| s.trim().to_string()).collect()
                    }
                    _ => {}
                }
            }
        }
        if in_proposed {
            proposed_lines.push(line.to_string());
        }
    }

    entry.content = proposed_lines.join("\n");

    if entry.timestamp.is_empty() {
        return Err(crate::Error::Parse(
            "Staging entry missing timestamp".to_string(),
        ));
    }

    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_staging_roundtrip() {
        let entry = StagingEntry {
            timestamp: "2026-05-24T14:30:00Z".to_string(),
            role: "agent-0".to_string(),
            task_id: "phase-0-contract-kernel".to_string(),
            files: vec!["aden-core/src/contract.rs".to_string()],
            confidence: 0.92,
            directives: vec!["forbid_import: unsafe::*".to_string()],
            content: "fn new_contract() {}".to_string(),
        };

        let formatted = format_staging_entry(&entry);
        let parsed = parse_staging_entry(&formatted).unwrap();

        assert_eq!(parsed.timestamp, entry.timestamp);
        assert_eq!(parsed.role, entry.role);
        assert_eq!(parsed.task_id, entry.task_id);
        assert_eq!(parsed.confidence, entry.confidence);
        assert_eq!(parsed.files, entry.files);
        assert_eq!(parsed.directives, entry.directives);
        assert_eq!(parsed.content.trim(), entry.content);
    }

    #[test]
    fn test_staging_manager_dir() {
        let tmp = std::env::temp_dir().join("aden-test-staging");
        let mgr = StagingManager::new(&tmp);
        mgr.ensure_dir().unwrap();
        assert!(mgr.dir().exists());
        // Cleanup
        let _ = std::fs::remove_dir_all(tmp.join(".aden"));
    }
}
