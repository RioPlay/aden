// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// All rights reserved.
//
// Aden: A Dense Referential Context Compiler
// Original author and maintainer: RioPlay
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
//! Context assembler: transforms a subgraph of `.aden` documents into a
//! single flat AsciiDoc string ready for LLM ingestion.

pub mod preprocess;
pub mod traverse;

pub use preprocess::{PreprocessError, preprocess};
pub use traverse::{AssemblyOptions, assemble, assemble_adg};

#[cfg(test)]
mod tests;
