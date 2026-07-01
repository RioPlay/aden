// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Context assembler: transforms a subgraph of `.aden` documents into a
//! single flat AsciiDoc string ready for LLM ingestion.

pub mod traverse;

pub use aden_parse::asciidoc_preprocess::{
    PreprocessError, PreprocessOptions, preprocess, preprocess_for_index, preprocess_with_options,
};
pub use traverse::{
    AssemblyOptions, assemble, assemble_adg, assemble_with_anchors, assemble_with_anchors_mmr,
};

#[cfg(test)]
mod tests;
