// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Constitutional policy engine: directive precedence, conflict resolution,
//! and `[constitution]` authority.
//!
//! Phase 0.7 of the Aden roadmap — the load-bearing security substrate.

use aden_core::contract::{ContractDocument, ContractRegion, RegionBlock};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Directive severity levels for contract governance.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DirectiveSeverity {
    /// Must pass CI. No automatic override possible without `[override]` block.
    Forbid,
    /// Surfaces in PR comments; fails CI if `--severity=CI`.
    Warn,
    /// Non-blocking recommendation visible in IDE and `aden check`.
    Suggest,
    /// Explicitly permitted; may require `[override]` with justification.
    Allow,
}

impl std::fmt::Display for DirectiveSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DirectiveSeverity::Forbid => write!(f, "Forbid"),
            DirectiveSeverity::Warn => write!(f, "Warn"),
            DirectiveSeverity::Suggest => write!(f, "Suggest"),
            DirectiveSeverity::Allow => write!(f, "Allow"),
        }
    }
}

impl std::str::FromStr for DirectiveSeverity {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "forbid" => Ok(DirectiveSeverity::Forbid),
            "warn" => Ok(DirectiveSeverity::Warn),
            "suggest" => Ok(DirectiveSeverity::Suggest),
            "allow" => Ok(DirectiveSeverity::Allow),
            _ => Err(format!("Unknown directive severity: {}", s)),
        }
    }
}

/// Kinds of policy directives that can appear in `[security]` or `[constitution]` blocks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DirectiveKind {
    /// Forbid/warn on specific imports.
    ForbidImport { pattern: String },
    /// Require a specific coding pattern.
    RequirePattern { pattern: String },
    /// Constraint on function contracts.
    ContractConstraint {
        pre: Option<String>,
        post: Option<String>,
    },
    /// Agent self-modification policy.
    SelfModificationPolicy { action: String },
    /// Custom directive parsed from block attributes.
    Custom {
        name: String,
        params: HashMap<String, String>,
    },
}

/// A single directive extracted from a `[security]` or `[constitution]` block.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Directive {
    pub severity: DirectiveSeverity,
    pub kind: DirectiveKind,
    pub source_tag: Option<String>,
    pub attributes: HashMap<String, String>,
}

impl Directive {
    /// True if this directive can be bypassed with an `[override]` block.
    pub fn can_override(&self) -> bool {
        self.severity != DirectiveSeverity::Forbid
    }
}

/// Parsed `[constitution]` block with precedence metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConstitutionBlock {
    /// The underlying region block.
    pub block: RegionBlock,
    /// Parsed directives inside this constitution.
    pub directives: Vec<Directive>,
    /// Precedence value: lower number = higher priority.
    /// Defaults to u32::MAX if not specified.
    pub precedence: u32,
    /// Source agent or human role that authored this constitution.
    pub author: Option<String>,
}

impl ConstitutionBlock {
    /// Parse a `[constitution]` RegionBlock into a structured ConstitutionBlock.
    pub fn from_region(block: &RegionBlock) -> Result<Self, String> {
        if block.region != ContractRegion::Constitution {
            return Err("Expected region type Constitution".to_string());
        }

        let precedence = block
            .attributes
            .get("precedence")
            .and_then(|s| s.parse().ok())
            .unwrap_or(u32::MAX);

        let author = block.attributes.get("author").cloned();

        let directives = parse_directives_from_content(&block.content);

        Ok(Self {
            block: block.clone(),
            directives,
            precedence,
            author,
        })
    }
}

/// The policy engine holds all loaded constitutions and resolves conflicts.
#[derive(Debug, Clone, Default)]
pub struct PolicyEngine {
    constitutions: Vec<ConstitutionBlock>,
    /// Global overrides indexed by directive kind hash.
    overrides: HashMap<String, Vec<RegionBlock>>,
}

