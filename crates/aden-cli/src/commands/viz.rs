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

/// The concrete graph type the cache yields — aliased to keep slice signatures short.
type Graph = aden_graph::AdenGraph<aden_graph::DocumentNode, aden_graph::AdenEdge>;

/// A typed node/edge slice: a flat set of anchors + the edges among them.
type Slice = (BTreeSet<String>, BTreeSet<(String, String, String)>);

pub fn cmd_viz(
    path: &Path,
    anchor: Option<&str>,
    depth: usize,
    format: &str,
    mode: &str,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // The global `-j/--json` flag is an alias for `--format json`, so it never
    // becomes a silent no-op the way an ignored global flag would.
    let format = if json { "json" } else { format };
    let root = find_project_root(path);
    // Keep the graph fresh so the rendered slice reflects the current code.
    super::ensure_fresh(&root);
    let graph = aden_graph::cache::build_from_directory_cached(&root)?;

    let diagram = match mode {
        // Whole-graph functional clusters → DOT `cluster_*` is the right default
        // (see research: viz-design). Anchor (if any) is ignored here.
        "communities" => {
            // JSON / viewer uses the collapsed super-node overview (connected and
            // legible); the static formats keep member cluster-boxes.
            if format == "json" {
                let (supers, weights) = communities_overview(&graph, 2, 1.0, MAX_COMMUNITIES);
                if supers.is_empty() {
                    return Err("no communities of size >= 2 found (try `aden communities`)".into());
                }
                render_communities_overview_json(&supers, &weights)
            } else {
                let (comms, edges) = communities_slice(&graph, 2, 1.0, MAX_COMMUNITIES, MEMBER_CAP);
                if comms.is_empty() {
                    return Err("no communities of size >= 2 found (try `aden communities`)".into());
                }
                render_communities(&comms, &edges, format)?
            }
        }
        // Anchor-centred views.
        "blast" | "connectivity" => {
            let anchor = anchor.ok_or_else(|| -> Box<dyn std::error::Error> {
                format!(
                    "--mode {mode} needs an ANCHOR (a symbol like `cmd_understand` or a full aden:// anchor)"
                )
                .into()
            })?;
            let root_anchor = resolve_anchor(&graph, anchor)?;
            let (nodes, edges) = if mode == "connectivity" {
                connectivity_slice(&graph, &root_anchor, depth, NODE_CAP)
            } else {
                blast_slice(&graph, &root_anchor, depth, NODE_CAP)
            };
            render_flat(&root_anchor, &nodes, &edges, format)?
        }
        other => {
            return Err(format!(
                "unknown --mode '{other}' (expected 'blast', 'connectivity', or 'communities')"
            )
            .into());
        }
    };
    println!("{diagram}");
    Ok(())
}

/// Default caps for the communities view, keeping output legible (see viz-design:
/// reduce before emit). Generous enough to be useful, small enough to render.
const MAX_COMMUNITIES: usize = 12;
const MEMBER_CAP: usize = 12;
/// Cap on nodes in a blast/connectivity slice — reduce *before* emit so the view
/// stays legible (hubs at depth 2 can otherwise pull in hundreds of nodes).
const NODE_CAP: usize = 60;

/// Produce the JSON slice for `aden view` — reuses the exact same slices + JSON
/// renderers as `viz`, so the viewer and the text formats can never diverge.
#[cfg(feature = "view")]
pub(crate) fn viz_json_for(
    path: &Path,
    anchor: Option<&str>,
    mode: &str,
    depth: usize,
) -> Result<String, Box<dyn std::error::Error>> {
    let root = find_project_root(path);
    super::ensure_fresh(&root);
    let graph = aden_graph::cache::build_from_directory_cached(&root)?;
    match mode {
        "communities" => {
            let (supers, weights) = communities_overview(&graph, 2, 1.0, MAX_COMMUNITIES);
            if supers.is_empty() {
                return Err("no communities of size >= 2 found (try `aden communities`)".into());
            }
            Ok(render_communities_overview_json(&supers, &weights))
        }
        "blast" | "connectivity" => {
            let anchor = anchor.ok_or_else(|| -> Box<dyn std::error::Error> {
                format!("--mode {mode} needs an ANCHOR (a symbol or full aden:// anchor)").into()
            })?;
            let root_anchor = resolve_anchor(&graph, anchor)?;
            let (nodes, edges) = if mode == "connectivity" {
                connectivity_slice(&graph, &root_anchor, depth, NODE_CAP)
            } else {
                blast_slice(&graph, &root_anchor, depth, NODE_CAP)
            };
            Ok(render_json(&root_anchor, &nodes, &edges))
        }
        other => Err(format!(
            "unknown --mode '{other}' (expected blast, connectivity, or communities)"
        )
        .into()),
    }
}

