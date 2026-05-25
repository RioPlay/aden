// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// All rights reserved.
//
// Aden: A Dense Referential Context Compiler
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Simulation module for Aden: diff predictions, history analysis, adversarial testing.
//!
//! Phase 5 of the Aden roadmap.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractDiff {
    pub anchor: String,
    pub added_blocks: usize,
    pub removed_blocks: usize,
    pub modified_blocks: usize,
    pub violations: Vec<ContractViolation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractViolation {
    pub severity: String,
    pub block_tag: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub timestamp: String,
    pub anchor: String,
    pub change_type: ChangeType,
    pub edge_growth: i32,
    pub override_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChangeType {
    Added,
    Removed,
    Modified,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreatModelReport {
    pub module: String,
    pub risk_score: f64,
    pub high_risk_overrides: usize,
    pub recommendations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdversarialTest {
    pub name: String,
    pub input: String,
    pub expected_violation: bool,
    pub actual_violation: bool,
    pub passed: bool,
}

pub fn simulate_diff(current: &[(String, String)], proposed: &[(String, String)]) -> ContractDiff {
    let current_map: HashMap<_, _> = current.iter().cloned().collect();
    let proposed_map: HashMap<_, _> = proposed.iter().cloned().collect();

    let mut added = 0;
    let mut removed = 0;
    let mut modified = 0;

    for (anchor, content) in proposed {
        if !current_map.contains_key(anchor) {
            added += 1;
        } else if current_map.get(anchor) != Some(content) {
            modified += 1;
        }
    }

    for (anchor, _) in current {
        if !proposed_map.contains_key(anchor) {
            removed += 1;
        }
    }

    ContractDiff {
        anchor: "simulated-diff".to_string(),
        added_blocks: added,
        removed_blocks: removed,
        modified_blocks: modified,
        violations: vec![],
    }
}

pub fn analyze_history(entries: &[HistoryEntry]) -> HashMap<String, i32> {
    let mut edge_growth: HashMap<String, i32> = HashMap::new();

    for entry in entries {
        *edge_growth.entry(entry.timestamp.clone()).or_insert(0) += entry.edge_growth;
    }

    edge_growth
}

pub fn detect_brittle_directives(overrides: &[(String, String)]) -> Vec<(String, String)> {
    let mut brittle = Vec::new();

    for (tag, _justification) in overrides {
        if tag.contains("transitive") || tag.contains("recursive") {
            brittle.push((tag.clone(), "Missing :transitive: attribute".to_string()));
        }
    }

    brittle
}

pub fn detect_brittle_in_tags(tags: &[String]) -> Vec<(String, String)> {
    let mut brittle = Vec::new();

    for tag in tags {
        if tag.contains("transitive") || tag.contains("recursive") {
            brittle.push((tag.clone(), "Missing :transitive: attribute".to_string()));
        }
    }

    brittle
}

pub fn generate_threat_report(
    module: &str,
    override_count: usize,
    high_risk_overrides: usize,
) -> ThreatModelReport {
    let risk_score = if override_count > 10 {
        0.8
    } else if override_count > 5 {
        0.5
    } else {
        0.2
    };

    let mut recommendations = vec![];
    if override_count > 5 {
        recommendations.push("Consider consolidating overrides in this module".to_string());
    }
    if high_risk_overrides > 0 {
        recommendations.push("Review high-risk overrides with security team".to_string());
    }
    recommendations.push("Implement guardrails for frequently overridden directives".to_string());

    ThreatModelReport {
        module: module.to_string(),
        risk_score,
        high_risk_overrides,
        recommendations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simulate_diff_added() {
        let current: Vec<(String, String)> = vec![];
        let proposed = vec![("anchor1".to_string(), "content1".to_string())];
        let diff = simulate_diff(&current, &proposed);
        assert_eq!(diff.added_blocks, 1);
        assert_eq!(diff.removed_blocks, 0);
    }

    #[test]
    fn test_simulate_diff_removed() {
        let current = vec![("anchor1".to_string(), "content1".to_string())];
        let proposed: Vec<(String, String)> = vec![];
        let diff = simulate_diff(&current, &proposed);
        assert_eq!(diff.added_blocks, 0);
        assert_eq!(diff.removed_blocks, 1);
    }

    #[test]
    fn test_simulate_diff_modified() {
        let current = vec![("anchor1".to_string(), "old content".to_string())];
        let proposed = vec![("anchor1".to_string(), "new content".to_string())];
        let diff = simulate_diff(&current, &proposed);
        assert_eq!(diff.modified_blocks, 1);
    }

    #[test]
    fn test_threat_report_high_risk() {
        let report = generate_threat_report("my-module", 15, 5);
        assert!(report.risk_score >= 0.8);
        assert!(!report.recommendations.is_empty());
    }

    #[test]
    fn test_threat_report_low_risk() {
        let report = generate_threat_report("safe-module", 2, 0);
        assert!(report.risk_score < 0.5);
    }

    #[test]
    fn test_detect_brittle_in_tags() {
        let tags = vec!["foo_transitive".to_string(), "bar".to_string()];
        let brittle = detect_brittle_in_tags(&tags);
        assert_eq!(brittle.len(), 1);
    }
}
