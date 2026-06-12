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
use crate::{Block, Document, Error, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// FNV-1a 32-bit hash of a byte slice.
///
/// Inline implementation — no new dependency.
/// offset_basis = 0x811c9dc5, prime = 0x01000193.
fn fnv1a32(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c9dc5;
    for &b in bytes {
        hash ^= b as u32;
        hash = hash.wrapping_mul(0x01000193);
    }
    hash
}

/// Map an anchor to a filesystem- and proposal-id-safe slug.
///
/// Keeps ASCII alphanumerics and `-`, `_`, `.`; every other byte (e.g. the `:`,
/// `/`, `#` in `aden://module/foo.rs#bar`) becomes `-`. Matches the character
/// class `aden_propose` accepts for proposal ids, so a slug is reusable there.
///
/// **Unicode disambiguation**: if the anchor contains any non-ASCII character,
/// an 8-hex-digit FNV-1a 32-bit hash suffix of the full anchor bytes is appended
/// (`"{sanitized}-{hash:08x}"`). This prevents two Unicode anchors that map to
/// the same ASCII-sanitized form from colliding. Pure-ASCII anchors are unchanged
/// (backward compat — existing overlay files on disk keep resolving).
///
/// NOTE: A collision between two *pure-ASCII* anchors is still theoretically
/// possible; the `:anchor:` header check in [`load_overlay`] guards against that
/// case misapplying intent.
pub fn sanitize_anchor_filename(anchor: &str) -> String {
    let has_non_ascii = anchor.bytes().any(|b| b > 0x7f);
    let sanitized: String = anchor
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if has_non_ascii {
        format!("{sanitized}-{:08x}", fnv1a32(anchor.as_bytes()))
    } else {
        sanitized
    }
}

/// Path to the (sparse, git-tracked) intent overlay for an anchor.
pub fn overlay_path(root: &Path, anchor: &str) -> PathBuf {
    root.join(".aden")
        .join("overlays")
        .join(format!("{}.adoc", sanitize_anchor_filename(anchor)))
}

/// Load the intent overlay for an anchor, if one exists.
///
/// Parsed permissively so freeform prose never fails. Returns `Ok(None)` when no
/// overlay is present (the common case) or it cannot be read/parsed.
///
/// Collision guard: an overlay carries an `:anchor:` header naming its true
/// anchor. If present and not matching `anchor`, this is a slug collision —
/// the file belongs to a different anchor. Rather than silently dropping intent,
/// this returns `Err(Error::InvalidAnchor(...))` so callers can surface the
/// conflict. The hash-suffix scheme in [`sanitize_anchor_filename`] prevents
/// collisions for Unicode anchors; this guard remains for pure-ASCII edge cases.
pub fn load_overlay(root: &Path, anchor: &str) -> Result<Option<ContractDocument>> {
    let path = overlay_path(root, anchor);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Ok(None),
    };
    let doc = match parse_contract(&text, ParseMode::Permissive) {
        Ok(d) => d,
        Err(_) => return Ok(None),
    };
    if let Some(declared) = doc.header_attrs.get("anchor")
        && declared != anchor
    {
        return Err(Error::InvalidAnchor(format!(
            "overlay {} declares anchor '{}' but was loaded for '{}' (slug collision)",
            path.display(),
            declared,
            anchor
        )));
    }
    Ok(Some(doc))
}

/// Convenience wrapper: like [`load_overlay`] but returns `None` on collision,
/// logging the error to stderr. Use in contexts that cannot propagate errors
/// (e.g. graph construction during cache build).
pub fn load_overlay_lossy(root: &Path, anchor: &str) -> Option<ContractDocument> {
    match load_overlay(root, anchor) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("ERROR: {e}");
            None
        }
    }
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

