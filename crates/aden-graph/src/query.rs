// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Aden Query (.adq) interpreter for graph queries.
//!
//! Supports: node(), incoming(), outgoing(), where, limit, order_by

use crate::graph::AdenGraph;
use crate::nodes::{AdenEdge, DocumentNode, GraphNode};
use aden_core::EdgeType;
use petgraph::Direction;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    pub nodes: Vec<String>,
    pub total: usize,
}

pub struct AdqInterpreter<'a> {
    graph: &'a AdenGraph<DocumentNode, AdenEdge>,
}

impl std::fmt::Debug for AdqInterpreter<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdqInterpreter")
            .field("graph", &"AdenGraph")
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error(
        "unknown ADQ function: '{0}'. Valid: node(anchor), incoming(anchor), outgoing(anchor), nodes, edges, where <predicate>"
    )]
    UnknownFunction(String),
    #[error("invalid anchor: {0}")]
    InvalidAnchor(String),
    #[error("ambiguous anchor: {0}")]
    AmbiguousAnchor(String),
    #[error("parse error: {0}")]
    Parse(String),
}

impl<'a> AdqInterpreter<'a> {
    pub fn new(graph: &'a AdenGraph<DocumentNode, AdenEdge>) -> Self {
        Self { graph }
    }

    pub fn execute(&self, adq_script: &str) -> Result<QueryResult, QueryError> {
        let script = adq_script.trim();

        // `where <predicate>` — graph-wide predicate selection (dead code,
        // reachability, …). Checked before the function-call form so a predicate
        // such as `where backlinks=0` is not mistaken for a function call.
        let lower = script.to_ascii_lowercase();
        if lower == "where" || lower.starts_with("where ") || lower.starts_with("where(") {
            let pred = script[5..]
                .trim()
                .trim_start_matches('(')
                .trim_end_matches(')')
                .trim();
            return self.exec_where(pred);
        }

        // Parse function calls: node(anchor), incoming(anchor), outgoing(anchor)
        // Also support: node anchor, incoming anchor
        if let Some(paren_pos) = script.find('(') {
            let func_name = script[..paren_pos].trim();
            let arg = script[paren_pos + 1..].trim_end_matches(')').trim();

            match func_name {
                "node" => self.exec_node(&[arg]),
                "incoming" => self.exec_incoming(&[arg]),
                "outgoing" => self.exec_outgoing(&[arg]),
                _ => Err(QueryError::UnknownFunction(func_name.to_string())),
            }
        } else {
            // Simple command without parentheses
            match script {
                "nodes" => self.exec_all_nodes(&[]),
                "edges" => self.exec_all_edges(&[]),
                _ => Err(QueryError::UnknownFunction(script.to_string())),
            }
        }
    }

    /// Resolve a user-supplied anchor argument to a full anchor present in the
    /// graph. Accepts either an exact anchor or a bare symbol name (the part
    /// after the final `#` of a full anchor), so `.adq` scripts stay writable by
    /// hand — matching how `locate`/`understand`/`asm` already resolve short
    /// names. Errors with `InvalidAnchor` when nothing matches and
    /// `AmbiguousAnchor` (listing the candidates) when more than one symbol
    /// shares the name. Never guesses on ambiguity — same policy as
    /// [`crate::cache::resolve_anchor_in_store`].
    fn resolve(&self, anchor: &str) -> Result<String, QueryError> {
        // Exact match — already a full anchor present in the graph.
        if self.graph.get_index(anchor).is_some() {
            return Ok(anchor.to_string());
        }
        // Bare-name fallback: match the `#suffix` of each known anchor.
        let mut matches: Vec<String> = self
            .graph
            .anchor_to_index
            .keys()
            .filter(|a| a.rsplit('#').next() == Some(anchor))
            .cloned()
            .collect();
        matches.sort();
        matches.dedup();
        match matches.len() {
            0 => Err(QueryError::InvalidAnchor(anchor.to_string())),
            1 => Ok(matches.remove(0)),
            _ => Err(QueryError::AmbiguousAnchor(format!(
                "'{}' matches {} symbols; use a full anchor. Candidates: {}",
                anchor,
                matches.len(),
                matches.join(", ")
            ))),
        }
    }

    fn exec_node(&self, args: &[&str]) -> Result<QueryResult, QueryError> {
        let anchor = args
            .first()
            .ok_or_else(|| QueryError::Parse("node() requires anchor".to_string()))?;
        let anchor = anchor.trim_matches(|c| c == '(' || c == ')' || c == ';');
        let anchor = self.resolve(anchor)?;

        Ok(QueryResult {
            nodes: vec![anchor],
            total: 1,
        })
    }

