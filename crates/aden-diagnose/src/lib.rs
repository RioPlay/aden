// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
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
//! - `CouplingHotspot` — many directed edges between two coarse areas
//! - `GodHub` — a code symbol whose combined in/out degree is very high
//! - `DuplicateModule` — same-named modules in different areas with high line overlap
//! - `Custom` — user-defined rules

use aden_core::EdgeType;
use aden_graph::NodeIndex;
use aden_graph::cycles::find_cycles;
use aden_graph::graph::AdenGraph;
use aden_graph::nodes::{AdenEdge, DocumentNode, GraphNode};
use aden_graph::parser::parse_file;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
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
            // Info is purely advisory (expected metadata, normal orphan docs, etc.)
            // and must NOT deduct from health — otherwise a large repo's hundreds of
            // expected-metadata Infos floor the score to 0 and disagree with `status`.
            Severity::Info => 0.0,
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
    /// Many directed edges run between two coarse source areas.
    CouplingHotspot,
    /// A code symbol's combined in-degree and out-degree is very high.
    GodHub,
    /// Two same-named modules in different areas share most of their lines.
    DuplicateModule,
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
            IssueCategory::CouplingHotspot => write!(f, "coupling-hotspot"),
            IssueCategory::GodHub => write!(f, "god-hub"),
            IssueCategory::DuplicateModule => write!(f, "duplicate-module"),
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
    /// Plain markdown/text orphans hidden because they have no explicit
    /// AsciiDoc `[[anchor]]` cross-references. Stays `0` when suppression is off
    /// and when older JSON omits the field.
    #[serde(default)]
    pub suppressed_plain_doc_orphans: usize,
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

    /// Hide `OrphanDocument` hits for plain `.md` / `.markdown` / `.txt` files
    /// that never declare an AsciiDoc `[[anchor]]`. `.adoc` orphans stay.
    /// Default `false` so existing callers keep every orphan.
    pub suppress_plain_doc_orphans: bool,

    /// When set, load the indexed code graph with `aden_graph::cache::try_load`
    /// and emit coupling, hub, and duplicate-module findings. A missing graph
    /// skips those scanners. Default `true`.
    pub enable_structural_diagnostics: bool,

    /// Minimum directed cross-area edges before a pair is a coupling hotspot.
    pub coupling_hotspot_threshold: usize,

    /// Minimum `in_degree + out_degree` before a code symbol is a god hub.
    pub god_hub_degree_threshold: usize,

    /// Minimum line-overlap ratio (`shared / min(len_a, len_b)`) for a
    /// duplicate-module pair. Default `0.30`.
    pub duplicate_module_min_overlap: f64,
}