impl PolicyEngine {
    /// Load the bootstrap constitution from `.aden/constitution.adoc`.
    /// This is the root-of-trust for all policy evaluation.
    pub fn load_bootstrap(repo_path: &std::path::Path) -> Result<Self, Box<dyn std::error::Error>> {
        use aden_core::contract::{ParseMode, parse_contract};
        use std::path::Path;

        let constitution_path: &Path = &repo_path.join(".aden/constitution.adoc");
        if !constitution_path.exists() {
            // No bootstrap constitution — return empty engine (backward compatible)
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(constitution_path)?;
        let doc = parse_contract(&content, ParseMode::Permissive)?;
        let mut engine = Self::default();
        engine.load_from_document(&doc);
        // Fallback: init-style `[rule="Forbid"]` bullets often live outside the
        // parsed RegionBlock content — scan the full file when blocks load empty.
        let loaded: usize = engine
            .constitutions
            .iter()
            .map(|c| c.directives.len())
            .sum();
        if loaded == 0 {
            let directives = parse_directives_from_content(&content);
            if !directives.is_empty() {
                engine.constitutions.push(ConstitutionBlock {
                    block: RegionBlock {
                        region: ContractRegion::Constitution,
                        tag: Some("bootstrap".to_string()),
                        attributes: HashMap::from([("precedence".to_string(), "100".to_string())]),
                        content: String::new(),
                        start_line: 1,
                        end_line: 1,
                    },
                    directives,
                    precedence: 100,
                    author: Some("bootstrap".to_string()),
                });
            }
        }
        Ok(engine)
    }

    /// Load constitutions from a parsed contract document.
    pub fn load_from_document(&mut self, doc: &ContractDocument) {
        for block in &doc.blocks {
            if block.region == ContractRegion::Constitution
                && let Ok(c) = ConstitutionBlock::from_region(block)
            {
                self.constitutions.push(c);
            }
            if block.region == ContractRegion::Override {
                let key = block.tag.clone().unwrap_or_default();
                self.overrides.entry(key).or_default().push(block.clone());
            }
        }
        // Sort by precedence (lowest number first)
        self.constitutions.sort_by_key(|c| c.precedence);
    }

    /// Resolve conflicts between two agents' directives on the same subject.
    ///
    /// Returns `Ok((winning_directive, reason))` or `Err` if no constitution governs the subject.
    pub fn resolve_conflict(
        &self,
        agent_a: &Directive,
        agent_b: &Directive,
        subject: &str,
    ) -> Result<(Directive, String), String> {
        // Find applicable constitutions for this subject
        let applicable: Vec<_> = self
            .constitutions
            .iter()
            .filter(|c| c.directives.iter().any(|d| matches_subject(d, subject)))
            .collect();

        if applicable.is_empty() {
            return Err(format!(
                "No constitution governs subject '{}' — require human review",
                subject
            ));
        }

        // Higher-precedence constitution wins
        let winner = applicable[0]; // already sorted by precedence

        let a_score = agent_a.severity as u32;
        let b_score = agent_b.severity as u32;

        if a_score == b_score {
            // Same severity: tie-break by constitution precedence
            let reason = format!(
                "Tie broken by constitution precedence (precedence={}) from author={:?}",
                winner.precedence, winner.author
            );
            // Prefer the directive that matches the constitution's author
            let chosen = if agent_a
                .attributes
                .get("agent")
                .map(|a| Some(a) == winner.author.as_ref())
                .unwrap_or(false)
            {
                agent_a.clone()
            } else {
                agent_b.clone()
            };
            return Ok((chosen, reason));
        }

        let chosen = if a_score < b_score {
            agent_a.clone()
        } else {
            agent_b.clone()
        };

        let reason = format!(
            "Resolved by severity ranking: {} (score {}) vs {} (score {})",
            chosen.severity,
            chosen.severity as u32,
            if a_score < b_score {
                agent_b.severity
            } else {
                agent_a.severity
            },
            if a_score < b_score { b_score } else { a_score }
        );

        Ok((chosen, reason))
    }

    /// Evaluate whether a given action is permitted under current policy.
    ///
    /// Returns the effective severity after considering overrides.
    pub fn evaluate(&self, action: &DirectiveKind, _subject: &str) -> DirectiveSeverity {
        // Collect all matching directives
        let mut matches = Vec::new();
        for c in &self.constitutions {
            for d in &c.directives {
                if directive_matches_kind(d, action) {
                    matches.push((c.precedence, d));
                }
            }
        }

        if matches.is_empty() {
            return DirectiveSeverity::Allow;
        }

        // Sort by precedence and return the highest-severity match
        matches.sort_by_key(|(prec, _)| *prec);
        let highest = matches.into_iter().map(|(_, d)| d).next().unwrap();
        highest.severity
    }

    /// Check if an override block is valid (has required `:justification:` and `:reviewer:`).
    pub fn validate_override(&self, block: &RegionBlock) -> Result<(), String> {
        if block.region != ContractRegion::Override {
            return Err("Not an override block".to_string());
        }
        if !block.attributes.contains_key("justification") {
            return Err("Override missing required attribute :justification:".to_string());
        }
        if !block.attributes.contains_key("reviewer") {
            return Err("Override missing required attribute :reviewer:".to_string());
        }
        Ok(())
    }

    /// List all active `[constitution]` blocks sorted by precedence.
    pub fn constitutions(&self) -> &[ConstitutionBlock] {
        &self.constitutions
    }
}

/// Active policy mode. Default is advisory until 0.3.0 enforce census.
pub fn policy_mode_label() -> &'static str {
    match std::env::var("ADEN_POLICY_MODE")
        .ok()
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase())
    {
        Some(ref s) if s == "enforce" => "enforce",
        _ => "advisory",
    }
}

