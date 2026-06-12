// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Contract kernel: three-way merge, region parsing, and constitutional authority.
//!
//! This module is the load-bearing foundation for all agent-native interfaces,
//! security enforcement, and attestation. Phase 0 of the Aden roadmap.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Semantic region of a contract document.
/// Determines merge behavior and authority rules.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ContractRegion {
    /// Machine-generated from AST. Always overwritten by `aden gen`.
    Generated,
    /// Human-written prose. Never silently overwritten.
    Human,
    /// AI agent output. Preserved unless the agent itself proposes an update.
    Agent,
    /// Security directive (`[forbid]`, `[warn]`, etc.). Requires `[override]` to bypass.
    Security,
    /// Design-by-contract block (`:pre:`, `:post:`, `:invariant:`).
    Contract,
    /// Constitutional policy with precedence rules.
    Constitution,
    /// Explicit human override of a directive.
    Override,
    /// Pending agent proposal (not yet promoted).
    Proposed,
}

impl std::fmt::Display for ContractRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContractRegion::Generated => write!(f, "generated"),
            ContractRegion::Human => write!(f, "human"),
            ContractRegion::Agent => write!(f, "agent"),
            ContractRegion::Security => write!(f, "security"),
            ContractRegion::Contract => write!(f, "contract"),
            ContractRegion::Constitution => write!(f, "constitution"),
            ContractRegion::Override => write!(f, "override"),
            ContractRegion::Proposed => write!(f, "proposed"),
        }
    }
}

impl std::str::FromStr for ContractRegion {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "generated" => Ok(ContractRegion::Generated),
            "human" => Ok(ContractRegion::Human),
            "agent" => Ok(ContractRegion::Agent),
            "security" => Ok(ContractRegion::Security),
            "contract" => Ok(ContractRegion::Contract),
            "constitution" => Ok(ContractRegion::Constitution),
            "override" => Ok(ContractRegion::Override),
            "proposed" => Ok(ContractRegion::Proposed),
            _ => Err(format!(
                "Unknown contract region: {} (expected one of: generated, human, agent, security, contract, constitution, override, proposed)",
                s
            )),
        }
    }
}

/// A parsed region block inside a contract document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegionBlock {
    /// Which region this block belongs to.
    pub region: ContractRegion,
    /// Optional tag after the region name, e.g. `[agent#perf]` → "perf".
    pub tag: Option<String>,
    /// AsciiDoc-style attributes parsed from the block header.
    pub attributes: HashMap<String, String>,
    /// Raw content of the block (everything between delimiters).
    pub content: String,
    /// 1-based start line in the source document.
    pub start_line: usize,
    /// 1-based end line in the source document.
    pub end_line: usize,
}

impl RegionBlock {
    /// True if this block is owned by humans (should not be overwritten by gen).
    pub fn is_human_owned(&self) -> bool {
        matches!(
            self.region,
            ContractRegion::Human
                | ContractRegion::Security
                | ContractRegion::Contract
                | ContractRegion::Constitution
                | ContractRegion::Override
        )
    }

    /// True if this block is owned by the generator (can be overwritten).
    pub fn is_generated(&self) -> bool {
        self.region == ContractRegion::Generated
    }

    /// True if this block carries durable intent that `gen` must never clobber:
    /// any human-owned region, or an `[agent]` block (preserved unless the agent
    /// itself proposes an update).
    pub fn is_durable(&self) -> bool {
        self.is_human_owned() || self.region == ContractRegion::Agent
    }
}

/// Parsed representation of a contract document with region blocks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ContractDocument {
    /// Document-level attributes (before any blocks).
    pub header_attrs: HashMap<String, String>,
    /// Ordered region blocks.
    pub blocks: Vec<RegionBlock>,
    /// Freeform text outside blocks (only present in permissive mode).
    pub prose: Vec<String>,
}

/// Three-way contract state for incremental generation.
///
/// * `ground` — the committed version (Git HEAD).
/// * `base`   — the version produced by the last `aden gen` run.
/// * `working` — the current working-tree version (may contain human edits).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContractState {
    pub ground: ContractDocument,
    pub base: ContractDocument,
    pub working: ContractDocument,
}

/// Actions the merge engine can take per block.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MergeAction {
    /// Replace a `[generated]` block with newly extracted AST.
    UpdateGenerated { index: usize, new_content: String },
    /// Keep the existing block untouched.
    PreserveHuman { index: usize },
    /// Two agents (or agent vs. human) disagree; needs resolution.
    Conflict { index: usize, reason: String },
    /// Insert a new `[generated]` block that did not exist in base.
    ///
    /// `tag` carries the inserted block's identity (typically the symbol's
    /// anchor or `anchor#n` sub-tag). Without it, `apply()` writes an
    /// untagged block that the next gen/heal cycle can't match by tag, so
    /// the symbol re-inserts on every run.
    InsertGenerated {
        after_index: usize,
        content: String,
        tag: Option<String>,
    },
    /// Delete a `[generated]` block whose source symbol no longer exists.
    DeleteGenerated { index: usize, reason: String },
}

/// Result of a dry-run (`aden gen --propose`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MergeProposal {
    pub actions: Vec<MergeAction>,
    pub preserved_count: usize,
    pub updated_count: usize,
    pub conflict_count: usize,
    pub inserted_count: usize,
    pub deleted_count: usize,
}

