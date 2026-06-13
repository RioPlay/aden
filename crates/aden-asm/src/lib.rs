// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Context assembler: transforms a subgraph of `.aden` documents into a
//! single flat AsciiDoc string ready for LLM ingestion.

pub mod preprocess;
pub mod traverse;

pub use preprocess::{PreprocessError, preprocess};
pub use traverse::{AssemblyOptions, assemble, assemble_adg, assemble_with_anchors};

#[cfg(test)]
mod tests;
