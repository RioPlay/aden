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
//! Deterministic diagnostic scanner for knowledge graphs.
//!
//! Scans a directory for structural issues in documents and the
//! knowledge graph, reporting them with severity, file location, and
//! actionable suggestions.
//!
//! ## How it works
//!
//! The scanner is **rule-based** and **configurable**. Instead of hardcoding
//! patterns like "adr-" or "mod-", it uses a `DiagnosticRules` struct that
//! lets you define what constitutes an issue for your domain.
//!
//! ## Example
//!
//! ```ignore
//! use aden_diagnose::{diagnose, DiagnosticRules};
//!
//! let rules = DiagnosticRules::default();
//! let result = diagnose("docs/", &rules)?;
//! println!("Health: {:.0}/100", result.health_score);
//! ```
//!
//! ## Issue categories
//!
//! - `UnresolvedRef` — references pointing to non-existent anchors
//! - `DuplicateAnchor` — multiple documents declaring the same anchor
//! - `EdgeViolation` — edges that violate type constraints
//! - `OrphanDocument` — documents with no edges
//! - `IncludeCycle` — circular includes between documents
//! - `MissingSource` — documents referencing non-existent source files
//! - `Custom` — user-defined rules

use aden_graph::graph::AdenGraph;
use aden_graph::nodes::{AdenEdge, DocumentNode, GraphNode};
use aden_graph::parser::parse_file;
use aden_graph::cycles::find_cycles;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A custom diagnostic check: receives the graph and file list, returns issues.
type CustomCheck =
    Arc<dyn Fn(&AdenGraph<DocumentNode, AdenEdge>, &[PathBuf]) -> Vec<Issue> + Send + Sync>;

// ── Severity ──────────────────────────────────────────────────────

/// Severity of a diagnostic issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Severity {
    /// Must fix — blocks CI.
    Error,
    /// Should fix — degrades health score.
    Warning,
    /// FYI — informational only.
    Info,
}

impl Severity {
    /// Weight for health score calculation.
    pub fn weight(&self) -> f64 {
        match self {
            Severity::Error => 5.0,
            Severity::Warning => 1.0,
            Severity::Info => 0.5,
        }
    }
}

// ── Issue ─────────────────────────────────────────────────────────

/// A single diagnostic issue found during scanning.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Issue {
    /// Category of the issue.
    pub category: IssueCategory,
    /// Severity level.
    pub severity: Severity,
    /// File path where the issue was found (if applicable).
    pub file: Option<String>,
    /// 1-based line number in the file (if applicable).
    pub line: Option<usize>,
    /// Human-readable description.
    pub message: String,
    /// Suggested fix (if applicable).
    pub suggestion: Option<String>,
    /// Raw data for programmatic consumption.
    pub raw: String,
}

/// Category of a diagnostic issue.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum IssueCategory {
    /// A `<<ref>>` points to an anchor that doesn't exist.
    UnresolvedRef,
    /// Multiple documents declare the same `[[anchor]]`.
    DuplicateAnchor,
    /// An edge violates type or node-type constraints.
    EdgeViolation,
    /// A document has no incoming or outgoing edges.
    OrphanDocument,
    /// Circular include chain between documents.
    IncludeCycle,
    /// A document references a source file that doesn't exist.
    MissingSource,
    /// A document has low confidence (self-reference or low-quality).
    LowConfidence,
    /// A user-defined rule fired.
    Custom(String),
}

impl std::fmt::Display for IssueCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IssueCategory::UnresolvedRef => write!(f, "unresolved-ref"),
            IssueCategory::DuplicateAnchor => write!(f, "duplicate-anchor"),
            IssueCategory::EdgeViolation => write!(f, "edge-violation"),
            IssueCategory::OrphanDocument => write!(f, "orphan-document"),
            IssueCategory::IncludeCycle => write!(f, "include-cycle"),
            IssueCategory::MissingSource => write!(f, "missing-source"),
            IssueCategory::LowConfidence => write!(f, "low-confidence"),
            IssueCategory::Custom(name) => write!(f, "custom-{}", name),
        }
    }
}

// ── Diagnosis ─────────────────────────────────────────────────────

/// Full diagnostic result from scanning a directory.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Diagnosis {
    /// All issues found.
    pub issues: Vec<Issue>,
    /// Count of Error-severity issues.
    pub error_count: usize,
    /// Count of Warning-severity issues.
    pub warning_count: usize,
    /// Count of Info-severity issues.
    pub info_count: usize,
    /// Health score from 0 (critical) to 100 (perfect).
    pub health_score: f64,
}

impl Diagnosis {
    /// Calculate health score from issues.
    ///
    /// Starts at 100, deducts:
    /// - 5 per error
    /// - 1 per warning
    /// - 0.5 per info
    ///
    /// Floor at 0.
    pub fn calc_health_score(issues: &[Issue]) -> f64 {
        let deduction: f64 = issues.iter().map(|i| i.severity.weight()).sum();
        (100.0 - deduction).clamp(0.0, 100.0)
    }

