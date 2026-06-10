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

/// Anchor → (absolute source file, 1-based line), for "open in editor" links.
type SrcMap = BTreeMap<String, (String, usize)>;

/// Make a (possibly relative) source path URI-ready for `vscode://file{file}` on
/// every OS: absolute, forward slashes, leading slash (Windows `C:\x` → `/C:/x`).
fn uri_path(root: &Path, file: &str) -> String {
    let abs = if Path::new(file).is_absolute() {
        file.to_string()
    } else {
        root.join(file).to_string_lossy().into_owned()
    };
    let s = abs.replace('\\', "/");
    if s.starts_with('/') {
        s
    } else {
        format!("/{s}")
    }
}

/// Build anchor → (URI-ready file, 1-based line) from the store's symbol spans —
/// the same source `impact-diff` uses — for "open in editor" links. (A graph node's
/// own `source_span` is empty for symbols; the spans live in the store.)
fn build_src_map(root: &Path) -> SrcMap {
    let mut m: SrcMap = BTreeMap::new();
    for (file, spans) in super::grep::load_symbol_spans(root) {
        let uri = uri_path(root, &file);
        for sp in spans {
            m.entry(sp.anchor).or_insert_with(|| (uri.clone(), sp.start));
        }
    }
    m
}

pub fn cmd_viz(
    path: &Path,
    anchor: Option<&str>,
    depth: usize,
    format: &str,
    mode: &str,
    json: bool,
    full: bool,
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
                render_communities_view_json(&graph, &root, 2, 1.0, MAX_COMMUNITIES, DRILL_CAP)?
            } else {
                let (comms, edges) = communities_slice(&graph, 2, 1.0, MAX_COMMUNITIES, MEMBER_CAP);
                if comms.is_empty() {
                    return Err("no communities of size >= 2 found (try `aden communities`)".into());
                }
                render_communities(&comms, &edges, format)?
            }
        }
        // Whole-graph view model — the comprehensive payload the interactive viewer's
        // lenses slice. JSON-only: a mermaid/dot of the entire project is unreadable.
        "graph" => {
            if format != "json" {
                return Err(
                    "--mode graph is JSON-only (the whole-graph view model for `aden view`); \
                     pass `-j`/`--format json`."
                        .into(),
                );
            }
            render_whole_graph_json(&graph, &root, if full { 0 } else { GRAPH_CAP })
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
            let src = build_src_map(&root);
            render_flat(&root_anchor, &nodes, &edges, format, &src)?
        }
        other => {
            return Err(format!(
                "unknown --mode '{other}' (expected 'blast', 'connectivity', 'communities', or 'graph')"
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
/// Members shown when drilling into a community (the connected core, ranked by
/// intra-community degree).
const DRILL_CAP: usize = 30;

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
        "graph" => Ok(render_whole_graph_json(&graph, &root, GRAPH_CAP)),
        "communities" => render_communities_view_json(&graph, &root, 2, 1.0, MAX_COMMUNITIES, DRILL_CAP),
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
            let src = build_src_map(&root);
            Ok(render_json(&root_anchor, &nodes, &edges, &src))
        }
        other => Err(format!(
            "unknown --mode '{other}' (expected blast, connectivity, communities, or graph)"
        )
        .into()),
    }
}