/// Blast radius: BFS *outgoing* over impact edges from `root_anchor`, depth-capped
/// and node-capped at `cap` (closest-first, so the cap keeps the nearest reach).
fn blast_slice(graph: &Graph, root_anchor: &str, depth: usize, cap: usize) -> Slice {
    let impact = impact_edge_types();
    let mut nodes: BTreeSet<String> = BTreeSet::new();
    nodes.insert(root_anchor.to_string());
    let mut edges: BTreeSet<(String, String, String)> = BTreeSet::new();
    let Some(start) = graph.get_index(root_anchor) else {
        return (nodes, edges);
    };

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
            let to = graph.graph[neighbor].doc.anchor.clone();
            // Respect the cap: only introduce a *new* node while under it (edges to
            // already-kept nodes still count, so the shown subgraph stays connected).
            if !nodes.contains(&to) && nodes.len() >= cap {
                continue;
            }
            let from = graph.graph[node].doc.anchor.clone();
            nodes.insert(to.clone());
            edges.insert((from, to, et));
            if visited.insert(neighbor) {
                queue.push_back((neighbor, d + 1));
            }
        }
    }
    (nodes, edges)
}

/// Connectivity: BFS in *both* directions over *all* edge types, depth-capped —
/// the symbol's neighbourhood (what it reaches AND what reaches it), each edge
/// recorded in its true orientation.
fn connectivity_slice(graph: &Graph, root_anchor: &str, depth: usize, cap: usize) -> Slice {
    let mut nodes: BTreeSet<String> = BTreeSet::new();
    nodes.insert(root_anchor.to_string());
    let mut edges: BTreeSet<(String, String, String)> = BTreeSet::new();
    let Some(start) = graph.get_index(root_anchor) else {
        return (nodes, edges);
    };

    let mut visited: HashSet<_> = HashSet::new();
    visited.insert(start);
    let mut queue: VecDeque<(_, usize)> = VecDeque::new();
    queue.push_back((start, 0usize));
    while let Some((node, d)) = queue.pop_front() {
        if d >= depth {
            continue;
        }
        for dir in [Direction::Outgoing, Direction::Incoming] {
            for neighbor in graph.graph.neighbors_directed(node, dir) {
                let nb_anchor = graph.graph[neighbor].doc.anchor.clone();
                if !nodes.contains(&nb_anchor) && nodes.len() >= cap {
                    continue;
                }
                // Orient the edge source→target regardless of traversal direction.
                let (from_idx, to_idx) = match dir {
                    Direction::Outgoing => (node, neighbor),
                    Direction::Incoming => (neighbor, node),
                };
                let Some(et) = graph
                    .graph
                    .edges_connecting(from_idx, to_idx)
                    .next()
                    .map(|e| format!("{:?}", e.weight().edge_type))
                else {
                    continue;
                };
                let from = graph.graph[from_idx].doc.anchor.clone();
                let to = graph.graph[to_idx].doc.anchor.clone();
                nodes.insert(nb_anchor);
                edges.insert((from, to, et));
                if visited.insert(neighbor) {
                    queue.push_back((neighbor, d + 1));
                }
            }
        }
    }
    (nodes, edges)
}

/// A functional cluster: a label, its true size, the (capped) members shown, and
/// how many were elided.
struct Community {
    label: String,
    size: usize,
    members: Vec<String>,
    overflow: usize,
}