    /// Count issues by severity.
    pub fn counts(issues: &[Issue]) -> (usize, usize, usize) {
        let mut errors = 0usize;
        let mut warnings = 0usize;
        let mut infos = 0usize;
        for issue in issues {
            match issue.severity {
                Severity::Error => errors += 1,
                Severity::Warning => warnings += 1,
                Severity::Info => infos += 1,
            }
        }
        (errors, warnings, infos)
    }
}

// ── Rules ─────────────────────────────────────────────────────────

/// Configurable rules that define what constitutes an issue.
///
/// This is the key to making the scanner general-purpose. Instead of
/// hardcoding patterns like "adr-" or "mod-", you configure rules that match
/// your domain's conventions.
///
/// ## Example: ADR consolidation
///
/// ```ignore
/// let rules = DiagnosticRules {
///     // Check for references to deleted ADRs
///     stale_ref_patterns: vec!["adr-".to_string()],
///     // Suggest similar anchors for stale refs
///     stale_ref_suggest_similar: true,
///     // Don't flag ADRs as orphans (they're metadata)
///     metadata_prefixes: vec!["adr-".to_string()],
///     ..Default::default()
/// };
/// ```
///
/// ## Example: Custom rules
///
/// ```ignore
/// let rules = DiagnosticRules {
///     custom_checks: vec![Arc::new(|graph, files| {
///         // Return issues for this domain
///         vec![]
///     })],
///     ..Default::default()
/// };
/// ```
///
/// ## Example: Custom rules
///
/// ```ignore
/// let rules = DiagnosticRules {
///     custom_checks: vec![Box::new(|graph, files| {
///         // Return issues for this domain
///         vec![]
///     })],
///     ..Default::default()
/// };
/// ```
pub struct DiagnosticRules {
    /// Anchor prefixes that indicate metadata documents (not real orphans).
    /// e.g., `["adr-", "plan-", "spec-"]`
    pub metadata_prefixes: Vec<String>,

    /// Anchor prefixes that indicate stale references.
    /// When a ref matches one of these but the target doesn't exist,
    /// the scanner suggests similar anchors.
    /// e.g., `["adr-"]` to detect deleted ADR refs
    pub stale_ref_patterns: Vec<String>,

    /// Anchor prefixes that are contract/module references (not doc refs).
    /// These trigger a warning when used with `<< >>` syntax in docs.
    /// e.g., `["mod-", "aden://"]`
    pub contract_ref_patterns: Vec<String>,

    /// Whether to suggest similar anchors for stale references.
    pub stale_ref_suggest_similar: bool,

    /// Custom rule functions to run during diagnosis.
    /// Each function receives the graph and file list, returns issues.
    pub custom_checks: Vec<CustomCheck>,
}

impl Default for DiagnosticRules {
    fn default() -> Self {
        Self {
            metadata_prefixes: vec![],
            stale_ref_patterns: vec![],
            contract_ref_patterns: vec![],
            stale_ref_suggest_similar: true,
            custom_checks: vec![],
        }
    }
}

// ── Errors ────────────────────────────────────────────────────────