impl ContractState {
    /// Create a new ContractState from three document snapshots.
    pub fn new(
        ground: ContractDocument,
        base: ContractDocument,
        working: ContractDocument,
    ) -> Self {
        Self {
            ground,
            base,
            working,
        }
    }

    /// Compute the three-way merge, returning per-block actions.
    ///
    /// Rules:
    /// 1. Every `[generated]` block in `base` is matched to `ground` by tag.
    ///    Tags are stable per-block identifiers (`anchor` for the first AST
    ///    block of a symbol, `anchor#n` for subsequent ones — see
    ///    [`ContractDocument::from_document`]). If the matched ground
    ///    content differs, emit `UpdateGenerated`.
    /// 2. If a `[generated]` block exists in `base` but not in `ground` (the
    ///    symbol or one of its AST blocks no longer exists), emit
    ///    `DeleteGenerated`. If a durable (human/agent) overlay block carries
    ///    the same tag, surface `Conflict` instead — deletion would orphan
    ///    intent.
    /// 3. If a `[generated]` tag appears in `ground` but not `base`, emit
    ///    `InsertGenerated` carrying the ground tag so the new block keeps
    ///    its identity through apply() and future cycles.
    /// 4. Every `[human]`, `[agent]`, `[security]`, `[contract]`,
    ///    `[constitution]`, `[override]` block in `working` is preserved
    ///    (`PreserveHuman`).
    /// 5. If a durable (human/agent) overlay block carries the same tag as a
    ///    changed `[generated]` block, emit `Conflict` instead of
    ///    `UpdateGenerated` — the overlay was pinned to that exact content.
    pub fn propose(&self) -> crate::Result<MergeProposal> {
        let mut actions = Vec::new();
        let mut preserved = 0usize;
        let mut updated = 0usize;
        let mut conflicts = 0usize;
        let mut inserted = 0usize;
        let mut deleted = 0usize;

        // Index working blocks by (region, tag) for quick lookup
        let working_index: HashMap<(ContractRegion, Option<String>), Vec<usize>> = self
            .working
            .blocks
            .iter()
            .enumerate()
            .fold(HashMap::new(), |mut acc, (i, b)| {
                acc.entry((b.region, b.tag.clone())).or_default().push(i);
                acc
            });

        // Match base → ground for generated blocks
        for (bi, base_block) in self.base.blocks.iter().enumerate() {
            if base_block.is_generated() {
                // Try to find matching generated block in ground by tag
                let ground_match =
                    self.ground.blocks.iter().position(|g| {
                        g.region == ContractRegion::Generated && g.tag == base_block.tag
                    });

                if let Some(gi) = ground_match {
                    let ground_block = &self.ground.blocks[gi];
                    if ground_block.content != base_block.content {
                        // AST changed → update, but check for a durable (human or
                        // agent) block with the same tag that the change collides with.
                        let durable_conflict = self
                            .working
                            .blocks
                            .iter()
                            .any(|w| w.is_durable() && w.tag == base_block.tag);

                        if durable_conflict {
                            actions.push(MergeAction::Conflict {
                                index: bi,
                                reason: format!(
                                    "Generated block '{}' changed but a durable (human/agent) block with the same tag exists in working",
                                    base_block.tag.as_deref().unwrap_or("(untagged)")
                                ),
                            });
                            conflicts += 1;
                        } else {
                            actions.push(MergeAction::UpdateGenerated {
                                index: bi,
                                new_content: ground_block.content.clone(),
                            });
                            updated += 1;
                        }
                    }
                } else {
                    // Symbol deleted in ground. If a durable overlay block is tagged
                    // to this anchor, deleting would orphan it — surface a Conflict
                    // for review instead of silently dropping the generated block.
                    let orphans_overlay = self
                        .working
                        .blocks
                        .iter()
                        .any(|w| w.is_durable() && w.tag == base_block.tag);

                    if orphans_overlay {
                        actions.push(MergeAction::Conflict {
                            index: bi,
                            reason: format!(
                                "Generated block '{}' no longer exists in latest AST but a durable (human/agent) block references it",
                                base_block.tag.as_deref().unwrap_or("(untagged)")
                            ),
                        });
                        conflicts += 1;
                    } else {
                        actions.push(MergeAction::DeleteGenerated {
                            index: bi,
                            reason: format!(
                                "Generated block '{}' no longer exists in latest AST",
                                base_block.tag.as_deref().unwrap_or("(untagged)")
                            ),
                        });
                        deleted += 1;
                    }
                }
            } else {
                // Non-generated blocks from base: preserve if still in working
                let key = (base_block.region, base_block.tag.clone());
                if working_index.contains_key(&key) {
                    actions.push(MergeAction::PreserveHuman { index: bi });
                    preserved += 1;
                }
            }
        }

        // Find new generated blocks in ground that are not in base
        for ground_block in &self.ground.blocks {
            if ground_block.is_generated() {
                let exists_in_base =
                    self.base.blocks.iter().any(|b| {
                        b.region == ContractRegion::Generated && b.tag == ground_block.tag
                    });
                if !exists_in_base {
                    // Determine insertion point: after the last generated block
                    // with a lower tag order, or at end.
                    let after = self.base.blocks.len().saturating_sub(1);
                    actions.push(MergeAction::InsertGenerated {
                        after_index: after,
                        content: ground_block.content.clone(),
                        tag: ground_block.tag.clone(),
                    });
                    inserted += 1;
                }
            }
        }

        Ok(MergeProposal {
            actions,
            preserved_count: preserved,
            updated_count: updated,
            conflict_count: conflicts,
            inserted_count: inserted,
            deleted_count: deleted,
        })
    }

