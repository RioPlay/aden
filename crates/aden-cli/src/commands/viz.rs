// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `aden viz` — export a slice of the knowledge graph as a text diagram (Mermaid
//! or Graphviz DOT) for human navigation. Text-first on purpose: the output drops
//! straight into AsciiDoc/Markdown, a PR comment, or CI, with zero runtime and no
//! interactive UI (ADR/roadmap M1 — the interactive viewer is deferred).
//!
//! Anchor-centred slices share `impact-diff` / `query --impact`'s edge SET but
//! split by direction, named honestly (ADR-007 §2): *blast* walks incoming
//! dependents (who breaks if this changes — agrees with `impact-diff`), *reach*
//! walks outgoing dependencies (what this relies on — agrees with
//! `query --impact`), *connectivity* walks both. All BFS-limited by `--depth`.

use crate::util::{find_project_root, impact_edge_types};
use aden_graph::Direction;
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::path::Path;

fn now_rfc3339() -> String {
    crate::time_util::unix_secs_to_rfc3339(crate::time_util::now_unix_secs())
}

fn git_head_short(root: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(root)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The concrete graph type the cache yields — aliased to keep slice signatures short.
type Graph = aden_graph::AdenGraph<aden_graph::DocumentNode, aden_graph::AdenEdge>;

/// A typed node/edge slice: a flat set of anchors + the edges among them.
type Slice = (BTreeSet<String>, BTreeSet<(String, String, String)>);

/// One source location in three deliberately separate representations.
///
/// `path` is the native filesystem path used by Rust file I/O and scope checks;
/// `display` preserves that native spelling for commands shown to the user;
/// `editor` is an RFC 3986-safe URL path for editor URI templates. Conflating
/// these was the source of `/C:/...` being fed back into Windows filesystem APIs.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceLocation {
    path: std::path::PathBuf,
    display: String,
    editor: String,
    line: usize,
    loc: usize,
}

/// Anchor → source location, for editor links, snippets, and node sizing.
type SrcMap = BTreeMap<String, SourceLocation>;

fn absolute_source_path(root: &Path, file: &str) -> std::path::PathBuf {
    let path = Path::new(file);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    absolute.canonicalize().unwrap_or(absolute)
}

/// Convert Windows verbatim paths to the normal native spelling users and
/// editors understand, while retaining the original `PathBuf` for filesystem I/O.
fn native_display_path(path: &Path, windows_style: bool) -> String {
    let value = path.to_string_lossy();
    if !windows_style {
        return value.into_owned();
    }
    if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    value.strip_prefix(r"\\?\").unwrap_or(&value).to_string()
}

/// Percent-encode an absolute native path for use inside editor URI templates.
/// URI path separators are `/` on every platform, while the native path remains
/// untouched elsewhere. `windows_style` is explicit so drive and UNC behavior is
/// regression-testable on non-Windows CI.
fn editor_path_from_native(native: &str, windows_style: bool) -> String {
    let normalized = if windows_style {
        native.replace('\\', "/")
    } else {
        native.to_string()
    };
    let rooted = if normalized.starts_with('/') {
        normalized
    } else {
        format!("/{normalized}")
    };
    let mut encoded = String::with_capacity(rooted.len());
    for byte in rooted.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/' | b':') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            write!(&mut encoded, "%{byte:02X}").expect("writing to String cannot fail");
        }
    }
    encoded
}

fn add_source_fields(obj: &mut serde_json::Value, source: &SourceLocation, line: usize) {
    obj["file"] = serde_json::json!(source.display);
    obj["editor_file"] = serde_json::json!(source.editor);
    obj["line"] = serde_json::json!(line);
    obj["loc"] = serde_json::json!(source.loc);
}

/// Build source locations from the store's symbol spans. Filesystem operations
/// always receive `path`; only browser/editor links receive `editor`.
fn build_src_map(root: &Path) -> SrcMap {
    let mut map = SrcMap::new();
    for (file, spans) in super::grep::load_symbol_spans(root) {
        let path = absolute_source_path(root, &file);
        let display = native_display_path(&path, cfg!(windows));
        let editor = editor_path_from_native(&display, cfg!(windows));
        for span in spans {
            let loc = span.end.saturating_sub(span.start) + 1;
            map.entry(span.anchor).or_insert_with(|| SourceLocation {
                path: path.clone(),
                display: display.clone(),
                editor: editor.clone(),
                line: span.start,
                loc,
            });
        }
    }
    map
}