/// A constitution or policy directive surfaced for agents (advisory by default).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PolicyViolation {
    pub severity: String,
    pub message: String,
    pub source: String,
}

/// Summary of policy wiring for diagnose/check/lint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PolicyAudit {
    pub mode: String,
    pub constitution_present: bool,
    pub directive_count: usize,
    pub unwired: bool,
    pub violations: Vec<PolicyViolation>,
}

/// Audit constitutional policy for a repo. Advisory mode never blocks callers.
pub fn audit_policy(repo_path: &std::path::Path) -> PolicyAudit {
    let constitution_path = repo_path.join(".aden/constitution.adoc");
    let present = constitution_path.exists();
    let raw_rules = present
        .then(|| std::fs::read_to_string(&constitution_path).ok())
        .flatten()
        .map(|c| count_rule_blocks(&c))
        .unwrap_or(0);

    let engine = PolicyEngine::load_bootstrap(repo_path).unwrap_or_default();
    let directive_count: usize = engine
        .constitutions()
        .iter()
        .map(|c| c.directives.len())
        .sum();
    let unwired = present && raw_rules > 0 && directive_count == 0;

    let mut violations = Vec::new();
    if unwired {
        violations.push(PolicyViolation {
            severity: "Warn".to_string(),
            message: format!(
                "constitution has {raw_rules} [rule=…] block(s) but PolicyEngine loaded 0 directives — parser/format mismatch"
            ),
            source: "policy-engine".to_string(),
        });
    }
    for block in engine.constitutions() {
        for d in &block.directives {
            if matches!(
                d.severity,
                DirectiveSeverity::Forbid | DirectiveSeverity::Warn
            ) {
                let msg = match &d.kind {
                    DirectiveKind::Custom { params, .. } => params
                        .get("text")
                        .cloned()
                        .or_else(|| params.get("value").cloned())
                        .unwrap_or_else(|| format!("{:?}", d.kind)),
                    DirectiveKind::ForbidImport { pattern } => {
                        format!("forbid import: {pattern}")
                    }
                    DirectiveKind::RequirePattern { pattern } => {
                        format!("require pattern: {pattern}")
                    }
                    other => format!("{other:?}"),
                };
                violations.push(PolicyViolation {
                    severity: d.severity.to_string(),
                    message: msg,
                    source: block
                        .author
                        .clone()
                        .unwrap_or_else(|| "constitution".to_string()),
                });
            }
        }
    }

    PolicyAudit {
        mode: policy_mode_label().to_string(),
        constitution_present: present,
        directive_count,
        unwired,
        violations,
    }
}

fn count_rule_blocks(content: &str) -> usize {
    content
        .lines()
        .filter(|l| {
            let t = l.trim();
            t.starts_with("[rule=\"") || t.starts_with("[rule='")
        })
        .count()
}