    fn exec_incoming(&self, args: &[&str]) -> Result<QueryResult, QueryError> {
        let anchor = args
            .first()
            .ok_or_else(|| QueryError::Parse("incoming() requires anchor".to_string()))?;
        let anchor = anchor.trim_matches(|c| c == '(' || c == ')' || c == ';');
        let anchor = self.resolve(anchor)?;

        let idx = self
            .graph
            .get_index(&anchor)
            .ok_or_else(|| QueryError::InvalidAnchor(anchor.clone()))?;

        let mut nodes = Vec::new();
        for neighbor in self
            .graph
            .graph
            .neighbors_directed(idx, Direction::Incoming)
        {
            if let Some(node) = self.graph.graph.node_weight(neighbor) {
                nodes.push(node.anchor().to_string());
            }
        }

        let total = nodes.len();
        Ok(QueryResult { nodes, total })
    }

    fn exec_outgoing(&self, args: &[&str]) -> Result<QueryResult, QueryError> {
        let anchor = args
            .first()
            .ok_or_else(|| QueryError::Parse("outgoing() requires anchor".to_string()))?;
        let anchor = anchor.trim_matches(|c| c == '(' || c == ')' || c == ';');
        let anchor = self.resolve(anchor)?;

        let idx = self
            .graph
            .get_index(&anchor)
            .ok_or_else(|| QueryError::InvalidAnchor(anchor.clone()))?;

        let mut nodes = Vec::new();
        for neighbor in self
            .graph
            .graph
            .neighbors_directed(idx, Direction::Outgoing)
        {
            if let Some(node) = self.graph.graph.node_weight(neighbor) {
                nodes.push(node.anchor().to_string());
            }
        }

        let total = nodes.len();
        Ok(QueryResult { nodes, total })
    }

    /// Graph-wide predicate selection: `where <predicate>`.
    ///
    /// The foundation for issue detectors (dead code, etc.) that need to select
    /// nodes by graph-derived facts rather than by a single anchor. Evaluates a
    /// boolean predicate over each node using facts computed from graph structure
    /// and node type — never from any one language — so it stays language-agnostic.
    ///
    /// Fields:
    /// - `callers`   — incoming `Calls` edges (0 ⇒ never called ⇒ dead-code candidate)
    /// - `refs`      — incoming *reference* edges (Calls/Uses/Invokes/RelatesTo/Tests/…),
    ///   excluding structural containment (the module-hub `Documents`/`PartOf` edges)
    /// - `backlinks` — incoming reference edges (same set as `refs`; excludes
    ///   structural containment like the module-hub `Documents`/`PartOf` edges)
    /// - `calls`     — outgoing `Calls` edges
    /// - `type`      — node type (Function, Type, Module, …)
    /// - `anchor` / `name` — substring match on the anchor
    ///
    /// Operators `=` `!=` `>` `<` `>=` `<=` `~`(contains); combine atoms with
    /// `and`/`or`; negate one with `not`. Example dead-code query:
    /// `where callers=0 and type=Function`.
    fn exec_where(&self, predicate: &str) -> Result<QueryResult, QueryError> {
        let pred = Predicate::parse(predicate)?;
        let mut nodes = Vec::new();
        for idx in self.graph.graph.node_indices() {
            let Some(node) = self.graph.graph.node_weight(idx) else {
                continue;
            };
            let facts = self.node_facts(idx, node);
            if pred.eval(&facts) {
                nodes.push(format!(
                    "{}  [type={} callers={} refs={} calls={}]",
                    facts.anchor, facts.type_name, facts.callers, facts.refs, facts.calls
                ));
            }
        }
        let total = nodes.len();
        Ok(QueryResult { nodes, total })
    }