/// Detect communities, keep the largest `max_comms` of size >= `min_size`, cap each
/// to `member_cap` members, and collect the edges that run between shown members.
fn communities_slice(
    graph: &Graph,
    min_size: usize,
    resolution: f64,
    max_comms: usize,
    member_cap: usize,
) -> (Vec<Community>, BTreeSet<(String, String, String)>) {
    let all = aden_graph::community::detect_communities(graph, resolution);
    let mut comms = Vec::new();
    let mut shown: BTreeSet<String> = BTreeSet::new();
    for members in all
        .into_iter()
        .filter(|c| c.len() >= min_size)
        .take(max_comms)
    {
        let label = community_label(&members);
        let size = members.len();
        let overflow = size.saturating_sub(member_cap);
        let kept: Vec<String> = members.into_iter().take(member_cap).collect();
        for m in &kept {
            shown.insert(m.clone());
        }
        comms.push(Community {
            label,
            size,
            members: kept,
            overflow,
        });
    }

    let mut edges: BTreeSet<(String, String, String)> = BTreeSet::new();
    for a in &shown {
        let Some(idx) = graph.get_index(a) else {
            continue;
        };
        for nb in graph.graph.neighbors_directed(idx, Direction::Outgoing) {
            let to = graph.graph[nb].doc.anchor.clone();
            if !shown.contains(&to) {
                continue;
            }
            if let Some(e) = graph.graph.edges_connecting(idx, nb).next() {
                edges.insert((a.clone(), to, format!("{:?}", e.weight().edge_type)));
            }
        }
    }
    (comms, edges)
}

/// A collapsed *overview* of the community structure: one super-node per community
/// (sized by membership) + aggregated inter-community edge weights. Far more legible
/// in a force layout than capped individual members (which float as disconnected
/// dots), so it is what the JSON / `aden view` communities view renders.
fn communities_overview(
    graph: &Graph,
    min_size: usize,
    resolution: f64,
    max_comms: usize,
) -> (Vec<(String, usize)>, BTreeMap<(usize, usize), usize>) {
    let kept: Vec<Vec<String>> = aden_graph::community::detect_communities(graph, resolution)
        .into_iter()
        .filter(|c| c.len() >= min_size)
        .take(max_comms)
        .collect();
    let mut comm_of: BTreeMap<String, usize> = BTreeMap::new();
    let mut supers: Vec<(String, usize)> = Vec::new();
    for (i, members) in kept.iter().enumerate() {
        supers.push((community_label(members), members.len()));
        for m in members {
            comm_of.insert(m.clone(), i);
        }
    }
    // Aggregate edges that cross community boundaries into weighted super-edges.
    let mut weights: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for (anchor, &ca) in &comm_of {
        let Some(idx) = graph.get_index(anchor) else {
            continue;
        };
        for nb in graph.graph.neighbors_directed(idx, Direction::Outgoing) {
            if let Some(&cb) = comm_of.get(&graph.graph[nb].doc.anchor) {
                if ca != cb {
                    let key = if ca < cb { (ca, cb) } else { (cb, ca) };
                    *weights.entry(key).or_default() += 1;
                }
            }
        }
    }
    (supers, weights)
}

