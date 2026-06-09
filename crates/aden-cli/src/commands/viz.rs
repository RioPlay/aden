// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `aden viz` — export a slice of the knowledge graph as a text diagram (Mermaid
//! or Graphviz DOT) for human navigation. Text-first on purpose: the output drops
//! straight into AsciiDoc/Markdown, a PR comment, or CI, with zero runtime and no
//! interactive UI (ADR/roadmap M1 — the interactive viewer is deferred).
//!
//! First slice: the *blast-radius* subgraph around an anchor — the same outgoing
//! "impact" edge set as `impact-diff` / `query --impact`, BFS-limited by `--depth`.

use crate::util::find_project_root;
use aden_graph::Direction;
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::path::Path;

/// Downstream (impact) edge types — mirrors `impact_diff` / `cmd_query --impact`
/// so every blast-radius view in aden agrees on what "downstream" means.
fn impact_edge_types() -> [aden_core::EdgeType; 6] {
    [
        aden_core::EdgeType::Uses,
        aden_core::EdgeType::Calls,
        aden_core::EdgeType::Constrains,
        aden_core::EdgeType::Invokes,
        aden_core::EdgeType::Implements,
        aden_core::EdgeType::Mutates,
    ]
}

pub fn cmd_viz(
    path: &Path,
    anchor: &str,
    depth: usize,
    format: &str,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // The global `-j/--json` flag is an alias for `--format json`, so it never
    // becomes a silent no-op the way an ignored global flag would.
    let format = if json { "json" } else { format };
    let root = find_project_root(path);
    // Keep the graph fresh so the rendered slice reflects the current code.
    super::ensure_fresh(&root);
    let graph = aden_graph::cache::build_from_directory_cached(&root)?;

    let root_anchor = resolve_anchor(&graph, anchor)?;
    let start = graph
        .get_index(&root_anchor)
        .expect("resolved anchor is present in the graph");

    // BFS outgoing over impact edges, capped at `depth` hops. Collect the node set
    // and the typed edges so the renderer is pure (and unit-testable).
    let impact = impact_edge_types();
    let mut nodes: BTreeSet<String> = BTreeSet::new();
    let mut edges: BTreeSet<(String, String, String)> = BTreeSet::new();
    nodes.insert(root_anchor.clone());

    let mut visited: HashSet<_> = HashSet::new();
    visited.insert(start);
    let mut queue: VecDeque<(_, usize)> = VecDeque::new();
    queue.push_back((start, 0usize));
    while let Some((node, d)) = queue.pop_front() {
        if d >= depth {
            continue;
        }
        for neighbor in graph.graph.neighbors_directed(node, Direction::Outgoing) {
            let etype = graph
                .graph
                .edges_connecting(node, neighbor)
                .find(|e| impact.contains(&e.weight().edge_type))
                .map(|e| format!("{:?}", e.weight().edge_type));
            let Some(et) = etype else { continue };
            let from = graph.graph[node].doc.anchor.clone();
            let to = graph.graph[neighbor].doc.anchor.clone();
            nodes.insert(to.clone());
            edges.insert((from, to, et));
            if visited.insert(neighbor) {
                queue.push_back((neighbor, d + 1));
            }
        }
    }

    let diagram = match format {
        "dot" => render_dot(&root_anchor, &nodes, &edges),
        "mermaid" => render_mermaid(&root_anchor, &nodes, &edges),
        // AsciiDoc wraps the Mermaid source in an asciidoctor-diagram block so it
        // drops straight into an `.adoc` and renders to SVG/PNG in the user's
        // existing asciidoctor pipeline — aden emits the source, never the renderer.
        "asciidoc" | "adoc" => render_asciidoc(&root_anchor, &nodes, &edges),
        // JSON is the machine-readable seam for a future interactive/3D viewer:
        // the same node/edge slice, ids matching the other formats.
        "json" => render_json(&root_anchor, &nodes, &edges),
        other => {
            return Err(format!(
                "unknown --format '{other}' (expected 'mermaid', 'dot', 'asciidoc', or 'json')"
            )
            .into());
        }
    };
    println!("{diagram}");
    Ok(())
}