impl Default for DiagnosticRules {
    fn default() -> Self {
        Self {
            metadata_prefixes: vec![],
            stale_ref_patterns: vec![],
            contract_ref_patterns: vec![],
            stale_ref_suggest_similar: true,
            custom_checks: vec![],
            suppress_plain_doc_orphans: false,
            enable_structural_diagnostics: true,
            coupling_hotspot_threshold: 25,
            god_hub_degree_threshold: 25,
            duplicate_module_min_overlap: 0.30,
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
pub fn diagnose_with_rules(
    path: &Path,
    rules: &DiagnosticRules,
) -> Result<Diagnosis, DiagnoseError> {
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
    let (orphan_issues, suppressed_plain_doc_orphans) = scan_orphans(&graph, rules);
    issues.extend(orphan_issues);

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

    // 11. Structural signal from the indexed code graph. `build_from_directory`
    // above only parses prose. `try_load` reads the code + edge graph; `None`
    // (no store / empty snapshot) skips these scanners without failing.
    if rules.enable_structural_diagnostics
        && let Some(code_graph) = aden_graph::cache::try_load(path)
    {
        issues.extend(scan_coupling_hotspots(&code_graph, rules));
        issues.extend(scan_god_hubs(&code_graph, rules));
        issues.extend(scan_duplicate_modules(&code_graph, path, rules));
    }

    let (error_count, warning_count, info_count) = Diagnosis::counts(&issues);
    let health_score = Diagnosis::calc_health_score(&issues);

    Ok(Diagnosis {
        issues,
        error_count,
        warning_count,
        info_count,
        health_score,
        suppressed_plain_doc_orphans,
    })
}

/// Run `diagnose` and return JSON-serialised output.
pub fn diagnose_json(path: &Path) -> Result<String, DiagnoseError> {
    let diagnosis = diagnose(path)?;
    serde_json::to_string_pretty(&diagnosis).map_err(|e| DiagnoseError::Generic(e.to_string()))
}

/// Run `diagnose_with_rules` and return JSON-serialised output.
pub fn diagnose_json_with_rules(
    path: &Path,
    rules: &DiagnosticRules,
) -> Result<String, DiagnoseError> {
    let diagnosis = diagnose_with_rules(path, rules)?;
    serde_json::to_string_pretty(&diagnosis).map_err(|e| DiagnoseError::Generic(e.to_string()))
}

// ── Individual scanners ───────────────────────────────────────────

/// Scan all `.adoc` files for `<<ref>>` patterns that don't exist in the graph.
fn scan_unresolved_refs(
    graph: &AdenGraph<DocumentNode, AdenEdge>,
    files: &[PathBuf],
    _rules: &DiagnosticRules,
) -> Vec<Issue> {
    let mut issues = Vec::new();
    let anchors: HashSet<&str> = graph.anchor_to_index.keys().map(|s| s.as_str()).collect();

    for file_path in files {
        let parsed = match parse_file(file_path) {
            Ok(p) => p,
            Err(_) => continue,
        };

        for ref_anchor in parsed.refs.iter() {
            if !anchors.contains(ref_anchor.as_str()) {
                // Try to find a similar anchor
                let suggestion = find_similar_anchor(ref_anchor, &anchors);
                issues.push(Issue {
                    category: IssueCategory::UnresolvedRef,
                    severity: Severity::Error,
                    file: Some(file_path.to_string_lossy().to_string()),
                    // `parsed.refs` is a `Vec<String>` with no source line info,
                    // so the vector index is NOT a real line number. Emit `None`
                    // (honest) rather than a fabricated 1,2,3… index.
                    line: None,
                    message: format!("Unresolved reference: <<{}>>", ref_anchor),
                    suggestion,
                    raw: serde_json::to_string(&serde_json::json!({
                        "ref": ref_anchor,
                    }))
                    .unwrap_or_default(),
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
            let file_paths: Vec<String> = files
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect();
            // A duplicate anchor across prose/doc files (e.g. a template and its
            // instantiated copy) is reference-material noise, not a code defect, so
            // it is Info — matching `status`/`check`. A duplicate in a code file is
            // a real collision (data loss on ingest) and stays a Warning.
            let all_doc = files.iter().all(|p| is_doc_source(p));
            issues.push(Issue {
                category: IssueCategory::DuplicateAnchor,
                severity: if all_doc {
                    Severity::Info
                } else {
                    Severity::Warning
                },
                file: Some(file_paths.first().cloned().unwrap_or_default()),
                line: None,
                message: format!(
                    "Duplicate anchor: [[{}]] declared in {} files",
                    anchor,
                    files.len()
                ),
                suggestion: Some(format!(
                    "Rename one of the anchors. Files: {}",
                    file_paths.join(", ")
                )),
                raw: serde_json::to_string(&serde_json::json!({
                    "anchor": anchor,
                    "files": file_paths,
                }))
                .unwrap_or_default(),
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
fn scan_edge_violations(
    graph: &AdenGraph<DocumentNode, AdenEdge>,
    _rules: &DiagnosticRules,
) -> Vec<Issue> {
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
fn scan_stale_refs(
    graph: &AdenGraph<DocumentNode, AdenEdge>,
    files: &[PathBuf],
    rules: &DiagnosticRules,
) -> Vec<Issue> {
    let mut issues = Vec::new();
    let anchors: HashSet<&str> = graph.anchor_to_index.keys().map(|s| s.as_str()).collect();

    for file_path in files {
        let parsed = match parse_file(file_path) {
            Ok(p) => p,
            Err(_) => continue,
        };

        for ref_anchor in parsed.refs.iter() {
            // Check if this ref matches any stale pattern
            let is_stale_pattern = rules
                .stale_ref_patterns
                .iter()
                .any(|pattern| ref_anchor.starts_with(pattern));

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
                    // `parsed.refs` carries no source line info; the vector index
                    // is not a real line number, so emit `None` rather than a
                    // fabricated index.
                    line: None,
                    message: format!("Possible stale reference: <<{}>>", ref_anchor),
                    suggestion,
                    raw: serde_json::to_string(&serde_json::json!({
                        "ref": ref_anchor,
                    }))
                    .unwrap_or_default(),
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

/// True when a source path is a prose/documentation file (reference material),
/// as opposed to a code file whose orphaned symbols are actionable. Language-
/// agnostic: keyed on the file extension, not on any project layout.
fn is_doc_source(path: &std::path::Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("md" | "markdown" | "adoc" | "asciidoc" | "txt" | "rst")
    )
}

/// Scan for orphan documents — documents with no edges.
///
/// Documents matching `metadata_prefixes`, or sourced from a prose/doc file, are
/// flagged as Info (expected orphans) rather than Warning (unexpected orphans).
///
/// Returns the issues and how many plain markdown/text orphans were hidden.
/// `.adoc` / `.asciidoc` orphans are never hidden.
fn scan_orphans(
    graph: &AdenGraph<DocumentNode, AdenEdge>,
    rules: &DiagnosticRules,
) -> (Vec<Issue>, usize) {
    let mut issues = Vec::new();
    let mut suppressed = 0usize;
    let orphans = graph.orphans();

    // Dedup by anchor: when several files collapse to the same anchor (e.g. every
    // `README.md` → `[[README]]`, every `LICENSE.txt` → `[[LICENSE]]`), `orphans()`
    // yields the anchor once per node but `get_node` resolves them all to the same
    // first node — which would emit identical lines (README ×3) that read as a bug.
    // One issue per distinct orphan anchor.
    let mut seen = std::collections::HashSet::new();

    for anchor in orphans {
        if !seen.insert(anchor.clone()) {
            continue;
        }
        let node = graph.get_node(&anchor);
        let is_metadata = node
            .map(|n| {
                // Anchor matches a configured metadata prefix (e.g. "adr-", "plan-")…
                rules.metadata_prefixes.iter().any(|prefix| anchor.starts_with(prefix))
                // …or this is a documentation/prose source file rather than a code
                // symbol. An orphaned doc (README, NOTICE, an .adoc heading, agent
                // scaffolding) is reference material with legitimately no edges; only
                // orphaned *code* symbols are actionable. The metadata/actionable
                // split mirrors `status`/`check` (`is_expected_metadata`), which now
                // apply the same anchor dedup via `classify_orphans`, so all three
                // tools agree on orphan counts.
                || is_doc_source(&n.source_path)
            })
            .unwrap_or(false);

        if rules.suppress_plain_doc_orphans && node.is_some_and(is_suppressible_plain_orphan) {
            suppressed += 1;
            continue;
        }

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
            suggestion: Some(
                "Add references to or from this document, or verify it should exist.".to_string(),
            ),
            raw: serde_json::to_string(&serde_json::json!({
                "anchor": anchor,
                "is_metadata": is_metadata,
            }))
            .unwrap_or_default(),
        });
    }

    (issues, suppressed)
}

/// Plain `.md` / `.markdown` / `.txt` orphan with no explicit `[[anchor]]`.
///
/// The prose parser synthesizes a filename-stem anchor when a file has no
/// `[[anchor]]` line, so an empty `parsed.anchors` list is not enough. Require
/// empty `<<refs>>`, no `explicit_anchor` attribute, no non-stem anchors, and
/// no `[[` in the parsed text (falling back to a single file read).
fn is_suppressible_plain_orphan(node: &DocumentNode) -> bool {
    if !is_plain_markdown_or_text(&node.source_path) {
        return false;
    }
    if !node
        .parsed
        .as_ref()
        .is_none_or(|parsed| parsed.refs.is_empty())
    {
        return false;
    }
    if explicit_anchor_marked(node) {
        return false;
    }
    let anchors_not_explicit = match node.parsed.as_ref() {
        Some(parsed) => {
            let stem = node
                .source_path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("");
            parsed.anchors.iter().all(|anchor| anchor == stem)
        }
        None => true,
    };
    anchors_not_explicit && !source_contains_explicit_anchor(node)
}

fn is_plain_markdown_or_text(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref(),
        Some("md" | "markdown" | "txt")
    )
}

fn explicit_anchor_marked(node: &DocumentNode) -> bool {
    node.doc
        .attributes
        .get("explicit_anchor")
        .is_some_and(|value| {
            let value = value.trim();
            !value.is_empty()
                && !matches!(value.to_ascii_lowercase().as_str(), "false" | "0" | "no")
        })
}

/// `true` when the node text contains an explicit `[[anchor]]` marker.
fn source_contains_explicit_anchor(node: &DocumentNode) -> bool {
    if let Some(parsed) = node.parsed.as_ref()
        && !parsed.raw_content.is_empty()
    {
        return parsed.raw_content.contains("[[");
    }
    std::fs::read_to_string(&node.source_path)
        .map(|text| text.contains("[["))
        .unwrap_or(false)
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
            suggestion: Some(
                "Break the circular include chain by removing one of the includes.".to_string(),
            ),
            raw: serde_json::to_string(&serde_json::json!({
                "cycle": cycle,
            }))
            .unwrap_or_default(),
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
                    }))
                    .unwrap_or_default(),
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
            // Self-references in prose/doc files (scaffolding that defines and
            // links its own anchor) are expected, not defects — Info, to match
            // `status`. Low confidence on a code node stays a Warning.
            let is_doc = is_doc_source(&node.source_path);
            issues.push(Issue {
                category: IssueCategory::LowConfidence,
                severity: if is_doc {
                    Severity::Info
                } else {
                    Severity::Warning
                },
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
                }))
                .unwrap_or_default(),
            });
        }
    }

    issues
}

// ── Structural scanners (indexed code graph) ──────────────────────

const COUPLING_HOTSPOT_CAP: usize = 10;
const GOD_HUB_CAP: usize = 10;
const DUPLICATE_MODULE_CAP: usize = 15;
const DUPLICATE_PAIR_COMPARISON_CAP: usize = 500;

/// Cross-area directed edges at or above `rules.coupling_hotspot_threshold`.
///
/// Areas are the first two `/`-separated segments of a code node's
/// `source_file` (or the file embedded in an `aden://module/` anchor).
/// `PartOf` / `Contains` edges that touch synthetic `mod-*` hubs are ignored.
/// Results are the top 10 pairs by edge count, then area name.
fn scan_coupling_hotspots(
    code_graph: &AdenGraph<DocumentNode, AdenEdge>,
    rules: &DiagnosticRules,
) -> Vec<Issue> {
    let mut area_of: HashMap<NodeIndex, String> = HashMap::new();
    for idx in code_graph.graph.node_indices() {
        let node = &code_graph.graph[idx];
        if let Some(area) = code_node_area(node) {
            area_of.insert(idx, area);
        }
    }

    let mut counts: HashMap<(String, String), usize> = HashMap::new();
    for edge_idx in code_graph.graph.edge_indices() {
        let Some((src, tgt)) = code_graph.graph.edge_endpoints(edge_idx) else {
            continue;
        };
        if synthetic_mod_edge(
            code_graph.graph[edge_idx].edge_type,
            code_graph.graph[src].anchor(),
            code_graph.graph[tgt].anchor(),
        ) {
            continue;
        }
        let (Some(area_a), Some(area_b)) = (area_of.get(&src), area_of.get(&tgt)) else {
            continue;
        };
        if area_a == area_b {
            continue;
        }
        *counts.entry((area_a.clone(), area_b.clone())).or_default() += 1;
    }

    let mut pairs: Vec<((String, String), usize)> = counts
        .into_iter()
        .filter(|(_, count)| *count >= rules.coupling_hotspot_threshold)
        .collect();
    pairs.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| left.0.0.cmp(&right.0.0))
            .then_with(|| left.0.1.cmp(&right.0.1))
    });
    pairs.truncate(COUPLING_HOTSPOT_CAP);

