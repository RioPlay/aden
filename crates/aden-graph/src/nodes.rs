// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
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
