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
//! Generic node and edge traits for the Aden knowledge graph.
//!
//! `aden-graph` is language-agnostic: any node/edge type can be used
//! as long as it implements the traits defined here.
//!
//! ## Example: Implementing for a custom node type
//!
//! ```ignore
//! use aden_graph::nodes::{GraphNode, GraphEdge};
//! use aden_graph::nodes::aden::{DocumentNode, AdenEdge};
//!
//! #[derive(Clone)]
//! struct MyNode {
//!     anchor: String,
//!     source_path: PathBuf,
//!     data: HashMap<String, String>,
//! }
//!
//! impl GraphNode for MyNode {
//!     fn anchor(&self) -> &str { &self.anchor }
//!     fn source_path(&self) -> &PathBuf { &self.source_path }
//!     fn attributes(&self) -> &HashMap<String, String> { &self.data }
//! }
//! ```

pub mod aden;

pub use aden::{AdenEdge, DocumentNode};

use std::collections::HashMap;
use std::path::PathBuf;

/// A node in the knowledge graph.
///
/// Any type implementing this can be used as a graph node.
/// The trait exposes the minimal interface needed for graph operations.
pub trait GraphNode: Clone + std::fmt::Debug {
    /// Unique identifier for this node (anchor).
    fn anchor(&self) -> &str;

    /// Source file path where this node was defined.
    fn source_path(&self) -> &PathBuf;

    /// Key-value metadata attached to this node.
    fn attributes(&self) -> &HashMap<String, String>;
}

/// An edge type in the knowledge graph.
///
/// Any type implementing this can be used as an edge weight.
/// The trait exposes the minimal interface needed for edge operations.
pub trait GraphEdge: Clone + PartialEq + Eq + std::hash::Hash + std::fmt::Debug {
    /// Human-readable kind/type of this edge.
    fn kind(&self) -> &str;

    /// Numeric weight for ranking/sorting edges.
    fn weight(&self) -> f64 {
        1.0
    }
}