/// Errors that can occur during diagnosis.
#[derive(Debug, thiserror::Error)]
pub enum DiagnoseError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("graph error: {0}")]
    Graph(#[from] aden_graph::graph::GraphError),
    #[error("parse error: {0}")]
    Parse(#[from] aden_graph::parser::ParseError),
    #[error("serialisation error: {0}")]
    Generic(String),
}

// ── Public API ────────────────────────────────────────────────────

/// Run a full diagnostic scan on the given directory path.
///
/// This is the main entry point. It builds the graph, scans all `.adoc`
/// files for references, and runs every check in a deterministic order.
///
/// Uses default rules. For custom rules, use `diagnose_with_rules`.
pub fn diagnose(path: &Path) -> Result<Diagnosis, DiagnoseError> {
    diagnose_with_rules(path, &DiagnosticRules::default())
}

/// Run a full diagnostic scan with custom rules.
pub fn diagnose_with_rules(path: &Path, rules: &DiagnosticRules) -> Result<Diagnosis, DiagnoseError> {
    let mut issues: Vec<Issue> = Vec::new();

    // Build the knowledge graph from the directory.
    let mut graph = AdenGraph::build_from_directory(path)?;

    // Mark self-references as low-confidence to prevent self-bias.
    let config = aden_core::AdenConfig::load(path);
    graph.mark_self_references(&config);

    // Collect all .adoc files for reference scanning.
    let files = collect_adoc_files(path);

    // 1. Unresolved references — scan all .adoc files for <<ref>> patterns.
    issues.extend(scan_unresolved_refs(&graph, &files, rules));

    // 2. Duplicate anchors — multiple files declaring the same [[anchor]].
    issues.extend(scan_duplicate_anchors(&files));

    // 3. Edge violations — edges that violate type constraints.
    issues.extend(scan_edge_violations(&graph, rules));

    // 4. Stale references — refs to deleted/renamed entities.
    issues.extend(scan_stale_refs(&graph, &files, rules));

    // 5. Contract refs in docs — refs to non-doc anchors.
    issues.extend(scan_contract_refs(&files, rules));

    // 6. Orphan documents.
    issues.extend(scan_orphans(&graph, rules));

    // 7. Include cycles.
    issues.extend(scan_include_cycles(&graph));

    // 8. Missing source files.
    issues.extend(scan_missing_source(&graph));

    // 9. Low confidence documents (self-references).
    issues.extend(scan_low_confidence(&graph));

    // 10. Custom rules.
    for check in &rules.custom_checks {
        issues.extend(check(&graph, &files));
    }

    let (error_count, warning_count, info_count) = Diagnosis::counts(&issues);
    let health_score = Diagnosis::calc_health_score(&issues);

    Ok(Diagnosis {
        issues,
        error_count,
        warning_count,
        info_count,
        health_score,
    })
}

/// Run `diagnose` and return JSON-serialised output.
pub fn diagnose_json(path: &Path) -> Result<String, DiagnoseError> {
    let diagnosis = diagnose(path)?;
    serde_json::to_string_pretty(&diagnosis).map_err(|e| DiagnoseError::Generic(e.to_string()))
}

/// Run `diagnose_with_rules` and return JSON-serialised output.
pub fn diagnose_json_with_rules(path: &Path, rules: &DiagnosticRules) -> Result<String, DiagnoseError> {
    let diagnosis = diagnose_with_rules(path, rules)?;
    serde_json::to_string_pretty(&diagnosis).map_err(|e| DiagnoseError::Generic(e.to_string()))
}

// ── Individual scanners ───────────────────────────────────────────

/// Scan all `.adoc` files for `<<ref>>` patterns that don't exist in the graph.
fn scan_unresolved_refs(graph: &AdenGraph<DocumentNode, AdenEdge>, files: &[PathBuf], _rules: &DiagnosticRules) -> Vec<Issue> {
    let mut issues = Vec::new();
    let anchors: HashSet<&str> = graph.anchor_to_index.keys().map(|s| s.as_str()).collect();

    for file_path in files {
        let parsed = match parse_file(file_path) {
            Ok(p) => p,
            Err(_) => continue,
        };

        for (line_idx, ref_anchor) in parsed.refs.iter().enumerate() {
            if !anchors.contains(ref_anchor.as_str()) {
                // Try to find a similar anchor
                let suggestion = find_similar_anchor(ref_anchor, &anchors);
                issues.push(Issue {
                    category: IssueCategory::UnresolvedRef,
                    severity: Severity::Error,
                    file: Some(file_path.to_string_lossy().to_string()),
                    line: Some(line_idx + 1),
                    message: format!("Unresolved reference: <<{}>>", ref_anchor),
                    suggestion,
                    raw: serde_json::to_string(&serde_json::json!({
                        "ref": ref_anchor,
                        "line": line_idx + 1,
                    })).unwrap_or_default(),
                });
            }
        }
    }

    issues
}

/// Scan all `.adoc` files for duplicate `[[anchor]]` declarations.
fn scan_duplicate_anchors(files: &[PathBuf]) -> Vec<Issue> {
    let mut issues = Vec::new();
    let mut anchor_to_files: HashMap<String, Vec<PathBuf>> = HashMap::new();

    for file_path in files {
        let parsed = match parse_file(file_path) {
            Ok(p) => p,
            Err(_) => continue,
        };

        for anchor in &parsed.anchors {
            anchor_to_files
                .entry(anchor.clone())
                .or_default()
                .push(file_path.clone());
        }
    }

    for (anchor, files) in &anchor_to_files {
        if files.len() > 1 {
            let file_paths: Vec<String> = files.iter().map(|p| p.to_string_lossy().to_string()).collect();
            issues.push(Issue {
                category: IssueCategory::DuplicateAnchor,
                severity: Severity::Warning,
                file: Some(file_paths.first().cloned().unwrap_or_default()),
                line: None,
                message: format!("Duplicate anchor: [[{}]] declared in {} files", anchor, files.len()),
                suggestion: Some(format!(
                    "Rename one of the anchors. Files: {}",
                    file_paths.join(", ")
                )),
                raw: serde_json::to_string(&serde_json::json!({
                    "anchor": anchor,
                    "files": file_paths,
                })).unwrap_or_default(),
            });
        }
    }

    issues
}

/// Scan for edge violations — edges that violate type or node-type constraints.
///
/// This is a general-purpose scanner that uses the graph's built-in
/// `validate_typed_edges()` method, which checks that code edges only
/// connect to code nodes and semantic edges only connect to semantic nodes.
fn scan_edge_violations(graph: &AdenGraph<DocumentNode, AdenEdge>, _rules: &DiagnosticRules) -> Vec<Issue> {
    let mut issues = Vec::new();
    let violations = graph.validate_typed_edges();

    for violation in violations {
        issues.push(Issue {
            category: IssueCategory::EdgeViolation,
            severity: Severity::Error,
            file: None,
            line: None,
            message: violation,
            suggestion: Some(
                "Check edge types against node types. Code edges (Uses, Implements, etc.) \
                 should not connect to semantic nodes."
                    .to_string(),
            ),
            raw: String::new(),
        });
    }

    issues
}

/// Scan for stale references — refs to deleted or renamed entities.
///
/// Uses the `stale_ref_patterns` from rules to detect patterns like
/// "adr-002" when only "adr-001" exists.
fn scan_stale_refs(graph: &AdenGraph<DocumentNode, AdenEdge>, files: &[PathBuf], rules: &DiagnosticRules) -> Vec<Issue> {
    let mut issues = Vec::new();
    let anchors: HashSet<&str> = graph.anchor_to_index.keys().map(|s| s.as_str()).collect();

    for file_path in files {
        let parsed = match parse_file(file_path) {
            Ok(p) => p,
            Err(_) => continue,
        };

        for (line_idx, ref_anchor) in parsed.refs.iter().enumerate() {
            // Check if this ref matches any stale pattern
            let is_stale_pattern = rules.stale_ref_patterns.iter().any(|pattern| {
                ref_anchor.starts_with(pattern)
            });

            if is_stale_pattern && !anchors.contains(ref_anchor.as_str()) {
                let suggestion = if rules.stale_ref_suggest_similar {
                    let similar = find_similar_anchor(ref_anchor, &anchors);
                    similar.map(|s| format!("Did you mean <<{}>>?", s))
                } else {
                    None
                };

                issues.push(Issue {
                    category: IssueCategory::UnresolvedRef,
                    severity: Severity::Warning,
                    file: Some(file_path.to_string_lossy().to_string()),
                    line: Some(line_idx + 1),
                    message: format!("Possible stale reference: <<{}>>", ref_anchor),
                    suggestion,
                    raw: serde_json::to_string(&serde_json::json!({
                        "ref": ref_anchor,
                        "line": line_idx + 1,
                    })).unwrap_or_default(),
                });
            }
        }
    }

    issues
}

/// Scan for contract/module references used with `<< >>` syntax in docs.
///
/// These are anchors that point to contracts (not documents) and should
/// not use `<< >>` syntax. Configurable via `contract_ref_patterns`.
fn scan_contract_refs(files: &[PathBuf], rules: &DiagnosticRules) -> Vec<Issue> {
    let mut issues = Vec::new();

    if rules.contract_ref_patterns.is_empty() {
        return issues;
    }

    for file_path in files {
        let parsed = match parse_file(file_path) {
            Ok(p) => p,
            Err(_) => continue,
        };

        for (line_idx, ref_anchor) in parsed.refs.iter().enumerate() {
            for pattern in &rules.contract_ref_patterns {
                if ref_anchor.starts_with(pattern) {
                    issues.push(Issue {
                        category: IssueCategory::Custom("contract-ref".to_string()),
                        severity: Severity::Info,
                        file: Some(file_path.to_string_lossy().to_string()),
                        line: Some(line_idx + 1),
                        message: format!(
                            "Contract ref in doc: <<{}>> — this is a contract anchor, not a doc anchor",
                            ref_anchor
                        ),
                        suggestion: Some(format!(
                            "Remove `<< >>` syntax for contract anchors. Use `{}` instead.",
                            ref_anchor
                        )),
                        raw: serde_json::to_string(&serde_json::json!({
                            "ref": ref_anchor,
                            "pattern": pattern,
                        })).unwrap_or_default(),
                    });
                    break;
                }
            }
        }
    }

    issues
}

/// Scan for orphan documents — documents with no edges.
///
/// Documents matching `metadata_prefixes` are flagged as Info (expected orphans)
/// rather than Warning (unexpected orphans).
fn scan_orphans(graph: &AdenGraph<DocumentNode, AdenEdge>, rules: &DiagnosticRules) -> Vec<Issue> {
    let mut issues = Vec::new();
    let orphans = graph.orphans();

    for anchor in orphans {
        let node = graph.get_node(&anchor);
        let is_metadata = node.map(|_n| {
            // Check if the anchor starts with a metadata prefix
            rules.metadata_prefixes.iter().any(|prefix| anchor.starts_with(prefix))
        }).unwrap_or(false);

        issues.push(Issue {
            category: IssueCategory::OrphanDocument,
            severity: if is_metadata {
                Severity::Info
            } else {
                Severity::Warning
            },
            file: node.map(|n| n.source_path.to_string_lossy().to_string()),
            line: None,
            message: format!("Orphan document: [[{}]] has no edges", anchor),
            suggestion: Some("Add references to or from this document, or verify it should exist.".to_string()),
            raw: serde_json::to_string(&serde_json::json!({
                "anchor": anchor,
                "is_metadata": is_metadata,
            })).unwrap_or_default(),
        });
    }

    issues
}

/// Scan for include cycles — circular `include::` directives.
fn scan_include_cycles(graph: &AdenGraph<DocumentNode, AdenEdge>) -> Vec<Issue> {
    let mut issues = Vec::new();
    let cycles = find_cycles(graph);

    for cycle in cycles {
        let cycle_str = cycle.join(" -> ");
        issues.push(Issue {
            category: IssueCategory::IncludeCycle,
            severity: Severity::Error,
            file: None,
            line: None,
            message: format!("Include cycle detected: {}", cycle_str),
            suggestion: Some("Break the circular include chain by removing one of the includes."
                .to_string()),
            raw: serde_json::to_string(&serde_json::json!({
                "cycle": cycle,
            })).unwrap_or_default(),
        });
    }

    issues
}

/// Scan for contracts referencing non-existent source files.
fn scan_missing_source(graph: &AdenGraph<DocumentNode, AdenEdge>) -> Vec<Issue> {
    let mut issues = Vec::new();

    for node_idx in graph.graph.node_indices() {
        let node = &graph.graph[node_idx];
        if let Some(source_file) = node.doc.attributes.get("source_file") {
            let target_path = PathBuf::from(source_file);
            if !target_path.exists() {
                issues.push(Issue {
                    category: IssueCategory::MissingSource,
                    severity: Severity::Warning,
                    file: Some(target_path.to_string_lossy().to_string()),
                    line: None,
                    message: format!(
                        "Missing source file for [[{}]]: {}",
                        node.anchor(),
                        source_file
                    ),
                    suggestion: Some("Restore the source file or remove the contract.".to_string()),
                    raw: serde_json::to_string(&serde_json::json!({
                        "anchor": node.anchor(),
                        "missing_file": source_file,
                    })).unwrap_or_default(),
                });
            }
        }
    }

    issues
}

/// Scan for documents with low confidence (self-references or low-quality).
fn scan_low_confidence(graph: &AdenGraph<DocumentNode, AdenEdge>) -> Vec<Issue> {
    let mut issues = Vec::new();
    let threshold = 0.5;

    for node_idx in graph.graph.node_indices() {
        let node = &graph.graph[node_idx];
        if node.doc.confidence < threshold {
            let reason = if node.doc.confidence == 0.1 {
                "self-reference detected"
            } else {
                "low confidence"
            };
            issues.push(Issue {
                category: IssueCategory::LowConfidence,
                severity: Severity::Warning,
                file: node.doc.attributes.get("source_file").cloned(),
                line: None,
                message: format!(
                    "Low confidence ({:.1}) for [[{}]]: {}",
                    node.doc.confidence,
                    node.anchor(),
                    reason
                ),
                suggestion: Some("Verify this document's claims independently.".to_string()),
                raw: serde_json::to_string(&serde_json::json!({
                    "anchor": node.anchor(),
                    "confidence": node.doc.confidence,
                    "reason": reason,
                })).unwrap_or_default(),
            });
        }
    }

    issues
}

// ── Helpers ───────────────────────────────────────────────────────

/// Recursively collect `.adoc` files from a directory.
fn collect_adoc_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return files,
    };
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if path.is_dir() {
            files.extend(collect_adoc_files(&path));
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str())
            && ext == "adoc" {
                files.push(path);
            }
    }
    files.sort();
    files
}