    /// Apply a proposal to `working`, producing the merged document.
    ///
    /// This consumes the proposal and returns a new `ContractDocument`.
    /// SAFETY: Human-owned blocks are never modified.
    pub fn apply(&self, proposal: &MergeProposal) -> crate::Result<ContractDocument> {
        let mut result = self.working.clone();

        // Sort actions by index in reverse so insertions/deletions don't shift
        let mut sorted = proposal.actions.clone();
        sorted.sort_by(|a, b| {
            let idx_a = action_index(a);
            let idx_b = action_index(b);
            idx_b.cmp(&idx_a) // descending
        });

        for action in &sorted {
            match action {
                MergeAction::UpdateGenerated { index, new_content } => {
                    if let Some(block) = result.blocks.get_mut(*index) {
                        if block.is_generated() {
                            block.content.clone_from(new_content);
                        } else {
                            // REVIEW: Should never happen if propose() is correct.
                            return Err(crate::Error::Generic(format!(
                                "MergeAction::UpdateGenerated targeted non-generated block at index {}",
                                index
                            )));
                        }
                    }
                }
                MergeAction::PreserveHuman { .. } => {
                    // No-op: working already contains the human block.
                }
                MergeAction::Conflict { index, reason } => {
                    // Insert a conflict marker as a `[proposed]` block adjacent to the conflict.
                    let conflict_block = RegionBlock {
                        region: ContractRegion::Proposed,
                        tag: None,
                        attributes: {
                            let mut m = HashMap::new();
                            m.insert("status".to_string(), "conflict".to_string());
                            m.insert("reason".to_string(), reason.clone());
                            m
                        },
                        content: format!(
                            "// CONFLICT: {}\n// Resolve manually or run `aden gen --propose` to review.",
                            reason
                        ),
                        start_line: 0,
                        end_line: 0,
                    };
                    // Insert after the conflicting block
                    let insert_at = (*index + 1).min(result.blocks.len());
                    result.blocks.insert(insert_at, conflict_block);
                }
                MergeAction::InsertGenerated {
                    after_index,
                    content,
                    tag,
                } => {
                    let new_block = RegionBlock {
                        region: ContractRegion::Generated,
                        tag: tag.clone(),
                        attributes: HashMap::new(),
                        content: content.clone(),
                        start_line: 0,
                        end_line: 0,
                    };
                    let insert_at = (*after_index + 1).min(result.blocks.len());
                    result.blocks.insert(insert_at, new_block);
                }
                MergeAction::DeleteGenerated { index, .. } => {
                    if result
                        .blocks
                        .get(*index)
                        .map(|b| b.is_generated())
                        .unwrap_or(false)
                    {
                        result.blocks.remove(*index);
                    }
                }
            }
        }

        Ok(result)
    }
}

impl MergeProposal {
    /// True when the merge found no conflict — safe to auto-apply without review.
    pub fn is_clean(&self) -> bool {
        self.conflict_count == 0
    }
}

/// Build the `working` layer for a per-anchor merge: the generated `base` plus
/// any non-generated (durable intent) blocks from an overlay. Generated blocks
/// in the overlay are ignored — overlays only carry intent, never generated content.
pub fn overlay_onto(
    base: &ContractDocument,
    overlay: Option<&ContractDocument>,
) -> ContractDocument {
    let mut working = base.clone();
    if let Some(ov) = overlay {
        for block in &ov.blocks {
            if !block.is_generated() {
                working.blocks.push(block.clone());
            }
        }
    }
    working
}

/// Reconcile a freshly-parsed `Document` against the stored generated document
/// and an optional intent overlay, returning the per-block merge proposal.
///
/// * `fresh`   — the document just parsed from source (becomes `ground`).
/// * `stored`  — the document currently in the store (becomes `base`); `None`
///   means a brand-new symbol, which yields an `InsertGenerated` proposal.
/// * `overlay` — durable human/agent blocks layered on top of `base` to form
///   `working`.
///
/// A clean proposal (`is_clean()`) means the caller may write `fresh` to the
/// store directly. A non-clean proposal should be surfaced for review and the
/// stored document left untouched.
pub fn reconcile_anchor(
    fresh: &crate::Document,
    stored: Option<&crate::Document>,
    overlay: Option<&ContractDocument>,
) -> crate::Result<MergeProposal> {
    let ground = ContractDocument::from_document(fresh);
    let base = stored
        .map(ContractDocument::from_document)
        .unwrap_or_default();
    let working = overlay_onto(&base, overlay);
    ContractState::new(ground, base, working).propose()
}