/// JSON for the collapsed communities overview (super-nodes + weighted super-edges).
fn render_communities_overview_json(
    supers: &[(String, usize)],
    weights: &BTreeMap<(usize, usize), usize>,
) -> String {
    let nodes: Vec<serde_json::Value> = supers
        .iter()
        .enumerate()
        .map(|(i, (label, size))| {
            serde_json::json!({ "id": format!("c{i}"), "label": label, "community": i, "size": size })
        })
        .collect();
    let edges: Vec<serde_json::Value> = weights
        .iter()
        .map(|(&(a, b), &w)| {
            serde_json::json!({ "from": format!("c{a}"), "to": format!("c{b}"), "type": format!("{w} edges") })
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::json!({
        "mode": "communities", "nodes": nodes, "edges": edges,
    }))
    .unwrap_or_else(|_| "{}".to_string())
}

/// The group segment (crate/dir) of an anchor: `aden://module/aden-cli/x#y` → `aden-cli`.
fn group_of(anchor: &str) -> &str {
    anchor
        .strip_prefix("aden://")
        .unwrap_or(anchor)
        .split('/')
        .nth(1)
        .unwrap_or("?")
}

/// A human label for a community: the most common group among its members.
fn community_label(members: &[String]) -> String {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for m in members {
        *counts.entry(group_of(m)).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(g, _)| g.to_string())
        .unwrap_or_else(|| "mixed".to_string())
}

/// Dispatch a flat (blast/connectivity) slice to the requested format.
fn render_flat(
    root: &str,
    nodes: &BTreeSet<String>,
    edges: &BTreeSet<(String, String, String)>,
    format: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    Ok(match format {
        "dot" => render_dot(root, nodes, edges),
        "mermaid" => render_mermaid(root, nodes, edges),
        "asciidoc" | "adoc" => render_asciidoc(root, nodes, edges),
        "json" => render_json(root, nodes, edges),
        other => {
            return Err(format!(
                "unknown --format '{other}' (expected 'mermaid', 'dot', 'asciidoc', or 'json')"
            )
            .into());
        }
    })
}

/// Dispatch a communities view to the requested format.
fn render_communities(
    comms: &[Community],
    edges: &BTreeSet<(String, String, String)>,
    format: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    Ok(match format {
        "dot" => render_communities_dot(comms, edges),
        "mermaid" => render_communities_mermaid(comms, edges),
        "asciidoc" | "adoc" => format!(
            ".Communities\n[mermaid]\n....\n{}....\n",
            render_communities_mermaid(comms, edges)
        ),
        "json" => render_communities_json(comms, edges),
        other => {
            return Err(format!(
                "unknown --format '{other}' (expected 'mermaid', 'dot', 'asciidoc', or 'json')"
            )
            .into());
        }
    })
}

/// Stable `n0,n1,…` ids over every shown community member, in sorted order.
fn community_ids(comms: &[Community]) -> BTreeMap<&str, String> {
    let mut all: BTreeSet<&str> = BTreeSet::new();
    for c in comms {
        for m in &c.members {
            all.insert(m.as_str());
        }
    }
    all.iter()
        .enumerate()
        .map(|(i, a)| (*a, format!("n{i}")))
        .collect()
}

fn render_communities_dot(
    comms: &[Community],
    edges: &BTreeSet<(String, String, String)>,
) -> String {
    let ids = community_ids(comms);
    let palette = ["#eef6ff", "#fff0f0", "#f0fff0", "#fffbe6", "#f5f0ff", "#f0ffff"];
    let mut out = String::from("digraph communities {\n  rankdir=LR;\n  node [shape=box];\n  compound=true;\n");
    for (i, c) in comms.iter().enumerate() {
        out.push_str(&format!(
            "  subgraph cluster_{i} {{\n    label=\"{} ({})\";\n    style=filled;\n    color=\"{}\";\n",
            c.label.replace('"', "'"),
            c.size,
            palette[i % palette.len()]
        ));
        for m in &c.members {
            out.push_str(&format!(
                "    {} [label=\"{}\"];\n",
                ids[m.as_str()],
                label(m).replace('"', "\\\"")
            ));
        }
        if c.overflow > 0 {
            out.push_str(&format!(
                "    more_{i} [label=\"+{} more\", shape=note, style=dashed];\n",
                c.overflow
            ));
        }
        out.push_str("  }\n");
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

fn render_communities_mermaid(
    comms: &[Community],
    edges: &BTreeSet<(String, String, String)>,
) -> String {
    let ids = community_ids(comms);
    let mut out = String::from("flowchart LR\n");
    for (i, c) in comms.iter().enumerate() {
        out.push_str(&format!(
            "  subgraph g{i}[\"{} ({})\"]\n",
            c.label.replace('"', "'"),
            c.size
        ));
        for m in &c.members {
            out.push_str(&format!(
                "    {}[\"{}\"]\n",
                ids[m.as_str()],
                label(m).replace('"', "'")
            ));
        }
        if c.overflow > 0 {
            out.push_str(&format!("    more{i}[\"+{} more\"]\n", c.overflow));
        }
        out.push_str("  end\n");
    }
    for (from, to, et) in edges {
        out.push_str(&format!(
            "  {} -->|{}| {}\n",
            ids[from.as_str()],
            et,
            ids[to.as_str()]
        ));
    }
    out
}

fn render_communities_json(
    comms: &[Community],
    edges: &BTreeSet<(String, String, String)>,
) -> String {
    let ids = community_ids(comms);
    let comms_json: Vec<serde_json::Value> = comms
        .iter()
        .enumerate()
        .map(|(i, c)| {
            serde_json::json!({
                "id": i,
                "label": c.label,
                "size": c.size,
                "shown": c.members.len(),
                "members": c.members.iter().map(|m| ids[m.as_str()].clone()).collect::<Vec<_>>(),
            })
        })
        .collect();
    let mut nodes_json: Vec<serde_json::Value> = Vec::new();
    for (i, c) in comms.iter().enumerate() {
        for m in &c.members {
            nodes_json.push(serde_json::json!({
                "id": ids[m.as_str()],
                "anchor": m,
                "label": label(m),
                "community": i,
            }));
        }
    }
    let edges_json: Vec<serde_json::Value> = edges
        .iter()
        .map(|(from, to, et)| {
            serde_json::json!({ "from": ids[from.as_str()], "to": ids[to.as_str()], "type": et })
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::json!({
        "mode": "communities",
        "communities": comms_json,
        "nodes": nodes_json,
        "edges": edges_json,
    }))
    .unwrap_or_else(|_| "{}".to_string())
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

    fn comm_sample() -> (Vec<Community>, BTreeSet<(String, String, String)>) {
        let a = "aden://module/x/a.rs#a".to_string();
        let b = "aden://module/x/b.rs#b".to_string();
        let c = "aden://module/y/c.rs#c".to_string();
        let comms = vec![
            Community { label: "x".into(), size: 3, members: vec![a.clone(), b.clone()], overflow: 1 },
            Community { label: "y".into(), size: 1, members: vec![c.clone()], overflow: 0 },
        ];
        // ids are assigned in sorted-anchor order: a=n0, b=n1, c=n2
        let edges = BTreeSet::from([(a, c, "Calls".to_string())]);
        (comms, edges)
    }

    #[test]
    fn group_and_label() {
        assert_eq!(group_of("aden://module/aden-cli/query.rs#cmd_understand"), "aden-cli");
        assert_eq!(group_of("aden://doc/aden/file.adoc#h"), "aden");
        let members = vec![
            "aden://module/aden-cli/a#x".to_string(),
            "aden://module/aden-cli/b#y".to_string(),
            "aden://module/aden-core/c#z".to_string(),
        ];
        assert_eq!(community_label(&members), "aden-cli");
    }

    #[test]
    fn communities_dot_has_clusters_overflow_and_edges() {
        let (comms, edges) = comm_sample();
        let d = render_communities_dot(&comms, &edges);
        assert!(d.starts_with("digraph communities {\n"));
        assert!(d.contains("subgraph cluster_0 {"));
        assert!(d.contains("label=\"x (3)\""));
        assert!(d.contains("more_0 [label=\"+1 more\"")); // overflow node
        assert!(d.contains("n0 -> n2 [label=\"Calls\"];")); // a→c
        assert!(d.trim_end().ends_with('}'));
    }

    #[test]
    fn communities_mermaid_has_subgraphs_and_edges() {
        let (comms, edges) = comm_sample();
        let m = render_communities_mermaid(&comms, &edges);
        assert!(m.starts_with("flowchart LR\n"));
        assert!(m.contains("subgraph g0[\"x (3)\"]"));
        assert!(m.contains("more0[\"+1 more\"]"));
        assert!(m.contains("n0 -->|Calls| n2"));
        assert!(m.contains("  end\n"));
    }

    #[test]
    fn communities_json_carries_membership() {
        let (comms, edges) = comm_sample();
        let v: serde_json::Value =
            serde_json::from_str(&render_communities_json(&comms, &edges)).expect("valid JSON");
        assert_eq!(v["mode"], "communities");
        assert_eq!(v["communities"][0]["size"], 3);
        assert_eq!(v["communities"][0]["shown"], 2);
        assert_eq!(v["nodes"][0]["community"], 0);
        assert_eq!(v["edges"][0]["from"], "n0");
        assert_eq!(v["edges"][0]["to"], "n2");
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
