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
//! Graph engine for the Aden knowledge graph.
//!
//! Parses `.adoc` / `.aden` files, builds a directed graph of Documents,
//! validates references, detects cycles, and injects backlinks.

pub mod backlinks;
pub mod cache;
pub mod cycles;
pub mod graph;
pub mod integrity;
pub mod parser;

pub use graph::{AdenGraph, DocumentNode};
pub use petgraph::Direction;

#[cfg(test)]
mod tests;
pub use parser::ParsedDocument;