impl ContractDocument {
    /// Convert a plain `Document` into a `ContractDocument` whose
    /// `[generated]` region carries one `RegionBlock` per AST block of the
    /// source document.
    ///
    /// Tagging gives each generated block a durable identity the merge
    /// engine can track across regenerations:
    ///
    /// * the first AST block gets tag `{anchor}` — backward-compatible with
    ///   the original "one block per symbol" tagging,
    /// * each subsequent block gets `{anchor}#{index}` (1-based).
    ///
    /// Tags are positional (stable as long as the extractor is
    /// deterministic) so a content change to the n-th block still matches
    /// base↔ground by tag and produces `UpdateGenerated`, not
    /// `Delete`+`Insert`.
    ///
    /// A `Document` with no AST blocks still produces one empty
    /// `[generated]` block so `propose()` can see the symbol's existence.
    pub fn from_document(doc: &crate::Document) -> Self {
        let mut attrs = doc.attributes.clone();
        attrs.insert("anchor".to_string(), doc.anchor.clone());
        attrs.insert("node_type".to_string(), format!("{:?}", doc.node_type));

        let blocks: Vec<RegionBlock> = if doc.blocks.is_empty() {
            vec![RegionBlock {
                region: ContractRegion::Generated,
                tag: Some(doc.anchor.clone()),
                attributes: HashMap::new(),
                content: String::new(),
                start_line: 1,
                end_line: 1,
            }]
        } else {
            doc.blocks
                .iter()
                .enumerate()
                .map(|(idx, block)| RegionBlock {
                    region: ContractRegion::Generated,
                    tag: Some(if idx == 0 {
                        doc.anchor.clone()
                    } else {
                        format!("{}#{}", doc.anchor, idx)
                    }),
                    attributes: HashMap::new(),
                    content: format_block(block),
                    start_line: 1,
                    end_line: 1,
                })
                .collect()
        };

        Self {
            header_attrs: attrs,
            blocks,
            prose: Vec::new(),
        }
    }
}

/// Serialize one `crate::Block` to the AsciiDoc-flavored body used inside a
/// `[generated]` region. Block kinds map 1:1 to their AsciiDoc form; the
/// trailing newline is included so adjacent blocks compose cleanly.
fn format_block(block: &crate::Block) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    match block {
        crate::Block::Paragraph(text) => {
            let _ = writeln!(out, "{text}");
        }
        crate::Block::Table(table) => {
            let _ = writeln!(out, "|===");
            let header: String = table.headers.iter().map(|h| format!("|{h}")).collect();
            let _ = writeln!(out, "{header}");
            for row in &table.rows {
                let row_str: String = row.iter().map(|c| format!("|{c}")).collect();
                let _ = writeln!(out, "{row_str}");
            }
            let _ = writeln!(out, "|===");
        }
        crate::Block::Listing { language, code } => {
            if let Some(lang) = language {
                let _ = writeln!(out, "[source,{lang}]");
            }
            let _ = writeln!(out, "----");
            let _ = writeln!(out, "{code}");
            let _ = writeln!(out, "----");
        }
        crate::Block::Admonition { kind, text } => {
            let label = match kind {
                crate::AdmonitionKind::Note => "NOTE",
                crate::AdmonitionKind::Tip => "TIP",
                crate::AdmonitionKind::Warning => "WARNING",
                crate::AdmonitionKind::Important => "IMPORTANT",
                crate::AdmonitionKind::Caution => "CAUTION",
            };
            let _ = writeln!(out, "{label}: {text}");
        }
        crate::Block::DescriptionList(items) => {
            for (term, def) in items {
                let _ = writeln!(out, "{term}:: {def}");
            }
        }
        crate::Block::Checklist(items) => {
            for item in items {
                let marker = if item.checked { "[x]" } else { "[ ]" };
                let _ = writeln!(out, "* {marker} {}", item.text);
            }
        }
        crate::Block::Incomplete {
            required_fields,
            hint,
        } => {
            let _ = writeln!(out, "[must-complete]");
            let _ = writeln!(out, "====");
            let _ = writeln!(out, "Required fields:");
            for field in required_fields {
                let _ = writeln!(out, "* {field}");
            }
            let _ = writeln!(out);
            let _ = writeln!(out, "Hint: {hint}");
            let _ = writeln!(out, "====");
        }
    }
    out
}

fn action_index(action: &MergeAction) -> usize {
    match action {
        MergeAction::UpdateGenerated { index, .. } => *index,
        MergeAction::PreserveHuman { index } => *index,
        MergeAction::Conflict { index, .. } => *index,
        MergeAction::InsertGenerated { after_index, .. } => *after_index,
        MergeAction::DeleteGenerated { index, .. } => *index,
    }
}

// ── Parser helpers (shared by aden-parse) ─────────────────────────

/// Parse block-header attributes.
///
/// Canonical form (what `aden_emit::emit_contract_document` writes):
/// `:key: value` pairs, where a value runs until the next `:key:` marker or
/// the end of the header — so values may contain spaces and colons. Falls
/// back to legacy whitespace-separated `key:value` tokens when no canonical
/// marker is present.
fn parse_block_attrs(attr_text: &str) -> HashMap<String, String> {
    let bytes = attr_text.as_bytes();
    // (marker_start, value_start, key)
    let mut markers: Vec<(usize, usize, &str)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        // A marker is `:key:` at the start of the header or after a space.
        if bytes[i] == b':' && (i == 0 || bytes[i - 1] == b' ') {
            let rest = &attr_text[i + 1..];
            if let Some(rel) = rest.find([':', ' '])
                && rest.as_bytes()[rel] == b':'
                && rel > 0
            {
                let key = &rest[..rel];
                let value_start = i + 1 + rel + 1;
                markers.push((i, value_start, key));
                i = value_start;
                continue;
            }
        }
        i += 1;
    }

    let mut attrs = HashMap::new();
    if markers.is_empty() {
        for attr in attr_text.split_whitespace() {
            if let Some((k, v)) = attr.split_once(':') {
                attrs.insert(k.trim_start_matches(':').to_string(), v.trim().to_string());
            }
        }
        return attrs;
    }
    for (n, (_, value_start, key)) in markers.iter().enumerate() {
        let value_end = markers.get(n + 1).map(|m| m.0).unwrap_or(attr_text.len());
        attrs.insert(
            key.to_string(),
            attr_text[*value_start..value_end].trim().to_string(),
        );
    }
    attrs
}