/// Resolve a user-supplied anchor to its canonical graph anchor: exact match
/// first, else a unique tail match (so `aden viz cmd_understand` works without the
/// full `aden://…` anchor). Ambiguous or missing inputs error with a hint.
fn resolve_anchor(
    graph: &aden_graph::AdenGraph<aden_graph::DocumentNode, aden_graph::AdenEdge>,
    anchor: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    if graph.get_index(anchor).is_some() {
        return Ok(anchor.to_string());
    }
    let matches: Vec<String> = graph
        .graph
        .node_indices()
        .map(|i| graph.graph[i].doc.anchor.clone())
        .filter(|a| anchor_tail(a) == anchor)
        .collect();
    match matches.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(format!(
            "no symbol matching '{anchor}'. Use `aden grep`/`aden locate` to find an anchor, \
             or pass the full `aden://…` anchor."
        )
        .into()),
        many => Err(format!(
            "'{anchor}' is ambiguous ({} matches); pass the full anchor. e.g. {}",
            many.len(),
            many.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
        )
        .into()),
    }
}

/// The trailing symbol segment of an anchor: everything after the last `/` or `#`.
/// `aden://module/aden-cli/query.rs#cmd_understand` -> `cmd_understand`.
fn anchor_tail(anchor: &str) -> &str {
    anchor.rsplit(['#', '/']).next().unwrap_or(anchor)
}

/// A readable node label: the path tail plus any `#fragment`.
/// `aden://module/aden-cli/query.rs#cmd_understand` -> `query.rs#cmd_understand`.
fn label(anchor: &str) -> &str {
    anchor.rsplit('/').next().unwrap_or(anchor)
}

/// Stable `n0,n1,…` ids for each anchor, assigned in sorted order so the output is
/// deterministic regardless of traversal order.
fn id_map(nodes: &BTreeSet<String>) -> BTreeMap<&str, String> {
    nodes
        .iter()
        .enumerate()
        .map(|(i, a)| (a.as_str(), format!("n{i}")))
        .collect()
}

fn render_mermaid(
    root: &str,
    nodes: &BTreeSet<String>,
    edges: &BTreeSet<(String, String, String)>,
) -> String {
    let ids = id_map(nodes);
    let mut out = String::from("flowchart LR\n");
    for a in nodes {
        let lbl = label(a).replace('"', "'");
        out.push_str(&format!("  {}[\"{}\"]\n", ids[a.as_str()], lbl));
    }
    for (from, to, et) in edges {
        out.push_str(&format!(
            "  {} -->|{}| {}\n",
            ids[from.as_str()],
            et,
            ids[to.as_str()]
        ));
    }
    // Highlight the root so the change origin is obvious.
    out.push_str(&format!("  class {} root;\n", ids[root]));
    out.push_str("  classDef root fill:#f9a,stroke:#333,stroke-width:2px;\n");
    out
}

fn render_dot(
    root: &str,
    nodes: &BTreeSet<String>,
    edges: &BTreeSet<(String, String, String)>,
) -> String {
    let ids = id_map(nodes);
    let mut out = String::from("digraph blast {\n  rankdir=LR;\n  node [shape=box];\n");
    for a in nodes {
        let lbl = label(a).replace('"', "\\\"");
        if a == root {
            out.push_str(&format!(
                "  {} [label=\"{}\", style=filled, fillcolor=\"#ffaa99\"];\n",
                ids[a.as_str()],
                lbl
            ));
        } else {
            out.push_str(&format!("  {} [label=\"{}\"];\n", ids[a.as_str()], lbl));
        }
    }
    for (from, to, et) in edges {
        out.push_str(&format!(
            "  {} -> {} [label=\"{}\"];\n",
            ids[from.as_str()],
            ids[to.as_str()],
            et
        ));
    }
    out.push_str("}\n");
    out
}

