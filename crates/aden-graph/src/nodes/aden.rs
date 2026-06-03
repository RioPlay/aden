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
//! Aden-specific node and edge implementations.
//!
//! Provides `DocumentNode` (wrapping `aden_core::Document`) and
//! `AdenEdge` (wrapping `aden_core::EdgeType`) so aden_core types
//! can be used with the generic `AdenGraph<N, E>` engine.

use crate::nodes::{GraphEdge, GraphNode};
use crate::parser::ParsedDocument;
use aden_core::{Document, EdgeType};
use std::collections::HashMap;
use std::path::PathBuf;

/// A node in the Aden knowledge graph.
///
/// Wraps `aden_core::Document` to implement `GraphNode`.
#[derive(Debug, Clone)]
pub struct DocumentNode {
    pub doc: Document,
    pub source_path: PathBuf,
    /// Parsed representation of the source file (for raw content, tags, etc.).
    pub parsed: Option<ParsedDocument>,
}

impl GraphNode for DocumentNode {
    fn anchor(&self) -> &str {
        &self.doc.anchor
    }

    fn source_path(&self) -> &PathBuf {
        &self.source_path
    }

    fn attributes(&self) -> &HashMap<String, String> {
        &self.doc.attributes
    }
}

/// Adapter for `aden_core::EdgeType` to implement `GraphEdge`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Copy)]
pub struct AdenEdge {
    pub edge_type: EdgeType,
}

impl GraphEdge for AdenEdge {
    fn kind(&self) -> &str {
        match self.edge_type {
            EdgeType::Uses => "Uses",
            EdgeType::UsedBy => "UsedBy",
            EdgeType::Implements => "Implements",
            EdgeType::Tests => "Tests",
            EdgeType::Documents => "Documents",
            EdgeType::Constrains => "Constrains",
            EdgeType::Justifies => "Justifies",
            EdgeType::Invokes => "Invokes",
            EdgeType::Requires => "Requires",
            EdgeType::Mutates => "Mutates",
            EdgeType::Calls => "Calls",
            EdgeType::Supersedes => "Supersedes",
            EdgeType::Amends => "Amends",
            EdgeType::Verifies => "Verifies",
            EdgeType::IsA => "IsA",
            EdgeType::PartOf => "PartOf",
            EdgeType::RelatesTo => "RelatesTo",
            EdgeType::SimilarTo => "SimilarTo",
            EdgeType::Causes => "Causes",
            EdgeType::Implies => "Implies",
            EdgeType::SynonymOf => "SynonymOf",
            EdgeType::AntonymOf => "AntonymOf",
            EdgeType::AssociatedWith => "AssociatedWith",
            EdgeType::PrerequisiteFor => "PrerequisiteFor",
            EdgeType::Explains => "Explains",
            EdgeType::IsEquivalentTo => "IsEquivalentTo",
        }
    }

    fn weight(&self) -> f64 {
        1.0
    }
}

impl std::fmt::Display for AdenEdge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.edge_type)
    }
}

impl From<EdgeType> for AdenEdge {
    fn from(edge_type: EdgeType) -> Self {
        Self { edge_type }
    }
}
