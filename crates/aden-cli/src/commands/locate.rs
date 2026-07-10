// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::{HashSet, VecDeque};
use std::path::Path;

use aden_graph::Direction;
use aden_store::GraphStorage;

use crate::util::{fmt_score, load_or_build_index, node_to_json, query_index};

/// Normalize the human spelling of a symbol without changing its identity:
/// whitespace is irrelevant and generic argument lists are elided. This makes
/// `AdenGraph::bfs`, `AdenGraph < N, E > :: bfs`, and the canonical stored
/// `AdenGraph<N, E>::bfs` comparable while preserving namespace/method order.
fn natural_symbol_form(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut generic_depth = 0usize;
    for ch in value.chars() {
        match ch {
            '<' => generic_depth += 1,
            '>' => generic_depth = generic_depth.saturating_sub(1),
            ch if generic_depth == 0 && !ch.is_whitespace() => out.push(ch),
            _ => {}
        }
    }
    out
}

/// Rank how well `anchor` matches the query `symbol` for symbol resolution
/// (lower is better). The anchor's trailing fragment is taken after the last
/// `#`/`/`; BOTH the whole fragment and its last `.`/`::` component (the bare
/// method/function name) are considered, so a method like `Scaffold.errorhandler`
/// is preferred for the query `errorhandler` over a substring superset such as
/// `Blueprint.app_errorhandler`. Shared by `cmd_locate` and `pick_symbol_anchor`
/// so the two resolvers can never disagree on what an exact match is.
///   0 — whole fragment == symbol, same case   (the exact definition)
///   1 — whole fragment == symbol, any case
///   2 — last `.`/`::` component == symbol, same case  (exact method name)
///   3 — last component == symbol, any case
///   4 — fragment starts `symbol.`/`symbol::`  (members of the queried symbol)
///   5 — any other substring match  (incidental)
fn anchor_match_rank(anchor: &str, symbol: &str) -> u8 {
    let sym_lower = natural_symbol_form(symbol).to_lowercase();
    let seg = natural_symbol_form(anchor.rsplit(['#', '/']).next().unwrap_or(""));
    let seg_lower = seg.to_lowercase();
    let leaf = seg.rsplit(['.', ':']).next().unwrap_or(&seg);
    let leaf_lower = leaf.to_lowercase();
    if seg == symbol {
        0
    } else if seg_lower == sym_lower {
        1
    } else if leaf == symbol {
        2
    } else if leaf_lower == sym_lower {
        3
    } else if seg_lower.starts_with(&format!("{sym_lower}."))
        || seg_lower.starts_with(&format!("{sym_lower}::"))
    {
        4
    } else if seg_lower.contains(&sym_lower) {
        5
    } else {
        u8::MAX
    }
}