/// Word count of a doc node's prose blocks — the prose analogue of LOC, for
/// content-mass node sizing in the viewer (a 2,000-word ADR should not render
/// the same as a one-line heading stub).
fn doc_word_count(doc: &aden_core::Document) -> usize {
    doc.blocks
        .iter()
        .map(|b| match b {
            aden_core::Block::Paragraph(t) => t.split_whitespace().count(),
            _ => 0,
        })
        .sum()
}

/// Cap above which snippet embedding is skipped: snippets exist for the
/// interactive viewer's flyby cards, and beyond this the payload (and the
/// file reads) stop being worth it — kernel-scale exports stay lean.
const SNIPPET_NODE_CAP: usize = 1500;
const SNIPPET_MAX_LINES: usize = 9;
const SNIPPET_MAX_CHARS: usize = 380;

/// First lines of each anchor's source span, read once per file. Returns
/// anchor → snippet text (trimmed, char-capped). Files are read directly from
/// the working tree at export time — the snippet shows what's on disk NOW,
/// which is exactly what the viewer's "open in editor" lands on.
fn collect_snippets(src: &SrcMap, kept: &BTreeSet<String>) -> BTreeMap<String, String> {
    let mut by_file: BTreeMap<&Path, Vec<(&str, usize, usize)>> = BTreeMap::new();
    for anchor in kept {
        if let Some(source) = src.get(anchor) {
            by_file.entry(source.path.as_path()).or_default().push((
                anchor.as_str(),
                source.line,
                source.loc,
            ));
        }
    }
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for (file, anchors) in by_file {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        for (anchor, start, loc) in anchors {
            let from = start.saturating_sub(1).min(lines.len());
            let to = (from + loc.min(SNIPPET_MAX_LINES)).min(lines.len());
            if from >= to {
                continue;
            }
            let mut snip = lines[from..to].join("\n");
            if snip.len() > SNIPPET_MAX_CHARS {
                let mut cut = SNIPPET_MAX_CHARS;
                while !snip.is_char_boundary(cut) {
                    cut -= 1;
                }
                snip.truncate(cut);
                snip.push('…');
            }
            if !snip.trim().is_empty() {
                out.insert(anchor.to_string(), snip);
            }
        }
    }
    out
}

/// First prose paragraph of a doc node, word-capped — the flyby card for
/// docs/terms shows what the section actually says.
fn doc_snippet(doc: &aden_core::Document) -> Option<String> {
    let text = doc.blocks.iter().find_map(|b| match b {
        aden_core::Block::Paragraph(t) if !t.trim().is_empty() => Some(t.as_str()),
        _ => None,
    })?;
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }
    let take = words.len().min(50);
    let mut s = words[..take].join(" ");
    if take < words.len() {
        s.push('…');
    }
    Some(s)
}

/// Restrict the graph to anchors whose source file lives under `scope` (a
/// project-relative subdirectory or file). This is the kernel-scale escape
/// hatch: `aden viz --mode communities --scope net/` runs detection on the
/// `net/` SUBGRAPH instead of post-filtering whole-project communities — the
/// clusters are the clusters *of that subtree*. Anchors without a recorded
/// source span (synthesized `mod-*` hubs, prose docs) drop out; an empty
/// result is an error, not an empty diagram.
fn scoped_subgraph(
    graph: &Graph,
    root: &Path,
    scope: &str,
) -> Result<Graph, Box<dyn std::error::Error>> {
    let src = build_src_map(root);
    let scope_path = absolute_source_path(root, scope);
    let allowed: HashSet<&str> = src
        .iter()
        .filter(|(_, source)| source.path == scope_path || source.path.starts_with(&scope_path))
        .map(|(anchor, _)| anchor.as_str())
        .collect();
    if allowed.is_empty() {
        return Err(format!(
            "--scope '{scope}' matched no indexed sources (paths are relative to the \
             project root, e.g. `--scope crates/aden-cli` or `--scope net/`)"
        )
        .into());
    }
    let mut g: Graph = aden_graph::AdenGraph::new();
    for idx in graph.graph.node_indices() {
        let n = &graph.graph[idx];
        if allowed.contains(n.doc.anchor.as_str()) {
            let _ = g.add_node(n.clone());
        }
    }
    for e in graph.graph.edge_indices() {
        let Some((s, t)) = graph.graph.edge_endpoints(e) else {
            continue;
        };
        let (sa, ta) = (&graph.graph[s].doc.anchor, &graph.graph[t].doc.anchor);
        if let (Some(si), Some(ti)) = (g.get_index(sa), g.get_index(ta)) {
            g.add_edge(si, ti, graph.graph[e]);
        }
    }
    Ok(g)
}