    pairs
        .into_iter()
        .map(|((area_a, area_b), edge_count)| Issue {
            category: IssueCategory::CouplingHotspot,
            severity: Severity::Info,
            file: None,
            line: None,
            message: format!(
                "Coupling hotspot: {area_a} -> {area_b} ({edge_count} cross-area edges)"
            ),
            suggestion: Some(
                "Inspect this boundary and narrow the dependency surface.".to_string(),
            ),
            raw: serde_json::to_string(&serde_json::json!({
                "area_a": area_a,
                "area_b": area_b,
                "edge_count": edge_count,
            }))
            .unwrap_or_default(),
        })
        .collect()
}

/// Code symbols whose directed degree meets `rules.god_hub_degree_threshold`.
///
/// Prose paths (`.md` / `.adoc` / `.txt`) and synthetic `mod-*` containment
/// edges are ignored. Results are the top 10 by total degree, then anchor.
fn scan_god_hubs(
    code_graph: &AdenGraph<DocumentNode, AdenEdge>,
    rules: &DiagnosticRules,
) -> Vec<Issue> {
    let mut degree: HashMap<NodeIndex, (usize, usize)> = HashMap::new();
    for idx in code_graph.graph.node_indices() {
        if is_god_hub_candidate(&code_graph.graph[idx]) {
            degree.insert(idx, (0, 0));
        }
    }

    for edge_idx in code_graph.graph.edge_indices() {
        let Some((src, tgt)) = code_graph.graph.edge_endpoints(edge_idx) else {
            continue;
        };
        if synthetic_mod_edge(
            code_graph.graph[edge_idx].edge_type,
            code_graph.graph[src].anchor(),
            code_graph.graph[tgt].anchor(),
        ) {
            continue;
        }
        if let Some(ends) = degree.get_mut(&src) {
            ends.1 += 1;
        }
        if let Some(ends) = degree.get_mut(&tgt) {
            ends.0 += 1;
        }
    }

    let mut hubs: Vec<(String, String, usize, usize)> = degree
        .into_iter()
        .filter_map(|(idx, (in_degree, out_degree))| {
            let total = in_degree + out_degree;
            if total < rules.god_hub_degree_threshold {
                return None;
            }
            let node = &code_graph.graph[idx];
            let file = indexed_source_file(node).unwrap_or_else(|| node.anchor().to_string());
            Some((node.anchor().to_string(), file, in_degree, out_degree))
        })
        .collect();
    hubs.sort_by(|left, right| {
        let left_total = left.2 + left.3;
        let right_total = right.2 + right.3;
        right_total
            .cmp(&left_total)
            .then_with(|| left.0.cmp(&right.0))
    });
    hubs.truncate(GOD_HUB_CAP);

    hubs.into_iter()
        .map(|(anchor, file, in_degree, out_degree)| {
            let total_degree = in_degree + out_degree;
            Issue {
                category: IssueCategory::GodHub,
                severity: Severity::Info,
                file: Some(file),
                line: None,
                message: format!(
                    "God hub: {anchor} degree {total_degree} (in {in_degree}, out {out_degree})"
                ),
                suggestion: Some(
                    "Split this hub or push dependencies behind a narrower interface.".to_string(),
                ),
                raw: serde_json::to_string(&serde_json::json!({
                    "anchor": anchor,
                    "in_degree": in_degree,
                    "out_degree": out_degree,
                    "total_degree": total_degree,
                }))
                .unwrap_or_default(),
            }
        })
        .collect()
}