/// Persist an intent overlay for `anchor` to disk.
///
/// Writes `.aden/overlays/<slug>.adoc`. The file begins with an `:anchor:` header
/// so [`load_overlay`] can verify ownership and detect slug collisions.
/// Creates `.aden/overlays/` if it does not exist.
pub fn save_overlay(root: &Path, anchor: &str, content: &str) -> Result<()> {
    let path = overlay_path(root, anchor);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::Io(e.to_string()))?;
    }
    // Prepend the anchor header if content does not already start with it.
    let header = format!(":anchor: {anchor}\n");
    let body = if content.starts_with(&header) {
        content.to_string()
    } else {
        format!("{header}\n{content}")
    };
    std::fs::write(&path, body).map_err(|e| Error::Io(e.to_string()))
}

/// Load the overlay for `anchor` (if any) and fold its durable intent into
/// `doc`. No-op when there is no overlay. Convenience for graph construction.
///
/// Uses [`load_overlay_lossy`] so slug-collision errors are reported to stderr
/// rather than propagated (this function is called from non-`Result` graph
/// construction paths).
pub fn fold_overlay(root: &Path, anchor: &str, doc: &mut Document) {
    if let Some(overlay) = load_overlay_lossy(root, anchor) {
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

    // --- Red-green: pure-ASCII anchor is unchanged (backward compat) ---
    #[test]
    fn ascii_anchor_slug_unchanged() {
        // Pin the exact output — existing overlay files on disk must keep resolving.
        let slug = sanitize_anchor_filename("aden://module/src/lib.rs#alpha");
        assert_eq!(
            slug, "aden---module-src-lib.rs-alpha",
            "pure-ASCII slug must be identical to the pre-hash scheme"
        );
    }

    // --- Red-green: two distinct Unicode anchors produce DIFFERENT filenames ---
    #[test]
    fn unicode_anchors_with_same_char_count_differ() {
        // 数据 (Chinese) vs データ (Japanese katakana): both are 2 chars but
        // different code points → same sanitized body but different FNV-1a hash.
        let anchor_zh = "aden://module/src/lib.rs#数据";
        let anchor_ja = "aden://module/src/lib.rs#データ";
        let slug_zh = sanitize_anchor_filename(anchor_zh);
        let slug_ja = sanitize_anchor_filename(anchor_ja);
        assert_ne!(
            slug_zh, slug_ja,
            "Unicode anchors of equal char count must produce different slugs"
        );
        // Both must still be filesystem-safe (ASCII only)
        for slug in [&slug_zh, &slug_ja] {
            assert!(
                slug.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.'),
                "slug must be ASCII-safe: {slug}"
            );
        }
    }

    // --- Red-green: save_overlay + load_overlay round-trips with a Unicode anchor ---
    #[test]
    fn unicode_anchor_roundtrips() {
        let dir = std::env::temp_dir().join(format!("aden-overlay-unicode-{}", std::process::id()));
        let anchor = "aden://module/src/lib.rs#数据";
        save_overlay(
            &dir,
            anchor,
            "[human#数据]\n----\nUnicode intent preserved.\n----\n",
        )
        .expect("save_overlay must succeed for a Unicode anchor");
        let doc = load_overlay(&dir, anchor)
            .expect("load_overlay must not error")
            .expect("overlay must be found after save");
        assert!(
            doc.blocks
                .iter()
                .any(|b| matches!(b, crate::contract::RegionBlock { content, .. } if content.contains("Unicode intent preserved"))),
            "saved content must round-trip through load"
        );
        let _ = std::fs::remove_dir_all(&dir);
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
    fn collision_header_mismatch_errors() {
        let dir = std::env::temp_dir().join(format!("aden-overlay-coll-{}", std::process::id()));
        let anchor = "aden://module/src/foo.rs#bar";
        let path = overlay_path(&dir, anchor);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            ":anchor: aden://module/src/other.rs#x\n\n[human#x]\n----\nwrong.\n----\n",
        )
        .unwrap();
        let result = load_overlay(&dir, anchor);
        assert!(
            matches!(result, Err(Error::InvalidAnchor(_))),
            "slug collision must return Err(InvalidAnchor), got: {result:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