    /// Compute the graph-derived facts for one node, consumed by `where`.
    fn node_facts(&self, idx: petgraph::graph::NodeIndex, node: &DocumentNode) -> NodeFacts {
        let mut callers = 0usize;
        let mut refs = 0usize;
        let mut backlinks = 0usize;
        for e in self.graph.graph.edges_directed(idx, Direction::Incoming) {
            match e.weight().edge_type {
                EdgeType::Calls => {
                    callers += 1;
                    refs += 1;
                    backlinks += 1;
                }
                EdgeType::Uses
                | EdgeType::Invokes
                | EdgeType::RelatesTo
                | EdgeType::Tests
                | EdgeType::Verifies
                | EdgeType::Implements
                | EdgeType::Requires
                | EdgeType::Mutates => {
                    refs += 1;
                    backlinks += 1;
                }
                // Documents / PartOf / IsA / … are structural, not references.
                _ => {}
            }
        }
        let calls = self
            .graph
            .graph
            .edges_directed(idx, Direction::Outgoing)
            .filter(|e| e.weight().edge_type == EdgeType::Calls)
            .count();
        NodeFacts {
            anchor: node.anchor().to_string(),
            type_name: format!("{:?}", node.doc.node_type),
            callers,
            refs,
            backlinks,
            calls,
        }
    }

    fn exec_all_nodes(&self, _args: &[&str]) -> Result<QueryResult, QueryError> {
        let mut nodes = Vec::new();
        for idx in self.graph.graph.node_indices() {
            if let Some(node) = self.graph.graph.node_weight(idx) {
                nodes.push(node.anchor().to_string());
            }
        }
        let total = nodes.len();
        Ok(QueryResult { nodes, total })
    }

    fn exec_all_edges(&self, _args: &[&str]) -> Result<QueryResult, QueryError> {
        let mut nodes = Vec::new();
        for edge in self.graph.graph.edge_indices() {
            let Some((src, dst)) = self.graph.graph.edge_endpoints(edge) else {
                continue;
            };
            if let (Some(src_node), Some(dst_node)) = (
                self.graph.graph.node_weight(src),
                self.graph.graph.node_weight(dst),
            ) {
                nodes.push(format!("{} -> {}", src_node.anchor(), dst_node.anchor()));
            }
        }
        let total = nodes.len();
        Ok(QueryResult { nodes, total })
    }
}

/// Graph-derived facts about a single node, evaluated by `where` predicates.
struct NodeFacts {
    anchor: String,
    type_name: String,
    callers: usize,
    refs: usize,
    backlinks: usize,
    calls: usize,
}

#[derive(Clone, Copy)]
enum Cmp {
    Eq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
    Contains,
}

/// A single comparison, e.g. `callers=0` or `not type=Module`.
struct Atom {
    field: String,
    cmp: Cmp,
    value: String,
    negated: bool,
}

impl Atom {
    fn parse(input: &str) -> Result<Atom, QueryError> {
        let mut s = input.trim();
        let mut negated = false;
        if let Some(rest) = strip_kw_prefix(s, "not") {
            s = rest.trim();
            negated = true;
        }
        // Longest operators first so `>=` is not parsed as `>` then `=`.
        const OPS: [(&str, Cmp); 7] = [
            (">=", Cmp::Ge),
            ("<=", Cmp::Le),
            ("!=", Cmp::Ne),
            ("~", Cmp::Contains),
            ("=", Cmp::Eq),
            (">", Cmp::Gt),
            ("<", Cmp::Lt),
        ];
        for (tok, cmp) in OPS {
            if let Some(p) = s.find(tok) {
                let field = s[..p].trim().to_ascii_lowercase();
                let value = s[p + tok.len()..]
                    .trim()
                    .trim_matches(|c| c == '"' || c == '\'')
                    .to_string();
                if field.is_empty() {
                    break;
                }
                return Ok(Atom {
                    field,
                    cmp,
                    value,
                    negated,
                });
            }
        }
        Err(QueryError::Parse(format!(
            "cannot parse predicate atom `{}` (expected `field <op> value`, e.g. `callers=0`)",
            s
        )))
    }

    fn eval(&self, f: &NodeFacts) -> bool {
        let matched = match self.field.as_str() {
            "callers" => self.cmp_num(f.callers),
            "refs" => self.cmp_num(f.refs),
            "backlinks" => self.cmp_num(f.backlinks),
            "calls" => self.cmp_num(f.calls),
            "type" => self.cmp_str(&f.type_name),
            "anchor" | "name" => self.cmp_str(&f.anchor),
            _ => false,
        };
        matched ^ self.negated
    }

    fn cmp_num(&self, lhs: usize) -> bool {
        let Ok(rhs) = self.value.parse::<usize>() else {
            return false;
        };
        match self.cmp {
            Cmp::Eq => lhs == rhs,
            Cmp::Ne => lhs != rhs,
            Cmp::Gt => lhs > rhs,
            Cmp::Lt => lhs < rhs,
            Cmp::Ge => lhs >= rhs,
            Cmp::Le => lhs <= rhs,
            Cmp::Contains => false,
        }
    }