/// Same-named modules in different coarse areas with high verbatim line overlap.
///
/// Generic names (`index.*`, `mod.rs`, …) and test paths are skipped. At most
/// 500 cross-area pairs are read, and at most 15 hits are returned.
fn scan_duplicate_modules(
    code_graph: &AdenGraph<DocumentNode, AdenEdge>,
    path: &Path,
    rules: &DiagnosticRules,
) -> Vec<Issue> {
    let mut files: BTreeSet<String> = BTreeSet::new();
    for idx in code_graph.graph.node_indices() {
        let node = &code_graph.graph[idx];
        if !node.anchor().starts_with("aden://module/") {
            continue;
        }
        let Some(source) = node.doc.attributes.get("source_file") else {
            continue;
        };
        let normalized = source.replace('\\', "/");
        if normalized.is_empty() || is_skipped_duplicate_path(&normalized) {
            continue;
        }
        files.insert(normalized);
    }

    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for file in files {
        let base = Path::new(&file)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&file);
        groups
            .entry(base.to_ascii_lowercase())
            .or_default()
            .push(file);
    }

    struct Hit {
        file_a: String,
        file_b: String,
        overlap: f64,
        shared: usize,
    }

    let mut hits: Vec<Hit> = Vec::new();
    let mut comparisons = 0usize;
    'groups: for group in groups.values() {
        if group.len() < 2 {
            continue;
        }
        for left in 0..group.len() {
            for right in (left + 1)..group.len() {
                if comparisons >= DUPLICATE_PAIR_COMPARISON_CAP {
                    break 'groups;
                }
                let file_a = &group[left];
                let file_b = &group[right];
                if coarse_area(file_a) == coarse_area(file_b) {
                    continue;
                }
                comparisons += 1;
                let Some((overlap, shared)) =
                    line_overlap(path, file_a, file_b, rules.duplicate_module_min_overlap)
                else {
                    continue;
                };
                hits.push(Hit {
                    file_a: file_a.clone(),
                    file_b: file_b.clone(),
                    overlap,
                    shared,
                });
            }
        }
    }

    hits.sort_by(|left, right| {
        right
            .overlap
            .total_cmp(&left.overlap)
            .then_with(|| right.shared.cmp(&left.shared))
            .then_with(|| left.file_a.cmp(&right.file_a))
            .then_with(|| left.file_b.cmp(&right.file_b))
    });
    hits.truncate(DUPLICATE_MODULE_CAP);

    hits.into_iter()
        .map(|hit| {
            let percent = hit.overlap * 100.0;
            Issue {
                category: IssueCategory::DuplicateModule,
                severity: Severity::Info,
                file: Some(hit.file_a.clone()),
                line: None,
                message: format!(
                    "Duplicate module: {} and {} overlap {percent:.0}% ({} shared lines)",
                    hit.file_a, hit.file_b, hit.shared
                ),
                suggestion: Some("Share one module or drop the twin copy.".to_string()),
                raw: serde_json::to_string(&serde_json::json!({
                    "file_a": hit.file_a,
                    "file_b": hit.file_b,
                    "overlap_ratio": hit.overlap,
                    "shared_lines": hit.shared,
                }))
                .unwrap_or_default(),
            }
        })
        .collect()
}

fn code_node_area(node: &DocumentNode) -> Option<String> {
    if !node.anchor().starts_with("aden://module/") {
        return None;
    }
    indexed_source_file(node).map(|path| coarse_area(&path))
}

fn indexed_source_file(node: &DocumentNode) -> Option<String> {
    if let Some(source) = node.doc.attributes.get("source_file")
        && !source.is_empty()
    {
        return Some(source.replace('\\', "/"));
    }
    anchor_file(node.anchor()).map(str::to_string)
}

/// File portion of `aden://module/<file>#<symbol>`.
fn anchor_file(anchor: &str) -> Option<&str> {
    let rest = anchor.strip_prefix("aden://module/")?;
    let file = rest.split('#').next().unwrap_or(rest);
    if file.is_empty() { None } else { Some(file) }
}

/// First two path segments (`apps/web`, `crates/aden-cli`), else the first.
fn coarse_area(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let trimmed = normalized.trim_start_matches("./");
    let mut segments = trimmed.split('/').filter(|segment| !segment.is_empty());
    match (segments.next(), segments.next()) {
        (Some(first), Some(second)) => format!("{first}/{second}"),
        (Some(first), None) => first.to_string(),
        (None, _) => "unknown".to_string(),
    }
}

fn is_god_hub_candidate(node: &DocumentNode) -> bool {
    if !node.anchor().starts_with("aden://module/") {
        return false;
    }
    node.doc
        .attributes
        .get("source_file")
        .is_none_or(|path| !ends_with_prose_doc(path))
}

fn ends_with_prose_doc(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".md") || lower.ends_with(".adoc") || lower.ends_with(".txt")
}

fn synthetic_mod_edge(edge_type: EdgeType, src_anchor: &str, tgt_anchor: &str) -> bool {
    matches!(edge_type, EdgeType::Contains | EdgeType::PartOf)
        && (src_anchor.starts_with("mod-") || tgt_anchor.starts_with("mod-"))
}

fn is_skipped_duplicate_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    if normalized.contains(".test.")
        || normalized.contains(".spec.")
        || normalized.contains("_test.")
        || has_path_segment(&normalized, "__tests__")
        || has_path_segment(&normalized, "tests")
        || has_path_segment(&normalized, "test")
    {
        return true;
    }
    let name = normalized.rsplit('/').next().unwrap_or(&normalized);
    is_generic_filename(name)
}

fn has_path_segment(path: &str, segment: &str) -> bool {
    path.split('/').any(|part| part == segment)
}

fn is_generic_filename(name: &str) -> bool {
    if name == "mod.rs" || name == "lib.rs" || name == "__init__.py" {
        return true;
    }
    const STEMS: &[&str] = &["index", "main", "types", "utils", "constants", "readme"];
    STEMS.iter().any(|stem| {
        name.strip_prefix(stem)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
    })
}