fn parse_directives_from_content(content: &str) -> Vec<Directive> {
    let mut directives = Vec::new();
    let mut rule_severity: Option<DirectiveSeverity> = None;
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(sev) = parse_rule_header(trimmed) {
            rule_severity = Some(sev);
            continue;
        }
        if let Some(text) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            && let Some(sev) = rule_severity
        {
            directives.push(Directive {
                severity: sev,
                kind: DirectiveKind::Custom {
                    name: "rule".to_string(),
                    params: {
                        let mut m = HashMap::new();
                        m.insert("text".to_string(), text.to_string());
                        m
                    },
                },
                source_tag: None,
                attributes: HashMap::new(),
            });
            continue;
        }
        if trimmed.starts_with(':') && trimmed.contains(':') {
            // attribute-style directive: :forbid_import: pattern
            let inner = trimmed.trim_start_matches(':');
            if let Some((key, value)) = inner.split_once(':') {
                let key = key.trim();
                let value = value.trim();
                let (severity, kind) = match key {
                    "forbid_import" | "forbid" => (
                        DirectiveSeverity::Forbid,
                        DirectiveKind::ForbidImport {
                            pattern: value.to_string(),
                        },
                    ),
                    "warn_import" | "warn" => (
                        DirectiveSeverity::Warn,
                        DirectiveKind::ForbidImport {
                            pattern: value.to_string(),
                        },
                    ),
                    "suggest_pattern" | "suggest" => (
                        DirectiveSeverity::Suggest,
                        DirectiveKind::RequirePattern {
                            pattern: value.to_string(),
                        },
                    ),
                    "allow" => (
                        DirectiveSeverity::Allow,
                        DirectiveKind::Custom {
                            name: key.to_string(),
                            params: {
                                let mut m = HashMap::new();
                                m.insert("value".to_string(), value.to_string());
                                m
                            },
                        },
                    ),
                    "precedence" | "author" => continue, // metadata, not directives
                    _ => (
                        DirectiveSeverity::Forbid,
                        DirectiveKind::Custom {
                            name: key.to_string(),
                            params: {
                                let mut m = HashMap::new();
                                m.insert("value".to_string(), value.to_string());
                                m
                            },
                        },
                    ),
                };
                directives.push(Directive {
                    severity,
                    kind,
                    source_tag: None,
                    attributes: HashMap::new(),
                });
            }
        }
    }
    directives
}

fn parse_rule_header(line: &str) -> Option<DirectiveSeverity> {
    let inner = line
        .strip_prefix("[rule=\"")
        .and_then(|s| s.strip_suffix("\"]"))
        .or_else(|| {
            line.strip_prefix("[rule='")
                .and_then(|s| s.strip_suffix("']"))
        });
    inner.and_then(|s| s.parse().ok())
}

fn matches_subject(directive: &Directive, subject: &str) -> bool {
    match &directive.kind {
        DirectiveKind::ForbidImport { pattern } => glob_match(subject, pattern),
        DirectiveKind::RequirePattern { pattern } => glob_match(subject, pattern),
        DirectiveKind::ContractConstraint { .. } => true,
        DirectiveKind::SelfModificationPolicy { action } => subject.contains(action),
        DirectiveKind::Custom { .. } => true,
    }
}

fn directive_matches_kind(directive: &Directive, kind: &DirectiveKind) -> bool {
    std::mem::discriminant(&directive.kind) == std::mem::discriminant(kind)
}