/// Parse a contract document from raw AsciiDoc text.
///
/// This is the exact inverse of `aden_emit::emit_contract_document`: for any
/// valid `ContractDocument`, parsing the emitted text reproduces the document
/// (block line spans are recomputed from the text, not preserved).
///
/// Valid inputs exclude what the line-based format cannot represent:
/// whitespace or newlines in tags, newlines in attribute keys/values,
/// attribute values embedding a ` :word: ` marker, and leading blank prose
/// lines in a document with no header attributes and no blocks.
///
/// # Arguments
/// * `text` — the raw AsciiDoc source.
/// * `mode` — strict (fail on unknown regions / unterminated blocks) or
///   permissive (collect freeform text as prose, never fail).
///
/// # Returns
/// `Ok(ContractDocument)` or `Err(Error::Parse(...))` in strict mode.
pub fn parse_contract(text: &str, mode: ParseMode) -> crate::Result<ContractDocument> {
    let mut doc = ContractDocument::default();
    let mut current_block: Option<RegionBlock> = None;
    let mut block_lines: Vec<String> = Vec::new();
    let mut in_delimited = false;
    let mut delimiter: Option<String> = None;
    let mut line_no = 0usize;
    let mut header_done = false;
    // The emitter writes one blank separator line after each block; consume
    // it instead of collecting it as prose, or every emit/parse cycle would
    // grow the document.
    let mut skip_separator_blank = false;

    for line in text.lines() {
        line_no += 1;
        let trimmed = line.trim();

        // Header attributes (before first block or region)
        if !header_done && current_block.is_none() {
            if trimmed.starts_with(':') {
                let inner = trimmed.trim_start_matches(':');
                if let Some((key, value)) = inner.split_once(':') {
                    doc.header_attrs
                        .insert(key.trim().to_string(), value.trim().to_string());
                    continue;
                }
            } else if trimmed.is_empty() {
                continue;
            } else {
                header_done = true;
            }
        }

        // Detect region header: [region] or [region#tag] or [region :attr: val]
        if !in_delimited
            && trimmed.starts_with('[')
            && trimmed.ends_with(']')
            && !trimmed.starts_with("[[")
        {
            // Flush any previous block
            if let Some(mut block) = current_block.take() {
                block.content = block_lines.join("\n");
                block.end_line = line_no - 1;
                doc.blocks.push(block);
                block_lines.clear();
            }

            let inner = trimmed[1..trimmed.len() - 1].trim();
            let (region_part, attr_text) = if let Some(space) = inner.find(' ') {
                (inner[..space].trim(), Some(inner[space + 1..].trim()))
            } else {
                (inner, None)
            };

            let (region_name, tag) = if let Some(hash) = region_part.find('#') {
                (
                    region_part[..hash].trim(),
                    Some(region_part[hash + 1..].to_string()),
                )
            } else {
                (region_part, None)
            };

            let region = match region_name.parse::<ContractRegion>() {
                Ok(r) => r,
                Err(e) => {
                    if mode == ParseMode::Strict {
                        return Err(crate::Error::Parse(format!(
                            "Line {}: Unknown contract region '{}': {}",
                            line_no, region_name, e
                        )));
                    } else {
                        doc.prose.push(line.to_string());
                        continue;
                    }
                }
            };

            let attrs = attr_text.map(parse_block_attrs).unwrap_or_default();

            current_block = Some(RegionBlock {
                region,
                tag,
                attributes: attrs,
                content: String::new(),
                start_line: line_no,
                end_line: line_no,
            });
            in_delimited = false;
            delimiter = None;
            continue;
        }

        // Handle delimiter lines (----, ====, ++++, ****, etc.)
        if current_block.is_some() {
            if !in_delimited {
                if trimmed.len() >= 4
                    && trimmed
                        .chars()
                        .all(|c| c == '-' || c == '=' || c == '+' || c == '*')
                {
                    in_delimited = true;
                    delimiter = Some(trimmed.to_string());
                    continue;
                } else if !trimmed.is_empty() {
                    block_lines.push(line.to_string());
                }
            } else if Some(trimmed.to_string()) == delimiter {
                in_delimited = false;
                delimiter = None;
                if let Some(mut block) = current_block.take() {
                    block.content = block_lines.join("\n");
                    block.end_line = line_no - 1;
                    doc.blocks.push(block);
                    block_lines.clear();
                }
                skip_separator_blank = true;
            } else {
                block_lines.push(line.to_string());
            }
        } else if mode == ParseMode::Permissive {
            if skip_separator_blank && trimmed.is_empty() {
                skip_separator_blank = false;
                continue;
            }
            skip_separator_blank = false;
            doc.prose.push(line.to_string());
        }
    }

    // Flush trailing block
    if let Some(mut block) = current_block.take() {
        if in_delimited && mode == ParseMode::Strict {
            return Err(crate::Error::Parse(format!(
                "Line {}: unterminated delimited block (started at line {})",
                line_no, block.start_line
            )));
        }
        block.content = block_lines.join("\n");
        block.end_line = line_no;
        doc.blocks.push(block);
    }

    Ok(doc)
}