fn line_overlap(root: &Path, file_a: &str, file_b: &str, min_overlap: f64) -> Option<(f64, usize)> {
    let text_a = std::fs::read_to_string(resolve_under(root, file_a)).ok()?;
    let text_b = std::fs::read_to_string(resolve_under(root, file_b)).ok()?;
    let lines_a = unique_trimmed_lines(&text_a);
    let lines_b = unique_trimmed_lines(&text_b);
    let min_len = lines_a.len().min(lines_b.len());
    if min_len < 10 {
        return None;
    }
    let shared = lines_a.intersection(&lines_b).count();
    let overlap = shared as f64 / min_len as f64;
    if overlap < min_overlap {
        return None;
    }
    Some((overlap, shared))
}

fn unique_trimmed_lines(text: &str) -> HashSet<&str> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect()
}

fn resolve_under(root: &Path, relative: &str) -> PathBuf {
    let candidate = PathBuf::from(relative);
    if candidate.is_absolute() {
        candidate
    } else {
        root.join(candidate)
    }
}

// ── Helpers ───────────────────────────────────────────────────────

/// Recursively collect `.adoc` files from a directory, honoring the same
/// exclusion rules as the rest of aden (`AdenFilter`). Without this, the scan
/// descended into excluded scaffolding such as `.agent/` (and `.agent/templates/`)
/// and `.claude/`, producing phantom DuplicateAnchor findings between a generated
/// file and its template — anchors that never coexist in the real graph.
fn collect_adoc_files(root: &Path) -> Vec<PathBuf> {
    let filter = aden_core::filter::AdenFilter::from_directory(root);
    let mut files = Vec::new();
    collect_adoc_files_inner(root, root, &filter, &mut files);
    files.sort();
    files
}