    fn cmp_str(&self, lhs: &str) -> bool {
        let l = lhs.to_ascii_lowercase();
        let r = self.value.to_ascii_lowercase();
        match self.cmp {
            Cmp::Eq => l == r,
            Cmp::Ne => l != r,
            Cmp::Contains => l.contains(&r),
            // Ordering comparisons are meaningless for strings.
            _ => false,
        }
    }
}

/// A predicate in disjunctive normal form: an OR of AND-groups of [`Atom`]s.
/// (`a and b or c` ⇒ `(a and b) or (c)`.)
struct Predicate {
    groups: Vec<Vec<Atom>>,
}

impl Predicate {
    fn parse(input: &str) -> Result<Self, QueryError> {
        let input = input.trim();
        if input.is_empty() {
            return Err(QueryError::Parse(
                "`where` requires a predicate, e.g. `where callers=0 and type=Function`".into(),
            ));
        }
        let mut groups = Vec::new();
        for or_part in split_kw(input, "or") {
            let mut atoms = Vec::new();
            for and_part in split_kw(&or_part, "and") {
                if and_part.trim().is_empty() {
                    continue;
                }
                atoms.push(Atom::parse(&and_part)?);
            }
            if !atoms.is_empty() {
                groups.push(atoms);
            }
        }
        if groups.is_empty() {
            return Err(QueryError::Parse("empty predicate".into()));
        }
        Ok(Predicate { groups })
    }

    fn eval(&self, f: &NodeFacts) -> bool {
        self.groups.iter().any(|g| g.iter().all(|a| a.eval(f)))
    }
}

/// Split on a whole-word keyword surrounded by spaces (case-insensitive), so
/// `and`/`or` inside identifiers (e.g. `android`) never trigger a split.
fn split_kw(input: &str, kw: &str) -> Vec<String> {
    let lower = input.to_ascii_lowercase();
    let pat = format!(" {} ", kw);
    let mut parts = Vec::new();
    let mut start = 0;
    let mut search = 0;
    while let Some(rel) = lower[search..].find(&pat) {
        let abs = search + rel;
        parts.push(input[start..abs].to_string());
        start = abs + pat.len();
        search = start;
    }
    parts.push(input[start..].to_string());
    parts
}

/// Strip a leading whole-word keyword (`not `) case-insensitively.
fn strip_kw_prefix<'a>(s: &'a str, kw: &str) -> Option<&'a str> {
    let pat = format!("{} ", kw);
    if s.len() >= pat.len() && s[..pat.len()].eq_ignore_ascii_case(&pat) {
        Some(&s[pat.len()..])
    } else {
        None
    }
}

pub fn execute_adq(
    graph: &AdenGraph<DocumentNode, AdenEdge>,
    script: &str,
) -> Result<QueryResult, QueryError> {
    let interpreter = AdqInterpreter::new(graph);
    interpreter.execute(script)
}

#[cfg(test)]
mod adq_resolve_tests {
    use super::*;
    use crate::{AdenEdge, AdenGraph, DocumentNode};
    use aden_core::{Document, NodeType};
    use std::collections::HashMap;

    fn node(anchor: &str) -> DocumentNode {
        DocumentNode {
            doc: Document {
                anchor: anchor.to_string(),
                node_type: NodeType::Function,
                attributes: HashMap::new(),
                blocks: Vec::new(),
                source_span: None,
                metadata: None,
                confidence: 0.9,
            },
            parsed: None,
            source_path: std::path::PathBuf::from("x.rs"),
        }
    }

    fn graph_with(anchors: &[&str]) -> AdenGraph<DocumentNode, AdenEdge> {
        let mut g = AdenGraph::<DocumentNode, AdenEdge>::new();
        for a in anchors {
            let _ = g.add_node(node(a));
        }
        g
    }

    #[test]
    fn short_name_resolves_to_full_anchor() {
        let g = graph_with(&["aden://module/x.rs#foo", "aden://module/y.rs#bar"]);
        let r = AdqInterpreter::new(&g).execute("node(foo)").unwrap();
        assert_eq!(r.nodes, vec!["aden://module/x.rs#foo".to_string()]);
    }

