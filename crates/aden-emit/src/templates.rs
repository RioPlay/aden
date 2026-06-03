// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use aden_core::{Block, Document, NodeType, Table};
use std::collections::HashMap;

/// Build a `module.adoc` contract Document.
pub fn module_contract(
    anchor: impl Into<String>,
    _title: impl Into<String>,
    signatures: Vec<Vec<String>>,
    invariants: Vec<String>,
    errors: Vec<String>,
    side_effects: Vec<String>,
    attributes: HashMap<String, String>,
) -> Document {
    let mut blocks = Vec::new();
    if !signatures.is_empty() {
        blocks.push(Block::Paragraph("== Signature".to_string()));
        blocks.push(Block::Table(Table {
            headers: vec![
                "Type".to_string(),
                "Kind".to_string(),
                "Description".to_string(),
            ],
            rows: signatures,
        }));
    }
    if !invariants.is_empty() {
        blocks.push(Block::Paragraph("== Invariants".to_string()));
        for inv in invariants {
            blocks.push(Block::Paragraph(inv));
        }
    }
    if !errors.is_empty() {
        blocks.push(Block::Paragraph("== Errors".to_string()));
        for e in errors {
            blocks.push(Block::Paragraph(e));
        }
    }
    if !side_effects.is_empty() {
        blocks.push(Block::Paragraph("== Side Effects".to_string()));
        for s in side_effects {
            blocks.push(Block::Paragraph(s));
        }
    }
    Document {
        anchor: anchor.into(),
        node_type: NodeType::Module,
        attributes,
        blocks,
        source_span: None,
        metadata: None,
        confidence: 0.9,
    }
}

/// Build an `adr.adoc` Architecture Decision Record Document.
pub fn adr(
    anchor: impl Into<String>,
    _title: impl Into<String>,
    decision: Vec<String>,
    consequences: Vec<String>,
    status: String,
    constraints: Vec<String>,
    attributes: HashMap<String, String>,
) -> Document {
    let mut blocks = Vec::new();
    blocks.push(Block::Paragraph("== Decision".to_string()));
    for d in decision {
        blocks.push(Block::Paragraph(d));
    }
    blocks.push(Block::Paragraph("== Consequences".to_string()));
    let mut rows = vec![vec!["Factor".to_string(), "Impact".to_string()]];
    for c in consequences {
        let parts: Vec<&str> = c.splitn(2, "|").collect();
        if parts.len() == 2 {
            rows.push(vec![parts[0].to_string(), parts[1].to_string()]);
        } else {
            rows.push(vec![c.clone(), c]);
        }
    }
    blocks.push(Block::Table(Table {
        headers: vec!["Factor".to_string(), "Impact".to_string()],
        rows,
    }));
    blocks.push(Block::Paragraph("== Status".to_string()));
    blocks.push(Block::Paragraph(status));
    if !constraints.is_empty() {
        blocks.push(Block::Paragraph("== Constraints".to_string()));
        for c in constraints {
            blocks.push(Block::Paragraph(c));
        }
    }
    Document {
        anchor: anchor.into(),
        node_type: NodeType::Adr,
        attributes,
        blocks,
        source_span: None,
        metadata: None,
        confidence: 0.9,
    }
}

/// Build an `rfc.adoc` Request for Comments Document.
pub fn rfc(
    anchor: impl Into<String>,
    _title: impl Into<String>,
    proposal: Vec<String>,
    alternatives: Vec<String>,
    open_questions: Vec<String>,
    attributes: HashMap<String, String>,
) -> Document {
    let mut blocks = Vec::new();
    blocks.push(Block::Paragraph("== Proposal".to_string()));
    for p in proposal {
        blocks.push(Block::Paragraph(p));
    }
    if !alternatives.is_empty() {
        blocks.push(Block::Paragraph("== Alternatives".to_string()));
        for a in alternatives {
            blocks.push(Block::Paragraph(a));
        }
    }
    if !open_questions.is_empty() {
        blocks.push(Block::Paragraph("== Open Questions".to_string()));
        for q in open_questions {
            blocks.push(Block::Admonition {
                kind: aden_core::AdmonitionKind::Note,
                text: format!("agent-note::{q}"),
            });
        }
    }
    Document {
        anchor: anchor.into(),
        node_type: NodeType::Spec,
        attributes,
        blocks,
        source_span: None,
        metadata: None,
        confidence: 0.9,
    }
}