/// Very simple glob-like matcher. Supports `*` anywhere.
fn glob_match(text: &str, pattern: &str) -> bool {
    if !pattern.contains('*') {
        return text == pattern;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut rest = text;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if let Some(pos) = rest.find(part) {
            if i == 0 && pos != 0 {
                return false; // first segment must match at start
            }
            rest = &rest[pos + part.len()..];
        } else {
            return false;
        }
    }
    // If pattern doesn't end with *, rest should be empty
    if !pattern.ends_with('*') && !rest.is_empty() {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use aden_core::contract::{ContractRegion, RegionBlock};
    use std::collections::HashMap;

    fn make_constitution_block(content: &str, precedence: &str) -> RegionBlock {
        let mut attrs = HashMap::new();
        attrs.insert("precedence".to_string(), precedence.to_string());
        attrs.insert("author".to_string(), "agent-core".to_string());
        RegionBlock {
            region: ContractRegion::Constitution,
            tag: Some("bootstrap".to_string()),
            attributes: attrs,
            content: content.to_string(),
            start_line: 1,
            end_line: 5,
        }
    }

    #[test]
    fn test_constitution_precedence_roundtrip() {
        let block = make_constitution_block(
            ":forbid_import: std::process::Command\n:allow: safe_pattern",
            "10",
        );
        let constitution = ConstitutionBlock::from_region(&block).unwrap();
        assert_eq!(constitution.precedence, 10);
        assert_eq!(constitution.author, Some("agent-core".to_string()));
        assert_eq!(constitution.directives.len(), 2);
    }

    #[test]
    fn test_conflict_resolution_by_severity() {
        let block = make_constitution_block(":forbid_import: unsafe::*\n", "5");
        let mut engine = PolicyEngine::default();
        let doc = ContractDocument {
            header_attrs: HashMap::new(),
            blocks: vec![block],
            prose: Vec::new(),
        };
        engine.load_from_document(&doc);

        let agent_a = Directive {
            severity: DirectiveSeverity::Forbid,
            kind: DirectiveKind::ForbidImport {
                pattern: "unsafe::*".to_string(),
            },
            source_tag: None,
            attributes: HashMap::new(),
        };
        let agent_b = Directive {
            severity: DirectiveSeverity::Allow,
            kind: DirectiveKind::ForbidImport {
                pattern: "unsafe::*".to_string(),
            },
            source_tag: None,
            attributes: HashMap::new(),
        };

        let (winner, reason) = engine
            .resolve_conflict(&agent_a, &agent_b, "unsafe::foo")
            .unwrap();
        assert_eq!(winner.severity, DirectiveSeverity::Forbid);
        assert!(!reason.is_empty());
    }

    #[test]
    fn test_override_validation() {
        let engine = PolicyEngine::default();
        let valid_override = RegionBlock {
            region: ContractRegion::Override,
            tag: Some("forbid_import".to_string()),
            attributes: {
                let mut m = HashMap::new();
                m.insert(
                    "justification".to_string(),
                    "Needed for test harness".to_string(),
                );
                m.insert("reviewer".to_string(), "alice".to_string());
                m
            },
            content: String::new(),
            start_line: 1,
            end_line: 1,
        };
        assert!(engine.validate_override(&valid_override).is_ok());

        let invalid_override = RegionBlock {
            region: ContractRegion::Override,
            tag: Some("forbid_import".to_string()),
            attributes: HashMap::new(),
            content: String::new(),
            start_line: 1,
            end_line: 1,
        };
        assert!(engine.validate_override(&invalid_override).is_err());
    }

    #[test]
    fn test_glob_match() {
        assert!(glob_match("foo::bar", "foo::bar"));
        assert!(glob_match("foo::bar::baz", "foo::*"));
        assert!(glob_match("foo::bar::baz", "*::baz"));
        assert!(glob_match("foo::bar::baz", "foo::*::baz"));
        assert!(!glob_match("foo::bar", "bar::foo"));
    }

    #[test]
    fn test_load_bootstrap_parses_rule_blocks() {
        let dir =
            std::env::temp_dir().join(format!("aden-policy-bootstrap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let aden = dir.join(".aden");
        std::fs::create_dir_all(&aden).unwrap();
        std::fs::write(
            aden.join("constitution.adoc"),
            r#"[constitution]
[rule="Forbid"]
- Never commit secrets
[rule="Warn"]
- Run tests before commit
"#,
        )
        .unwrap();
        let engine = PolicyEngine::load_bootstrap(&dir).unwrap();
        let n: usize = engine
            .constitutions()
            .iter()
            .map(|c| c.directives.len())
            .sum();
        assert_eq!(n, 2, "bootstrap fallback should load rule bullets");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_parse_rule_block_directives() {
        let content = r#"[rule="Forbid"]
- Never commit secrets
[rule="Warn"]
- All tests must pass
"#;
        let directives = parse_directives_from_content(content);
        assert_eq!(directives.len(), 2);
        assert_eq!(directives[0].severity, DirectiveSeverity::Forbid);
        assert_eq!(directives[1].severity, DirectiveSeverity::Warn);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContractConstraint {
    pub pre: Option<String>,
    pub post: Option<String>,
    pub invariant: Option<String>,
}

impl ContractConstraint {
    pub fn from_block(block: &RegionBlock) -> Result<Self, String> {
        if block.region != ContractRegion::Contract {
            return Err("Not a contract block".to_string());
        }
        Ok(Self {
            pre: block.attributes.get("pre").cloned(),
            post: block.attributes.get("post").cloned(),
            invariant: block.attributes.get("invariant").cloned(),
        })
    }
}

#[derive(Debug)]
pub struct ContractViolation {
    pub block_tag: Option<String>,
    pub constraint_type: String,
    pub message: String,
    pub line: usize,
}

pub fn check_contract_constraints(block: &RegionBlock) -> Vec<ContractViolation> {
    let mut violations = Vec::new();
    if block.region != ContractRegion::Contract {
        return violations;
    }
    if let Ok(constraint) = ContractConstraint::from_block(block) {
        if constraint.pre.is_none() && constraint.post.is_none() && constraint.invariant.is_none() {
            violations.push(ContractViolation {
                block_tag: block.tag.clone(),
                constraint_type: "empty".to_string(),
                message: "Contract block has no :pre:, :post:, or :invariant: constraints"
                    .to_string(),
                line: block.start_line,
            });
        }
        if let Some(pre) = &constraint.pre
            && pre.trim().is_empty()
        {
            violations.push(ContractViolation {
                block_tag: block.tag.clone(),
                constraint_type: "pre".to_string(),
                message: ":pre: constraint is empty".to_string(),
                line: block.start_line,
            });
        }
        if let Some(post) = &constraint.post
            && post.trim().is_empty()
        {
            violations.push(ContractViolation {
                block_tag: block.tag.clone(),
                constraint_type: "post".to_string(),
                message: ":post: constraint is empty".to_string(),
                line: block.start_line,
            });
        }
        if let Some(inv) = &constraint.invariant
            && inv.trim().is_empty()
        {
            violations.push(ContractViolation {
                block_tag: block.tag.clone(),
                constraint_type: "invariant".to_string(),
                message: ":invariant: constraint is empty".to_string(),
                line: block.start_line,
            });
        }
    }
    violations
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentRole {
    pub namespace: String,
    pub role: String,
    pub confidence: f64,
    pub directives_count: usize,
    pub successful_directives: usize,
}

impl AgentRole {
    pub fn new(namespace: &str, role: &str) -> Self {
        Self {
            namespace: namespace.to_string(),
            role: role.to_string(),
            confidence: 0.5,
            directives_count: 0,
            successful_directives: 0,
        }
    }

    pub fn update_confidence(&mut self) {
        if self.directives_count > 0 {
            self.confidence = self.successful_directives as f64 / self.directives_count as f64;
        }
    }

    pub fn is_trusted(&self, threshold: f64) -> bool {
        self.confidence >= threshold
    }
}

pub const DEFAULT_CONFIDENCE_THRESHOLD: f64 = 0.70;

pub fn parse_agent_role_from_block(block: &RegionBlock) -> Option<AgentRole> {
    let tag = block.tag.as_ref()?;

    if let Some(colon) = tag.find(':') {
        let namespace = tag[..colon].to_string();
        let role = tag[colon + 1..].to_string();

        let confidence = block
            .attributes
            .get("confidence")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.5);

        let directives_count = block
            .attributes
            .get("directives")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let successful_directives = block
            .attributes
            .get("successful")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let mut agent = AgentRole {
            namespace,
            role,
            confidence,
            directives_count,
            successful_directives,
        };
        agent.update_confidence();

        Some(agent)
    } else {
        None
    }
}