/// Other store anchors that share `symbol`'s trailing `#symbol` segment, excluding
/// `chosen` (the anchor `understand` resolved to). Non-empty means the name is
/// defined in more than one place, so the single resolved view is incomplete — the
/// siblings carry their own backlinks/impact (M16). Case-insensitive match, sorted
/// + deduped for deterministic output.
fn alternate_anchors<'a>(
    symbol: &str,
    chosen: &str,
    anchors: impl Iterator<Item = &'a str>,
) -> Vec<String> {
    let sym_suffix = format!("#{}", symbol.to_lowercase());
    let mut v: Vec<String> = anchors
        .filter(|a| *a != chosen && a.to_lowercase().ends_with(&sym_suffix))
        .map(|a| a.to_string())
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Resolve a bare symbol name to a single full store anchor (the `understand`
/// resolver). Filters to anchors that contain the symbol, then picks the best by
/// [`anchor_match_rank`] (exact fragment > exact method-name > member > substring),
/// tie-broken by anchor. `None` when nothing matches — callers turn that into a
/// helpful "not found" message. Factored out so it is unit-testable without a live
/// store. Shares its ranking with `cmd_locate` so the two resolvers never disagree.
pub(crate) fn pick_symbol_anchor(symbol: &str, anchors: &[String]) -> Option<String> {
    ranked_symbol_candidates(symbol, anchors).into_iter().next()
}

/// Return every equally-best canonical anchor for a natural symbol spelling.
/// Callers that need a definitive target (notably `understand`) must treat more
/// than one candidate as ambiguity instead of silently choosing lexical order.
fn ranked_symbol_candidates(symbol: &str, anchors: &[String]) -> Vec<String> {
    let sym = symbol.to_lowercase();
    let mut matched: Vec<&String> = anchors
        .iter()
        .filter(|a| {
            let al = a.to_lowercase();
            al.ends_with(&format!("#{}", sym))
                || al.ends_with(&sym)
                || al.contains(&format!("#{}", sym))
                || anchor_match_rank(a, symbol) != u8::MAX
        })
        .collect();
    // Prefer an exact symbol or method-name match over an incidental substring,
    // so `errorhandler` resolves to `…#Scaffold.errorhandler` (method-name match)
    // not the substring superset `…#Blueprint.app_errorhandler`. Deterministic
    // tie-break by anchor string.
    matched.sort_by(|a, b| {
        anchor_match_rank(a, symbol)
            .cmp(&anchor_match_rank(b, symbol))
            .then_with(|| a.cmp(b))
    });
    let Some(best) = matched.first() else {
        return Vec::new();
    };
    let rank = anchor_match_rank(best, symbol);
    matched
        .into_iter()
        .filter(|anchor| anchor_match_rank(anchor, symbol) == rank)
        .map(|anchor| anchor.clone())
        .collect()
}

/// Backlinks of `anchor` (incoming references) as JSON nodes, one entry per
/// distinct referencer in iteration order. petgraph is a multigraph, so
/// `neighbors_directed` yields a neighbor once per parallel edge (e.g. a module
/// that both Contains and Calls the symbol); without the dedup `understand`
/// listed the same backlink multiple times.
fn collect_unique_backlinks(
    graph: &aden_graph::AdenGraph<aden_graph::DocumentNode, aden_graph::AdenEdge>,
    anchor: &str,
) -> Vec<serde_json::Value> {
    let mut out: Vec<serde_json::Value> = Vec::new();
    let Some(idx) = graph.get_index(anchor) else {
        return out;
    };
    let mut seen = HashSet::new();
    for neighbor in graph.graph.neighbors_directed(idx, Direction::Incoming) {
        if seen.insert(neighbor) {
            out.push(node_to_json(&graph.graph[neighbor], 1));
        }
    }
    out
}

/// `aden understand <symbol>` — one-shot symbol comprehension.
///
/// Bundles what previously took four separate invocations (`locate`,
/// `query --backlinks`, `query --impact`, `asm`) into a single coherent report:
///
/// 1. resolve the symbol to its store anchor + definition location,
/// 2. list backlinks (incoming references — who calls/references it),
/// 3. list downstream impact (outgoing reach over the shared impact edge set),
/// 4. assemble a context block from that anchor within `budget` tokens.
///
/// Reuses the shared `resolve_anchor_in_store` resolution and the same graph
/// traversal / assembly internals the individual commands use.
/// Downstream-impact reach: BFS over OUTGOING edges from `start`, keeping a
/// neighbor when ANY parallel edge between the pair is an impact edge. Returns
/// `(node, depth)` in BFS order. Multigraph-correct — `find_edge` returns one
/// arbitrary edge and would drop a neighbor whose impact edge (e.g. `Calls`)
/// coexists with a non-impact one (e.g. `Contains`). Mirrors `query --impact`.
fn impact_reachable(
    graph: &aden_graph::AdenGraph<aden_graph::DocumentNode, aden_graph::AdenEdge>,
    start: aden_graph::NodeIndex,
    impact_types: &[aden_core::EdgeType],
) -> Vec<(aden_graph::NodeIndex, usize)> {
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    visited.insert(start);
    queue.push_back((start, 0usize));
    while let Some((node, d)) = queue.pop_front() {
        for neighbor in graph.graph.neighbors_directed(node, Direction::Outgoing) {
            let is_impact = graph
                .graph
                .edges_connecting(node, neighbor)
                .any(|e| impact_types.contains(&e.weight().edge_type));
            if is_impact && visited.insert(neighbor) {
                out.push((neighbor, d + 1));
                queue.push_back((neighbor, d + 1));
            }
        }
    }
    out
}

pub fn cmd_understand(
    symbol: &str,
    path: &Path,
    budget: usize,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use aden_asm::traverse::{AssemblyOptions, assemble};
    use serde_json::json;

    if !path.is_dir() {
        return Err("understand requires a directory path".into());
    }
    let _stale_hint = super::StaleHintGuard::new(path, json);
    // Decision-grade: short-wait so blast radius is not silently stale.
    super::ensure_fresh_decision(path);

    // Step 1: resolve the symbol to a full store anchor. Try the shared exact
    // resolver first; fall back to suffix matching over the store's anchor keys
    // (same strategy `locate` uses) so a bare symbol name still resolves.
    let anchor = match aden_graph::cache::resolve_anchor_in_store(path, symbol) {
        Some(a) => a,
        None => {
            // Prefer snapshot for the anchor list in bare symbol fallback (lock-free).
            let anchors = if let Some((docs, _)) = aden_graph::snapshot::try_read_fresh(path) {
                docs.keys().cloned().collect::<Vec<_>>()
            } else {
                let (store_path, _) = aden_paths::resolve_read_store(path);
                aden_store::Storage::open_existing(store_path.to_str().ok_or("invalid store path")?)
                    .ok()
                    .and_then(|s| s.get_all_documents().ok())
                    .unwrap_or_default()
                    .into_keys()
                    .collect::<Vec<_>>()
            };
            match ranked_symbol_candidates(symbol, &anchors).as_slice() {
                [a] => a.clone(),
                [] => {
                    let msg = format!(
                        "No symbol found matching '{}'. Try 'aden locate --symbol {} .' for ranked recovery candidates.",
                        symbol, symbol
                    );
                    if json {
                        let env = super::augment_read_json(
                            path,
                            json!({
                                "symbol": symbol,
                                "anchor": null,
                                "error": msg,
                            }),
                        );
                        println!("{}", serde_json::to_string_pretty(&env)?);
                    } else {
                        println!("{}", msg);
                    }
                    return Ok(());
                }
                candidates => {
                    let recovery = format!(
                        "Ambiguous symbol '{}'; use an exact anchor or run 'aden locate --symbol {} .' to choose a candidate.",
                        symbol, symbol
                    );
                    if json {
                        let env = super::augment_read_json(
                            path,
                            json!({
                                "symbol": symbol,
                                "anchor": null,
                                "resolution": {
                                    "state": "ambiguous",
                                    "complete": false,
                                    "candidates": candidates,
                                    "recovery": recovery,
                                },
                            }),
                        );
                        println!("{}", serde_json::to_string_pretty(&env)?);
                    } else {
                        println!("{recovery}");
                        for candidate in candidates {
                            println!("  - {candidate}");
                        }
                    }
                    return Ok(());
                }
            }
        }
    };

    // Load the full graph once; all three structural views read from it.
    let graph = aden_graph::cache::build_from_directory_cached(path)?;
    let idx = graph.get_index(&anchor).ok_or_else(|| {
        format!(
            "Anchor '{}' not found in graph. Try 'aden locate --symbol {} .' for ranked recovery candidates.",
            anchor
            , anchor
        )
    })?;

    // M16: surface the OTHER definitions that share this symbol name. `understand`
    // resolves to exactly one anchor, so when a name is defined in several places
    // the siblings — and their distinct backlinks/impact — were silently hidden.
    let alternates = alternate_anchors(
        symbol,
        &anchor,
        graph.anchor_to_index.keys().map(|s| s.as_str()),
    );

    // Definition location from the node's attributes.
    let def = {
        let node = &graph.graph[idx];
        let attrs = &node.doc.attributes;
        json!({
            "anchor": anchor,
            "node_type": attrs.get("node-type").cloned()
                .unwrap_or_else(|| format!("{:?}", node.doc.node_type)),
            "file": attrs.get("source_file").cloned().unwrap_or_default(),
            "start_line": attrs.get("start_line").cloned().unwrap_or_default(),
            "end_line": attrs.get("end_line").cloned().unwrap_or_default(),
        })
    };

    // Step 2: backlinks — incoming references (mirrors `query --backlinks`).
    let backlinks = collect_unique_backlinks(&graph, &anchor);

    // Step 3: downstream impact — outgoing reach over impact edge types
    // (mirrors `query --impact`). Uses the one shared SET: this local copy had
    // silently drifted (it was missing Implements/Mutates, so understand's
    // impact view truncated at trait boundaries that `query --impact` crossed).
    let impact_types = crate::util::impact_edge_types();
    let impact: Vec<serde_json::Value> = impact_reachable(&graph, idx, &impact_types)
        .into_iter()
        .map(|(n, d)| node_to_json(&graph.graph[n], d))
        .collect();

    // Step 4: assemble a context block from the anchor within budget, via the
    // same neighborhood-stream + assemble path `asm` uses.
    let edge_types: Vec<aden_core::EdgeType> = Vec::new();
    let neigh = aden_graph::cache::build_neighborhood_cached(path, &anchor, 3, &edge_types)?;
    let asm_opts = AssemblyOptions {
        start_anchor: anchor.clone(),
        max_depth: 3,
        token_budget: budget,
        edge_types,
        block_filter: Vec::new(),
        include_tags: Vec::new(),
        exclude_tags: Vec::new(),
        attributes: Vec::new(),
        llm_mode: true,
        hydrate_root: None,
        relevance: None,
        relevance_select: false,
        relevance_confidence: None,
    };
    let context = assemble(&neigh, &asm_opts)?;

    if json {
        let env = super::augment_read_json(
            path,
            json!({
                "symbol": symbol,
                "anchor": anchor,
                "alternates": alternates,
                "definition": def,
                "backlinks": backlinks,
                "impact": impact,
                "context": context,
            }),
        );
        let body = serde_json::to_string_pretty(&env)?;
        println!("{body}");
        return Ok(());
    }

    // Human report.
    println!("# Understanding '{}'", symbol);
    println!();
    println!("## Definition");
    let file = def["file"].as_str().unwrap_or("");
    let line = def["start_line"].as_str().unwrap_or("");
    let nt = def["node_type"].as_str().unwrap_or("");
    if file.is_empty() {
        println!("  {} [{}]", nt, anchor);
    } else {
        println!("  {} {} ({}:{})", anchor, nt, file, line);
    }
    println!();

    if !alternates.is_empty() {
        println!(
            "## Other definitions ({}) — '{}' is defined in more than one place",
            alternates.len(),
            symbol
        );
        for a in &alternates {
            println!("  {}", a);
        }
        println!("  (showing the first above; re-run with a fuller anchor to inspect another)");
        println!();
    }

    println!("## Backlinks ({} reference(s))", backlinks.len());
    if backlinks.is_empty() {
        println!("  (none — unused, an entry point, or invoked via dynamic dispatch)");
    } else {
        for b in &backlinks {
            println!("  {}", b["anchor"].as_str().unwrap_or(""));
        }
    }
    println!();

    println!("## Downstream impact ({} node(s))", impact.len());
    if impact.is_empty() {
        println!("  (none)");
    } else {
        for i in &impact {
            println!("  [{}] {}", i["depth"], i["anchor"].as_str().unwrap_or(""));
        }
    }
    println!();

    println!("## Context (budget {} tokens)", budget);
    println!();
    println!("{}", context);
    Ok(())
}

fn print_locate_results(hits: &[serde_json::Value], format: &str, context: Option<usize>) {
    if format == "json" {
        println!(
            "{}",
            serde_json::to_string_pretty(&hits).unwrap_or_default()
        );
        return;
    }
    let ctx = context.unwrap_or(0);
    for h in hits {
        let file = h["file"].as_str().unwrap_or("");
        let start = h["start_line"].as_str().unwrap_or("");
        let end = h["end_line"].as_str().unwrap_or("");
        let anchor = h["anchor"].as_str().unwrap_or("");
        let nt = h["node_type"].as_str().unwrap_or("");

        // Extract symbol name from anchor for brevity
        let symbol = anchor.split('#').next_back().unwrap_or(anchor);

        if file.is_empty() || start.is_empty() {
            println!("{} {} [{}]", symbol, nt, anchor);
        } else {
            println!("{} {} {}:{}", symbol, nt, file, start);
        }

        // Show context if requested
        if ctx > 0
            && !file.is_empty()
            && let Ok(lines) = std::fs::read_to_string(file)
        {
            let start_num: usize = start.parse().unwrap_or(1);
            let end_num: usize = end.parse().unwrap_or(start_num);
            let before = start_num.saturating_sub(ctx);
            let after = end_num + ctx;
            let all_lines: Vec<&str> = lines.lines().collect();
            if before < all_lines.len() && before < after {
                println!(
                    "  Context (lines {}-{}):",
                    before + 1,
                    after.min(all_lines.len())
                );
                for (i, line) in all_lines.iter().enumerate().take(after).skip(before) {
                    let line_num = i + 1;
                    let marker = if line_num >= start_num && line_num <= end_num {
                        ">"
                    } else {
                        " "
                    };
                    println!("{}{:4}: {}", marker, line_num, line);
                }
            }
        }
    }
    // Self-document the discovery→assembly loop: the symbol shown is exactly the
    // anchor `asm`/`understand` take, so the agent can pivot from a locate hit
    // straight to full context without a second lookup.
    if let Some(first) = hits.first() {
        let anchor = first["anchor"].as_str().unwrap_or("");
        let symbol = anchor.split('#').next_back().unwrap_or(anchor);
        if !symbol.is_empty() {
            println!(
                "  ↳ expand into full context: `asm --from {symbol}` (or `understand {symbol}`)"
            );
        }
    }
}

pub fn cmd_locate(
    path: &Path,
    symbol: Option<&str>,
    caller_of: Option<&str>,
    format: &str,
    limit: usize,
    context: Option<usize>,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use serde_json::json;

    if !path.is_dir() {
        // The positional argument is the project DIR (default "."); the symbol
        // goes in `--symbol`. A user who typed `aden locate myFn` lands here with
        // a non-directory positional and no flag — point them at the right form
        // instead of the bare "requires a directory path".
        if symbol.is_none() && caller_of.is_none() {
            return Err(format!(
                "'{}' is not a directory. To find a symbol use:\n  \
                 aden locate --symbol {} [DIR]\n  \
                 aden locate --caller-of {} [DIR]",
                path.display(),
                path.display(),
                path.display(),
            )
            .into());
        }
        return Err(format!("locate: '{}' is not a directory", path.display()).into());
    }
    let want_json = json || format == "json";
    let _stale_hint = super::StaleHintGuard::new(path, want_json);
    super::ensure_fresh(path);

    // JSON is requested via either the global `-j/--json` flag or `--format json`.
    // In JSON mode every human header ("Found N match(es)…") is suppressed so the
    // stream is a single machine-parseable value, never JSON prefixed by prose.

    // If --symbol is given, find the definition.
    if let Some(sym) = symbol {
        // Match against anchor *keys* in the store and deserialize only the
        // documents that match. Building the full petgraph here is what made
        // `locate` take ~47s on the kernel (1.2M nodes); this is bounded by the
        // number of matches.
        // Prefer snapshot for lock-free reads (ADR-011).
        let docs: std::collections::HashMap<String, aden_core::Document> =
            if let Some((docs, _)) = aden_graph::snapshot::try_read_fresh(path) {
                docs
            } else {
                let (store_path, _) = aden_paths::resolve_read_store(path);
                let storage = aden_store::Storage::open_existing(
                    store_path.to_str().ok_or("invalid store path")?,
                )
                .map_err(|e| format!("failed to open store: {}", e))?;
                storage.get_all_documents().unwrap_or_default()
            };
        let all_anchors: Vec<String> = docs.keys().cloned().collect();

        let mut matched: Vec<&String> = all_anchors
            .iter()
            .filter(|a| anchor_match_rank(a, sym) != u8::MAX)
            .collect();
        // Precision: surface the exact symbol definition (and its members) before
        // incidental substring hits (doc headings, `OtherGroup`, code blocks).
        // Shared with `understand`'s resolver via `anchor_match_rank`, which also
        // ranks a method-name match (`Scaffold.errorhandler` for `errorhandler`)
        // above an unrelated substring superset (`Blueprint.app_errorhandler`).
        matched.sort_by(|a, b| {
            anchor_match_rank(a, sym)
                .cmp(&anchor_match_rank(b, sym))
                .then_with(|| a.cmp(b))
        });

        let hits: Vec<serde_json::Value> = matched
            .iter()
            .take(limit)
            .filter_map(|a| {
                let doc = docs.get(*a)?;
                let attrs = &doc.attributes;
                Some(json!({
                    "anchor": a,
                    "node_type": attrs.get("node-type").cloned()
                        .unwrap_or_else(|| format!("{:?}", doc.node_type)),
                    "file": attrs.get("source_file").cloned().unwrap_or_default(),
                    "start_line": attrs.get("start_line").cloned().unwrap_or_default(),
                    "end_line": attrs.get("end_line").cloned().unwrap_or_default(),
                }))
            })
            .collect();

        if hits.is_empty() {
            // Fall back to the full-text search index.
            let index = load_or_build_index(path)?;
            let search_results = query_index(&index, sym);
            if want_json {
                // Machine-readable: emit the (possibly empty) full-text hits as a
                // JSON array, never the human "Found … / No symbol found" prose.
                let arr: Vec<serde_json::Value> = search_results
                    .iter()
                    .take(limit)
                    .map(|r| json!({ "anchor": r.anchor, "score": r.score, "snippet": r.snippet }))
                    .collect();
                let env = super::augment_read_json(path, serde_json::Value::Array(arr));
                println!("{}", serde_json::to_string_pretty(&env)?);
                return Ok(());
            }
            if !search_results.is_empty() {
                println!(
                    "Found {} match(es) in full-text index for '{}':",
                    search_results.len(),
                    sym
                );
                println!("| Anchor | Score | Snippet |");
                println!("|=== |");
                for r in search_results.iter().take(limit) {
                    let snippet = if r.snippet.len() > 60 {
                        format!("{}...", &r.snippet[..60])
                    } else {
                        r.snippet.clone()
                    };
                    println!("| {} | {} | {} |", r.anchor, fmt_score(r.score), snippet);
                }
                return Ok(());
            }
            println!("No symbol found matching '{}'", sym);
            println!(
                "Hint: Try 'aden search \"{}\"' to find related anchors",
                sym
            );
            return Ok(());
        }

        if want_json {
            let env = super::augment_read_json(path, serde_json::Value::Array(hits));
            println!("{}", serde_json::to_string_pretty(&env)?);
            return Ok(());
        }
        println!("Found {} match(es) for '{}':", matched.len(), sym);
        print_locate_results(&hits, format, context);
        return Ok(());
    }

    // If --caller-of is given, list callers via incoming `Calls` edges in the
    // knowledge graph. This is the reverse of `query --backlinks`, filtered to
    // call edges, with each caller enriched by its source file + line from the
    // store. The call graph is already populated by `gen` (link_store_edges),
    // so no new metadata is required — earlier this branch was a stub.
    if let Some(target) = caller_of {
        use serde_json::json;

        let graph = aden_graph::cache::build_from_directory_cached(path)?;

        // A bare symbol (e.g. `assemble`) may resolve to several anchors across
        // modules; union the callers of every matching definition.
        let tl = target.to_lowercase();
        let targets: Vec<_> = graph
            .graph
            .node_indices()
            .filter(|&i| {
                let al = graph.graph[i].doc.anchor.to_lowercase();
                al.ends_with(&tl) || al.contains(&format!("#{}", tl))
            })
            .collect();

        if targets.is_empty() {
            if want_json {
                let env = super::augment_read_json(path, serde_json::json!([]));
                println!("{}", serde_json::to_string_pretty(&env)?);
                return Ok(());
            }
            println!("No symbol found matching '{}'", target);
            println!(
                "Hint: Try 'aden locate . --symbol {}' to confirm it is indexed.",
                target
            );
            return Ok(());
        }

        // The matched definitions themselves are never their own callers. A bare
        // name matches loosely (`#fold_overlay` also matches `#fold_overlay_blocks`),
        // so a target that legitimately calls a sibling target would otherwise be
        // reported as a self-caller on its own definition line — exclude them.
        let target_anchors: HashSet<String> = targets
            .iter()
            .map(|&i| graph.graph[i].doc.anchor.clone())
            .collect();

        // Collect unique callers via incoming `Calls` edges.
        let mut seen = HashSet::new();
        let mut callers: Vec<String> = Vec::new();
        for &t in &targets {
            for neighbor in graph.graph.neighbors_directed(t, Direction::Incoming) {
                let is_call = graph
                    .graph
                    .find_edge(neighbor, t)
                    .and_then(|e| graph.graph.edge_weight(e))
                    .map(|e| e.edge_type == aden_core::EdgeType::Calls)
                    .unwrap_or(false);
                if is_call {
                    let a = graph.graph[neighbor].doc.anchor.clone();
                    if !target_anchors.contains(&a) && seen.insert(a.clone()) {
                        callers.push(a);
                    }
                }
            }
        }
        callers.sort();

        if callers.is_empty() {
            if want_json {
                let env = super::augment_read_json(path, serde_json::json!([]));
                println!("{}", serde_json::to_string_pretty(&env)?);
                return Ok(());
            }
            println!(
                "No callers found for '{}' (unused, an entry point, or invoked via dynamic dispatch).",
                target
            );
            return Ok(());
        }

        // Enrich each caller with file:line (best-effort). Prefer snapshot.
        let docs = aden_graph::snapshot::try_read_fresh(path).map(|(d, _)| d);
        let (store_path, _) = aden_paths::resolve_read_store(path);
        let storage =
            aden_store::Storage::open_existing(store_path.to_str().ok_or("invalid store path")?)
                .ok();
        let hits: Vec<serde_json::Value> = callers
            .iter()
            .take(limit)
            .map(|a| {
                let doc = docs.as_ref().and_then(|d| d.get(a).cloned()).or_else(|| {
                    storage
                        .as_ref()
                        .and_then(|s| s.get_document(a).ok().flatten())
                });
                let (file, line) = doc
                    .map(|d| {
                        (
                            d.attributes.get("source_file").cloned().unwrap_or_default(),
                            d.attributes.get("start_line").cloned().unwrap_or_default(),
                        )
                    })
                    .unwrap_or_default();
                json!({ "anchor": a, "file": file, "start_line": line })
            })
            .collect();

        if want_json {
            let env = super::augment_read_json(path, serde_json::Value::Array(hits));
            println!("{}", serde_json::to_string_pretty(&env)?);
            return Ok(());
        }
        println!("Found {} caller(s) of '{}':", hits.len(), target);
        for h in &hits {
            let file = h["file"].as_str().unwrap_or("");
            let line = h["start_line"].as_str().unwrap_or("");
            let loc = if file.is_empty() {
                String::new()
            } else {
                format!("  ({}:{})", file, line)
            };
            println!("  {}{}", h["anchor"].as_str().unwrap_or(""), loc);
        }
        return Ok(());
    }

    Err("locate requires one of --symbol or --caller-of".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ---- understand: backlink listing dedups parallel-edge referencers.

    fn backlink_fixture_node(anchor: &str) -> aden_graph::DocumentNode {
        aden_graph::DocumentNode {
            doc: aden_core::Document {
                anchor: anchor.to_string(),
                node_type: aden_core::NodeType::Function,
                attributes: std::collections::HashMap::new(),
                blocks: Vec::new(),
                source_span: None,
                metadata: None,
                confidence: 0.9,
            },
            parsed: None,
            source_path: PathBuf::from(format!("{anchor}.adoc")),
        }
    }

    /// Regression: `understand` printed the same backlink once per parallel
    /// edge (observed: `mod-aden-mcp` listed twice). A referencer connected by
    /// several edge types (Contains + Calls + Uses) must appear exactly once,
    /// and distinct referencers must all survive the dedup.
    #[test]
    fn understand_backlinks_dedup_parallel_edges() {
        let mut g = aden_graph::AdenGraph::<aden_graph::DocumentNode, aden_graph::AdenEdge>::new();
        let target = g
            .add_node(backlink_fixture_node("target"))
            .expect("unique fixture anchor");
        let module = g
            .add_node(backlink_fixture_node("mod-caller"))
            .expect("unique fixture anchor");
        let other = g
            .add_node(backlink_fixture_node("other-caller"))
            .expect("unique fixture anchor");
        // Use raw petgraph add_edge: AdenGraph::add_edge skips duplicates, but
        // real builds create parallel edges of different types directly.
        for et in [
            aden_core::EdgeType::Contains,
            aden_core::EdgeType::Calls,
            aden_core::EdgeType::Uses,
        ] {
            g.graph
                .add_edge(module, target, aden_graph::AdenEdge { edge_type: et });
        }
        g.graph.add_edge(
            other,
            target,
            aden_graph::AdenEdge {
                edge_type: aden_core::EdgeType::Calls,
            },
        );

        let backlinks = collect_unique_backlinks(&g, "target");
        let anchors: Vec<&str> = backlinks
            .iter()
            .map(|b| b["anchor"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(
            anchors.len(),
            2,
            "each referencer must appear exactly once, got {anchors:?}"
        );
        assert!(anchors.contains(&"mod-caller"), "got {anchors:?}");
        assert!(anchors.contains(&"other-caller"), "got {anchors:?}");
    }

    /// `understand`'s downstream impact must keep a neighbor reachable via a
    /// `Calls` edge even when a NON-impact edge (`Documents`/`Contains`) runs in
    /// parallel between the same pair. The old `find_edge` could return the
    /// non-impact edge and silently drop the neighbor; `impact_reachable` checks
    /// all parallel edges. New method-call edges make this collision more common.
    #[test]
    fn understand_impact_keeps_neighbor_with_parallel_non_impact_edge() {
        let mut g = aden_graph::AdenGraph::<aden_graph::DocumentNode, aden_graph::AdenEdge>::new();
        let caller = g
            .add_node(backlink_fixture_node("caller"))
            .expect("unique fixture anchor");
        let callee = g
            .add_node(backlink_fixture_node("callee"))
            .expect("unique fixture anchor");
        // Non-impact edge added FIRST, then the real Calls edge.
        for et in [aden_core::EdgeType::Documents, aden_core::EdgeType::Calls] {
            g.graph
                .add_edge(caller, callee, aden_graph::AdenEdge { edge_type: et });
        }
        let impact_types = crate::util::impact_edge_types();
        let reached: Vec<String> = super::impact_reachable(&g, caller, &impact_types)
            .into_iter()
            .map(|(n, _)| g.graph[n].doc.anchor.clone())
            .collect();
        assert!(
            reached.contains(&"callee".to_string()),
            "impact must include the callee reachable via a parallel Calls edge; got: {reached:?}"
        );
    }

    /// An anchor missing from the graph yields no backlinks (and no panic).
    #[test]
    fn understand_backlinks_unknown_anchor_is_empty() {
        let g = aden_graph::AdenGraph::<aden_graph::DocumentNode, aden_graph::AdenEdge>::new();
        assert!(collect_unique_backlinks(&g, "nope").is_empty());
    }

    // ---- understand: symbol -> anchor resolution.

    /// An exact `#symbol` suffix wins over a looser tail match, so `parse`
    /// resolves to `…#parse` and never to `…#reparse`.
    #[test]
    fn understand_picks_exact_symbol_suffix() {
        let anchors = vec![
            "src/a.rs#reparse".to_string(),
            "src/b.rs#parse".to_string(),
            "src/c.rs#parser".to_string(),
        ];
        assert_eq!(
            super::pick_symbol_anchor("parse", &anchors),
            Some("src/b.rs#parse".to_string())
        );
    }

    /// A bare method name must resolve to the symbol whose LAST component matches
    /// exactly (`Scaffold.errorhandler`), not a substring superset that merely ends
    /// with the query (`Blueprint.app_errorhandler`). Regression for the external
    /// blast-radius eval, where `understand errorhandler` mis-resolved.
    #[test]
    fn understand_prefers_exact_method_name_over_substring_superset() {
        let anchors = vec![
            "src/blueprints.rs#Blueprint.app_errorhandler".to_string(),
            "src/scaffold.rs#Scaffold.errorhandler".to_string(),
        ];
        assert_eq!(
            super::pick_symbol_anchor("errorhandler", &anchors),
            Some("src/scaffold.rs#Scaffold.errorhandler".to_string())
        );
        // And a whole-fragment exact match still beats a method-name match.
        let anchors2 = vec![
            "src/a.rs#Scaffold.errorhandler".to_string(),
            "src/b.rs#errorhandler".to_string(),
        ];
        assert_eq!(
            super::pick_symbol_anchor("errorhandler", &anchors2),
            Some("src/b.rs#errorhandler".to_string())
        );
    }

    /// Case-insensitive match, and an unknown symbol yields None so the caller
    /// can emit the "run aden list" hint.
    #[test]
    fn understand_resolution_is_case_insensitive_and_missing_is_none() {
        let anchors = vec!["crates/x.rs#AssembleContext".to_string()];
        assert_eq!(
            super::pick_symbol_anchor("assemblecontext", &anchors),
            Some("crates/x.rs#AssembleContext".to_string())
        );
        assert_eq!(super::pick_symbol_anchor("nope_not_here", &anchors), None);
    }

    #[test]
    fn understand_resolves_qualified_generic_shorthand_without_whitespace_sensitivity() {
        let anchors = vec!["src/graph.rs#AdenGraph<N, E>::bfs".to_string()];
        for spelling in ["AdenGraph::bfs", "AdenGraph <N,E> :: bfs", "adengraph::bfs"] {
            assert_eq!(
                super::pick_symbol_anchor(spelling, &anchors),
                Some("src/graph.rs#AdenGraph<N, E>::bfs".to_string()),
                "{spelling}"
            );
        }
    }

    #[test]
    fn equally_ranked_natural_symbols_remain_explicitly_ambiguous() {
        let anchors = vec![
            "src/a.rs#AdenGraph<N, E>::bfs".to_string(),
            "src/b.rs#AdenGraph<T, U>::bfs".to_string(),
        ];
        assert_eq!(
            super::ranked_symbol_candidates("AdenGraph::bfs", &anchors),
            anchors
        );
    }

    #[test]
    fn understand_alternates_surface_duplicate_symbol_definitions() {
        let anchors = [
            "aden://module/aden-cli/src/util.rs#is_expected_metadata",
            "aden://module/aden-heal/src/drift.rs#is_expected_metadata",
            "aden://module/aden-cli/src/util.rs#classify_orphans",
        ];
        let chosen = "aden://module/aden-cli/src/util.rs#is_expected_metadata";
        let alts =
            super::alternate_anchors("is_expected_metadata", chosen, anchors.iter().copied());
        assert_eq!(
            alts,
            vec!["aden://module/aden-heal/src/drift.rs#is_expected_metadata".to_string()]
        );
    }

    #[test]
    fn understand_alternates_empty_for_unique_symbol_and_case_insensitive() {
        let anchors = ["crates/x.rs#AssembleContext", "crates/y.rs#other"];
        assert!(
            super::alternate_anchors(
                "assemblecontext",
                "crates/z.rs#nope",
                anchors.iter().copied()
            )
            .contains(&"crates/x.rs#AssembleContext".to_string())
        );
        assert_eq!(
            super::alternate_anchors(
                "AssembleContext",
                "crates/x.rs#AssembleContext",
                anchors.iter().copied()
            ),
            Vec::<String>::new()
        );
    }
}