/// Build a graph JSON from an explicit *set of anchors* (the union of symbols
/// touched across git history, for `--replay`): nodes are those anchors present in
/// the graph (capped), edges are the graph edges among them. No single root.
#[cfg(feature = "view")]
pub(crate) fn anchors_json(
    path: &Path,
    anchors: &BTreeSet<String>,
    cap: usize,
) -> Result<String, Box<dyn std::error::Error>> {
    let root = find_project_root(path);
    super::ensure_fresh(&root);
    let graph = aden_graph::cache::build_from_directory_cached(&root)?;
    // Candidates present in the graph, ranked by *intra-set* degree so the cap keeps
    // the connected core (not an arbitrary alphabetical slice with no edges).
    let cand: Vec<String> = anchors
        .iter()
        .filter(|a| graph.get_index(a).is_some())
        .cloned()
        .collect();
    let cand_set: BTreeSet<&str> = cand.iter().map(|s| s.as_str()).collect();
    let mut deg: BTreeMap<String, usize> = BTreeMap::new();
    for a in &cand {
        let Some(idx) = graph.get_index(a) else {
            continue;
        };
        for nb in graph.graph.neighbors_directed(idx, Direction::Outgoing) {
            let to = graph.graph[nb].doc.anchor.clone();
            if cand_set.contains(to.as_str()) {
                *deg.entry(a.clone()).or_default() += 1;
                *deg.entry(to).or_default() += 1;
            }
        }
    }
    let mut ranked = cand;
    ranked.sort_by(|a, b| {
        deg.get(b)
            .unwrap_or(&0)
            .cmp(deg.get(a).unwrap_or(&0))
            .then_with(|| a.cmp(b))
    });
    let nodes: BTreeSet<String> = ranked.into_iter().take(cap).collect();
    let mut edges: BTreeSet<(String, String, String)> = BTreeSet::new();
    for a in &nodes {
        let Some(idx) = graph.get_index(a) else {
            continue;
        };
        for nb in graph.graph.neighbors_directed(idx, Direction::Outgoing) {
            let to = graph.graph[nb].doc.anchor.clone();
            if !nodes.contains(&to) {
                continue;
            }
            // ALL typed edges between the pair — parallel types are real data
            // (a test's call is both `Calls` and `Tests` since Wave 1).
            for e in graph.graph.edges_connecting(idx, nb) {
                edges.insert((
                    a.clone(),
                    to.clone(),
                    format!("{:?}", e.weight().edge_type),
                ));
            }
        }
    }
    let src = build_src_map(&root);
    Ok(render_json("", &nodes, &edges, &src))
}

/// Default cap on the whole-graph export — keep the most *important* (highest total
/// degree) nodes so a large project stays renderable in the browser. `--full` (cap 0)
/// emits everything. Generous: the viewer lenses slice this down client-side.
const GRAPH_CAP: usize = 800;

