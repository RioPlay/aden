// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::Path;

/// Slim a document before storing it. Drops the `edge::calls[...]` listing
/// block — it is redundant with the `Callee` table for display and is no longer
/// needed for linking (callees are carried out of the parse phase directly), so
/// storing it just bloats the (already large) store on big repos.
/// `pub(crate)`: heal's merge reconciliation re-parses source to build the
/// `ground` layer and must apply the same slimming, or every reconcile sees
/// phantom diffs against the slimmed store/base.
pub(crate) fn slim_doc_for_store(doc: &mut aden_core::Document) {
    use aden_core::Block;
    doc.blocks.retain(|b| {
        let Block::Listing { code, .. } = b else {
            return true;
        };
        // Drop only listings that are purely `edge::` macros.
        !code
            .lines()
            .filter(|l| !l.trim().is_empty())
            .all(|l| l.trim().starts_with("edge::"))
    });
}
/// Persist one review *notice* per guarded change into `.aden/proposals/`,
/// reusing the existing `aden_propose` pipeline. A notice records that a symbol
/// carrying durable overlay intent had its generated content updated, so the
/// author re-reviews the overlay; it is informational (the store was already
/// updated and the overlay preserved). Ids are deterministic
/// (`overlay-review-<sanitized-anchor>`) so the same change overwrites the same
/// file rather than accumulating. Returns the number written.
pub(crate) fn write_merge_proposals(
    root: &Path,
    conflicts: &[(String, aden_core::contract::MergeProposal)],
) -> usize {
    use crate::commands::overlay;
    use std::fmt::Write as _;

    let mut written = 0usize;
    for (anchor, proposal) in conflicts {
        let slug = overlay::sanitize_anchor_filename(anchor);
        let mut patch = String::new();
        let _ = writeln!(patch, "// Overlay review notice for {anchor}");
        for action in &proposal.actions {
            if let aden_core::contract::MergeAction::Conflict { reason, .. } = action {
                let _ = writeln!(patch, "// CHANGED: {reason}");
            }
        }
        let _ = writeln!(
            patch,
            "//\n// The generated layer was updated and your overlay was preserved.\n// Re-check that your intent still holds: .aden/overlays/{slug}.adoc"
        );

        let prop = aden_propose::Proposal {
            id: format!("overlay-review-{slug}"),
            target_path: overlay::overlay_path(root, anchor),
            drift_type: "OverlayReview".to_string(),
            confidence: 0.5,
            status: aden_propose::ProposalStatus::PendingReview,
            rationale: format!(
                "Generated content for {anchor} changed while a durable [human]/[agent] overlay annotates it; store updated, overlay preserved — re-review the annotation."
            ),
            patch_asciidoc: patch,
        };
        if aden_propose::persist(&prop, root).is_ok() {
            written += 1;
        }
    }
    written
}
