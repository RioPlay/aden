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
//! Generic graph engine for the Aden knowledge graph.
//!
//! `.adoc` files are the structural harness — the canonical format for
//! defining nodes, edges, and references. But the engine itself is
//! language-agnostic: any knowledge type can be indexed.
//!
//! ## Architecture
//!
//! ```text
//! .adoc files (human-readable, git-tracked)
//!     -> parse
//! DocumentNode (implements GraphNode trait)
//!     ->
//! AdenGraph<N, E> (generic graph engine)
//!     -> persist
//! aden-store (Sled + Postcard, or RocksDB/TiKV later)
//! ```
//!
//! ## Key modules
//!
//! - `nodes` — `GraphNode` and `GraphEdge` traits (language-agnostic)
//! - `nodes::aden` — `DocumentNode` and `AdenEdge` (aden-specific impl)
//! - `graph` — `AdenGraph<N, E>` generic graph engine
//! - `parser` — `.adoc` → `DocumentNode`
//! - `cache` — persistence layer (Sled/Postcard + JSON fallback)
//! - `bridge` — sync between in-memory graph and storage

pub mod backlinks;
pub mod bridge;
pub mod cache;
pub mod cycles;
pub mod graph;
pub mod integrity;
pub mod nodes;
pub mod parser;
pub mod query;

pub use graph::AdenGraph;
pub use nodes::aden::{AdenEdge, DocumentNode};
pub use nodes::{GraphEdge, GraphNode};
pub use petgraph::Direction;

#[cfg(test)]
mod tests;
pub use parser::ParsedDocument;