/// Whole-graph JSON — the single comprehensive payload the interactive viewer's
/// client-side *lenses* (overview / neighborhood / impact / replay) slice, so a view
/// switch or re-root never re-calls aden. Every node carries the full "view model":
/// `{id, anchor, label, group, community, kind, degree, file, line}`; every typed edge
/// among the kept nodes is emitted in its true orientation. Nodes are ranked by total
/// degree (importance) and capped at `cap` (0 = full) so big projects stay renderable.
///
/// This is the export the architecture note (research: viewer-unified-explorer) calls
/// for: aden computes the rich, whole-graph, code+prose view model *once* and any
/// consumer (viewer, agent, CI gate) lenses it, instead of each re-deriving it.
fn render_whole_graph_json(graph: &Graph, root: &Path, cap: usize) -> String {
    // Total (in+out) degree per node — the importance signal the cap ranks on, and a
    // first-class field every consumer wants (centrality without a re-derivation).
    let mut degree: BTreeMap<String, usize> = BTreeMap::new();
    for idx in graph.graph.node_indices() {
        let a = graph.graph[idx].doc.anchor.clone();
        let out = graph.graph.neighbors_directed(idx, Direction::Outgoing).count();
        let inc = graph.graph.neighbors_directed(idx, Direction::Incoming).count();
        degree.insert(a, out + inc);
    }

    // Community of every member + a human label per community (the most common group).
    let comms = aden_graph::community::detect_communities(graph, 1.0);
    let mut comm_of: BTreeMap<String, usize> = BTreeMap::new();
    let mut comm_meta: Vec<serde_json::Value> = Vec::new();
    for (i, members) in comms.iter().enumerate() {
        for m in members {
            comm_of.insert(m.clone(), i);
        }
        if members.len() >= 2 {
            comm_meta.push(serde_json::json!({
                "id": i, "label": community_label(members), "size": members.len(),
            }));
        }
    }

    // Rank all nodes by degree (desc), tiebreak anchor (asc, deterministic), cap.
    let mut ranked: Vec<String> = degree.keys().cloned().collect();
    ranked.sort_by(|a, b| {
        degree.get(b).unwrap_or(&0).cmp(degree.get(a).unwrap_or(&0)).then_with(|| a.cmp(b))
    });
    let total = ranked.len();
    if cap > 0 && ranked.len() > cap {
        ranked.truncate(cap);
    }
    let kept: BTreeSet<String> = ranked.into_iter().collect();
    let ids: BTreeMap<&str, String> = kept
        .iter()
        .enumerate()
        .map(|(i, a)| (a.as_str(), format!("n{i}")))
        .collect();

    let src = build_src_map(root);
    let nodes_json: Vec<serde_json::Value> = kept
        .iter()
        .map(|a| {
            let idx = graph.get_index(a);
            let kind = idx
                .map(|i| format!("{:?}", graph.graph[i].doc.node_type))
                .unwrap_or_else(|| "Note".to_string());
            let mut obj = serde_json::json!({
                "id": ids[a.as_str()],
                "anchor": a,
                "label": label(a),
                "group": group_of(a),
                "kind": kind,
                "degree": degree.get(a).copied().unwrap_or(0),
            });
            if let Some(&c) = comm_of.get(a) {
                obj["community"] = serde_json::json!(c);
            }
            if let Some((file, line)) = src.get(a) {
                obj["file"] = serde_json::json!(file);
                obj["line"] = serde_json::json!(line);
            }
            obj
        })
        .collect();

    // Every typed edge among kept nodes, oriented source→target.
    let mut edge_set: BTreeSet<(String, String, String)> = BTreeSet::new();
    for a in &kept {
        let Some(idx) = graph.get_index(a) else { continue };
        for nb in graph.graph.neighbors_directed(idx, Direction::Outgoing) {
            let to = graph.graph[nb].doc.anchor.clone();
            if !kept.contains(&to) {
                continue;
            }
            // ALL typed edges between the pair — collapsing to the first one
            // hid co-emitted types (e.g. `Tests` alongside `Calls`) from the
            // census this JSON feeds.
            for e in graph.graph.edges_connecting(idx, nb) {
                edge_set.insert((
                    ids[a.as_str()].clone(),
                    ids[to.as_str()].clone(),
                    format!("{:?}", e.weight().edge_type),
                ));
            }
        }
    }
    let edges_json: Vec<serde_json::Value> = edge_set
        .iter()
        .map(|(f, t, ty)| serde_json::json!({ "from": f, "to": t, "type": ty }))
        .collect();

    serde_json::to_string_pretty(&serde_json::json!({
        "mode": "graph",
        "nodes": nodes_json,
        "edges": edges_json,
        "communities": comm_meta,
        "total_nodes": total,
        "shown_nodes": kept.len(),
    }))
    .unwrap_or_else(|_| "{}".to_string())
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
                // ALL typed edges between the pair (parallel types are real
                // data — e.g. `Tests` co-emitted with `Calls`).
                let ets: Vec<String> = graph
                    .graph
                    .edges_connecting(from_idx, to_idx)
                    .map(|e| format!("{:?}", e.weight().edge_type))
                    .collect();
                if ets.is_empty() {
                    continue;
                }
                let from = graph.graph[from_idx].doc.anchor.clone();
                let to = graph.graph[to_idx].doc.anchor.clone();
                nodes.insert(nb_anchor);
                for et in ets {
                    edges.insert((from.clone(), to.clone(), et));
                }
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
            // ALL typed edges between the pair, not just the first.
            for e in graph.graph.edges_connecting(idx, nb) {
                edges.insert((
                    a.clone(),
                    to.clone(),
                    format!("{:?}", e.weight().edge_type),
                ));
            }
        }
    }
    (comms, edges)
}