/// Parsing mode for contract documents.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum ParseMode {
    /// CI-enforced: reject non-profile syntax with line-specific errors.
    Strict,
    /// Authoring-friendly: allow freeform prose outside blocks; warn but never fail.
    Permissive,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_doc(blocks: Vec<RegionBlock>) -> ContractDocument {
        ContractDocument {
            header_attrs: HashMap::new(),
            blocks,
            prose: Vec::new(),
        }
    }

    fn gen_block(tag: Option<&str>, content: &str) -> RegionBlock {
        RegionBlock {
            region: ContractRegion::Generated,
            tag: tag.map(|s| s.to_string()),
            attributes: HashMap::new(),
            content: content.to_string(),
            start_line: 1,
            end_line: 1,
        }
    }

    fn human_block(tag: Option<&str>, content: &str) -> RegionBlock {
        RegionBlock {
            region: ContractRegion::Human,
            tag: tag.map(|s| s.to_string()),
            attributes: HashMap::new(),
            content: content.to_string(),
            start_line: 1,
            end_line: 1,
        }
    }

    #[test]
    fn test_preserve_human_blocks() {
        let base = make_doc(vec![
            gen_block(Some("foo"), "fn foo() {}"),
            human_block(Some("note"), "TODO: refactor"),
        ]);
        let ground = make_doc(vec![gen_block(Some("foo"), "fn foo() -> i32 {}")]);
        let working = make_doc(vec![
            gen_block(Some("foo"), "fn foo() {}"),
            human_block(Some("note"), "TODO: refactor"),
        ]);

        let state = ContractState::new(ground, base, working);
        let proposal = state.propose().unwrap();

        assert_eq!(proposal.updated_count, 1);
        assert_eq!(proposal.preserved_count, 1);
        assert_eq!(proposal.conflict_count, 0);
    }

    #[test]
    fn test_conflict_when_human_block_same_tag() {
        let base = make_doc(vec![gen_block(Some("foo"), "fn foo() {}")]);
        let ground = make_doc(vec![gen_block(Some("foo"), "fn foo() -> i32 {}")]);
        let working = make_doc(vec![
            gen_block(Some("foo"), "fn foo() {}"),
            human_block(Some("foo"), "Don't change this"),
        ]);

        let state = ContractState::new(ground, base, working);
        let proposal = state.propose().unwrap();

        assert_eq!(proposal.conflict_count, 1);
    }

    #[test]
    fn test_delete_generated_when_symbol_removed() {
        let base = make_doc(vec![
            gen_block(Some("foo"), "fn foo() {}"),
            gen_block(Some("bar"), "fn bar() {}"),
        ]);
        let ground = make_doc(vec![gen_block(Some("foo"), "fn foo() {}")]);
        let working = base.clone();

        let state = ContractState::new(ground, base, working);
        let proposal = state.propose().unwrap();

        assert_eq!(proposal.deleted_count, 1);
    }

    #[test]
    fn test_insert_new_generated_block() {
        let base = make_doc(vec![gen_block(Some("foo"), "fn foo() {}")]);
        let ground = make_doc(vec![
            gen_block(Some("foo"), "fn foo() {}"),
            gen_block(Some("baz"), "fn baz() {}"),
        ]);
        let working = base.clone();

        let state = ContractState::new(ground, base, working);
        let proposal = state.propose().unwrap();

        assert_eq!(proposal.inserted_count, 1);
    }

    #[test]
    fn test_region_roundtrip() {
        for region in [
            ContractRegion::Generated,
            ContractRegion::Human,
            ContractRegion::Agent,
            ContractRegion::Security,
            ContractRegion::Contract,
            ContractRegion::Constitution,
            ContractRegion::Override,
            ContractRegion::Proposed,
        ] {
            let serialized = serde_json::to_string(&region).unwrap();
            let deserialized: ContractRegion = serde_json::from_str(&serialized).unwrap();
            assert_eq!(region, deserialized);
        }
    }

    #[test]
    fn test_apply_updates_generated_only() {
        let base = make_doc(vec![gen_block(Some("foo"), "old")]);
        let ground = make_doc(vec![gen_block(Some("foo"), "new")]);
        let working = base.clone();

        let state = ContractState::new(ground, base, working);
        let proposal = state.propose().unwrap();
        let merged = state.apply(&proposal).unwrap();

        assert_eq!(merged.blocks[0].content, "new");
    }

    // ── reconcile_anchor / overlay integration ───────────────────────

    fn doc(anchor: &str, body: &str) -> crate::Document {
        crate::Document {
            anchor: anchor.to_string(),
            blocks: vec![crate::Block::Paragraph(body.to_string())],
            ..Default::default()
        }
    }

    #[test]
    fn reconcile_update_no_overlay() {
        let stored = doc("foo", "old");
        let fresh = doc("foo", "new");
        let p = reconcile_anchor(&fresh, Some(&stored), None).unwrap();
        assert!(p.is_clean());
        assert_eq!(p.updated_count, 1);
    }

    #[test]
    fn reconcile_insert_new_symbol() {
        let fresh = doc("foo", "body");
        let p = reconcile_anchor(&fresh, None, None).unwrap();
        assert!(p.is_clean());
        assert_eq!(p.inserted_count, 1);
    }

    #[test]
    fn reconcile_preserves_unrelated_overlay() {
        let stored = doc("foo", "old");
        let fresh = doc("foo", "new");
        let overlay = make_doc(vec![human_block(Some("notes"), "design rationale")]);
        let p = reconcile_anchor(&fresh, Some(&stored), Some(&overlay)).unwrap();
        assert!(p.is_clean(), "unrelated overlay tag must not conflict");
        assert_eq!(p.updated_count, 1);
    }

    #[test]
    fn reconcile_conflict_same_tag() {
        let stored = doc("foo", "old");
        let fresh = doc("foo", "new");
        let overlay = make_doc(vec![human_block(Some("foo"), "do not change foo")]);
        let p = reconcile_anchor(&fresh, Some(&stored), Some(&overlay)).unwrap();
        assert!(!p.is_clean());
        assert_eq!(p.conflict_count, 1);
        assert_eq!(p.updated_count, 0);
    }

    #[test]
    fn reconcile_agent_overlay_conflicts_too() {
        let stored = doc("foo", "old");
        let fresh = doc("foo", "new");
        let agent = RegionBlock {
            region: ContractRegion::Agent,
            tag: Some("foo".to_string()),
            attributes: HashMap::new(),
            content: "agent note about foo".to_string(),
            start_line: 1,
            end_line: 1,
        };
        let overlay = make_doc(vec![agent]);
        let p = reconcile_anchor(&fresh, Some(&stored), Some(&overlay)).unwrap();
        assert_eq!(p.conflict_count, 1, "[agent] blocks are durable");
    }

    #[test]
    fn delete_orphaning_durable_block_is_conflict() {
        let base = make_doc(vec![
            gen_block(Some("foo"), "fn foo() {}"),
            gen_block(Some("bar"), "fn bar() {}"),
        ]);
        let ground = make_doc(vec![gen_block(Some("foo"), "fn foo() {}")]); // bar deleted
        let working = make_doc(vec![
            gen_block(Some("foo"), "fn foo() {}"),
            gen_block(Some("bar"), "fn bar() {}"),
            human_block(Some("bar"), "keep these notes about bar"),
        ]);
        let p = ContractState::new(ground, base, working).propose().unwrap();
        assert_eq!(p.conflict_count, 1);
        assert_eq!(p.deleted_count, 0, "must not silently orphan the overlay");
    }

    // ── parse_contract: malformed input and header parsing ───────────

    // ── Phase 2: per-symbol [generated] blocks + InsertGenerated.tag ──

    #[test]
    fn from_document_emits_one_region_per_block() {
        // A Document for a symbol with three AST-derived blocks (summary
        // paragraph, signature listing, parameter table) must produce three
        // separate `[generated]` RegionBlocks so the merge engine can update
        // them individually instead of treating the whole symbol as one opaque
        // string.
        let doc = crate::Document {
            anchor: "foo".to_string(),
            blocks: vec![
                crate::Block::Paragraph("does the foo".to_string()),
                crate::Block::Listing {
                    language: Some("rust".to_string()),
                    code: "fn foo() {}".to_string(),
                },
                crate::Block::Table(crate::Table {
                    headers: vec!["param".to_string(), "type".to_string()],
                    rows: vec![vec!["x".to_string(), "i32".to_string()]],
                }),
            ],
            ..Default::default()
        };
        let cd = ContractDocument::from_document(&doc);
        assert_eq!(
            cd.blocks.len(),
            3,
            "one RegionBlock per AST block, got {} blocks",
            cd.blocks.len()
        );
        let tags: Vec<Option<&str>> = cd.blocks.iter().map(|b| b.tag.as_deref()).collect();
        assert_eq!(
            tags,
            vec![Some("foo"), Some("foo#1"), Some("foo#2")],
            "block tags must give each AST block durable identity",
        );
        assert!(
            cd.blocks[0].content.contains("does the foo"),
            "first block carries the paragraph"
        );
        assert!(
            cd.blocks[1].content.contains("fn foo()"),
            "second block carries the listing"
        );
        assert!(
            cd.blocks[2].content.contains("|param"),
            "third block carries the table"
        );
    }

    #[test]
    fn from_document_with_no_blocks_still_represents_symbol() {
        // A Document with empty `blocks` (declaration-only symbol) must still
        // produce one RegionBlock so `reconcile_anchor` records its existence
        // — otherwise propose() can't even see the symbol to update or delete.
        let doc = crate::Document {
            anchor: "stub".to_string(),
            blocks: Vec::new(),
            ..Default::default()
        };
        let cd = ContractDocument::from_document(&doc);
        assert_eq!(cd.blocks.len(), 1);
        assert_eq!(cd.blocks[0].tag.as_deref(), Some("stub"));
    }

    #[test]
    fn propose_insert_carries_ground_tag() {
        // When a new generated block appears in `ground` that `base` doesn't
        // have, the `InsertGenerated` action must carry the ground block's
        // tag — otherwise apply() inserts an untagged block and the next
        // gen/heal cycle can't match it back to its source symbol.
        let base = make_doc(vec![gen_block(Some("foo"), "fn foo() {}")]);
        let ground = make_doc(vec![
            gen_block(Some("foo"), "fn foo() {}"),
            gen_block(Some("baz"), "fn baz() {}"),
        ]);
        let working = base.clone();
        let proposal = ContractState::new(ground, base, working).propose().unwrap();
        let inserted_tags: Vec<&Option<String>> = proposal
            .actions
            .iter()
            .filter_map(|a| match a {
                MergeAction::InsertGenerated { tag, .. } => Some(tag),
                _ => None,
            })
            .collect();
        assert_eq!(inserted_tags.len(), 1);
        assert_eq!(
            inserted_tags[0].as_deref(),
            Some("baz"),
            "InsertGenerated must record the inserted block's tag"
        );
    }

    #[test]
    fn apply_inserted_block_keeps_tag() {
        // Round-trip: the block apply() writes into `working` must carry the
        // tag from the action, so a follow-up propose() finds it by tag in
        // base→ground matching.
        let base = make_doc(vec![gen_block(Some("foo"), "fn foo() {}")]);
        let ground = make_doc(vec![
            gen_block(Some("foo"), "fn foo() {}"),
            gen_block(Some("baz"), "fn baz() {}"),
        ]);
        let working = base.clone();
        let state = ContractState::new(ground, base, working);
        let proposal = state.propose().unwrap();
        let merged = state.apply(&proposal).unwrap();

        let baz = merged
            .blocks
            .iter()
            .find(|b| b.tag.as_deref() == Some("baz"))
            .expect("inserted block must be findable by its tag");
        assert!(baz.is_generated());
        assert!(baz.content.contains("fn baz()"));
    }

    #[test]
    fn parse_strict_unknown_region_is_error() {
        let text = "[bogus]\n----\nx\n----\n";
        assert!(matches!(
            parse_contract(text, ParseMode::Strict),
            Err(crate::Error::Parse(_))
        ));
    }

    #[test]
    fn parse_permissive_unknown_region_becomes_prose() {
        let text = "[bogus]\n----\nx\n----\n";
        let doc = parse_contract(text, ParseMode::Permissive).unwrap();
        assert!(doc.blocks.is_empty());
        assert!(doc.prose.iter().any(|l| l.contains("[bogus]")));
    }

    #[test]
    fn parse_strict_unterminated_block_is_error() {
        let text = "[generated#foo]\n----\nnever closed";
        assert!(matches!(
            parse_contract(text, ParseMode::Strict),
            Err(crate::Error::Parse(_))
        ));
    }

    #[test]
    fn parse_permissive_unterminated_block_flushes() {
        let text = "[generated#foo]\n----\nnever closed";
        let doc = parse_contract(text, ParseMode::Permissive).unwrap();
        assert_eq!(doc.blocks.len(), 1);
        assert_eq!(doc.blocks[0].content, "never closed");
    }

    #[test]
    fn parse_block_attrs_with_spaced_values() {
        let text = "[proposed#foo :reason: two words here :status: conflict]\n----\nc\n----\n";
        let doc = parse_contract(text, ParseMode::Strict).unwrap();
        let b = &doc.blocks[0];
        assert_eq!(b.region, ContractRegion::Proposed);
        assert_eq!(b.tag.as_deref(), Some("foo"));
        assert_eq!(
            b.attributes.get("reason").map(String::as_str),
            Some("two words here")
        );
        assert_eq!(
            b.attributes.get("status").map(String::as_str),
            Some("conflict")
        );
    }

    #[test]
    fn parse_legacy_token_attrs_still_supported() {
        let text = "[generated#foo k:v]\n----\nc\n----\n";
        let doc = parse_contract(text, ParseMode::Strict).unwrap();
        assert_eq!(
            doc.blocks[0].attributes.get("k").map(String::as_str),
            Some("v")
        );
    }

    #[test]
    fn parse_header_attrs_and_prose() {
        let text = ":source_hash: abc123\n\nFreeform prose line.\n";
        let doc = parse_contract(text, ParseMode::Permissive).unwrap();
        assert_eq!(
            doc.header_attrs.get("source_hash").map(String::as_str),
            Some("abc123")
        );
        assert_eq!(doc.prose, vec!["Freeform prose line.".to_string()]);
    }

    #[test]
    fn overlay_onto_ignores_generated_overlay_blocks() {
        let base = make_doc(vec![gen_block(Some("foo"), "g")]);
        let overlay = make_doc(vec![
            human_block(Some("n"), "note"),
            gen_block(Some("foo"), "SHOULD BE IGNORED"),
        ]);
        let working = overlay_onto(&base, Some(&overlay));
        assert_eq!(
            working.blocks.len(),
            2,
            "only base gen + human overlay block"
        );
        assert!(
            working
                .blocks
                .iter()
                .any(|b| b.region == ContractRegion::Human)
        );
        assert!(
            working.blocks.iter().filter(|b| b.is_generated()).count() == 1,
            "overlay generated blocks must be dropped"
        );
    }
}