    #[test]
    fn exact_full_anchor_still_resolves() {
        let g = graph_with(&["aden://module/x.rs#foo"]);
        let r = AdqInterpreter::new(&g)
            .execute("node(aden://module/x.rs#foo)")
            .unwrap();
        assert_eq!(r.nodes, vec!["aden://module/x.rs#foo".to_string()]);
    }

    #[test]
    fn unknown_short_name_is_invalid_anchor() {
        let g = graph_with(&["aden://module/x.rs#foo"]);
        let e = AdqInterpreter::new(&g).execute("node(nope)").unwrap_err();
        assert!(matches!(e, QueryError::InvalidAnchor(_)));
    }

    #[test]
    fn ambiguous_short_name_errors_with_candidates() {
        let g = graph_with(&["aden://module/x.rs#foo", "aden://module/y.rs#foo"]);
        let e = AdqInterpreter::new(&g).execute("node(foo)").unwrap_err();
        match e {
            QueryError::AmbiguousAnchor(msg) => assert!(
                msg.contains("x.rs#foo") && msg.contains("y.rs#foo"),
                "candidates missing from message: {msg}"
            ),
            other => panic!("expected AmbiguousAnchor, got {other:?}"),
        }
    }

    #[test]
    fn incoming_and_outgoing_accept_short_names() {
        let g = graph_with(&["aden://module/x.rs#foo"]);
        let interp = AdqInterpreter::new(&g);
        // Resolution succeeds (no edges -> empty result, not InvalidAnchor).
        assert_eq!(interp.execute("incoming(foo)").unwrap().total, 0);
        assert_eq!(interp.execute("outgoing(foo)").unwrap().total, 0);
    }
}

#[cfg(test)]
mod predicate_tests {
    use super::*;

    fn facts(anchor: &str, ty: &str, callers: usize, refs: usize, calls: usize) -> NodeFacts {
        NodeFacts {
            anchor: anchor.to_string(),
            type_name: ty.to_string(),
            callers,
            refs,
            backlinks: callers,
            calls,
        }
    }

    #[test]
    fn dead_code_predicate_selects_zero_caller_symbols() {
        let p = Predicate::parse("callers=0 and type=Function").unwrap();
        assert!(p.eval(&facts("a#dead", "Function", 0, 0, 2)));
        assert!(!p.eval(&facts("a#live", "Function", 3, 3, 0)));
        assert!(!p.eval(&facts("a#type", "Type", 0, 0, 0)));
    }

    #[test]
    fn comparison_operators_parse_and_eval() {
        assert!(
            Predicate::parse("callers>=2")
                .unwrap()
                .eval(&facts("x", "Function", 2, 2, 0))
        );
        assert!(
            Predicate::parse("callers>1")
                .unwrap()
                .eval(&facts("x", "Function", 2, 0, 0))
        );
        assert!(
            Predicate::parse("calls<=0")
                .unwrap()
                .eval(&facts("x", "Function", 0, 0, 0))
        );
        assert!(
            Predicate::parse("callers!=0")
                .unwrap()
                .eval(&facts("x", "Function", 1, 0, 0))
        );
    }

    #[test]
    fn not_and_or_combine() {
        // `not pub` style negation on a string field
        let p = Predicate::parse("not type=Module").unwrap();
        assert!(p.eval(&facts("x", "Function", 0, 0, 0)));
        assert!(!p.eval(&facts("x", "Module", 0, 0, 0)));
        // OR of two AND-groups
        let p = Predicate::parse("type=Module or callers=0").unwrap();
        assert!(p.eval(&facts("x", "Module", 5, 5, 0)));
        assert!(p.eval(&facts("x", "Function", 0, 0, 0)));
        assert!(!p.eval(&facts("x", "Function", 4, 4, 0)));
    }

    #[test]
    fn contains_match_on_anchor() {
        let p = Predicate::parse("anchor~resolve_callee").unwrap();
        assert!(p.eval(&facts(
            "aden://module/x/y.rs#resolve_callee",
            "Function",
            1,
            1,
            0
        )));
        assert!(!p.eval(&facts("aden://module/x/y.rs#other", "Function", 1, 1, 0)));
    }

    #[test]
    fn keyword_inside_identifier_does_not_split() {
        // `android` contains "and" but must not be treated as a conjunction.
        let p = Predicate::parse("anchor~android").unwrap();
        assert!(p.eval(&facts("pkg#android_init", "Function", 0, 0, 0)));
    }

    #[test]
    fn empty_predicate_is_an_error() {
        assert!(Predicate::parse("   ").is_err());
    }
}