/// Hierarchical JSON for the communities view: a super-node *overview* (one node per
/// community, sized by membership, + aggregated inter-community edges) PLUS a
/// per-community *drill* subgraph (capped members + intra-community edges). The
/// interactive viewer shows the overview and expands a community on click — restoring
/// the per-symbol detail that the collapse hides — without re-calling aden.
fn render_communities_view_json(
    graph: &Graph,
    root: &Path,
    min_size: usize,
    resolution: f64,
    max_comms: usize,
    member_cap: usize,
) -> Result<String, Box<dyn std::error::Error>> {
    let kept: Vec<Vec<String>> = aden_graph::community::detect_communities(graph, resolution)
        .into_iter()
        .filter(|c| c.len() >= min_size)
        .take(max_comms)
        .collect();
    if kept.is_empty() {
        return Err("no communities of size >= 2 found (try `aden communities`)".into());
    }

    // community index of every member (for inter-community edge aggregation)
    let mut comm_of: BTreeMap<String, usize> = BTreeMap::new();
    for (i, members) in kept.iter().enumerate() {
        for m in members {
            comm_of.insert(m.clone(), i);
        }
    }

    // overview super-nodes (sized by membership)
    let super_nodes: Vec<serde_json::Value> = kept
        .iter()
        .enumerate()
        .map(|(i, members)| {
            serde_json::json!({
                "id": format!("c{i}"),
                "label": community_label(members),
                "community": i,
                "size": members.len(),
            })
        })
        .collect();

    // inter-community weighted edges
    let mut weights: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for (anchor, &ca) in &comm_of {
        let Some(idx) = graph.get_index(anchor) else {
            continue;
        };
        for nb in graph.graph.neighbors_directed(idx, Direction::Outgoing) {
            let Some(&cb) = comm_of.get(&graph.graph[nb].doc.anchor) else {
                continue;
            };
            if ca == cb {
                continue;
            }
            let key = if ca < cb { (ca, cb) } else { (cb, ca) };
            *weights.entry(key).or_default() += 1;
        }
    }
    let super_edges: Vec<serde_json::Value> = weights
        .iter()
        .map(|(&(a, b), &w)| {
            serde_json::json!({ "from": format!("c{a}"), "to": format!("c{b}"), "type": format!("{w} edges") })
        })
        .collect();

    // per-community drill subgraph: the most intra-connected members + their edges
    let src = build_src_map(root);
    let mut drill = serde_json::Map::new();
    for (i, members) in kept.iter().enumerate() {
        let member_set: BTreeSet<&str> = members.iter().map(|s| s.as_str()).collect();
        // Rank members by *intra-community* degree so the drill shows the connected
        // core — alphabetical-first members are usually mutually unconnected.
        let mut degree: BTreeMap<String, usize> = BTreeMap::new();
        for m in members {
            let Some(idx) = graph.get_index(m) else {
                continue;
            };
            for nb in graph.graph.neighbors_directed(idx, Direction::Outgoing) {
                let to = graph.graph[nb].doc.anchor.clone();
                if member_set.contains(to.as_str()) {
                    *degree.entry(m.clone()).or_default() += 1;
                    *degree.entry(to).or_default() += 1;
                }
            }
        }
        let mut ranked: Vec<String> = members.clone();
        ranked.sort_by(|a, b| {
            degree
                .get(b)
                .unwrap_or(&0)
                .cmp(degree.get(a).unwrap_or(&0))
                .then_with(|| a.cmp(b))
        });
        let shown: Vec<String> = ranked.into_iter().take(member_cap).collect();
        let local: BTreeMap<&str, String> = shown
            .iter()
            .enumerate()
            .map(|(j, m)| (m.as_str(), format!("m{j}")))
            .collect();
        let shown_set: BTreeSet<&str> = shown.iter().map(|s| s.as_str()).collect();
        let nodes: Vec<serde_json::Value> = shown
            .iter()
            .map(|m| {
                let mut obj = serde_json::json!({ "id": local[m.as_str()], "anchor": m, "label": label(m), "community": i, "group": group_of(m) });
                if let Some((file, line)) = src.get(m) {
                    obj["file"] = serde_json::json!(file);
                    obj["line"] = serde_json::json!(line);
                }
                obj
            })
            .collect();
        let mut edge_set: BTreeSet<(String, String, String)> = BTreeSet::new();
        for m in &shown {
            let Some(idx) = graph.get_index(m) else {
                continue;
            };
            for nb in graph.graph.neighbors_directed(idx, Direction::Outgoing) {
                let to = graph.graph[nb].doc.anchor.clone();
                if !shown_set.contains(to.as_str()) {
                    continue;
                }
                // ALL typed edges between the pair, not just the first.
                for e in graph.graph.edges_connecting(idx, nb) {
                    edge_set.insert((
                        local[m.as_str()].clone(),
                        local[to.as_str()].clone(),
                        format!("{:?}", e.weight().edge_type),
                    ));
                }
            }
        }
        let edges: Vec<serde_json::Value> = edge_set
            .iter()
            .map(|(f, t, ty)| serde_json::json!({ "from": f, "to": t, "type": ty }))
            .collect();
        drill.insert(
            format!("c{i}"),
            serde_json::json!({ "nodes": nodes, "edges": edges }),
        );
    }

    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "mode": "communities",
        "nodes": super_nodes,
        "edges": super_edges,
        "drill": serde_json::Value::Object(drill),
    }))
    .unwrap_or_else(|_| "{}".to_string()))
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

