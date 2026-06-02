// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Intent-overlay helpers. The implementation lives in [`aden_core::overlay`]
//! so lower crates (e.g. `aden-graph`, which folds overlays into the read graph)
//! can share it; this module re-exports it for CLI call sites.

pub use aden_core::overlay::{
    load_overlay, overlay_path, overlay_slugs, sanitize_anchor_filename,
};

use std::path::Path;

/// `aden overlay <anchor>` — scaffold (or locate) an intent overlay for a symbol.
///
/// Resolves a bare symbol name to its full anchor against the store when
/// possible, then writes a starter overlay carrying the `:anchor:` header and a
/// guard-form `[human#<anchor>]` block. Never clobbers an existing overlay.
pub fn cmd_overlay(path: &Path, anchor: &str) -> Result<(), Box<dyn std::error::Error>> {
    // Prefer the fully-resolved anchor so the file binds to a real symbol; fall
    // back to the literal input for pre-authoring a not-yet-generated symbol.
    let resolved = aden_graph::cache::resolve_anchor_in_store(path, anchor).unwrap_or_else(|| {
        eprintln!(
            "Note: '{anchor}' not found in the store yet — scaffolding an overlay for it anyway."
        );
        anchor.to_string()
    });

    let file = overlay_path(path, &resolved);
    if file.exists() {
        println!("Overlay already exists: {}", file.display());
        println!("Edit it directly; `aden gen` will preserve and deliver its contents.");
        return Ok(());
    }
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let scaffold = format!(
        ":anchor: {resolved}\n\
         \n\
         // Intent overlay for {resolved}.\n\
         // Blocks here survive `aden gen` and are delivered to the context-assembling readers (asm/ask).\n\
         //\n\
         // GUARD: the [human#<anchor>] block below is tagged with the full anchor, so\n\
         // when this symbol's generated content changes you get a one-time review\n\
         // notice to re-check your note. Use a plain [human] block (no tag) to annotate\n\
         // without the notice. Both are always preserved and delivered.\n\
         \n\
         [human#{resolved}]\n\
         ----\n\
         TODO: describe the invariant or intent to preserve for this symbol.\n\
         ----\n"
    );
    std::fs::write(&file, scaffold)?;
    println!("Created overlay: {}", file.display());
    println!("Edit it, then run `aden gen` — your intent is preserved and delivered to readers.");
    Ok(())
}