/// Reject a directory path passed in the ANCHOR position. The positional
/// order is `viz [ANCHOR] [DIR]`, so `aden viz <path>` puts the path in
/// ANCHOR — and the modes that ignore ANCHOR (graph/communities) would then
/// silently visualize the CWD instead of the intended project. A real symbol
/// is never also an existing directory; when both could be meant, the full
/// `aden://…` anchor form disambiguates.
fn reject_directory_anchor(anchor: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(a) = anchor
        && !a.starts_with("aden://")
        && Path::new(a).is_dir()
    {
        return Err(format!(
            "'{a}' is a directory, but the first positional is ANCHOR (a symbol or aden:// \
             anchor) — the project directory comes last: `aden viz [ANCHOR] {a}` or \
             `aden viz --mode communities {a}`."
        )
        .into());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)] // CLI handler mirroring the subcommand's flags 1:1
pub fn cmd_viz(
    path: &Path,
    anchor: Option<&str>,
    depth: usize,
    format: &str,
    mode: &str,
    json: bool,
    full: bool,
    scope: Option<&str>,
    resolution: f64,
) -> Result<(), Box<dyn std::error::Error>> {
    reject_directory_anchor(anchor)?;
    // The global `-j/--json` flag is an alias for `--format json`, so it never
    // becomes a silent no-op the way an ignored global flag would.
    let format = if json { "json" } else { format };
    let root = find_project_root(path);
    let _stale_hint = super::StaleHintGuard::new(&root, format == "json");
    // Keep the graph fresh so the rendered slice reflects the current code.
    super::ensure_fresh(&root);
    let mut graph = aden_graph::cache::build_from_directory_cached(&root)?;
    if let Some(sub) = scope {
        graph = scoped_subgraph(&graph, &root, sub)?;
    }

    let diagram = match mode {
        // Whole-graph functional clusters → DOT `cluster_*` is the right default
        // (see research: viz-design). Anchor (if any) is ignored here.
        "communities" => {
            // JSON / viewer uses the collapsed super-node overview (connected and
            // legible); the static formats keep member cluster-boxes.
            if format == "json" {
                render_communities_view_json(
                    &graph,
                    &root,
                    2,
                    resolution,
                    MAX_COMMUNITIES,
                    DRILL_CAP,
                )?
            } else {
                let (comms, edges) =
                    communities_slice(&graph, 2, resolution, MAX_COMMUNITIES, MEMBER_CAP);
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
            render_whole_graph_json(&graph, &root, if full { 0 } else { GRAPH_CAP }, resolution)
        }
        // Anchor-centred views.
        "blast" | "reach" | "connectivity" => {
            let anchor = anchor.ok_or_else(|| -> Box<dyn std::error::Error> {
                format!(
                    "--mode {mode} needs an ANCHOR (a symbol like `cmd_understand` or a full aden:// anchor)"
                )
                .into()
            })?;
            let root_anchor = resolve_anchor(&graph, anchor)?;
            let (nodes, edges) = match mode {
                "connectivity" => connectivity_slice(&graph, &root_anchor, depth, NODE_CAP),
                "reach" => reach_slice(&graph, &root_anchor, depth, NODE_CAP),
                _ => blast_slice(&graph, &root_anchor, depth, NODE_CAP),
            };
            let src = build_src_map(&root);
            render_flat(
                &root_anchor,
                &nodes,
                &edges,
                format,
                &src,
                &graph,
                &root,
                mode,
            )?
        }
        other => {
            return Err(format!(
                "unknown --mode '{other}' (expected 'blast', 'reach', 'connectivity', 'communities', or 'graph')"
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
    scope: Option<&str>,
    resolution: f64,
) -> Result<String, Box<dyn std::error::Error>> {
    // `view` shares the positional trap (`aden view .`): same guard.
    reject_directory_anchor(anchor)?;
    let root = find_project_root(path);
    let _stale_hint = super::StaleHintGuard::new(&root, true);
    super::ensure_fresh(&root);
    let mut graph = aden_graph::cache::build_from_directory_cached(&root)?;
    if let Some(sub) = scope {
        graph = scoped_subgraph(&graph, &root, sub)?;
    }
    match mode {
        "graph" => Ok(render_whole_graph_json(
            &graph, &root, GRAPH_CAP, resolution,
        )),
        "communities" => {
            render_communities_view_json(&graph, &root, 2, resolution, MAX_COMMUNITIES, DRILL_CAP)
        }
        "blast" | "reach" | "connectivity" => {
            let anchor = anchor.ok_or_else(|| -> Box<dyn std::error::Error> {
                format!("--mode {mode} needs an ANCHOR (a symbol or full aden:// anchor)").into()
            })?;
            let root_anchor = resolve_anchor(&graph, anchor)?;
            let (nodes, edges) = match mode {
                "connectivity" => connectivity_slice(&graph, &root_anchor, depth, NODE_CAP),
                "reach" => reach_slice(&graph, &root_anchor, depth, NODE_CAP),
                _ => blast_slice(&graph, &root_anchor, depth, NODE_CAP),
            };
            let src = build_src_map(&root);
            Ok(render_json(
                &root_anchor,
                &nodes,
                &edges,
                &src,
                &graph,
                &root,
                mode,
            ))
        }
        other => Err(format!(
            "unknown --mode '{other}' (expected blast, reach, connectivity, communities, or graph)"
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
    let _stale_hint = super::StaleHintGuard::new(&root, true);
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
                edges.insert((a.clone(), to.clone(), format!("{:?}", e.weight().edge_type)));
            }
        }
    }
    let src = build_src_map(&root);
    Ok(render_json(
        "", &nodes, &edges, &src, &graph, &root, "graph",
    ))
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
fn render_whole_graph_json(graph: &Graph, root: &Path, cap: usize, resolution: f64) -> String {
    // Total (in+out) degree per node — the importance signal the cap ranks on, and a
    // first-class field every consumer wants (centrality without a re-derivation).
    let mut degree: BTreeMap<String, usize> = BTreeMap::new();
    for idx in graph.graph.node_indices() {
        let a = graph.graph[idx].doc.anchor.clone();
        let out = graph
            .graph
            .neighbors_directed(idx, Direction::Outgoing)
            .count();
        let inc = graph
            .graph
            .neighbors_directed(idx, Direction::Incoming)
            .count();
        degree.insert(a, out + inc);
    }

    // Community of every member + a human label per community (the most common group).
    let comms = aden_graph::community::detect_communities(graph, resolution);
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
        degree
            .get(b)
            .unwrap_or(&0)
            .cmp(degree.get(a).unwrap_or(&0))
            .then_with(|| a.cmp(b))
    });
    let total = ranked.len();
    // Compact global search index — all anchors ranked by degree, capped so the page stays
    // fast. Each entry uses short keys {n,a,k,g,d} to minimise payload.
    const ALL_ANCHORS_CAP: usize = 4000;
    let all_anchors: Vec<serde_json::Value> = ranked
        .iter()
        .take(ALL_ANCHORS_CAP)
        .map(|a| {
            let k = graph
                .get_index(a)
                .map(|i| format!("{:?}", graph.graph[i].doc.node_type))
                .unwrap_or_else(|| "Note".to_string());
            serde_json::json!({
                "n": label(a),
                "a": a,
                "k": k,
                "g": group_of(a),
                "d": degree.get(a).copied().unwrap_or(0),
            })
        })
        .collect();
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
    let snippets = if kept.len() <= SNIPPET_NODE_CAP {
        collect_snippets(&src, &kept)
    } else {
        BTreeMap::new()
    };
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
            if let Some(source) = src.get(a) {
                add_source_fields(&mut obj, source, source.line);
            } else if let Some(rest) = a.strip_prefix("mod-") {
                // Aggregate hub: no own span — aim "open in editor" at the crate entry.
                if let Some(source) = module_entry_file(rest, &src) {
                    add_source_fields(&mut obj, source, 1);
                }
            }
            // Prose mass: word count for doc/term nodes (no source span).
            if (a.starts_with("aden://doc/") || a.starts_with("aden://term/"))
                && let Some(i) = idx
            {
                let w = doc_word_count(&graph.graph[i].doc);
                if w > 0 {
                    obj["words"] = serde_json::json!(w);
                }
                if kept.len() <= SNIPPET_NODE_CAP
                    && let Some(s) = doc_snippet(&graph.graph[i].doc)
                {
                    obj["snippet"] = serde_json::json!(s);
                }
            }
            if let Some(s) = snippets.get(a.as_str()) {
                obj["snippet"] = serde_json::json!(s);
            }
            obj
        })
        .collect();

    // Every typed edge among kept nodes, oriented source→target.
    let mut edge_set: BTreeSet<(String, String, String)> = BTreeSet::new();
    for a in &kept {
        let Some(idx) = graph.get_index(a) else {
            continue;
        };
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

    let generated_at = now_rfc3339();
    let git_hash = git_head_short(root).unwrap_or_default();
    serde_json::to_string_pretty(&serde_json::json!({
        "mode": "graph",
        "generated_at": generated_at,
        "git_hash": git_hash,
        "nodes": nodes_json,
        "edges": edges_json,
        "communities": comm_meta,
        "total_nodes": total,
        "shown_nodes": kept.len(),
        "all_anchors": all_anchors,
        "all_anchors_total": total,
    }))
    .unwrap_or_else(|_| "{}".to_string())
}

/// Blast radius: BFS *incoming* over impact edges from `root_anchor` — the
/// transitive dependents (callers/referencers) at risk if the anchor changes.
/// Same direction as `impact-diff`'s `dependents_of` (ADR-007 §2). Edges are
/// stored referencer→referencee, so the connecting edge runs neighbor→node and
/// is rendered that way (arrows keep pointing at what gets used). Depth-capped
/// and node-capped at `cap` (closest-first, so the cap keeps the nearest
/// dependents).
fn blast_slice(graph: &Graph, root_anchor: &str, depth: usize, cap: usize) -> Slice {
    directed_slice(graph, root_anchor, depth, cap, Direction::Incoming)
}

/// Downstream reach: BFS *outgoing* over impact edges — the transitive
/// dependencies `root_anchor` relies on. Same direction as `query --impact`.
fn reach_slice(graph: &Graph, root_anchor: &str, depth: usize, cap: usize) -> Slice {
    directed_slice(graph, root_anchor, depth, cap, Direction::Outgoing)
}

fn directed_slice(
    graph: &Graph,
    root_anchor: &str,
    depth: usize,
    cap: usize,
    dir: Direction,
) -> Slice {
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
        for neighbor in graph.graph.neighbors_directed(node, dir) {
            // The stored edge runs source→target regardless of which side we
            // walked in from; keep that orientation in the rendered slice.
            let (src, dst) = match dir {
                Direction::Outgoing => (node, neighbor),
                Direction::Incoming => (neighbor, node),
            };
            let etype = graph
                .graph
                .edges_connecting(src, dst)
                .find(|e| impact.contains(&e.weight().edge_type))
                .map(|e| format!("{:?}", e.weight().edge_type));
            let Some(et) = etype else { continue };
            let nb_anchor = graph.graph[neighbor].doc.anchor.clone();
            // Respect the cap: only introduce a *new* node while under it (edges to
            // already-kept nodes still count, so the shown subgraph stays connected).
            if !nodes.contains(&nb_anchor) && nodes.len() >= cap {
                continue;
            }
            let from = graph.graph[src].doc.anchor.clone();
            let to = graph.graph[dst].doc.anchor.clone();
            nodes.insert(nb_anchor);
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
                edges.insert((a.clone(), to.clone(), format!("{:?}", e.weight().edge_type)));
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
                if let Some(source) = src.get(m) {
                    add_source_fields(&mut obj, source, source.line);
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
    // Synthesized aggregate nodes ("mod-aden-parse", "mod-project") are NOT
    // aden:// anchors — without this they fall through to "?" and collapse into a
    // meaningless "?" region in the viewer legend/colours. Resolve them to their
    // project (the segment after "mod-").
    if let Some(rest) = anchor.strip_prefix("mod-") {
        return rest;
    }
    anchor
        .strip_prefix("aden://")
        .unwrap_or(anchor)
        .split('/')
        .nth(1)
        .unwrap_or("other")
}

/// Resolve a synthesized `mod-<crate>` hub to a representative source file — its
/// crate entry point — so the aggregate node can still offer an "open in editor"
/// link despite having no span of its own. Prefers `src/lib.rs`, then
/// `src/main.rs`, then any `lib.rs`/`main.rs`/`mod.rs`, else the shortest member
/// path. Members are every indexed anchor whose group resolves to this crate.
fn module_entry_file<'a>(crate_name: &str, src: &'a SrcMap) -> Option<&'a SourceLocation> {
    let mut files: Vec<&SourceLocation> = src
        .iter()
        .filter(|(anchor, _)| group_of(anchor) == crate_name)
        .map(|(_, source)| source)
        .collect();
    files.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    files.dedup_by(|a, b| a.path == b.path);
    files
        .iter()
        .find(|source| source.path.ends_with(Path::new("src").join("lib.rs")))
        .or_else(|| {
            files
                .iter()
                .find(|source| source.path.ends_with(Path::new("src").join("main.rs")))
        })
        .or_else(|| files.iter().find(|source| source.path.ends_with("lib.rs")))
        .or_else(|| files.iter().find(|source| source.path.ends_with("main.rs")))
        .or_else(|| files.iter().find(|source| source.path.ends_with("mod.rs")))
        .or_else(|| {
            files
                .iter()
                .min_by_key(|source| source.path.as_os_str().len())
        })
        .copied()
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
    let Some((top, n)) = counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(a.0)))
    else {
        return "mixed".to_string();
    };
    if (n as f64) < 0.6 * members.len() as f64 {
        return "mixed".to_string();
    }
    top.to_string()
}

/// Dispatch a flat (blast/connectivity) slice to the requested format.
#[allow(clippy::too_many_arguments)] // mirror of render_json's full context, all one shape
fn render_flat(
    root: &str,
    nodes: &BTreeSet<String>,
    edges: &BTreeSet<(String, String, String)>,
    format: &str,
    src: &SrcMap,
    graph: &Graph,
    proj_root: &Path,
    mode: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    Ok(match format {
        "dot" => render_dot(root, nodes, edges),
        "mermaid" => render_mermaid(root, nodes, edges),
        "asciidoc" | "adoc" => render_asciidoc(root, nodes, edges),
        "json" => render_json(root, nodes, edges, src, graph, proj_root, mode),
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
    let palette = [
        "#eef6ff", "#fff0f0", "#f0fff0", "#fffbe6", "#f5f0ff", "#f0ffff",
    ];
    let mut out = String::from(
        "digraph communities {\n  rankdir=LR;\n  node [shape=box];\n  compound=true;\n",
    );
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
    graph: &Graph,
    proj_root: &Path,
    mode: &str,
) -> String {
    let ids = id_map(nodes);
    let snippets = if nodes.len() <= SNIPPET_NODE_CAP {
        collect_snippets(src, nodes)
    } else {
        BTreeMap::new()
    };
    let nodes_json: Vec<serde_json::Value> = nodes
        .iter()
        .map(|a| {
            let kind = graph
                .get_index(a)
                .map(|i| format!("{:?}", graph.graph[i].doc.node_type))
                .unwrap_or_else(|| "Note".to_string());
            let mut obj = serde_json::json!({
                "id": ids[a.as_str()],
                "anchor": a,
                "label": label(a),
                "group": group_of(a),
                "kind": kind,
                "root": a == root,
            });
            if let Some(source) = src.get(a) {
                add_source_fields(&mut obj, source, source.line);
            } else if let Some(rest) = a.strip_prefix("mod-") {
                // Aggregate hub: no own span — aim "open in editor" at the crate entry.
                if let Some(source) = module_entry_file(rest, src) {
                    add_source_fields(&mut obj, source, 1);
                }
            }
            if (a.starts_with("aden://doc/") || a.starts_with("aden://term/"))
                && let Some(i) = graph.get_index(a)
            {
                let w = doc_word_count(&graph.graph[i].doc);
                if w > 0 {
                    obj["words"] = serde_json::json!(w);
                }
                if nodes.len() <= SNIPPET_NODE_CAP
                    && let Some(s) = doc_snippet(&graph.graph[i].doc)
                {
                    obj["snippet"] = serde_json::json!(s);
                }
            }
            if let Some(s) = snippets.get(a.as_str()) {
                obj["snippet"] = serde_json::json!(s);
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
    let generated_at = now_rfc3339();
    let git_hash = git_head_short(proj_root).unwrap_or_default();
    serde_json::to_string_pretty(&serde_json::json!({
        "root": root,
        "mode": mode,
        "generated_at": generated_at,
        "git_hash": git_hash,
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
        assert_eq!(
            anchor_tail("aden://module/aden-cli/query.rs#cmd_understand"),
            "cmd_understand"
        );
        assert_eq!(
            label("aden://module/aden-cli/query.rs#cmd_understand"),
            "query.rs#cmd_understand"
        );
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
            Community {
                label: "x".into(),
                size: 3,
                members: vec![a.clone(), b.clone()],
                overflow: 1,
            },
            Community {
                label: "y".into(),
                size: 1,
                members: vec![c.clone()],
                overflow: 0,
            },
        ];
        // ids are assigned in sorted-anchor order: a=n0, b=n1, c=n2
        let edges = BTreeSet::from([(a, c, "Calls".to_string())]);
        (comms, edges)
    }

    #[test]
    fn group_and_label() {
        assert_eq!(
            group_of("aden://module/aden-cli/query.rs#cmd_understand"),
            "aden-cli"
        );
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
        let empty_graph = aden_graph::AdenGraph::new();
        let j = render_json(
            &root,
            &nodes,
            &edges,
            &SrcMap::new(),
            &empty_graph,
            std::path::Path::new("."),
            "blast",
        );
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
    fn editor_paths_are_encoded_without_changing_native_path_syntax() {
        assert_eq!(
            native_display_path(Path::new(r"\\?\C:\repo\src\lib.rs"), true),
            r"C:\repo\src\lib.rs"
        );
        assert_eq!(
            native_display_path(Path::new(r"\\?\UNC\server\share\lib.rs"), true),
            r"\\server\share\lib.rs"
        );
        assert_eq!(
            editor_path_from_native(r"C:\Users\Ada Lovelace\repo#1\src\lib.rs", true),
            "/C:/Users/Ada%20Lovelace/repo%231/src/lib.rs"
        );
        assert_eq!(
            editor_path_from_native(r"\\server\share\dir name\lib.rs", true),
            "//server/share/dir%20name/lib.rs"
        );
        assert_eq!(
            editor_path_from_native("/tmp/back\\slash/a b.rs", false),
            "/tmp/back%5Cslash/a%20b.rs"
        );
    }

    #[test]
    fn viewer_json_separates_native_and_editor_paths() {
        let source = SourceLocation {
            path: std::path::PathBuf::from(r"C:\repo dir\src\lib.rs"),
            display: r"C:\repo dir\src\lib.rs".to_string(),
            editor: "/C:/repo%20dir/src/lib.rs".to_string(),
            line: 7,
            loc: 3,
        };
        let mut value = serde_json::json!({});
        add_source_fields(&mut value, &source, source.line);
        assert_eq!(value["file"], r"C:\repo dir\src\lib.rs");
        assert_eq!(value["editor_file"], "/C:/repo%20dir/src/lib.rs");
        assert_eq!(value["line"], 7);
        assert_eq!(value["loc"], 3);
    }

    #[test]
    fn snippet_reads_use_native_filesystem_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("source with space.rs");
        std::fs::write(&file, "fn native_path() {}\n").expect("write fixture");
        let anchor = "aden://module/test/source.rs#native_path".to_string();
        let source = SourceLocation {
            path: file.clone(),
            display: file.to_string_lossy().into_owned(),
            editor: editor_path_from_native(&file.to_string_lossy(), cfg!(windows)),
            line: 1,
            loc: 1,
        };
        let snippets = collect_snippets(
            &SrcMap::from([(anchor.clone(), source)]),
            &BTreeSet::from([anchor.clone()]),
        );
        assert_eq!(
            snippets.get(&anchor).map(String::as_str),
            Some("fn native_path() {}")
        );
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