/// A human label for a community: the dominant subsystem among its members — but
/// "mixed" when no subsystem holds a majority. A low-purity community is a Louvain
/// "misc" merge of small peripheral modules (e.g. aden-mcp + aden-lsp + benches with
/// no edges between them); labelling it with one crate name is misleading, so be honest.
fn community_label(members: &[String]) -> String {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for m in members {
        *counts.entry(group_of(m)).or_default() += 1;
    }
    // Deterministic: ties resolve to the alphabetically-first group (BTreeMap order).
    let Some((top, n)) = counts.into_iter().max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(a.0)))
    else {
        return "mixed".to_string();
    };
    if (n as f64) < 0.6 * members.len() as f64 {
        return "mixed".to_string();
    }
    top.to_string()
}

/// Dispatch a flat (blast/connectivity) slice to the requested format.
fn render_flat(
    root: &str,
    nodes: &BTreeSet<String>,
    edges: &BTreeSet<(String, String, String)>,
    format: &str,
    src: &SrcMap,
) -> Result<String, Box<dyn std::error::Error>> {
    Ok(match format {
        "dot" => render_dot(root, nodes, edges),
        "mermaid" => render_mermaid(root, nodes, edges),
        "asciidoc" | "adoc" => render_asciidoc(root, nodes, edges),
        "json" => render_json(root, nodes, edges, src),
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
    src: &SrcMap,
) -> String {
    let ids = id_map(nodes);
    let nodes_json: Vec<serde_json::Value> = nodes
        .iter()
        .map(|a| {
            let mut obj = serde_json::json!({
                "id": ids[a.as_str()],
                "anchor": a,
                "label": label(a),
                "group": group_of(a),
                "root": a == root,
            });
            if let Some((file, line)) = src.get(a) {
                obj["file"] = serde_json::json!(file);
                obj["line"] = serde_json::json!(line);
            }
            obj
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
        // aden-cli is 2/3 (≥60%) → dominant.
        assert_eq!(community_label(&members), "aden-cli");
        // No subsystem holds a majority → honest "mixed" (the low-purity-merge case).
        let mixed = vec![
            "aden://module/aden-mcp/a#x".to_string(),
            "aden://module/aden-lsp/b#y".to_string(),
            "aden://module/benches/c#z".to_string(),
        ];
        assert_eq!(community_label(&mixed), "mixed");
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
        let j = render_json(&root, &nodes, &edges, &SrcMap::new());
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