/// Find anchors similar to the given name using simple string similarity.
/// Returns up to 3 suggestions.
fn find_similar_anchor(target: &str, anchors: &HashSet<&str>) -> Option<String> {
    let mut candidates: Vec<(String, u32)> = anchors
        .iter()
        .filter(|a| {
            // Must share at least a prefix or suffix
            a.starts_with(&target[..1.min(target.len())])
                || target.starts_with(&a[..1.min(a.len())])
        })
        .map(|a| {
            let distance = levenshtein_distance(target, a);
            (a.to_string(), distance)
        })
        .filter(|(_, d)| *d <= 3 && *d > 0) // exact match is not a suggestion
        .collect();

    candidates.sort_by_key(|(_, d)| *d);
    candidates.first().map(|(s, _)| s.clone())
}

/// Compute the Levenshtein edit distance between two strings.
fn levenshtein_distance(a: &str, b: &str) -> u32 {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();

    if m == 0 {
        return n as u32;
    }
    if n == 0 {
        return m as u32;
    }

    let mut matrix = vec![vec![0u32; n + 1]; m + 1];

    for (i, row) in matrix.iter_mut().enumerate() {
        row[0] = i as u32;
    }
    for (j, cell) in matrix[0].iter_mut().enumerate() {
        *cell = j as u32;
    }

    for i in 1..=m {
        for j in 1..=n {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            matrix[i][j] = (matrix[i - 1][j] + 1)
                .min(matrix[i][j - 1] + 1)
                .min(matrix[i - 1][j - 1] + cost);
        }
    }

    matrix[m][n]
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_temp_dir(test_name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aden-diagnose-{}", test_name));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn write_file(dir: &Path, rel_path: &str, content: &str) {
        let path = dir.join(rel_path);
        let mut file = std::fs::File::create(&path).expect("create file");
        file.write_all(content.as_bytes()).expect("write file");
    }

    #[test]
    fn test_health_score_calculation() {
        let issues: Vec<Issue> = vec![
            Issue {
                category: IssueCategory::UnresolvedRef,
                severity: Severity::Error,
                file: None,
                line: None,
                message: "test".to_string(),
                suggestion: None,
                raw: String::new(),
            },
            Issue {
                category: IssueCategory::DuplicateAnchor,
                severity: Severity::Warning,
                file: None,
                line: None,
                message: "test".to_string(),
                suggestion: None,
                raw: String::new(),
            },
            Issue {
                category: IssueCategory::OrphanDocument,
                severity: Severity::Info,
                file: None,
                line: None,
                message: "test".to_string(),
                suggestion: None,
                raw: String::new(),
            },
        ];

        let score = Diagnosis::calc_health_score(&issues);
        assert!((score - 93.5).abs() < 0.01); // 100 - 5 - 1 - 0.5 = 93.5
    }

    #[test]
    fn test_health_score_floor() {
        let issues: Vec<Issue> = (0..20).map(|_| Issue {
            category: IssueCategory::UnresolvedRef,
            severity: Severity::Error,
            file: None,
            line: None,
            message: "test".to_string(),
            suggestion: None,
            raw: String::new(),
        }).collect();

        let score = Diagnosis::calc_health_score(&issues);
        assert_eq!(score, 0.0); // 20 errors * 5 = 100 deduction, floor at 0
    }

    #[test]
    fn test_issue_counts() {
        let issues = vec![
            Issue {
                category: IssueCategory::UnresolvedRef,
                severity: Severity::Error,
                file: None,
                line: None,
                message: "e1".to_string(),
                suggestion: None,
                raw: String::new(),
            },
            Issue {
                category: IssueCategory::DuplicateAnchor,
                severity: Severity::Error,
                file: None,
                line: None,
                message: "e2".to_string(),
                suggestion: None,
                raw: String::new(),
            },
            Issue {
                category: IssueCategory::OrphanDocument,
                severity: Severity::Warning,
                file: None,
                line: None,
                message: "w1".to_string(),
                suggestion: None,
                raw: String::new(),
            },
            Issue {
                category: IssueCategory::Custom("test".to_string()),
                severity: Severity::Info,
                file: None,
                line: None,
                message: "i1".to_string(),
                suggestion: None,
                raw: String::new(),
            },
        ];

        let (errors, warnings, infos) = Diagnosis::counts(&issues);
        assert_eq!(errors, 2);
        assert_eq!(warnings, 1);
        assert_eq!(infos, 1);
    }

    #[test]
    fn test_levenshtein_distance() {
        assert_eq!(levenshtein_distance("", ""), 0);
        assert_eq!(levenshtein_distance("a", ""), 1);
        assert_eq!(levenshtein_distance("", "b"), 1);
        assert_eq!(levenshtein_distance("abc", "abc"), 0);
        assert_eq!(levenshtein_distance("abc", "abd"), 1);
        assert_eq!(levenshtein_distance("kitten", "sitting"), 3);
    }

    #[test]
    fn test_find_similar_anchor() {
        let mut anchors = HashSet::new();
        anchors.insert("doc-intro");
        anchors.insert("doc-overview");
        anchors.insert("doc-api");

        let suggestion = find_similar_anchor("doc-intro", &anchors);
        assert!(suggestion.is_none()); // exact match, no suggestion

        let suggestion = find_similar_anchor("doc-introd", &anchors);
        assert!(suggestion.is_some());
        assert_eq!(suggestion.unwrap(), "doc-intro");
    }

    #[test]
    fn test_empty_directory() {
        let dir = make_temp_dir("empty");
        let result = diagnose(&dir);
        assert!(result.is_ok());
        let diagnosis = result.unwrap();
        assert_eq!(diagnosis.health_score, 100.0);
        assert_eq!(diagnosis.error_count, 0);
    }

    #[test]
    fn test_unresolved_ref_detection() {
        let dir = make_temp_dir("unresolved-ref");
        write_file(
            &dir,
            "test.adoc",
            "[[test-doc]]\n= Test\n\nSee <<nonexistent-anchor>> for details.\n",
        );
        let result = diagnose(&dir);
        assert!(result.is_ok());
        let diagnosis = result.unwrap();
        // Should find at least one unresolved ref
        let unresolved = diagnosis.issues.iter().filter(|i| i.category == IssueCategory::UnresolvedRef).count();
        assert!(unresolved > 0, "Expected unresolved ref, got: {:?}", diagnosis.issues);
    }

    #[test]
    fn test_duplicate_anchor_detection() {
        let dir = make_temp_dir("duplicate-anchor");
        write_file(
            &dir,
            "file1.adoc",
            "[[shared-anchor]]\n= File 1\n\nContent.\n",
        );
        write_file(
            &dir,
            "file2.adoc",
            "[[shared-anchor]]\n= File 2\n\nContent.\n",
        );
        let result = diagnose(&dir);
        assert!(result.is_ok());
        let diagnosis = result.unwrap();
        let duplicates = diagnosis.issues.iter().filter(|i| i.category == IssueCategory::DuplicateAnchor).count();
        assert!(duplicates > 0, "Expected duplicate anchor, got: {:?}", diagnosis.issues);
    }

    #[test]
    fn test_diagnosis_json_output() {
        let dir = make_temp_dir("json-output");
        write_file(
            &dir,
            "test.adoc",
            "[[test-doc]]\n= Test\n\nSee <<nonexistent>> for details.\n",
        );
        let result = diagnose_json(&dir);
        assert!(result.is_ok());
        let json = result.unwrap();
        // Should be valid JSON
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed["error_count"], 1);
        // 1 unresolved ref + 1 orphan document = 2 issues
        assert_eq!(parsed["issues"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_custom_rules() {
        let dir = make_temp_dir("custom-rules");
        write_file(
            &dir,
            "doc-a.adoc",
            "[[doc-a]]\n= Document A\n\nSee <<doc-b>>.\n",
        );
        write_file(
            &dir,
            "doc-b.adoc",
            "[[doc-b]]\n= Document B\n\nSee <<doc-a>>.\n",
        );

        // Add a custom rule that always fires
        let rules = DiagnosticRules {
            custom_checks: vec![Arc::new(|_graph, _files| {
                vec![Issue {
                    category: IssueCategory::Custom("test-rule".to_string()),
                    severity: Severity::Info,
                    file: None,
                    line: None,
                    message: "Custom rule fired".to_string(),
                    suggestion: None,
                    raw: String::new(),
                }]
            })],
            ..Default::default()
        };

        let result = diagnose_with_rules(&dir, &rules);
        assert!(result.is_ok());
        let diagnosis = result.unwrap();
        let custom = diagnosis.issues.iter().filter(|i| i.category == IssueCategory::Custom("test-rule".to_string())).count();
        assert_eq!(custom, 1);
    }

    #[test]
    fn test_metadata_prefixes() {
        let dir = make_temp_dir("metadata-prefixes");
        // ADR with no edges — it's an orphan but should be Info (metadata)
        write_file(
            &dir,
            "adr-001.adoc",
            "[[adr-001]]\n= ADR 001\n\nSome decision.\n",
        );
        // Regular doc with no edges — it's an orphan and should be Warning
        write_file(
            &dir,
            "doc-b.adoc",
            "[[doc-b]]\n= Document B\n\nSome content.\n",
        );

        let rules = DiagnosticRules {
            metadata_prefixes: vec!["adr-".to_string()],
            ..Default::default()
        };

        let result = diagnose_with_rules(&dir, &rules);
        assert!(result.is_ok());
        let diagnosis = result.unwrap();
        let orphans = diagnosis.issues.iter().filter(|i| i.category == IssueCategory::OrphanDocument).collect::<Vec<_>>();
        assert!(orphans.len() >= 2, "Expected at least 2 orphans, got: {}", orphans.len());
        for orphan in orphans {
            if orphan.file.as_deref().map(|f| f.contains("adr-001")).unwrap_or(false) {
                assert_eq!(orphan.severity, Severity::Info, "ADR should be Info, not {:?}", orphan.severity);
            }
            if orphan.file.as_deref().map(|f| f.contains("doc-b")).unwrap_or(false) {
                assert_eq!(orphan.severity, Severity::Warning, "Regular doc should be Warning, not {:?}", orphan.severity);
            }
        }
    }

    #[test]
    fn test_stale_ref_patterns() {
        let dir = make_temp_dir("stale-refs");
        write_file(
            &dir,
            "test.adoc",
            "[[test-doc]]\n= Test\n\nSee <<adr-999>> for the old decision.\n",
        );
        write_file(
            &dir,
            "adr-001.adoc",
            "[[adr-001]]\n= ADR 001\n\nSome decision.\n",
        );

        let rules = DiagnosticRules {
            stale_ref_patterns: vec!["adr-".to_string()],
            stale_ref_suggest_similar: true,
            ..Default::default()
        };

        let result = diagnose_with_rules(&dir, &rules);
        assert!(result.is_ok());
        let diagnosis = result.unwrap();
        let stale_refs = diagnosis.issues.iter().filter(|i| {
            i.category == IssueCategory::UnresolvedRef && i.message.contains("adr-999")
        }).count();
        assert!(stale_refs > 0, "Expected stale ref, got: {:?}", diagnosis.issues);
    }

    #[test]
    fn test_contract_ref_patterns() {
        let dir = make_temp_dir("contract-refs");
        write_file(
            &dir,
            "test.adoc",
            "[[test-doc]]\n= Test\n\nSee <<mod-aden-core>> for the core module.\n",
        );

        let rules = DiagnosticRules {
            contract_ref_patterns: vec!["mod-".to_string()],
            ..Default::default()
        };

        let result = diagnose_with_rules(&dir, &rules);
        assert!(result.is_ok());
        let diagnosis = result.unwrap();
        let contract_refs = diagnosis.issues.iter().filter(|i| {
            i.category == IssueCategory::Custom("contract-ref".to_string())
        }).count();
        assert!(contract_refs > 0, "Expected contract ref issue, got: {:?}", diagnosis.issues);
    }

    #[test]
    fn test_valid_graph_no_issues() {
        let dir = make_temp_dir("valid-graph");
        write_file(
            &dir,
            "doc-a.adoc",
            "[[doc-a]]\n= Document A\n\nSee <<doc-b>>.\n",
        );
        write_file(
            &dir,
            "doc-b.adoc",
            "[[doc-b]]\n= Document B\n\nSee <<doc-a>>.\n",
        );
        let result = diagnose(&dir);
        assert!(result.is_ok());
        let diagnosis = result.unwrap();
        // No errors should be reported for a valid connected graph
        assert_eq!(diagnosis.error_count, 0);
    }

    #[test]
    fn test_orphan_document_detection() {
        let dir = make_temp_dir("orphan");
        write_file(
            &dir,
            "doc-a.adoc",
            "[[doc-a]]\n= Document A\n\nContent.\n",
        );
        write_file(
            &dir,
            "doc-b.adoc",
            "[[doc-b]]\n= Document B\n\nContent.\n",
        );
        let result = diagnose(&dir);
        assert!(result.is_ok());
        let diagnosis = result.unwrap();
        let orphans = diagnosis.issues.iter().filter(|i| i.category == IssueCategory::OrphanDocument).count();
        // Both docs are orphans (no edges between them since they don't reference each other)
        assert!(orphans >= 1, "Expected at least 1 orphan, got: {:?}", diagnosis.issues);
    }

    #[test]
    fn test_severity_weights() {
        assert_eq!(Severity::Error.weight(), 5.0);
        assert_eq!(Severity::Warning.weight(), 1.0);
        assert_eq!(Severity::Info.weight(), 0.5);
    }

    #[test]
    fn test_issue_category_display() {
        assert_eq!(format!("{}", IssueCategory::UnresolvedRef), "unresolved-ref");
        assert_eq!(format!("{}", IssueCategory::DuplicateAnchor), "duplicate-anchor");
        assert_eq!(format!("{}", IssueCategory::EdgeViolation), "edge-violation");
        assert_eq!(format!("{}", IssueCategory::OrphanDocument), "orphan-document");
        assert_eq!(format!("{}", IssueCategory::IncludeCycle), "include-cycle");
        assert_eq!(format!("{}", IssueCategory::MissingSource), "missing-source");
        assert_eq!(format!("{}", IssueCategory::Custom("test".to_string())), "custom-test");
    }

    #[test]
    fn test_diagnosis_serialization() {
        let dir = make_temp_dir("serialize");
        let result = diagnose(&dir);
        assert!(result.is_ok());
        let diagnosis = result.unwrap();
        // Should serialize to JSON without errors
        let json = serde_json::to_string(&diagnosis).expect("serialise diagnosis");
        // Should round-trip
        let deserialized: Diagnosis = serde_json::from_str(&json).expect("deserialise diagnosis");
        assert_eq!(deserialized.error_count, diagnosis.error_count);
        assert_eq!(deserialized.warning_count, diagnosis.warning_count);
        assert_eq!(deserialized.info_count, diagnosis.info_count);
        assert!((deserialized.health_score - diagnosis.health_score).abs() < 0.001);
    }
}