fn collect_adoc_files_inner(
    root: &Path,
    dir: &Path,
    filter: &aden_core::filter::AdenFilter,
    files: &mut Vec<PathBuf>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        // AdenFilter rules are expressed relative to the project root.
        let rel = path.strip_prefix(root).unwrap_or(&path);
        if filter.should_skip(rel) {
            continue;
        }
        if path.is_dir() {
            collect_adoc_files_inner(root, &path, filter, files);
        } else if path.extension().and_then(|e| e.to_str()) == Some("adoc") {
            files.push(path);
        }
    }
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
        assert!((score - 94.0).abs() < 0.01); // 100 - 5 (error) - 1 (warning) - 0 (info) = 94.0
    }

    #[test]
    fn test_health_score_floor() {
        let issues: Vec<Issue> = (0..20)
            .map(|_| Issue {
                category: IssueCategory::UnresolvedRef,
                severity: Severity::Error,
                file: None,
                line: None,
                message: "test".to_string(),
                suggestion: None,
                raw: String::new(),
            })
            .collect();

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
        let unresolved = diagnosis
            .issues
            .iter()
            .filter(|i| i.category == IssueCategory::UnresolvedRef)
            .count();
        assert!(
            unresolved > 0,
            "Expected unresolved ref, got: {:?}",
            diagnosis.issues
        );
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
        let duplicates = diagnosis
            .issues
            .iter()
            .filter(|i| i.category == IssueCategory::DuplicateAnchor)
            .count();
        assert!(
            duplicates > 0,
            "Expected duplicate anchor, got: {:?}",
            diagnosis.issues
        );
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
        let custom = diagnosis
            .issues
            .iter()
            .filter(|i| i.category == IssueCategory::Custom("test-rule".to_string()))
            .count();
        assert_eq!(custom, 1);
    }

    #[test]
    fn test_metadata_prefixes() {
        let dir = make_temp_dir("metadata-prefixes");
        // ADR with no edges — orphan, Info (matches metadata prefix).
        write_file(
            &dir,
            "adr-001.adoc",
            "[[adr-001]]\n= ADR 001\n\nSome decision.\n",
        );
        // A plain doc with no edges — also Info: an orphaned prose/doc file is
        // reference material, not actionable dead code. Only orphaned CODE symbols
        // are Warnings. This keeps `diagnose` aligned with `status`/`check` (which
        // both treat doc orphans as expected metadata) so their counts can't diverge.
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
        let orphans = diagnosis
            .issues
            .iter()
            .filter(|i| i.category == IssueCategory::OrphanDocument)
            .collect::<Vec<_>>();
        assert!(
            orphans.len() >= 2,
            "Expected at least 2 orphans, got: {}",
            orphans.len()
        );
        for orphan in orphans {
            // Every orphan here is sourced from an .adoc (prose) file, so all are Info.
            assert_eq!(
                orphan.severity,
                Severity::Info,
                "Doc/prose orphan should be Info, not {:?} ({:?})",
                orphan.severity,
                orphan.file
            );
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
        let stale_refs = diagnosis
            .issues
            .iter()
            .filter(|i| i.category == IssueCategory::UnresolvedRef && i.message.contains("adr-999"))
            .count();
        assert!(
            stale_refs > 0,
            "Expected stale ref, got: {:?}",
            diagnosis.issues
        );
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
        let contract_refs = diagnosis
            .issues
            .iter()
            .filter(|i| i.category == IssueCategory::Custom("contract-ref".to_string()))
            .count();
        assert!(
            contract_refs > 0,
            "Expected contract ref issue, got: {:?}",
            diagnosis.issues
        );
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
        write_file(&dir, "doc-a.adoc", "[[doc-a]]\n= Document A\n\nContent.\n");
        write_file(&dir, "doc-b.adoc", "[[doc-b]]\n= Document B\n\nContent.\n");
        let result = diagnose(&dir);
        assert!(result.is_ok());
        let diagnosis = result.unwrap();
        let orphans = diagnosis
            .issues
            .iter()
            .filter(|i| i.category == IssueCategory::OrphanDocument)
            .count();
        // Both docs are orphans (no edges between them since they don't reference each other)
        assert!(
            orphans >= 1,
            "Expected at least 1 orphan, got: {:?}",
            diagnosis.issues
        );
    }

    #[test]
    fn test_orphan_dedup_for_colliding_anchors() {
        // Two distinct files that collapse to the SAME anchor must yield ONE
        // orphan issue, not one per node. `orphans()` iterates by node (so it
        // returns the anchor once per colliding node), but `get_node()` resolves
        // them all to a single node — without the dedup in `scan_orphans` this
        // emits N identical "[[dup]] has no edges" lines that read as a bug.
        let dir = make_temp_dir("orphan-dedup");
        write_file(&dir, "one.adoc", "[[dup]]\n= First\n\nContent.\n");
        write_file(&dir, "two.adoc", "[[dup]]\n= Second\n\nContent.\n");
        let diagnosis = diagnose(&dir).expect("diagnose should succeed");
        let dup_orphans = diagnosis
            .issues
            .iter()
            .filter(|i| i.category == IssueCategory::OrphanDocument)
            .filter(|i| i.message.contains("[[dup]]"))
            .count();
        assert_eq!(
            dup_orphans, 1,
            "colliding anchor [[dup]] must produce exactly one orphan issue, got {}: {:?}",
            dup_orphans, diagnosis.issues
        );
    }

    #[test]
    fn test_severity_weights() {
        assert_eq!(Severity::Error.weight(), 5.0);
        assert_eq!(Severity::Warning.weight(), 1.0);
        assert_eq!(Severity::Info.weight(), 0.0);
    }

    #[test]
    fn test_issue_category_display() {
        assert_eq!(
            format!("{}", IssueCategory::UnresolvedRef),
            "unresolved-ref"
        );
        assert_eq!(
            format!("{}", IssueCategory::DuplicateAnchor),
            "duplicate-anchor"
        );
        assert_eq!(
            format!("{}", IssueCategory::EdgeViolation),
            "edge-violation"
        );
        assert_eq!(
            format!("{}", IssueCategory::OrphanDocument),
            "orphan-document"
        );
        assert_eq!(format!("{}", IssueCategory::IncludeCycle), "include-cycle");
        assert_eq!(
            format!("{}", IssueCategory::MissingSource),
            "missing-source"
        );
        assert_eq!(
            format!("{}", IssueCategory::LowConfidence),
            "low-confidence"
        );
        assert_eq!(
            format!("{}", IssueCategory::CouplingHotspot),
            "coupling-hotspot"
        );
        assert_eq!(format!("{}", IssueCategory::GodHub), "god-hub");
        assert_eq!(
            format!("{}", IssueCategory::DuplicateModule),
            "duplicate-module"
        );
        assert_eq!(
            format!("{}", IssueCategory::Custom("test".to_string())),
            "custom-test"
        );
    }

    #[test]
    fn test_structural_issue_category_serde_round_trip() {
        for category in [
            IssueCategory::CouplingHotspot,
            IssueCategory::GodHub,
            IssueCategory::DuplicateModule,
        ] {
            let json = serde_json::to_string(&category).expect("serialise category");
            let back: IssueCategory = serde_json::from_str(&json).expect("deserialise category");
            assert_eq!(back, category);
        }
    }

    #[test]
    fn test_suppressed_count_defaults_when_absent() {
        let legacy = r#"{"issues":[],"error_count":0,"warning_count":0,"info_count":0,"health_score":100.0}"#;
        let diagnosis: Diagnosis = serde_json::from_str(legacy).expect("legacy diagnosis");
        assert_eq!(diagnosis.suppressed_plain_doc_orphans, 0);
    }

    #[test]
    fn test_default_rules_keep_existing_switches() {
        let rules = DiagnosticRules::default();
        assert!(!rules.suppress_plain_doc_orphans);
        assert!(rules.enable_structural_diagnostics);
        assert_eq!(rules.coupling_hotspot_threshold, 25);
        assert_eq!(rules.god_hub_degree_threshold, 25);
        assert!((rules.duplicate_module_min_overlap - 0.30).abs() < f64::EPSILON);
    }

    #[test]
    fn test_suppress_plain_doc_orphans_keeps_adoc() {
        let dir = make_temp_dir("suppress-plain-orphans");
        write_file(&dir, "notes.md", "# Notes\n\nJust prose without anchors.\n");
        write_file(&dir, "also.txt", "plain text file\nno anchors here\n");
        write_file(&dir, "guide.adoc", "[[guide]]\n= Guide\n\nNo refs.\n");
        write_file(
            &dir,
            "explicit.md",
            "[[explicit-md]]\n\nThis markdown uses an AsciiDoc anchor.\n",
        );

        let rules = DiagnosticRules {
            suppress_plain_doc_orphans: true,
            ..Default::default()
        };
        let diagnosis = diagnose_with_rules(&dir, &rules).expect("diagnose");
        let orphan_files: Vec<String> = diagnosis
            .issues
            .iter()
            .filter(|issue| issue.category == IssueCategory::OrphanDocument)
            .filter_map(|issue| issue.file.clone())
            .collect();
        assert!(
            orphan_files.iter().any(|file| file.ends_with("guide.adoc")),
            "adoc orphan must remain, got {orphan_files:?}"
        );
        assert!(
            orphan_files
                .iter()
                .any(|file| file.ends_with("explicit.md")),
            "markdown with [[anchor]] must remain, got {orphan_files:?}"
        );
        assert!(
            orphan_files
                .iter()
                .all(|file| !file.ends_with("notes.md") && !file.ends_with("also.txt")),
            "plain markdown/text orphans must be hidden, got {orphan_files:?}"
        );
        assert_eq!(diagnosis.suppressed_plain_doc_orphans, 2);
        assert_eq!(diagnosis.error_count, 0);
        assert_eq!(diagnosis.warning_count, 0);

        let baseline = diagnose(&dir).expect("baseline diagnose");
        assert_eq!(baseline.suppressed_plain_doc_orphans, 0);
        let baseline_orphans = baseline
            .issues
            .iter()
            .filter(|issue| issue.category == IssueCategory::OrphanDocument)
            .count();
        assert!(baseline_orphans > orphan_files.len());
    }

    fn push_node(
        graph: &mut AdenGraph<DocumentNode, AdenEdge>,
        anchor: &str,
        source_file: Option<&str>,
    ) {
        let mut attributes = HashMap::new();
        if let Some(source_file) = source_file {
            attributes.insert("source_file".to_string(), source_file.to_string());
        }
        graph
            .add_node(DocumentNode {
                doc: aden_core::Document {
                    anchor: anchor.to_string(),
                    node_type: aden_core::NodeType::Function,
                    attributes,
                    blocks: Vec::new(),
                    source_span: None,
                    metadata: None,
                    confidence: 1.0,
                },
                source_path: PathBuf::from(source_file.unwrap_or(anchor)),
                parsed: None,
            })
            .expect("add node");
    }

    fn link(
        graph: &mut AdenGraph<DocumentNode, AdenEdge>,
        src: &str,
        tgt: &str,
        edge_type: EdgeType,
    ) {
        graph
            .add_edge_by_anchor(src, tgt, AdenEdge { edge_type })
            .expect("add edge");
    }

    #[test]
    fn test_scan_coupling_hotspots_orders_cross_area_edges() {
        let mut graph = AdenGraph::new();
        push_node(
            &mut graph,
            "aden://module/apps/web/src/a.ts#w1",
            Some("apps/web/src/a.ts"),
        );
        push_node(
            &mut graph,
            "aden://module/apps/web/src/b.ts#w2",
            Some("apps/web/src/b.ts"),
        );
        push_node(&mut graph, "aden://module/apps/web/src/c.ts#w3", None);
        push_node(
            &mut graph,
            "aden://module/apps/mobile/src/a.ts#m1",
            Some("apps/mobile/src/a.ts"),
        );
        push_node(
            &mut graph,
            "aden://module/apps/mobile/src/b.ts#m2",
            Some("apps/mobile/src/b.ts"),
        );
        push_node(
            &mut graph,
            "aden://module/packages/contracts/src/api.ts#c1",
            Some("packages/contracts/src/api.ts"),
        );
        push_node(&mut graph, "mod-web", None);
        push_node(&mut graph, "aden://doc/readme#h", Some("docs/readme.md"));

        link(
            &mut graph,
            "aden://module/apps/web/src/a.ts#w1",
            "aden://module/apps/mobile/src/a.ts#m1",
            EdgeType::Calls,
        );
        link(
            &mut graph,
            "aden://module/apps/web/src/b.ts#w2",
            "aden://module/apps/mobile/src/a.ts#m1",
            EdgeType::Calls,
        );
        link(
            &mut graph,
            "aden://module/apps/web/src/a.ts#w1",
            "aden://module/apps/mobile/src/b.ts#m2",
            EdgeType::Imports,
        );
        // Anchor file fallback: aden://module/apps/web/src/c.ts → area apps/web.
        link(
            &mut graph,
            "aden://module/apps/web/src/c.ts#w3",
            "aden://module/apps/mobile/src/a.ts#m1",
            EdgeType::Calls,
        );

        link(
            &mut graph,
            "aden://module/apps/mobile/src/a.ts#m1",
            "aden://module/apps/web/src/a.ts#w1",
            EdgeType::Calls,
        );
        link(
            &mut graph,
            "aden://module/apps/mobile/src/b.ts#m2",
            "aden://module/apps/web/src/b.ts#w2",
            EdgeType::Calls,
        );
        link(
            &mut graph,
            "aden://module/apps/mobile/src/a.ts#m1",
            "aden://module/apps/web/src/b.ts#w2",
            EdgeType::Imports,
        );

        link(
            &mut graph,
            "aden://module/packages/contracts/src/api.ts#c1",
            "aden://module/apps/web/src/a.ts#w1",
            EdgeType::Calls,
        );
        link(
            &mut graph,
            "aden://module/packages/contracts/src/api.ts#c1",
            "aden://module/apps/web/src/a.ts#w1",
            EdgeType::Imports,
        );
        link(
            &mut graph,
            "aden://module/packages/contracts/src/api.ts#c1",
            "aden://module/apps/web/src/a.ts#w1",
            EdgeType::Uses,
        );

        // Same-area and synthetic containment must not count.
        link(
            &mut graph,
            "aden://module/apps/web/src/a.ts#w1",
            "aden://module/apps/web/src/b.ts#w2",
            EdgeType::Calls,
        );
        link(
            &mut graph,
            "mod-web",
            "aden://module/apps/web/src/a.ts#w1",
            EdgeType::Contains,
        );
        link(
            &mut graph,
            "aden://module/apps/web/src/a.ts#w1",
            "mod-web",
            EdgeType::PartOf,
        );
        // Below the threshold.
        link(
            &mut graph,
            "aden://module/apps/web/src/a.ts#w1",
            "aden://module/packages/contracts/src/api.ts#c1",
            EdgeType::Calls,
        );

        let rules = DiagnosticRules {
            coupling_hotspot_threshold: 3,
            ..Default::default()
        };
        let issues = scan_coupling_hotspots(&graph, &rules);
        assert!(issues.iter().all(|issue| issue.severity == Severity::Info));
        assert!(
            issues
                .iter()
                .all(|issue| issue.category == IssueCategory::CouplingHotspot)
        );
        let pairs: Vec<(String, String, u64)> = issues
            .iter()
            .map(|issue| {
                let raw: serde_json::Value = serde_json::from_str(&issue.raw).expect("raw json");
                (
                    raw["area_a"].as_str().unwrap().to_string(),
                    raw["area_b"].as_str().unwrap().to_string(),
                    raw["edge_count"].as_u64().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("apps/web".to_string(), "apps/mobile".to_string(), 4),
                ("apps/mobile".to_string(), "apps/web".to_string(), 3),
                ("packages/contracts".to_string(), "apps/web".to_string(), 3),
            ]
        );
    }

    #[test]
    fn test_scan_god_hubs_ignores_prose_and_mod_containment() {
        let mut graph = AdenGraph::new();
        let hub = "aden://module/apps/web/src/hub.ts#Hub";
        let a_hub = "aden://module/apps/web/src/a_hub.ts#A";
        let b_hub = "aden://module/apps/web/src/b_hub.ts#B";
        push_node(&mut graph, hub, Some("apps/web/src/hub.ts"));
        push_node(&mut graph, a_hub, Some("apps/web/src/a_hub.ts"));
        push_node(&mut graph, b_hub, Some("apps/web/src/b_hub.ts"));
        for name in [
            "h1", "h2", "h3", "h4", "a1", "a2", "a3", "b1", "b2", "b3", "r1", "r2", "r3", "r4",
        ] {
            push_node(
                &mut graph,
                &format!("aden://module/apps/web/src/{name}.ts#{name}"),
                Some(&format!("apps/web/src/{name}.ts")),
            );
        }
        push_node(
            &mut graph,
            "aden://module/apps/web/README.md#doc",
            Some("apps/web/README.md"),
        );
        push_node(&mut graph, "mod-project", None);

        for name in ["h1", "h2", "h3", "h4"] {
            link(
                &mut graph,
                hub,
                &format!("aden://module/apps/web/src/{name}.ts#{name}"),
                EdgeType::Calls,
            );
        }
        link(
            &mut graph,
            "aden://module/apps/web/src/h1.ts#h1",
            hub,
            EdgeType::Calls,
        );
        link(
            &mut graph,
            "aden://module/apps/web/src/h2.ts#h2",
            hub,
            EdgeType::Calls,
        );
        for name in ["a1", "a2", "a3"] {
            link(
                &mut graph,
                a_hub,
                &format!("aden://module/apps/web/src/{name}.ts#{name}"),
                EdgeType::Calls,
            );
        }
        link(
            &mut graph,
            "aden://module/apps/web/src/a1.ts#a1",
            a_hub,
            EdgeType::Calls,
        );
        for name in ["b1", "b2", "b3"] {
            link(
                &mut graph,
                b_hub,
                &format!("aden://module/apps/web/src/{name}.ts#{name}"),
                EdgeType::Calls,
            );
        }
        link(
            &mut graph,
            "aden://module/apps/web/src/b1.ts#b1",
            b_hub,
            EdgeType::Calls,
        );
        for name in ["r1", "r2", "r3", "r4"] {
            link(
                &mut graph,
                "aden://module/apps/web/README.md#doc",
                &format!("aden://module/apps/web/src/{name}.ts#{name}"),
                EdgeType::Calls,
            );
        }
        link(&mut graph, "mod-project", hub, EdgeType::Contains);
        link(&mut graph, hub, "mod-project", EdgeType::PartOf);

        let rules = DiagnosticRules {
            god_hub_degree_threshold: 4,
            ..Default::default()
        };
        let issues = scan_god_hubs(&graph, &rules);
        assert!(issues.iter().all(|issue| issue.severity == Severity::Info));
        let hubs: Vec<(String, u64, u64, u64)> = issues
            .iter()
            .map(|issue| {
                let raw: serde_json::Value = serde_json::from_str(&issue.raw).expect("raw json");
                (
                    raw["anchor"].as_str().unwrap().to_string(),
                    raw["in_degree"].as_u64().unwrap(),
                    raw["out_degree"].as_u64().unwrap(),
                    raw["total_degree"].as_u64().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            hubs,
            vec![
                (hub.to_string(), 2, 4, 6),
                (a_hub.to_string(), 1, 3, 4),
                (b_hub.to_string(), 1, 3, 4),
            ]
        );
        assert_eq!(issues[0].file.as_deref(), Some("apps/web/src/hub.ts"));
        assert!(issues.iter().all(|issue| {
            issue
                .file
                .as_deref()
                .is_none_or(|file| !file.ends_with(".md"))
        }));
    }

    #[test]
    fn test_scan_duplicate_modules_overlap_across_areas() {
        let dir = make_temp_dir("duplicate-modules");
        let sheet: String = (0..12).map(|i| format!("sheet line {i}\n")).collect();
        let partial_a: String = (0..10).map(|i| format!("p {i}\n")).collect();
        let mut partial_b = String::new();
        for i in 0..3 {
            partial_b.push_str(&format!("p {i}\n"));
        }
        for i in 0..7 {
            partial_b.push_str(&format!("q {i}\n"));
        }
        let same: String = (0..12).map(|i| format!("same {i}\n")).collect();
        let loose_a: String = (0..12).map(|i| format!("left {i}\n")).collect();
        let loose_b: String = (0..12).map(|i| format!("right {i}\n")).collect();
        let tiny = "one\ntwo\nthree\nfour\nfive\n";

        for (rel, body) in [
            ("apps/web/src/NewTaskSheet.tsx", sheet.as_str()),
            ("apps/mobile/src/NewTaskSheet.tsx", sheet.as_str()),
            ("apps/web/src/Partial.tsx", partial_a.as_str()),
            ("apps/mobile/src/Partial.tsx", partial_b.as_str()),
            ("apps/web/src/a/Same.tsx", same.as_str()),
            ("apps/web/src/b/Same.tsx", same.as_str()),
            ("apps/web/src/index.tsx", same.as_str()),
            ("apps/mobile/src/index.tsx", same.as_str()),
            ("apps/web/src/Widget.test.tsx", same.as_str()),
            ("apps/mobile/src/Widget.test.tsx", same.as_str()),
            ("apps/web/src/Tiny.tsx", tiny),
            ("apps/mobile/src/Tiny.tsx", tiny),
            ("apps/web/src/Loose.tsx", loose_a.as_str()),
            ("apps/mobile/src/Loose.tsx", loose_b.as_str()),
        ] {
            let path = dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }

        let mut graph = AdenGraph::new();
        for rel in [
            "apps/web/src/NewTaskSheet.tsx",
            "apps/mobile/src/NewTaskSheet.tsx",
            "apps/web/src/Partial.tsx",
            "apps/mobile/src/Partial.tsx",
            "apps/web/src/a/Same.tsx",
            "apps/web/src/b/Same.tsx",
            "apps/web/src/index.tsx",
            "apps/mobile/src/index.tsx",
            "apps/web/src/Widget.test.tsx",
            "apps/mobile/src/Widget.test.tsx",
            "apps/web/src/Tiny.tsx",
            "apps/mobile/src/Tiny.tsx",
            "apps/web/src/Loose.tsx",
            "apps/mobile/src/Loose.tsx",
        ] {
            push_node(&mut graph, &format!("aden://module/{rel}#sym"), Some(rel));
        }

        let rules = DiagnosticRules {
            duplicate_module_min_overlap: 0.30,
            ..Default::default()
        };
        let issues = scan_duplicate_modules(&graph, &dir, &rules);
        assert_eq!(issues.len(), 2);
        assert!(issues.iter().all(|issue| {
            issue.category == IssueCategory::DuplicateModule && issue.severity == Severity::Info
        }));
        let pairs: Vec<(String, String, f64, u64)> = issues
            .iter()
            .map(|issue| {
                let raw: serde_json::Value = serde_json::from_str(&issue.raw).expect("raw json");
                (
                    raw["file_a"].as_str().unwrap().to_string(),
                    raw["file_b"].as_str().unwrap().to_string(),
                    raw["overlap_ratio"].as_f64().unwrap(),
                    raw["shared_lines"].as_u64().unwrap(),
                )
            })
            .collect();
        assert_eq!(pairs[0].0, "apps/mobile/src/NewTaskSheet.tsx");
        assert_eq!(pairs[0].1, "apps/web/src/NewTaskSheet.tsx");
        assert!((pairs[0].2 - 1.0).abs() < f64::EPSILON);
        assert_eq!(pairs[0].3, 12);
        assert_eq!(issues[0].file.as_deref(), Some(pairs[0].0.as_str()));
        assert_eq!(pairs[1].0, "apps/mobile/src/Partial.tsx");
        assert_eq!(pairs[1].1, "apps/web/src/Partial.tsx");
        assert!((pairs[1].2 - 0.3).abs() < 1e-9);
        assert_eq!(pairs[1].3, 3);
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
