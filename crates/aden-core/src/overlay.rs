// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Intent overlays: sparse, git-tracked, per-anchor `.adoc` files holding
//! durable `[human]`/`[agent]` contract content. The generated layer lives in
//! the rebuildable store; overlays are the *only* place durable human/agent
//! intent is persisted, so they live in version control while the store stays
//! ignored.
//!
//! Two roles:
//! * `gen` reads an overlay as the `working` layer of the three-way merge (see
//!   [`crate::contract::reconcile_anchor`]) so regeneration never clobbers it.
//! * graph construction folds an overlay's durable blocks into the in-memory
//!   document ([`fold_overlay`]) so the context-assembling readers (`asm`/`ask`)
//!   surface the intent — without polluting the pure-generated store. (Structural
//!   readers like `query`/`check` traverse the same graph but emit topology, not
//!   prose, so the folded block text appears only in `asm`/`ask` output.)

use crate::contract::{ContractDocument, ParseMode, parse_contract};
use crate::{Block, Document};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Map an anchor to a filesystem- and proposal-id-safe slug.
///
/// Keeps ASCII alphanumerics and `-`, `_`, `.`; every other byte (e.g. the `:`,
/// `/`, `#` in `aden://module/foo.rs#bar`) becomes `-`. Matches the character
/// class `aden_propose` accepts for proposal ids, so a slug is reusable there.
///
/// NOTE: this is lossy — two distinct anchors *could* collapse to the same slug.
/// The `:anchor:` header check in [`load_overlay`] guards against a clash
/// misapplying intent.
pub fn sanitize_anchor_filename(anchor: &str) -> String {
    anchor
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Path to the (sparse, git-tracked) intent overlay for an anchor.
pub fn overlay_path(root: &Path, anchor: &str) -> PathBuf {
    root.join(".aden")
        .join("overlays")
        .join(format!("{}.adoc", sanitize_anchor_filename(anchor)))
}

/// Load the intent overlay for an anchor, if one exists.
///
/// Parsed permissively so freeform prose never fails. Returns `None` when no
/// overlay is present (the common case) or it cannot be read/parsed.
///
/// Collision guard: an overlay carries an `:anchor:` header naming its true
/// anchor. If present and not matching `anchor`, the file belongs to a different
/// (slug-colliding) anchor and is ignored, so a clash never misapplies intent.
pub fn load_overlay(root: &Path, anchor: &str) -> Option<ContractDocument> {
    let path = overlay_path(root, anchor);
    let text = std::fs::read_to_string(&path).ok()?;
    let doc = parse_contract(&text, ParseMode::Permissive).ok()?;
    if let Some(declared) = doc.header_attrs.get("anchor")
        && declared != anchor
    {
        eprintln!(
            "WARN: overlay {} declares anchor '{}' but was loaded for '{}' (slug collision); ignoring.",
            path.display(),
            declared,
            anchor
        );
        return None;
    }
    Some(doc)
}

/// The set of anchor slugs that have an overlay file on disk.
///
/// Computed once per run so callers can skip per-symbol work for the
/// overwhelming majority of symbols that have no overlay. Empty (and cheap) when
/// `.aden/overlays/` is absent.
pub fn overlay_slugs(root: &Path) -> HashSet<String> {
    let dir = root.join(".aden").join("overlays");
    let mut slugs = HashSet::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("adoc")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                slugs.insert(stem.to_string());
            }
        }
    }
    slugs
}

/// Append an overlay's durable (`[human]`/`[agent]`/…) blocks to a document as
/// readable paragraphs, so any consumer of the document surfaces the intent.
///
/// Generated blocks in the overlay are ignored — overlays carry intent only.
/// The store document itself is never modified on disk; this mutates the
/// in-memory copy used to build the read graph, keeping the store pure-generated.
pub fn fold_overlay_blocks(doc: &mut Document, overlay: &ContractDocument) {
    for block in &overlay.blocks {
        if block.is_durable() {
            let label = block
                .tag
                .as_deref()
                .map(|t| format!("{}#{t}", block.region))
                .unwrap_or_else(|| block.region.to_string());
            doc.blocks
                .push(Block::Paragraph(format!("[{label}] {}", block.content)));
        }
    }
}

/// Load the overlay for `anchor` (if any) and fold its durable intent into
/// `doc`. No-op when there is no overlay. Convenience for graph construction.
pub fn fold_overlay(root: &Path, anchor: &str, doc: &mut Document) {
    if let Some(overlay) = load_overlay(root, anchor) {
        fold_overlay_blocks(doc, &overlay);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_real_anchor() {
        let slug = sanitize_anchor_filename("aden://module/src/foo.rs#bar");
        assert!(
            slug.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
        );
        assert!(slug.contains("foo.rs"));
    }

    #[test]
    fn overlay_roundtrips_and_folds() {
        let dir = std::env::temp_dir().join(format!("aden-overlay-core-{}", std::process::id()));
        let anchor = "aden://module/src/foo.rs#bar";
        let path = overlay_path(&dir, anchor);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            ":anchor: aden://module/src/foo.rs#bar\n\n[human#bar]\n----\nKeep this rationale.\n----\n",
        )
        .unwrap();

        let mut doc = Document {
            anchor: anchor.to_string(),
            blocks: vec![Block::Paragraph("fn bar()".into())],
            ..Default::default()
        };
        fold_overlay(&dir, anchor, &mut doc);

        assert!(
            doc.blocks
                .iter()
                .any(|b| matches!(b, Block::Paragraph(p) if p.contains("Keep this rationale"))),
            "durable overlay block must be folded into the document"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collision_header_mismatch_ignored() {
        let dir = std::env::temp_dir().join(format!("aden-overlay-coll-{}", std::process::id()));
        let anchor = "aden://module/src/foo.rs#bar";
        let path = overlay_path(&dir, anchor);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            ":anchor: aden://module/src/other.rs#x\n\n[human#x]\n----\nwrong.\n----\n",
        )
        .unwrap();
        assert!(
            load_overlay(&dir, anchor).is_none(),
            "mismatched anchor must be ignored"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