/// Machine-readable slice: `root`, `blast_radius` (downstream count), and the
/// `nodes`/`edges` with the same `n0,n1,…` ids the other formats use, so a viewer
/// can cross-reference them. Pretty-printed for human diffing.
fn render_json(
    root: &str,
    nodes: &BTreeSet<String>,
    edges: &BTreeSet<(String, String, String)>,
) -> String {
    let ids = id_map(nodes);
    let nodes_json: Vec<serde_json::Value> = nodes
        .iter()
        .map(|a| {
            serde_json::json!({
                "id": ids[a.as_str()],
                "anchor": a,
                "label": label(a),
                "root": a == root,
            })
        })
        .collect();
    let edges_json: Vec<serde_json::Value> = edges
        .iter()
        .map(|(from, to, et)| {
            serde_json::json!({
                "from": ids[from.as_str()],
                "to": ids[to.as_str()],
                "type": et,
            })
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::json!({
        "root": root,
        "blast_radius": nodes.len().saturating_sub(1),
        "nodes": nodes_json,
        "edges": edges_json,
    }))
    .unwrap_or_else(|_| "{}".to_string())
}

/// Wrap the Mermaid diagram in an asciidoctor-diagram `[mermaid]` block plus a
/// title, so it renders inline in an AsciiDoc site (with `asciidoctor-diagram`)
/// while staying readable as plain source. The fenced delimiter is `....` (a
/// literal block) — asciidoctor-diagram reads the `[mermaid]` attribute above it.
fn render_asciidoc(
    root: &str,
    nodes: &BTreeSet<String>,
    edges: &BTreeSet<(String, String, String)>,
) -> String {
    let mermaid = render_mermaid(root, nodes, edges);
    format!(
        ".Blast radius of `{}`\n[mermaid]\n....\n{}....\n",
        label(root),
        mermaid
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (String, BTreeSet<String>, BTreeSet<(String, String, String)>) {
        let root = "aden://module/x/a.rs#root".to_string();
        let child = "aden://module/x/b.rs#child".to_string();
        let nodes = BTreeSet::from([root.clone(), child.clone()]);
        let edges = BTreeSet::from([(root.clone(), child.clone(), "Calls".to_string())]);
        (root, nodes, edges)
    }

    #[test]
    fn tail_and_label() {
        assert_eq!(anchor_tail("aden://module/aden-cli/query.rs#cmd_understand"), "cmd_understand");
        assert_eq!(label("aden://module/aden-cli/query.rs#cmd_understand"), "query.rs#cmd_understand");
        assert_eq!(anchor_tail("bare"), "bare");
    }

    #[test]
    fn ids_are_deterministic_and_sorted() {
        let (_root, nodes, _edges) = sample();
        let ids = id_map(&nodes);
        // a.rs sorts before b.rs, so the root gets n0.
        assert_eq!(ids["aden://module/x/a.rs#root"], "n0");
        assert_eq!(ids["aden://module/x/b.rs#child"], "n1");
    }

    #[test]
    fn mermaid_has_nodes_edge_and_root_class() {
        let (root, nodes, edges) = sample();
        let m = render_mermaid(&root, &nodes, &edges);
        assert!(m.starts_with("flowchart LR\n"));
        assert!(m.contains("n0[\"a.rs#root\"]"));
        assert!(m.contains("n0 -->|Calls| n1"));
        assert!(m.contains("class n0 root;"));
    }

    #[test]
    fn asciidoc_wraps_mermaid_in_a_diagram_block() {
        let (root, nodes, edges) = sample();
        let a = render_asciidoc(&root, &nodes, &edges);
        assert!(a.contains(".Blast radius of `a.rs#root`"));
        assert!(a.contains("[mermaid]\n....\nflowchart LR\n"));
        assert!(a.trim_end().ends_with("...."));
        // The Mermaid body is preserved verbatim inside the block.
        assert!(a.contains("n0 -->|Calls| n1"));
    }

    #[test]
    fn json_has_root_blast_radius_nodes_and_edges() {
        let (root, nodes, edges) = sample();
        let j = render_json(&root, &nodes, &edges);
        let v: serde_json::Value = serde_json::from_str(&j).expect("valid JSON");
        assert_eq!(v["root"], "aden://module/x/a.rs#root");
        assert_eq!(v["blast_radius"], 1); // one downstream node (child)
        assert_eq!(v["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(v["edges"][0]["from"], "n0");
        assert_eq!(v["edges"][0]["to"], "n1");
        assert_eq!(v["edges"][0]["type"], "Calls");
        // ids cross-reference the other formats; root node is flagged.
        assert_eq!(v["nodes"][0]["id"], "n0");
        assert_eq!(v["nodes"][0]["root"], true);
    }

    #[test]
    fn dot_marks_root_filled_and_closes() {
        let (root, nodes, edges) = sample();
        let d = render_dot(&root, &nodes, &edges);
        assert!(d.starts_with("digraph blast {\n"));
        assert!(d.contains("n0 [label=\"a.rs#root\", style=filled"));
        assert!(d.contains("n0 -> n1 [label=\"Calls\"];"));
        assert!(d.trim_end().ends_with('}'));
    }
}