/// Build a `runbook.adoc` operational procedure Document.
pub fn runbook(
    anchor: impl Into<String>,
    _title: impl Into<String>,
    procedure: Vec<String>,
    invokes: Vec<String>,
    prerequisites: Vec<String>,
    rollback: Vec<String>,
    attributes: HashMap<String, String>,
) -> Document {
    let mut blocks = Vec::new();
    if !prerequisites.is_empty() {
        blocks.push(Block::Paragraph("== Prerequisites".to_string()));
        for p in prerequisites {
            blocks.push(Block::Paragraph(p));
        }
    }
    blocks.push(Block::Paragraph("== Procedure".to_string()));
    for step in procedure {
        blocks.push(Block::Paragraph(step));
    }
    if !invokes.is_empty() {
        blocks.push(Block::Paragraph("== Invokes".to_string()));
        for i in invokes {
            blocks.push(Block::Paragraph(i));
        }
    }
    if !rollback.is_empty() {
        blocks.push(Block::Paragraph("== Rollback".to_string()));
        for r in rollback {
            blocks.push(Block::Paragraph(r));
        }
    }
    Document {
        anchor: anchor.into(),
        node_type: NodeType::Runbook,
        attributes,
        blocks,
        source_span: None,
        metadata: None,
        confidence: 0.9,
    }
}

/// Build a `plan.adoc` initiative / milestone Document.
pub fn plan(
    anchor: impl Into<String>,
    _title: impl Into<String>,
    objective: String,
    contracts: Vec<Vec<String>>,
    dependencies: Vec<String>,
    trace: Vec<String>,
    attributes: HashMap<String, String>,
) -> Document {
    let mut blocks = Vec::new();
    blocks.push(Block::Paragraph("== Objective".to_string()));
    blocks.push(Block::Paragraph(objective));
    if !contracts.is_empty() {
        blocks.push(Block::Paragraph("== Contracts".to_string()));
        blocks.push(Block::Table(Table {
            headers: vec![
                "Module".to_string(),
                "Deliverable".to_string(),
                "Acceptance Criteria".to_string(),
            ],
            rows: contracts,
        }));
    }
    if !dependencies.is_empty() {
        blocks.push(Block::Paragraph("== Dependencies".to_string()));
        for d in dependencies {
            blocks.push(Block::Paragraph(d));
        }
    }
    if !trace.is_empty() {
        blocks.push(Block::Paragraph("== Trace".to_string()));
        for t in trace {
            blocks.push(Block::Paragraph(t));
        }
    }
    Document {
        anchor: anchor.into(),
        node_type: NodeType::Plan,
        attributes,
        blocks,
        source_span: None,
        metadata: None,
        confidence: 0.9,
    }
}

/// Build a `context.adoc` shared standards Document.
pub fn context(
    anchor: impl Into<String>,
    _title: impl Into<String>,
    glossary: Vec<(String, String)>,
    defaults: Vec<String>,
    conventions: Vec<String>,
    attributes: HashMap<String, String>,
) -> Document {
    let mut blocks = Vec::new();
    if !defaults.is_empty() {
        blocks.push(Block::Paragraph("== Language Defaults".to_string()));
        for d in defaults {
            blocks.push(Block::Paragraph(d));
        }
    }
    if !glossary.is_empty() {
        blocks.push(Block::Paragraph("== Project Glossary".to_string()));
        blocks.push(Block::DescriptionList(glossary));
    }
    if !conventions.is_empty() {
        blocks.push(Block::Paragraph("== Agent Conventions".to_string()));
        for c in conventions {
            blocks.push(Block::Paragraph(c));
        }
    }
    Document {
        anchor: anchor.into(),
        node_type: NodeType::Context,
        attributes,
        blocks,
        source_span: None,
        metadata: None,
        confidence: 0.9,
    }
}
