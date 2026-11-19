// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use aden_core::source::{STORED_SOURCE_MAX_BYTES, StoredSourceReader};
use aden_core::{Block, EdgeType};
use aden_graph::{AdenEdge, AdenGraph, DocumentNode};
use petgraph::Direction;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

/// Traversal priority of an edge type (lower = more structurally load-bearing,
/// hence visited first within the budget). On a high-fan-out "god object" the
/// budget would otherwise be spent on whatever petgraph happened to yield first;
/// ordering by importance keeps the assembled context focused and deterministic.
fn edge_priority(et: &EdgeType) -> u8 {
    match et {
        EdgeType::Calls => 0,
        EdgeType::Invokes => 1,
        EdgeType::Implements => 2,
        EdgeType::IsA => 3,
        EdgeType::PartOf => 4,
        EdgeType::Uses => 5,
        EdgeType::Requires => 6,
        EdgeType::Mutates => 7,
        EdgeType::Constrains => 8,
        EdgeType::Tests => 9,
        EdgeType::Verifies => 10,
        EdgeType::Documents => 11,
        // Containment ranks exactly where these edges ranked as `Documents` before
        // the split, so top-down module→symbol traversal order is unchanged.
        EdgeType::Contains => 11,
        // A listing that exercises the symbol is as valuable as its docs.
        EdgeType::Demonstrates => 11,
        _ => 12, // everything else (incl. Mentions — a hint, not a contract)
    }
}

/// Collect the eligible outgoing neighbors of `node` (those whose edge passes the
/// `edge_types` filter) and sort them deterministically by (edge priority, then
/// target anchor). Shared by `assemble` and `assemble_adg` so both spend the
/// budget on the same, structurally-ranked frontier regardless of petgraph's
/// arbitrary neighbor iteration order.
fn ordered_neighbors(
    graph: &AdenGraph<DocumentNode, AdenEdge>,
    node: NodeIndex,
    edge_types: &[EdgeType],
    relevance: Option<&HashMap<String, f32>>,
) -> Vec<NodeIndex> {
    // The graph is a petgraph multigraph: a single (node -> target) pair can
    // carry several parallel edges of *different* types (e.g. Calls AND
    // Documents). Iterate the edges (not neighbors_directed + find_edge, which
    // yields the target once per parallel edge and resolves to an arbitrary,
    // last-inserted edge) and keep the BEST (minimum) priority across all of a
    // target's parallel edges that pass the filter. This both picks the right
    // edge type per target and deduplicates the neighbor to a single visit.
    let mut best: HashMap<NodeIndex, u8> = HashMap::new();
    for edge in graph.graph.edges_directed(node, Direction::Outgoing) {
        let edge_type = &edge.weight().edge_type;
        if edge_types.is_empty() || edge_types.contains(edge_type) {
            let prio = edge_priority(edge_type);
            best.entry(edge.target())
                .and_modify(|p| {
                    if prio < *p {
                        *p = prio;
                    }
                })
                .or_insert(prio);
        }
    }
    let mut neighbors: Vec<(u8, f32, &str, NodeIndex)> = best
        .into_iter()
        .filter(|(target, _)| {
            // Synthetic crate hubs (`mod-aden-cli`) exist so `asm --from
            // mod-foo` can open a module. Following PartOf/Contains *to* them
            // from a symbol climbs the hub and then fans out to every sibling —
            // the context bomb ISSUES.md already called out for ask.
            !graph.graph[*target].doc.anchor.starts_with("mod-")
        })
        .map(|(target, prio)| {
            let anchor = graph.graph[target].doc.anchor.as_str();
            // 0.0 when there is no relevance map or no entry for this target — so
            // the absent path reduces EXACTLY to the old (edge_priority, anchor)
            // order, leaving `asm` and every current caller unchanged.
            let rel = relevance
                .and_then(|m| m.get(anchor))
                .copied()
                .unwrap_or(0.0);
            (prio, rel, anchor, target)
        })
        .collect();
    // edge_priority ascending, then relevance DESCENDING, then anchor ascending.
    // Anchor stays the final tiebreaker so the order is fully deterministic even
    // under relevance ties — and identical to before when relevance is uniformly 0.
    neighbors.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| a.2.cmp(b.2))
    });
    neighbors.into_iter().map(|(_, _, _, n)| n).collect()
}

/// Lowercased alphanumeric token set of `text` (tokens of length > 1), the unit
/// of comparison for MMR lexical redundancy.
fn token_set(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 1)
        .map(str::to_ascii_lowercase)
        .collect()
}

/// Jaccard overlap |A∩B| / |A∪B| of two token sets, in `[0, 1]`. 0 when either
/// is empty, so a blockless/empty node never reads as redundant.
fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.iter().filter(|t| b.contains(*t)).count();
    inter as f32 / (a.len() + b.len() - inter) as f32
}

/// Which block types to include when assembling a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockKind {
    Table,
    Paragraph,
    Listing,
    Admonition,
    DescriptionList,
    Checklist,
}

/// Options for assembling a context prompt.
#[derive(Debug, Clone)]
pub struct AssemblyOptions {
    pub start_anchor: String,
    pub max_depth: usize,
    /// Token budget (approximate byte-pair estimation).
    pub token_budget: usize,
    /// Edge types to follow. If empty, follow all.
    pub edge_types: Vec<EdgeType>,
    /// Block types to include when emitting documents.
    /// If empty, include all blocks.
    pub block_filter: Vec<BlockKind>,
    /// Tags to include. Only content in these tagged regions will be included.
    /// If empty, all content is included (unless exclude_tags is set).
    pub include_tags: Vec<String>,
    /// Tags to exclude. Content in these tagged regions will be excluded.
    pub exclude_tags: Vec<String>,
    /// Attributes to set for conditional processing.
    /// If set, ifdef/ifndef blocks will be filtered accordingly.
    pub attributes: Vec<String>,
    /// Strip AsciiDoc markup syntax and emit clean prose for LLM consumption.
    /// Default: true — LLM-dense output is the baseline. Pass false only when
    /// raw AsciiDoc is explicitly needed (e.g. IDE rendering, --format aden).
    pub llm_mode: bool,
    /// Project root for budget-aware source-span hydration. When set and the
    /// assembled neighborhood leaves most of the budget unspent, included
    /// nodes are hydrated with their actual source spans (`source_file` +
    /// `start_line`/`end_line` node attributes), nearest-first in visit order
    /// with the seed first, each span re-verified against `source_hash` so a
    /// stale store degrades to summary-only rendering instead of shipping
    /// stale source. `None` (the default) disables hydration entirely.
    pub hydrate_root: Option<std::path::PathBuf>,
    /// Optional per-anchor query-relevance scores (anchor → score). When set,
    /// `ordered_neighbors` breaks structural-priority ties toward more
    /// query-relevant neighbors, so a tight budget is spent on the parts of a
    /// high-fan-out neighborhood that actually match the question. `None` (the
    /// default) preserves the pure structural `(edge_priority, anchor)` order:
    /// `asm <anchor>` has no query, so it never sets this; only `ask` would,
    /// and only once an assembly-quality eval validates it.
    pub relevance: Option<HashMap<String, f32>>,
    /// Gather-then-select assembly. Instead of stopping the priority-ordered walk
    /// when the budget runs out, gather the reachable neighborhood (bounded) and
    /// then SELECT down to the budget by relevance. This reaches the answer even
    /// when it sits behind low-priority `Contains` members (a class's methods rank
    /// behind its Calls/Uses neighbors), then compresses. The promotion strength is
    /// DERIVED from the relevance distribution's own confidence (a clear winner
    /// crosses structural-priority buckets; a flat distribution defers to
    /// structure), so no weight is hardcoded. Requires `relevance` to be set;
    /// `false` (the default) keeps the original in-budget priority walk byte-for-byte.
    pub relevance_select: bool,
    /// Cross-query CALIBRATED confidence that the query has a genuinely good match
    /// at all (∈[0,1]), supplied by the caller because only it knows the signal
    /// type and has the embedder. This is the off-topic SAFETY gate for
    /// gather-then-select: the within-neighborhood `relevance` distribution cannot
    /// tell an on-topic query (real answer present) from an off-topic one
    /// (best-of-noise), because every scale-free measure (separation, z-score,
    /// central tendency) reads best-of-noise as a confident peak. The only
    /// discriminator is the ABSOLUTE magnitude of the best match — for dense that
    /// is the raw cosine, which RRF discards — and absolute magnitude only means
    /// something once calibrated to the embedder's own semantic band. So the caller
    /// maps the best raw cosine through that band to a [0,1] confidence and passes
    /// it here; the gate multiplies its derived promotion strength by it, so an
    /// off-topic query (low confidence) defers to structure instead of churning the
    /// bundle on noise. `None` (the default) means "no calibration available"
    /// (e.g. BM25-only, no embedder) and leaves promotion at full strength — the
    /// prior behavior, byte-for-byte.
    pub relevance_confidence: Option<f32>,
}

impl Default for AssemblyOptions {
    fn default() -> Self {
        Self {
            start_anchor: String::new(),
            max_depth: 2,
            token_budget: 4096,
            edge_types: Vec::new(),
            block_filter: Vec::new(),
            include_tags: Vec::new(),
            exclude_tags: Vec::new(),
            attributes: Vec::new(),
            llm_mode: true, // LLM-dense by default everywhere
            hydrate_root: None,
            relevance: None,
            relevance_select: false,
            relevance_confidence: None,
        }
    }
}

/// Gather-then-select assembly (see `AssemblyOptions::relevance_select`).
///
/// Phase 1 GATHERS the reachable neighborhood (BFS to `max_depth`) up to a
/// generous multiple of the output budget, recording each node's text, token
/// cost, BFS (structural) order, and relevance. Phase 2 SELECTS down to the
/// output budget: the seed always leads, the rest are ordered by a value that
/// blends normalized relevance with structural proximity, weighted by the
/// relevance distribution's own CONFIDENCE. A peaked distribution (a clear
/// winner) lets relevance lead and cross structural-priority buckets; a flat one
/// defers to structural order. The blend weight is derived from the signal, not
/// hardcoded. Returns (included nodes, rendered texts, total tokens) for the
/// caller's shared hydration + join.
fn gather_then_select(
    graph: &AdenGraph<DocumentNode, AdenEdge>,
    opts: &AssemblyOptions,
    start: NodeIndex,
    sep_cost: usize,
) -> (Vec<NodeIndex>, Vec<String>, usize) {
    let Some(rel) = opts.relevance.as_ref() else {
        return (Vec::new(), Vec::new(), 0);
    };

    // Phase 1: gather. Bounded so a high-degree hub cannot blow up: stop at a
    // generous multiple of the output budget (we only need to reach well past it).
    const GATHER_FACTOR: usize = 16;
    const MAX_GATHER_NODES: usize = 10_000;
    let gather_cap = opts.token_budget.saturating_mul(GATHER_FACTOR).max(8192);
    let mut visited = std::collections::HashSet::new();
    let mut queue = VecDeque::new();
    let mut cands: Vec<(NodeIndex, String, usize, f32)> = Vec::new();
    let mut gathered_tokens = 0usize;
    queue.push_back((start, 0usize));
    while let Some((node, depth)) = queue.pop_front() {
        if cands.len() >= MAX_GATHER_NODES || gathered_tokens >= gather_cap {
            break;
        }
        if !visited.insert(node) || depth > opts.max_depth {
            continue;
        }
        let doc = &graph.graph[node];
        let raw = document_to_text(
            doc,
            &opts.block_filter,
            &opts.include_tags,
            &opts.exclude_tags,
            &opts.attributes,
        );
        let text = if opts.llm_mode {
            strip_asciidoc_markup(&raw)
        } else {
            raw
        };
        let tokens = estimate_tokens(&text);
        let score = rel.get(doc.doc.anchor.as_str()).copied().unwrap_or(0.0);
        gathered_tokens += tokens;
        cands.push((node, text, tokens, score));
        for neighbor in ordered_neighbors(graph, node, &opts.edge_types, opts.relevance.as_ref()) {
            queue.push_back((neighbor, depth + 1));
        }
    }
    if cands.is_empty() {
        return (Vec::new(), Vec::new(), 0);
    }

    // Phase 2: GLOBAL-normalized selection. Relevance is scaled against the best match
    // ANYWHERE for this query (the max of the whole relevance map), not the local
    // gathered set. This is the fix for the sparse-zero bug: an off-topic neighborhood
    // whose nodes barely match the query (its real matches are elsewhere) stays low, so
    // promotion does not fire and structure is preserved; a neighborhood that actually
    // contains the query's top match promotes it. The blend weight (confidence) is the
    // strongest in-neighborhood match as a fraction of that global best, so it too is
    // derived from the signal, with no hardcoded threshold.
    let scores: Vec<f32> = cands.iter().map(|c| c.3).collect();
    let global_max = rel.values().copied().fold(0.0f32, f32::max);
    // Confidence = top-1 SEPARATION among the gathered matches: how far the best
    // match stands above the second best. A real answer has a clear leader (big gap);
    // best-of-noise is a flat field of similar weak scores (small gap), which gates
    // promotion off. Scale-free and derived, not fooled by a sea of zeros (which a
    // central-tendency measure was) nor by a weak global best inside the neighborhood
    // (which a max/global ratio was).
    let mut desc = scores.clone();
    desc.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let top1 = desc.first().copied().unwrap_or(0.0);
    let top2 = desc.get(1).copied().unwrap_or(0.0);
    let separation = if top1 > 0.0 {
        ((top1 - top2) / top1).clamp(0.0, 1.0)
    } else {
        0.0
    };
    // Off-topic SAFETY gate. `separation` is scale-free, so it reads best-of-noise as a
    // confident peak just as it reads a real answer (validated: off-topic queries score
    // separation/z just as high as on-topic ones). The only signal that tells the two
    // apart is the caller's CROSS-QUERY calibrated confidence — the absolute quality of
    // the best match, mapped through the embedder's semantic band. Multiply them: an
    // off-topic query (low calibrated confidence) collapses the gate toward structure
    // even when its within-neighborhood separation is high, so the bundle is preserved
    // instead of churned on noise. Absent (BM25-only, no embedder) it is 1.0 — full
    // strength, the prior behavior.
    let conf = separation * opts.relevance_confidence.unwrap_or(1.0).clamp(0.0, 1.0);
    let n = cands.len() as f32;
    let value = |idx: usize, score: f32| -> f32 {
        let rel_frac = if global_max > 0.0 {
            (score / global_max).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let struct_val = 1.0 - (idx as f32 / n); // earlier BFS index = closer = higher
        conf * rel_frac + (1.0 - conf) * struct_val
    };

    // Seed (index 0) always leads; the rest by value descending (anchor breaks ties
    // for determinism).
    let mut order: Vec<usize> = (1..cands.len()).collect();
    order.sort_by(|&a, &b| {
        value(b, cands[b].3)
            .partial_cmp(&value(a, cands[a].3))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                graph.graph[cands[a].0]
                    .doc
                    .anchor
                    .cmp(&graph.graph[cands[b].0].doc.anchor)
            })
    });

    // Greedily pack to the OUTPUT budget. Skip a node that does not fit and try the
    // next (smaller) one, so a single large doc cannot starve the rest; the seed is
    // truncated if needed so the anchor always appears. Separator accounting mirrors
    // the priority walk.
    let mut result = Vec::new();
    let mut included = Vec::new();
    let mut total = 0usize;
    let mut seen_imports = HashMap::new();
    for (pos, &idx) in std::iter::once(&0usize).chain(order.iter()).enumerate() {
        let node = cands[idx].0;
        let doc = &graph.graph[node];
        let (text, import_tables) = render_assembly_document(doc, opts, &seen_imports);
        let tokens = estimate_tokens(&text);
        let sep = if result.is_empty() { 0 } else { sep_cost };
        let remaining = opts.token_budget.saturating_sub(total);
        if tokens + sep <= remaining {
            total += tokens + sep;
            included.push(node);
            result.push(text);
            record_import_tables(&mut seen_imports, doc, import_tables);
        } else if pos == 0 && opts.llm_mode && remaining.saturating_sub(sep) > 32 {
            let truncated = truncate_to_tokens(&text, remaining.saturating_sub(sep));
            total += estimate_tokens(&truncated) + sep;
            included.push(node);
            result.push(truncated);
        }
        // otherwise skip this node and try the next (smaller) candidate
    }
    (included, result, total)
}

/// Assemble a context prompt from a graph neighborhood.
///
/// In `llm_mode` the output is stripped of AsciiDoc markup (anchors, refs,
/// attribute lines, block delimiters) so every token carries signal rather
/// than format noise. Large documents that would exceed the remaining budget
/// are truncated at a word boundary rather than skipped entirely.
/// Assemble a context prompt from a graph neighborhood, returning both the
/// assembled text and the list of included node anchors in BFS visit order.
///
/// The anchor list contains the `anchor` of each `DocumentNode` that was
/// actually emitted into the output (same nodes the traversal included). This
/// is the authoritative set for downstream work such as baseline-file
/// resolution for source-file attribution.
pub fn assemble_with_anchors(
    graph: &AdenGraph<DocumentNode, AdenEdge>,
    opts: &AssemblyOptions,
) -> Result<(String, Vec<String>), AssemblyError> {
    assemble_with_anchors_mmr(graph, opts, None)
}

/// Like [`assemble_with_anchors`], but with optional MMR-style redundancy
/// pruning. When `mmr` is `Some(tau)`, a candidate whose lexical Jaccard overlap
/// with any already-included node reaches `tau` is skipped — freeing its budget
/// for more diverse context — while its neighbors are still traversed. `None`
/// reproduces `assemble_with_anchors` byte-for-byte. Deterministic (token-set
/// Jaccard, no RNG), so it preserves aden's model-invariance.
pub fn assemble_with_anchors_mmr(
    graph: &AdenGraph<DocumentNode, AdenEdge>,
    opts: &AssemblyOptions,
    mmr: Option<f32>,
) -> Result<(String, Vec<String>), AssemblyError> {
    let start_idx = graph
        .get_index(&opts.start_anchor)
        .ok_or_else(|| AssemblyError::AnchorNotFound(opts.start_anchor.clone()))?;

    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    let mut result = Vec::new();
    // Nodes actually emitted, parallel to `result`, in visit (BFS) order —
    // the hydration pass packs source spans nearest-first along this order.
    let mut included: Vec<NodeIndex> = Vec::new();
    // Token sets of included nodes, for MMR redundancy checks (only when enabled).
    let mut included_tokens: Vec<HashSet<String>> = Vec::new();
    let mut total_tokens = 0usize;
    let mut seen_imports = HashMap::new();

    // Single-source the separator so its byte cost is counted in the budget by
    // the same value that the final join concatenates. The separators are
    // emitted *between* documents (one per doc after the first), so for any doc
    // that isn't first its preceding separator must fit alongside it; otherwise
    // the joined output overshoots the budget by sep_cost per gap.
    let separator = if opts.llm_mode {
        "\n\n---\n\n"
    } else {
        "\n<<<\n"
    };
    let sep_cost = estimate_tokens(separator);

    // Gather-then-select (gated): gather the reachable neighborhood, then compress
    // to budget by relevance, so the answer surfaces even behind low-priority
    // members. This leaves `queue` empty, so the priority walk below is skipped
    // unchanged; the default path pushes the seed and runs the walk exactly as before.
    //
    // SAFETY GATE: a calibrated confidence of 0 means the caller's embedder found no
    // good match for this query anywhere (off-topic). Selection cannot help — it can
    // only churn the bundle on noise — so we BYPASS it and fall back to the structural
    // walk, which gives graceful degradation (the structural bundle, unchanged) rather
    // than the softened-but-still-different bundle a near-zero multiplier would leave.
    // `None` (no calibration, e.g. BM25-only) is treated as full confidence, so the
    // prior behavior is byte-for-byte preserved.
    let calibrated = opts.relevance_confidence.unwrap_or(1.0);
    if opts.relevance_select && opts.relevance.is_some() && calibrated > 0.0 {
        let (sel_included, sel_result, sel_total) =
            gather_then_select(graph, opts, start_idx, sep_cost);
        included = sel_included;
        result = sel_result;
        total_tokens = sel_total;
    } else {
        queue.push_back((start_idx, 0usize));
    }
    // The walk's within-bucket relevance tie-break is gated by the SAME calibrated
    // confidence: an off-topic query (calibrated 0) reorders the frontier on noise
    // relevance just as harmfully as gather-then-select would, so below the floor the
    // walk falls back to PURE structural order. Above it (or with no calibration —
    // `None` → 1.0, the prior behavior) the query-aware tie-break stays on.
    let walk_relevance = if calibrated > 0.0 {
        opts.relevance.as_ref()
    } else {
        None
    };

    const MAX_VISITED_NODES: usize = 10_000;
    while let Some((node, depth)) = queue.pop_front() {
        if visited.len() >= MAX_VISITED_NODES {
            break; // DoS guard: hard limit on nodes processed
        }
        if visited.contains(&node) {
            continue;
        }
        if depth > opts.max_depth {
            continue;
        }
        let doc = &graph.graph[node];
        let (text, import_tables) = render_assembly_document(doc, opts, &seen_imports);

        let tokens = estimate_tokens(&text);

        // MMR redundancy skip: a near-duplicate of already-included content wastes
        // budget. Skip its inclusion (freeing the budget for more diverse context)
        // but still traverse its neighbors so downstream context isn't lost. The
        // `None` default path leaves traversal and budget arithmetic unchanged.
        let toks = mmr.is_some().then(|| token_set(&text));
        if let (Some(tau), Some(ts)) = (mmr, toks.as_ref())
            && included_tokens.iter().any(|s| jaccard(ts, s) >= tau)
        {
            visited.insert(node);
            for neighbor in
                ordered_neighbors(graph, node, &opts.edge_types, opts.relevance.as_ref())
            {
                queue.push_back((neighbor, depth + 1));
            }
            continue;
        }

        let remaining = opts.token_budget.saturating_sub(total_tokens);
        // A separator precedes this doc in the final join iff something is
        // already in `result`. Count it against the budget here so the joined
        // output (docs + separators) stays within `token_budget`.
        let sep = if result.is_empty() { 0 } else { sep_cost };

        if tokens + sep <= remaining {
            // Document (plus its preceding separator) fits entirely — include it.
            total_tokens += tokens + sep;
            visited.insert(node);
            included.push(node);
            if let Some(ts) = toks {
                included_tokens.push(ts);
            }
            result.push(text);
            record_import_tables(&mut seen_imports, doc, import_tables);
        } else if opts.llm_mode && remaining.saturating_sub(sep) > 32 {
            // LLM mode: partial inclusion — truncate so the doc PLUS its
            // preceding separator fit within the remaining budget.
            let truncated = truncate_to_tokens(&text, remaining.saturating_sub(sep));
            total_tokens += estimate_tokens(&truncated) + sep;
            visited.insert(node);
            included.push(node);
            result.push(truncated);
            break; // Budget exhausted after partial doc
        } else {
            break; // Not in LLM mode or nothing meaningful fits
        }

        // Add neighbors in deterministic, structural-priority order.
        for neighbor in ordered_neighbors(graph, node, &opts.edge_types, walk_relevance) {
            queue.push_back((neighbor, depth + 1));
        }
    }

    // Budget-aware leaf hydration: the graph stores summaries, not bodies —
    // for an undocumented symbol the only place the answer exists is the
    // source itself. When the summary-level assembly leaves most of the
    // budget unspent, pack it with the included nodes' actual source spans.
    if let Some(root) = &opts.hydrate_root {
        hydrate_with_source(
            graph,
            &included,
            root,
            opts.token_budget,
            &mut total_tokens,
            &mut result,
        );
    }

    // Collect the anchor string for each included node, in BFS visit order.
    let anchors: Vec<String> = included
        .iter()
        .map(|&idx| graph.graph[idx].doc.anchor.clone())
        .collect();

    Ok((result.join(separator), anchors))
}

pub fn assemble(
    graph: &AdenGraph<DocumentNode, AdenEdge>,
    opts: &AssemblyOptions,
) -> Result<String, AssemblyError> {
    assemble_with_anchors(graph, opts).map(|(text, _)| text)
}

/// Hydration trigger: only when the assembled summaries consumed less than
/// this share of the budget is source packing worth the duplication risk.
const HYDRATE_TRIGGER_PCT: usize = 85;
/// Per-node share of the total budget a single hydrated span may consume, so
/// one large function body cannot starve every neighbor of source context.
/// The cap is dynamic: when the remaining budget is large relative to the
/// remaining nodes, half of what remains may go to one span (a near-empty
/// assembly with one giant, on-point body should ship that body, not ration
/// it), but never less than this base share.
const HYDRATE_PER_NODE_DIVISOR: usize = 4;
/// Below this many remaining tokens a further span adds noise, not signal —
/// mirrors the main loop's minimum partial-inclusion threshold.
const HYDRATE_MIN_REMAINING: usize = 32;

// A small invocation-local LRU reuses files across the BFS frontier. The
// sparse line index keeps offset storage proportional to every 128th line.
const SOURCE_CACHE_FILES: usize = 4;
const SOURCE_LINE_STRIDE: usize = 128;

struct SourceFile {
    text: String,
    hash: Option<String>,
    checkpoints: Vec<usize>,
    line_count: usize,
}

impl SourceFile {
    fn new(text: String) -> Self {
        let mut checkpoints = Vec::new();
        let mut line_count = 0usize;
        let mut offset = 0;
        for line in text.split_inclusive('\n') {
            if line_count.is_multiple_of(SOURCE_LINE_STRIDE) {
                checkpoints.push(offset);
            }
            line_count += 1;
            offset += line.len();
        }
        Self {
            text,
            hash: None,
            checkpoints,
            line_count,
        }
    }

    fn matches_hash(&mut self, expected: &str) -> bool {
        self.hash
            .get_or_insert_with(|| aden_core::hash_source(&self.text))
            .as_str()
            == expected
    }

    fn line_start(&self, index: usize) -> usize {
        if index == self.line_count {
            return self.text.len();
        }
        let start = self.checkpoints[index / SOURCE_LINE_STRIDE];
        start
            + self.text[start..]
                .split_inclusive('\n')
                .take(index % SOURCE_LINE_STRIDE)
                .map(str::len)
                .sum::<usize>()
    }

    fn span(&self, start: usize, end: usize) -> Option<(Cow<'_, str>, usize)> {
        let end = end.min(self.line_count);
        if start == 0 || start > end {
            return None;
        }
        let text = &self.text[self.line_start(start - 1)..self.line_start(end)];
        // Match str::lines().join("\n"), including empty lines and bare CR.
        let text = if let Some(line) = text.strip_suffix('\n') {
            line.strip_suffix('\r').unwrap_or(line)
        } else {
            text
        };
        let text = if text.contains("\r\n") {
            Cow::Owned(text.replace("\r\n", "\n"))
        } else {
            Cow::Borrowed(text)
        };
        Some((text, end))
    }
}

struct SourceCache<'root, 'file> {
    root: &'root Path,
    reader: Option<std::io::Result<StoredSourceReader>>,
    files: VecDeque<(&'file str, Option<SourceFile>)>,
    retained_bytes: usize,
}

impl<'root, 'file> SourceCache<'root, 'file> {
    fn new(root: &'root Path) -> Self {
        Self {
            root,
            reader: None,
            files: VecDeque::new(),
            retained_bytes: 0,
        }
    }

    fn get(&mut self, file: &'file str) -> Option<&mut SourceFile> {
        let entry = if let Some(index) = self.files.iter().position(|(path, _)| *path == file) {
            self.files.remove(index)?
        } else {
            let reader = self
                .reader
                .get_or_insert_with(|| StoredSourceReader::new(self.root))
                .as_ref()
                .ok()?;
            let text = reader.read(Path::new(file)).ok();
            let size = text.as_ref().map_or(0, String::len);
            while self.files.len() == SOURCE_CACHE_FILES
                || self.retained_bytes + size > STORED_SOURCE_MAX_BYTES
            {
                let (_, removed) = self.files.pop_back()?;
                self.retained_bytes -= removed.as_ref().map_or(0, |file| file.text.len());
            }
            self.retained_bytes += size;
            (file, text.map(SourceFile::new))
        };
        self.files.push_front(entry);
        self.files.front_mut()?.1.as_mut()
    }
}

fn hydration_threshold(token_budget: usize) -> usize {
    token_budget / 100 * HYDRATE_TRIGGER_PCT + token_budget % 100 * HYDRATE_TRIGGER_PCT / 100
}

fn source_span(attrs: &HashMap<String, String>) -> Option<(&str, usize, usize)> {
    let file = attrs.get("source_file")?;
    let start = attrs.get("start_line")?.parse::<usize>().ok()?;
    let end = attrs.get("end_line")?.parse::<usize>().ok()?;
    (start != 0 && start <= end).then_some((file.as_str(), start, end))
}

/// Pack the unspent token budget with the actual source spans of the included
/// nodes, nearest-first (BFS visit order — the seed is index 0 and is always
/// hydrated first). Language-agnostic by construction: it reads line spans
/// recorded at gen time, never syntax.
///
/// Guarantees:
/// - the budget is respected exactly — every appended byte (including the
///   joining blank line) is charged with the same estimator the assembler
///   budgets by, and `estimate_tokens` is subadditive under concatenation;
/// - `source_hash` is re-verified against the CURRENT file content before any
///   span is included; on mismatch the span is skipped with a one-line note,
///   so a stale store can never ship stale source;
/// - deterministic: visit order is deterministic and file reads are pure.
fn hydrate_with_source(
    graph: &AdenGraph<DocumentNode, AdenEdge>,
    included: &[NodeIndex],
    root: &std::path::Path,
    token_budget: usize,
    total_tokens: &mut usize,
    result: &mut [String],
) {
    if *total_tokens >= hydration_threshold(token_budget) {
        return; // summaries already filled the budget — nothing to pack
    }
    let per_node_cap = (token_budget / HYDRATE_PER_NODE_DIVISOR).max(HYDRATE_MIN_REMAINING);
    let mut cache = SourceCache::new(root);

    for (i, &node) in included.iter().enumerate() {
        let remaining = token_budget.saturating_sub(*total_tokens);
        if remaining < HYDRATE_MIN_REMAINING {
            break;
        }
        let attrs = &graph.graph[node].doc.attributes;
        let Some((file, start, end)) = source_span(attrs) else {
            continue; // synthesized hubs / docs without spans have no source
        };
        let Some(source) = cache.get(file) else {
            continue;
        };

        // Stale-store guard: the span's line numbers are only meaningful for
        // the file content that was hashed at gen time.
        if let Some(expected) = attrs.get("source_hash")
            && !source.matches_hash(expected)
        {
            let note = format!(
                "\n\n// source span omitted: {} changed since gen (hash mismatch)",
                file
            );
            let cost = estimate_tokens(&note);
            if cost <= remaining {
                result[i].push_str(&note);
                *total_tokens += cost;
            }
            continue;
        }

        let Some((span, _)) = source.span(start, end) else {
            continue;
        };
        if span.trim().is_empty() {
            continue;
        }
        let block = format!("\n\nsource ({}:{}-{}):\n{}", file, start, end, span);
        let allowed = per_node_cap.max(remaining / 2).min(remaining);
        let trimmed = truncate_to_tokens(&block, allowed);
        // A cut that leaves only the header line carries no source — skip it.
        if trimmed.trim_end_matches('…').trim().lines().count() < 2 {
            continue;
        }
        *total_tokens += estimate_tokens(&trimmed);
        result[i].push_str(&trimmed);
    }
}

/// Enrich one compact ADG document with a verified source span. The source
/// allowance is separate from the document summary and remains bounded before
/// the outer assembly budget is charged.
fn emit_adg_with_source<'a>(
    doc: &'a DocumentNode,
    cache: &mut SourceCache<'_, 'a>,
    source_token_budget: usize,
) -> Result<String, AssemblyError> {
    use aden_emit::emit_adg;

    let base = emit_adg(&doc.doc).map_err(|error| AssemblyError::Graph(error.to_string()))?;
    if source_token_budget < HYDRATE_MIN_REMAINING {
        return Ok(base);
    }
    let attrs = &doc.doc.attributes;
    let (Some((file, start, end)), Some(expected_hash)) =
        (source_span(attrs), attrs.get("source_hash"))
    else {
        return Ok(base);
    };
    let Some(source) = cache.get(file) else {
        return Ok(base);
    };
    if !source.matches_hash(expected_hash) {
        return Ok(base);
    }
    let Some((span, actual_end)) = source.span(start, end) else {
        return Ok(base);
    };
    if span.trim().is_empty() {
        return Ok(base);
    }

    let mut value: serde_json::Value =
        serde_json::from_str(&base).map_err(|error| AssemblyError::Graph(error.to_string()))?;
    value["source"] = serde_json::json!({
        "state": "complete",
        "file": file,
        "start_line": start,
        "end_line": actual_end,
        "hash_verified": true,
        "text": "",
    });
    let empty =
        serde_json::to_string(&value).map_err(|error| AssemblyError::Graph(error.to_string()))?;
    let overhead = estimate_tokens(&empty).saturating_sub(estimate_tokens(&base));
    let text_budget = source_token_budget.saturating_sub(overhead);
    if text_budget < HYDRATE_MIN_REMAINING {
        return Ok(base);
    }
    let text = truncate_to_tokens(&span, text_budget);
    let clipped = text != span.as_ref();
    value["source"]["text"] = text.into();
    if clipped {
        value["source"]["state"] = "clipped".into();
    }

    // JSON escaping can cost more than the plain-text estimator (quotes,
    // backslashes, and controls). Shrink the text until the serialized source
    // stays inside its allowance; fall back to the summary if only metadata fits.
    let allowed = estimate_tokens(&base).saturating_add(source_token_budget);
    loop {
        let output = serde_json::to_string(&value)
            .map_err(|error| AssemblyError::Graph(error.to_string()))?;
        if estimate_tokens(&output) <= allowed {
            return Ok(output);
        }
        let Some(current) = value["source"]["text"].as_str() else {
            return Ok(base);
        };
        let next_budget = estimate_tokens(current) / 2;
        if next_budget < HYDRATE_MIN_REMAINING {
            return Ok(base);
        }
        value["source"]["text"] = truncate_to_tokens(current, next_budget).into();
        value["source"]["state"] = "clipped".into();
    }
}

/// Assemble documents in ADG (compact JSON) format for token-efficient LLM context.
pub fn assemble_adg(
    graph: &AdenGraph<DocumentNode, AdenEdge>,
    opts: &AssemblyOptions,
) -> Result<String, AssemblyError> {
    use aden_emit::emit_adg;

    let start_idx = graph
        .get_index(&opts.start_anchor)
        .ok_or_else(|| AssemblyError::AnchorNotFound(opts.start_anchor.clone()))?;

    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    let mut results = Vec::new();
    let mut total_tokens = 0usize;
    let mut source_cache = opts
        .hydrate_root
        .as_deref()
        .filter(|_| opts.token_budget / 2 >= HYDRATE_MIN_REMAINING)
        .map(SourceCache::new);

    // The elements are joined with ",\n" and the whole thing wrapped in
    // "[\n" ... "\n]" — bytes concatenated *after* budgeting that, left
    // uncounted, push the joined output past the budget. Single-source both so
    // the cost charged here matches the bytes the final `format!` emits.
    const SEPARATOR: &str = ",\n"; // between adjacent elements
    const WRAPPER: &str = "[\n\n]"; // constant "[\n" prefix + "\n]" suffix (4 bytes)
    let sep_cost = estimate_tokens(SEPARATOR);
    // Reserve the constant wrapper up front so the total (wrapper + docs +
    // separators) stays within budget.
    total_tokens += estimate_tokens(WRAPPER);

    queue.push_back((start_idx, 0usize));

    const MAX_VISITED_NODES: usize = 10_000;
    while let Some((node, depth)) = queue.pop_front() {
        if visited.len() >= MAX_VISITED_NODES {
            break;
        }
        if visited.contains(&node) {
            continue;
        }
        if depth > opts.max_depth {
            continue;
        }
        let doc = &graph.graph[node];
        let adg_json = if let Some(cache) = source_cache.as_mut() {
            let source_budget = if results.is_empty() {
                opts.token_budget / 2
            } else {
                opts.token_budget / HYDRATE_PER_NODE_DIVISOR
            };
            emit_adg_with_source(doc, cache, source_budget)?
        } else {
            emit_adg(&doc.doc).map_err(|error| AssemblyError::Graph(error.to_string()))?
        };
        // Use the same div_ceil estimator as the text path (not a floor) so the
        // per-element charge is always >= the element's real token cost. By
        // subadditivity of div_ceil, the running total then stays >= the final
        // joined output's estimate — closing the +1-token ADG rounding overshoot.
        let tokens = estimate_tokens(&adg_json);
        // A separator precedes this element in the final join iff something is
        // already buffered. Count it against the budget here.
        let sep = if results.is_empty() { 0 } else { sep_cost };
        if total_tokens + tokens + sep > opts.token_budget {
            break;
        }
        total_tokens += tokens + sep;
        visited.insert(node);
        results.push(adg_json);

        for neighbor in ordered_neighbors(graph, node, &opts.edge_types, opts.relevance.as_ref()) {
            queue.push_back((neighbor, depth + 1));
        }
    }

    let output = format!("[\n{}\n]", results.join(SEPARATOR));
    Ok(output)
}

#[derive(Debug, thiserror::Error)]
pub enum AssemblyError {
    #[error("Anchor '{0}' not found. Run 'aden list .' to see available anchors.")]
    AnchorNotFound(String),
    #[error("graph error: {0}")]
    Graph(String),
}

fn document_to_text(
    doc: &DocumentNode,
    block_filter: &[BlockKind],
    include_tags: &[String],
    exclude_tags: &[String],
    attributes: &[String],
) -> String {
    document_to_text_with_imports(
        doc,
        block_filter,
        include_tags,
        exclude_tags,
        attributes,
        None,
    )
    .0
}

fn document_to_text_with_imports(
    doc: &DocumentNode,
    block_filter: &[BlockKind],
    include_tags: &[String],
    exclude_tags: &[String],
    attributes: &[String],
    previous_imports: Option<&[aden_core::Table]>,
) -> (String, Vec<aden_core::Table>) {
    let mut rendered_imports = Vec::new();
    let has_filter = !block_filter.is_empty();
    // Docstring and signature are HEADER content, exempt from block filters:
    // the leading Paragraph (the docstring — often the only substantive prose
    // a code node has) and the Property/Value signature table must survive
    // every intent filter, otherwise a List/Usage-classified question deletes
    // the very answer the graph holds. Filters apply to the extra blocks only.
    let first_paragraph = doc
        .doc
        .blocks
        .iter()
        .position(|b| matches!(b, Block::Paragraph(_)));
    let is_signature_table = |b: &Block| {
        matches!(b, Block::Table(t) if t.headers.len() >= 2
            && t.headers[0].trim().eq_ignore_ascii_case("property")
            && t.headers[1].trim().eq_ignore_ascii_case("value"))
    };
    let should_include = |idx: usize, b: &Block| -> bool {
        if !has_filter {
            return true;
        }
        if Some(idx) == first_paragraph || is_signature_table(b) {
            return true; // header content — never filtered
        }
        let kind = match b {
            Block::Table(_) => BlockKind::Table,
            Block::Paragraph(_) => BlockKind::Paragraph,
            Block::Listing { .. } => BlockKind::Listing,
            Block::Admonition { .. } => BlockKind::Admonition,
            Block::DescriptionList(_) => BlockKind::DescriptionList,
            Block::Checklist(_) => BlockKind::Checklist,
            Block::Incomplete { .. } => BlockKind::Checklist, // Treat as checklist for filtering
        };
        block_filter.contains(&kind)
    };
    // Cells are emitted one row per line; a cell containing a newline (e.g. a
    // multi-line closure callee captured verbatim at gen time) would otherwise
    // break the row-per-line contract and corrupt downstream table compaction.
    let clean_cell = |c: &str| c.split_whitespace().collect::<Vec<_>>().join(" ");

    // Check if we should filter by tags
    let has_tag_filter = !include_tags.is_empty() || !exclude_tags.is_empty();
    let use_tagged_regions = has_tag_filter
        && doc
            .parsed
            .as_ref()
            .map(|p| !p.tagged_regions.is_empty())
            .unwrap_or(false);

    // If blocks were populated during parsing, emit structured content.
    // Otherwise fall back to the original raw source so the assembled
    // context is never empty.
    if !doc.doc.blocks.is_empty() {
        use aden_core::AdmonitionKind;
        let mut out = String::new();
        // Attributes
        for (key, value) in &doc.doc.attributes {
            out.push_str(&format!(":{key}: {value}\n"));
        }
        out.push('\n');
        // Anchor + Title
        out.push_str(&format!("[[{}]]\n", doc.doc.anchor));
        let title = doc
            .doc
            .anchor
            .rfind('#')
            .map(|p| &doc.doc.anchor[p + 1..])
            .unwrap_or(&doc.doc.anchor);
        out.push_str(&format!("= {title}\n\n"));
        // Blocks
        for (idx, block) in doc.doc.blocks.iter().enumerate() {
            if !should_include(idx, block) {
                continue;
            }
            match block {
                Block::Paragraph(t) => {
                    out.push_str(t);
                    out.push('\n');
                }
                Block::Table(table) => {
                    if let Some(previous) = previous_imports
                        && is_file_import_table(table)
                    {
                        if previous.contains(table) || rendered_imports.contains(table) {
                            continue;
                        }
                        rendered_imports.push(table.clone());
                    }
                    out.push_str("|===\n");
                    let header = table
                        .headers
                        .iter()
                        .map(|h| format!("|{}", clean_cell(h)))
                        .collect::<String>();
                    out.push_str(&header);
                    out.push('\n');
                    for row in &table.rows {
                        let row_str = row
                            .iter()
                            .map(|c| format!("|{}", clean_cell(c)))
                            .collect::<String>();
                        out.push_str(&row_str);
                        out.push('\n');
                    }
                    out.push_str("|===\n");
                }
                Block::Listing { language, code } => {
                    if let Some(lang) = language {
                        out.push_str(&format!("[source,{lang}]\n"));
                    } else {
                        out.push_str("[listing]\n");
                    }
                    out.push_str("----\n");
                    out.push_str(code);
                    out.push_str("\n----\n");
                }
                Block::Admonition { kind, text } => {
                    let label = match kind {
                        AdmonitionKind::Note => "NOTE",
                        AdmonitionKind::Tip => "TIP",
                        AdmonitionKind::Warning => "WARNING",
                        AdmonitionKind::Important => "IMPORTANT",
                        AdmonitionKind::Caution => "CAUTION",
                    };
                    out.push_str(&format!("{label}: {text}\n"));
                }
                Block::DescriptionList(items) => {
                    for (term, def) in items {
                        out.push_str(&format!("{term}:: {def}\n"));
                    }
                }
                Block::Checklist(items) => {
                    for item in items {
                        let marker = if item.checked { "[x]" } else { "[ ]" };
                        out.push_str(&format!("* {marker} {}\n", item.text));
                    }
                }
                Block::Incomplete {
                    required_fields,
                    hint,
                } => {
                    out.push_str("[must-complete]\n");
                    out.push_str("====\n");
                    out.push_str("Required fields:\n");
                    for field in required_fields {
                        out.push_str(&format!("* {field}\n"));
                    }
                    out.push_str(&format!("\nHint: {hint}\n"));
                    out.push_str("====\n");
                }
            }
        }

        // If tag filtering is enabled and we have tagged regions, filter content
        if use_tagged_regions {
            let mut filtered_out = String::new();
            let relevant_tags: Vec<_> = doc
                .parsed
                .as_ref()
                .map(|p| &p.tagged_regions)
                .map_or(&[] as &[_], |v| v)
                .iter()
                .filter(|t| {
                    let tag_matches =
                        include_tags.is_empty() || include_tags.iter().any(|i| i == &t.tag_name);
                    let not_excluded =
                        exclude_tags.is_empty() || !exclude_tags.iter().any(|e| e == &t.tag_name);
                    tag_matches && not_excluded
                })
                .collect();

            if !relevant_tags.is_empty() {
                // Add header
                for (key, value) in &doc.doc.attributes {
                    filtered_out.push_str(&format!("{key}: {value}\n"));
                }
                filtered_out.push('\n');
                filtered_out.push_str(&format!("[[{}]]\n", doc.doc.anchor));
                let title = doc
                    .doc
                    .anchor
                    .rfind('#')
                    .map(|p| &doc.doc.anchor[p + 1..])
                    .unwrap_or(&doc.doc.anchor);
                filtered_out.push_str(&format!("= {title}\n\n"));

                // Add tagged regions
                for region in relevant_tags {
                    filtered_out.push_str(&region.content);
                    filtered_out.push_str("\n\n");
                }
                return (filtered_out, Vec::new());
            }
        }

        // If attributes are set, filter by conditional regions
        let has_attrs = !attributes.is_empty();
        let has_conditionals = doc
            .parsed
            .as_ref()
            .map(|p| !p.conditional_regions.is_empty())
            .unwrap_or(false);
        if has_attrs && has_conditionals {
            let attr_set: HashSet<_> = attributes.iter().collect();
            let relevant_conditionals: Vec<_> = doc
                .parsed
                .as_ref()
                .map(|p| &p.conditional_regions)
                .map_or(&[] as &[_], |v| v)
                .iter()
                .filter(|c| {
                    // Include if the attribute is set (active) OR if it's not in our attr set
                    // For ifdef: include if attr is set
                    // For ifndef: include if attr is NOT set (but we track via is_active)
                    c.is_active || !attr_set.contains(&c.attribute)
                })
                .collect();

            if !relevant_conditionals.is_empty() {
                let mut filtered_out = String::new();
                for (key, value) in &doc.doc.attributes {
                    filtered_out.push_str(&format!("{key}: {value}\n"));
                }
                filtered_out.push('\n');
                filtered_out.push_str(&format!("[[{}]]\n", doc.doc.anchor));
                let title = doc
                    .doc
                    .anchor
                    .rfind('#')
                    .map(|p| &doc.doc.anchor[p + 1..])
                    .unwrap_or(&doc.doc.anchor);
                filtered_out.push_str(&format!("= {title}\n\n"));

                for region in relevant_conditionals {
                    filtered_out.push_str(&region.content);
                    filtered_out.push_str("\n\n");
                }
                return (filtered_out, Vec::new());
            }
        }

        (out, rendered_imports)
    } else {
        (
            doc.parsed
                .as_ref()
                .map(|p| p.raw_content.trim().to_string())
                .unwrap_or_default(),
            Vec::new(),
        )
    }
}

fn is_file_import_table(table: &aden_core::Table) -> bool {
    table.headers.len() == 2
        && table.headers[0].trim().eq_ignore_ascii_case("include")
        && table.headers[1].trim().eq_ignore_ascii_case("kind")
}

type FileImportTables = HashMap<std::path::PathBuf, Vec<aden_core::Table>>;

/// Deduplicate structured blocks before either output format is rendered.
/// Unknown file identities and non-block rendering remain untouched.
fn render_assembly_document(
    doc: &DocumentNode,
    opts: &AssemblyOptions,
    seen: &FileImportTables,
) -> (String, Vec<aden_core::Table>) {
    let previous = (!doc.source_path.as_os_str().is_empty()).then(|| {
        seen.get(&doc.source_path)
            .map(Vec::as_slice)
            .unwrap_or_default()
    });
    let (raw, imports) = document_to_text_with_imports(
        doc,
        &opts.block_filter,
        &opts.include_tags,
        &opts.exclude_tags,
        &opts.attributes,
        previous,
    );
    let text = if opts.llm_mode {
        strip_asciidoc_markup(&raw)
    } else {
        raw
    };
    (text, imports)
}

/// Only complete accepted documents can claim table visibility. A clipped
/// document may have lost any table, so conservatively retain later copies.
fn record_import_tables(
    seen: &mut FileImportTables,
    doc: &DocumentNode,
    imports: Vec<aden_core::Table>,
) {
    if !imports.is_empty() {
        seen.entry(doc.source_path.clone())
            .or_default()
            .extend(imports);
    }
}

/// Strip AsciiDoc markup and compact tables for maximally dense LLM output.
///
/// Three classes of waste are eliminated:
///
/// 1. **Format noise** — anchors, attribute lines, block delimiters, role
///    annotations, table delimiters.  These carry zero semantic signal.
///
/// 2. **`edge::calls[...]` lines** — internal graph edge declarations that
///    duplicate the callee table already present above them.
///
/// 3. **Verbose tables** — `|Property|Value` / `|Callee|Line` AsciiDoc tables
///    are compressed to compact inline forms:
///    - Property table row `|Name|foo` → `name: foo`
///    - Callee table rows → single line `calls: foo(12), bar(34)`
///
/// The result is semantically identical but 40–60 % fewer tokens.
pub fn strip_asciidoc_markup(text: &str) -> String {
    let mut out: Vec<Cow<'_, str>> = Vec::new();
    let mut prev_blank = false;

    // State machine for table compression.
    #[derive(PartialEq)]
    enum TableMode {
        None,
        Property, // |Property|Value header seen
        Callee,   // |Callee|Line header seen
    }
    let mut table_mode = TableMode::None;
    let mut callee_calls: Vec<String> = Vec::new();
    let mut callee_count = 0;
    // Accumulated rows of the current Signature property table, collapsed into a
    // single dense line on close.
    let mut sig_acc: Vec<(String, String)> = Vec::new();
    // The document's title (`= Title` heading) — the synthesis fallback for
    // the signature name (see `flush_signature`).
    let mut doc_title = None;

    // Cap on inlined callee names: a hub's Callee/Line table would otherwise
    // spend hundreds of tokens on call-graph scaffolding before any neighbor's
    // prose is reached. The first MAX_CALLEES (source order) are kept and the
    // rest summarized as `(+K more)`.
    const MAX_CALLEES: usize = 12;
    fn flush_callees(calls: &mut Vec<String>, total: &mut usize, out: &mut Vec<Cow<'_, str>>) {
        if *total == 0 {
            return;
        }
        let mut line = format!("calls: {}", calls.join(", "));
        if *total > MAX_CALLEES {
            line.push_str(&format!(" (+{} more)", *total - MAX_CALLEES));
        }
        out.push(Cow::Owned(line));
        calls.clear();
        *total = 0;
    }

    // Collapse a function's signature table into one line:
    // `[async] [unsafe] name(p1: T1, p2: T2) -> Ret`. No language resolver
    // emits a `Name` row (the rows are Visibility/Async/param…/Returns), so
    // the name is synthesized from the document title (`fallback_name`) — the
    // anchor's `#`-suffix — without which every signature table rendered to
    // nothing. `Visibility` repeats on every symbol and is dropped;
    // non-function tables (no params/return) fall back to key:value lines.
    // Every language extractor emits this same Property/Value schema, so this
    // is language-agnostic.
    fn flush_signature(
        acc: &mut Vec<(String, String)>,
        out: &mut Vec<Cow<'_, str>>,
        fallback_name: Option<&str>,
    ) {
        if acc.is_empty() {
            return;
        }
        let mut name: Option<String> = None;
        let mut params: Vec<String> = Vec::new();
        let mut ret: Option<String> = None;
        let (mut is_async, mut is_unsafe) = (false, false);
        let mut extras: Vec<(String, String)> = Vec::new();
        for (k, v) in acc.drain(..) {
            match k.to_lowercase().as_str() {
                "name" => name = Some(v),
                "visibility" => {} // low signal, present on every symbol
                "async" => is_async = v.eq_ignore_ascii_case("true"),
                "unsafe" => is_unsafe = v.eq_ignore_ascii_case("true"),
                k if k.starts_with("param") => params.push(v),
                "returns" | "return" => ret = Some(v),
                _ => extras.push((k, v)),
            }
        }
        let name = name.or_else(|| fallback_name.map(str::to_string));
        if let Some(n) = &name
            && (!params.is_empty() || ret.is_some())
        {
            let mut line = String::new();
            if is_async {
                line.push_str("async ");
            }
            if is_unsafe {
                line.push_str("unsafe ");
            }
            line.push_str(n);
            line.push('(');
            line.push_str(&params.join(", "));
            line.push(')');
            if let Some(r) = &ret {
                line.push_str(" -> ");
                line.push_str(r.trim_start_matches("->").trim());
            }
            out.push(Cow::Owned(line));
        }
        for (k, v) in extras {
            out.push(Cow::Owned(format!(
                "{}: {}",
                k.to_lowercase().replace(' ', "_"),
                v
            )));
        }
    }

    fn header_cell_matches(cell: &str, header: &str) -> bool {
        cell.eq_ignore_ascii_case(header) || (!cell.is_ascii() && cell.to_lowercase() == header)
    }

    for line in text.lines() {
        let trimmed = line.trim();

        // --- Skip: [[anchor]] lines ---
        if trimmed.starts_with("[[") && trimmed.ends_with("]]") {
            continue;
        }

        // --- Skip: :key: value AsciiDoc attribute lines ---
        // Matches ":source_file: foo.rs", ":author: Alice", ":toc:", ":!numbered:"
        if trimmed.starts_with(':')
            && let Some(rest) = trimmed.strip_prefix(':')
        {
            let rest = rest.strip_prefix('!').unwrap_or(rest);
            if let Some(colon) = rest.find(':') {
                let key = &rest[..colon];
                if !key.is_empty() && !key.contains(' ') {
                    continue;
                }
            }
        }

        // --- Skip: block delimiters ---
        if matches!(
            trimmed,
            "----" | "====" | "****" | "____" | "--" | "'''" | "<<<"
        ) {
            continue;
        }

        // --- Skip: role/block annotations like [source,rust], [listing], [NOTE] ---
        if trimmed.starts_with('[') && trimmed.ends_with(']') && !trimmed.contains(' ') {
            continue;
        }

        // --- Skip: edge::calls[...] lines — duplicate of callee table ---
        if trimmed.starts_with("edge::calls[") || trimmed.starts_with("edge::") {
            continue;
        }

        // --- Table delimiter |=== ---
        if trimmed == "|===" {
            // Flush accumulated callee list / signature when the table closes
            flush_callees(&mut callee_calls, &mut callee_count, &mut out);
            flush_signature(&mut sig_acc, &mut out, doc_title);
            table_mode = TableMode::None;
            continue;
        }

        // --- Table rows ---
        if trimmed.starts_with('|') && trimmed.chars().filter(|&c| c == '|').count() >= 2 {
            let cells: Vec<&str> = trimmed
                .trim_matches('|')
                .split('|')
                .map(str::trim)
                .collect();

            // Detect table headers
            if cells.len() >= 2 {
                if header_cell_matches(cells[1], "value")
                    && (header_cell_matches(cells[0], "property")
                        || header_cell_matches(cells[0], "name"))
                {
                    table_mode = TableMode::Property;
                    continue;
                }
                if header_cell_matches(cells[1], "line") && header_cell_matches(cells[0], "callee")
                {
                    table_mode = TableMode::Callee;
                    continue;
                }

                match table_mode {
                    TableMode::Property => {
                        // Accumulate; collapsed into one signature line on close.
                        let key = cells[0].trim().to_string();
                        let val = cells[1..].join(" ").trim().to_string();
                        if !key.is_empty() && !val.is_empty() {
                            sig_acc.push((key, val));
                        }
                        continue;
                    }
                    TableMode::Callee => {
                        // Accumulate: |foo|12| → "foo(12)"
                        let callee = cells[0].trim();
                        if !callee.is_empty() {
                            callee_count += 1;
                            if callee_count <= MAX_CALLEES {
                                let lineno = cells[1];
                                let call = if lineno.is_empty() || lineno == "0" {
                                    callee.to_string()
                                } else {
                                    format!("{}({})", callee, lineno)
                                };
                                callee_calls.push(call);
                            }
                        }
                        continue;
                    }
                    TableMode::None => {
                        // Generic table row — keep as compact space-separated values
                        let row = cells.join("  ").trim().to_string();
                        if !row.is_empty() {
                            out.push(Cow::Owned(row));
                            prev_blank = false;
                        }
                        continue;
                    }
                }
            }
        } else if table_mode != TableMode::None {
            // Non-table line encountered — flush accumulated table state
            flush_callees(&mut callee_calls, &mut callee_count, &mut out);
            flush_signature(&mut sig_acc, &mut out, doc_title);
            table_mode = TableMode::None;
        }

        // --- Headings: convert = Title → Title, == Section → Section: ---
        let processed = if let Some(rest) = trimmed.strip_prefix("= ") {
            doc_title.get_or_insert(rest.trim());
            Cow::Borrowed(rest)
        } else if trimmed.starts_with("== ")
            || trimmed.starts_with("=== ")
            || trimmed.starts_with("==== ")
        {
            let stripped = trimmed.trim_start_matches('=').trim();
            Cow::Owned(format!("{}:", stripped))
        } else {
            // Inline xref cleanup: <<anchor,text>> → text, <<anchor>> → anchor
            replace_xrefs(line)
        };

        // --- Skip: per-symbol boilerplate that repeats on every node and
        //     carries zero signal. The generic parent-module relationship is
        //     now redundant (the graph has real Calls/PartOf edges), and the
        //     tree-sitter provenance note repeats verbatim on every symbol —
        //     pure token tax across a multi-node assembly. Checked here, after
        //     xref replacement turns "<<mod-x,module>>:: ..." into "module:: ...".
        {
            let p = processed.trim();
            if p == "module:: This symbol is part of the parent module."
                || p == "NOTE: Extracted from source code via tree-sitter. Confidence is heuristic."
                || p == "Extracted from source code via tree-sitter. Confidence is heuristic."
                // The collapsed `name(params) -> ret` line is self-evidently a
                // signature; the label is one redundant line per symbol.
                || p == "Signature:"
            {
                continue;
            }
        }

        // Collapse multiple blank lines to one
        let is_blank = processed.trim().is_empty();
        if is_blank {
            if !prev_blank {
                out.push(Cow::Borrowed(""));
            }
            prev_blank = true;
            continue;
        }
        prev_blank = false;
        out.push(processed);
    }

    // Final flush of any pending callee list / signature
    flush_callees(&mut callee_calls, &mut callee_count, &mut out);
    flush_signature(&mut sig_acc, &mut out, doc_title);

    // Drop a trailing empty section header. A header line (ends with ':' and
    // has no value after the colon, e.g. "Relationships:") with no real content
    // after it carries no signal — common once the parent-module boilerplate is
    // suppressed and the section is left dangling at a node's end. Only the
    // trailing case is removed so genuine heading hierarchies stay intact.
    let is_empty_header = |s: &str| {
        let t = s.trim_end();
        t.ends_with(':') && !t[..t.len() - 1].contains(' ') && !t[..t.len() - 1].contains(':')
    };
    let mut nonblank = out
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, line)| !line.trim().is_empty());
    let Some((mut end, last)) = nonblank.next() else {
        return String::new();
    };
    if is_empty_header(last)
        && let Some((previous, line)) = nonblank.next()
        && !is_empty_header(line)
    {
        end = previous;
    }

    let start = out[..=end]
        .iter()
        .position(|line| !line.trim().is_empty())
        .unwrap_or(end);
    let lines = &out[start..=end];
    let [first, middle @ .., last] = lines else {
        return lines[0].trim().to_owned();
    };

    let first = first.trim_start();
    let last = last.trim_end();
    let middle_len: usize = middle.iter().map(|line| line.len() + 1).sum();
    let mut rendered = String::with_capacity(first.len() + middle_len + last.len() + 1);
    rendered.push_str(first);
    for line in middle {
        rendered.push('\n');
        rendered.push_str(line);
    }
    rendered.push('\n');
    rendered.push_str(last);
    rendered
}

/// Replace AsciiDoc cross-reference macros with their display text or target.
/// `<<anchor,display text>>` → `display text`
/// `<<anchor>>` → `anchor`
fn replace_xrefs(line: &str) -> Cow<'_, str> {
    let Some(first) = line.find("<<") else {
        return Cow::Borrowed(line);
    };
    let mut result = String::with_capacity(line.len());
    let mut remaining = line;
    let mut next_start = Some(first);

    while let Some(start) = next_start {
        let after = &remaining[start + 2..];
        if let Some(end) = after.find(">>") {
            result.push_str(&remaining[..start]);
            let inner = &after[..end];
            if let Some(comma) = inner.find(',') {
                // <<anchor,display text>> → display text
                result.push_str(inner[comma + 1..].trim());
            } else {
                // <<anchor>> → anchor (strip internal ID prefixes like aden://)
                let anchor = inner.trim();
                let display = anchor
                    .rsplit('#')
                    .next()
                    .unwrap_or(anchor)
                    .replace('-', " ");
                result.push_str(&display);
            }
            remaining = &after[end + 2..];
        } else {
            break;
        }
        next_start = remaining.find("<<");
    }
    result.push_str(remaining);
    Cow::Owned(result)
}

/// Truncate text to fit within `max_tokens`, consistent with `estimate_tokens`.
///
/// `estimate_tokens` counts bytes (4 bytes ≈ 1 token), so the cap here is a byte
/// budget (`max_tokens * 4`) rather than a word count — a word heuristic
/// over-counted long tokens (e.g. code) and let the truncated doc still blow past
/// the byte-based budget. The cut is taken at a UTF-8 char boundary, preferring a
/// word/whitespace boundary at or before the limit, and the trailing " …" is
/// included within the budget so the doc's `estimate_tokens` stays <= `max_tokens`.
fn truncate_to_tokens(text: &str, max_tokens: usize) -> String {
    let max_bytes = max_tokens.saturating_mul(4);
    if text.len() <= max_bytes {
        return text.to_string();
    }

    // Reserve room for the " …" ellipsis (4 bytes) so the final string —
    // including the marker — never exceeds the byte budget.
    const ELLIPSIS: &str = " …";
    let content_budget = max_bytes.saturating_sub(ELLIPSIS.len());

    // Largest char boundary at or before the content budget.
    let mut cut = content_budget.min(text.len());
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }

    // Prefer a whitespace boundary at or before the cut so we don't split a word.
    if let Some(ws) = text[..cut].rfind(char::is_whitespace) {
        cut = ws;
    }

    let mut truncated = text[..cut].trim_end().to_string();
    truncated.push_str(ELLIPSIS);
    truncated
}

/// Improved token estimation using a word-based heuristic.
/// Typical LLM tokenization yields roughly 0.75 words per token.
/// Estimate the token cost of `text`.
///
/// Uses the standard ~4-bytes-per-token heuristic (the same one the docs
/// advertise). A previous word-count heuristic ignored punctuation and
/// operators, which under-counted code by ~2x and let `asm`/`ask` blow far past
/// their token budget (e.g. emitting 28 KB under a 4096-token budget). For dense
/// LLM context the budget must mean what it says, so estimate from byte length.
fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aden_core::{Document, NodeType};

    // ── FIX 1: truncate_to_tokens budget guarantee ───────────────────────────

    /// Build a `DocumentNode` with the given anchor (no blocks, no parsed data).
    fn node(anchor: &str) -> DocumentNode {
        DocumentNode {
            doc: Document {
                anchor: anchor.to_string(),
                node_type: NodeType::Function,
                attributes: std::collections::HashMap::new(),
                blocks: Vec::new(),
                source_span: None,
                metadata: None,
                confidence: 1.0,
            },
            parsed: None,
            source_path: std::path::PathBuf::from("x.adoc"),
        }
    }

    /// The core invariant of FIX 1: whatever `truncate_to_tokens` returns,
    /// `estimate_tokens` of it must be within budget. Exercised across plain
    /// ASCII, repeated multibyte UTF-8, and a single giant whitespace-free token
    /// (the case the old word-count heuristic blew through), for a spread of
    /// budgets. A failure here means the budget no longer means what it says.
    #[test]
    fn truncate_never_exceeds_budget() {
        let ascii = "the quick brown fox jumps over the lazy dog ".repeat(200);
        let multibyte = "café 你好 🚀 ".repeat(300);
        let giant_word = "x".repeat(20_000); // no whitespace anywhere
        let inputs = [
            ascii.as_str(),
            multibyte.as_str(),
            giant_word.as_str(),
            "short",
        ];
        for &text in &inputs {
            for &budget in &[1usize, 8, 64, 1000] {
                let out = truncate_to_tokens(text, budget);
                let est = estimate_tokens(&out);
                assert!(
                    est <= budget,
                    "estimate_tokens({est}) exceeded budget {budget} for input len {} (out len {})",
                    text.len(),
                    out.len()
                );
            }
        }
    }

    /// When truncation actually happens the result must end with the ellipsis
    /// marker, signalling the doc was cut.
    #[test]
    fn truncate_marks_cut_with_ellipsis() {
        let text = "alpha beta gamma delta epsilon zeta eta theta ".repeat(50);
        let out = truncate_to_tokens(&text, 8);
        assert!(
            out.len() < text.len(),
            "expected truncation to shorten text"
        );
        assert!(
            out.ends_with('…'),
            "truncated text must end with ellipsis: {out:?}"
        );
    }

    /// When the text already fits the byte budget it is returned verbatim — no
    /// ellipsis, no allocation churn that changes content.
    #[test]
    fn truncate_returns_unchanged_when_within_budget() {
        let text = "this easily fits";
        // budget 1000 tokens == 4000 bytes, far more than this string.
        let out = truncate_to_tokens(text, 1000);
        assert_eq!(out, text);
        assert!(!out.ends_with('…'));
    }

    /// Multibyte input must never be cut mid-codepoint. The result is a `String`,
    /// so an invalid cut would have panicked inside `truncate_to_tokens`; reaching
    /// the asserts proves it stayed on a char boundary. Cover several budgets that
    /// land near multibyte boundaries.
    #[test]
    fn truncate_respects_multibyte_boundaries() {
        // Each "café 你好 🚀 " unit mixes 1-, 2-, 3- and 4-byte codepoints.
        let text = "café 你好 🚀 ".repeat(100);
        for budget in 1..=80 {
            let out = truncate_to_tokens(&text, budget);
            // String is UTF-8 by construction; the real check is "did not panic".
            assert!(out.is_char_boundary(out.len()));
            assert!(estimate_tokens(&out) <= budget, "budget {budget} violated");
        }
    }

    // ── FIX 2: edge_priority + ordered_neighbors structural ranking ───────────

    /// edge_priority encodes the intended monotonic structural ordering:
    /// Calls is the most load-bearing, then Uses, then Documents, and any
    /// "other" edge type (here RelatesTo) sorts last.
    #[test]
    fn edge_priority_is_monotonic_calls_uses_documents_other() {
        let calls = edge_priority(&EdgeType::Calls);
        let uses = edge_priority(&EdgeType::Uses);
        let documents = edge_priority(&EdgeType::Documents);
        let other = edge_priority(&EdgeType::RelatesTo); // falls into the `_ => 12` arm
        assert!(
            calls < uses,
            "Calls ({calls}) must rank before Uses ({uses})"
        );
        assert!(
            uses < documents,
            "Uses ({uses}) must rank before Documents ({documents})"
        );
        assert!(
            documents < other,
            "Documents ({documents}) must rank before other/RelatesTo ({other})"
        );
    }

    /// ordered_neighbors must return the frontier sorted by (edge_priority, then
    /// target anchor) regardless of insertion / petgraph iteration order. This is
    /// FIX 2's core: a high-fan-out node spends the budget on the most structural
    /// edges first, deterministically.
    #[test]
    fn ordered_neighbors_sorts_by_priority_then_anchor() {
        let mut graph = AdenGraph::<DocumentNode, AdenEdge>::new();
        let src = graph.graph.add_node(node("src"));

        // Two Calls neighbors with anchors out of order, plus one Uses and one
        // Documents neighbor. Insertion order deliberately scrambles priority so
        // a naive (unsorted) implementation would fail.
        let documents_n = graph.graph.add_node(node("d-documents"));
        let calls_b = graph.graph.add_node(node("calls-b"));
        let uses_n = graph.graph.add_node(node("c-uses"));
        let calls_a = graph.graph.add_node(node("calls-a"));

        graph.graph.add_edge(
            src,
            documents_n,
            AdenEdge {
                edge_type: EdgeType::Documents,
            },
        );
        graph.graph.add_edge(
            src,
            calls_b,
            AdenEdge {
                edge_type: EdgeType::Calls,
            },
        );
        graph.graph.add_edge(
            src,
            uses_n,
            AdenEdge {
                edge_type: EdgeType::Uses,
            },
        );
        graph.graph.add_edge(
            src,
            calls_a,
            AdenEdge {
                edge_type: EdgeType::Calls,
            },
        );

        // edge_types empty => follow all.
        let ordered = ordered_neighbors(&graph, src, &[], None);
        let anchors: Vec<&str> = ordered
            .iter()
            .map(|&idx| graph.graph[idx].doc.anchor.as_str())
            .collect();

        // Calls (priority 0) first, the two Calls in anchor order; then Uses (5),
        // then Documents (11).
        assert_eq!(
            anchors,
            vec!["calls-a", "calls-b", "c-uses", "d-documents"],
            "neighbors must be ordered by (edge_priority, anchor)"
        );
    }

    #[test]
    fn ordered_neighbors_skips_synthetic_module_hubs() {
        let mut graph = AdenGraph::<DocumentNode, AdenEdge>::new();
        let src = graph.graph.add_node(node("src"));
        let hub = graph.graph.add_node(node("mod-aden-paths"));
        let callee = graph.graph.add_node(node("compare_key"));
        graph.graph.add_edge(
            src,
            hub,
            AdenEdge {
                edge_type: EdgeType::PartOf,
            },
        );
        graph.graph.add_edge(
            src,
            callee,
            AdenEdge {
                edge_type: EdgeType::Calls,
            },
        );
        let anchors: Vec<&str> = ordered_neighbors(&graph, src, &[], None)
            .iter()
            .map(|&idx| graph.graph[idx].doc.anchor.as_str())
            .collect();
        assert_eq!(anchors, vec!["compare_key"]);
        assert!(!anchors.iter().any(|a| a.starts_with("mod-")));
    }

    /// The `edge_types` filter must drop non-matching neighbors entirely while
    /// preserving the priority/anchor ordering of the rest.
    #[test]
    fn ordered_neighbors_respects_edge_type_filter() {
        let mut graph = AdenGraph::<DocumentNode, AdenEdge>::new();
        let src = graph.graph.add_node(node("src"));
        let calls_n = graph.graph.add_node(node("calls-x"));
        let uses_n = graph.graph.add_node(node("uses-y"));
        graph.graph.add_edge(
            src,
            calls_n,
            AdenEdge {
                edge_type: EdgeType::Calls,
            },
        );
        graph.graph.add_edge(
            src,
            uses_n,
            AdenEdge {
                edge_type: EdgeType::Uses,
            },
        );

        let ordered = ordered_neighbors(&graph, src, &[EdgeType::Calls], None);
        let anchors: Vec<&str> = ordered
            .iter()
            .map(|&idx| graph.graph[idx].doc.anchor.as_str())
            .collect();
        assert_eq!(
            anchors,
            vec!["calls-x"],
            "filter must keep only Calls neighbors"
        );
    }

    /// Query-aware ordering: with a relevance map, ordered_neighbors breaks ties
    /// WITHIN a priority bucket toward the more query-relevant target — the
    /// high-fan-out prose case (one section Contains many equal-priority
    /// paragraphs). Without a map (None) the order is byte-identical to before.
    #[test]
    fn ordered_neighbors_relevance_breaks_ties_within_priority() {
        let mut graph = AdenGraph::<DocumentNode, AdenEdge>::new();
        let src = graph.graph.add_node(node("src"));
        // Three Contains neighbors, all priority 11 — only relevance can reorder.
        for anchor in ["para-a", "para-b", "para-c"] {
            let t = graph.graph.add_node(node(anchor));
            graph.graph.add_edge(
                src,
                t,
                AdenEdge {
                    edge_type: EdgeType::Contains,
                },
            );
        }

        // None → deterministic anchor order, unchanged from today.
        let structural: Vec<String> = ordered_neighbors(&graph, src, &[], None)
            .iter()
            .map(|&i| graph.graph[i].doc.anchor.clone())
            .collect();
        assert_eq!(structural, vec!["para-a", "para-b", "para-c"]);

        // Relevance favouring para-c → it leads; para-b (no entry → 0.0) trails.
        let mut rel = HashMap::new();
        rel.insert("para-c".to_string(), 0.9_f32);
        rel.insert("para-a".to_string(), 0.1_f32);
        let aware: Vec<String> = ordered_neighbors(&graph, src, &[], Some(&rel))
            .iter()
            .map(|&i| graph.graph[i].doc.anchor.clone())
            .collect();
        assert_eq!(aware, vec!["para-c", "para-a", "para-b"]);
    }

    // ── FIX B: parallel-edge (multigraph) priority folding ────────────────────

    /// Build a `DocumentNode` carrying a single Paragraph block so
    /// `document_to_text` / `assemble` emit real, byte-bearing content (the
    /// blockless `node()` helper above would assemble to the empty string and
    /// could not exercise the budget arithmetic).
    fn node_with_text(anchor: &str, text: &str) -> DocumentNode {
        DocumentNode {
            doc: Document {
                anchor: anchor.to_string(),
                node_type: NodeType::Function,
                attributes: std::collections::HashMap::new(),
                blocks: vec![Block::Paragraph(text.to_string())],
                source_span: None,
                metadata: None,
                confidence: 1.0,
            },
            parsed: None,
            source_path: std::path::PathBuf::from("x.adoc"),
        }
    }

    fn node_with_imports(anchor: &str, path: &str, imports: &[(&str, &str)]) -> DocumentNode {
        let mut node = node_with_text(anchor, "symbol summary");
        node.source_path = std::path::PathBuf::from(path);
        node.doc.blocks.push(Block::Table(aden_core::Table {
            headers: vec!["Include".to_string(), "Kind".to_string()],
            rows: imports
                .iter()
                .map(|(include, kind)| vec![include.to_string(), kind.to_string()])
                .collect(),
        }));
        node
    }

    #[test]
    fn repeated_file_import_tables_are_removed_but_distinct_metadata_survives() {
        let mut graph = AdenGraph::<DocumentNode, AdenEdge>::new();
        let first = graph
            .add_node(node_with_imports(
                "first",
                "src/a.rs",
                &[("std::fmt", "use")],
            ))
            .unwrap();
        let duplicate = graph
            .add_node(node_with_imports(
                "duplicate",
                "src/a.rs",
                &[("std::fmt", "use")],
            ))
            .unwrap();
        let distinct_row = graph
            .add_node(node_with_imports(
                "distinct-row",
                "src/a.rs",
                &[("std::io", "use")],
            ))
            .unwrap();
        let other_file = graph
            .add_node(node_with_imports(
                "other-file",
                "src/b.rs",
                &[("std::fmt", "use")],
            ))
            .unwrap();
        for pair in [first, duplicate, distinct_row, other_file].windows(2) {
            graph.graph.add_edge(
                pair[0],
                pair[1],
                AdenEdge {
                    edge_type: EdgeType::Calls,
                },
            );
        }
        for llm_mode in [false, true] {
            for relevance_select in [false, true] {
                let opts = AssemblyOptions {
                    start_anchor: "first".to_string(),
                    max_depth: 4,
                    token_budget: 4096,
                    llm_mode,
                    relevance_select,
                    relevance: Some(HashMap::new()),
                    ..Default::default()
                };
                let (output, anchors) = assemble_with_anchors(&graph, &opts).unwrap();
                assert_eq!(
                    anchors,
                    ["first", "duplicate", "distinct-row", "other-file"]
                );
                let separator = if llm_mode { "\n\n---\n\n" } else { "\n<<<\n" };
                let rendered: Vec<_> = output.split(separator).collect();
                assert_eq!(rendered.len(), 4);
                assert_eq!(rendered[0].matches("std::fmt").count(), 1);
                assert_eq!(rendered[1].matches("std::fmt").count(), 0);
                assert_eq!(rendered[2].matches("std::io").count(), 1);
                assert_eq!(rendered[3].matches("std::fmt").count(), 1);
                assert!(estimate_tokens(&output) <= opts.token_budget);
            }
        }
    }

    #[test]
    fn import_dedup_preserves_table_text_in_source_listings() {
        let mut graph = AdenGraph::<DocumentNode, AdenEdge>::new();
        let first = graph
            .add_node(node_with_imports(
                "first",
                "src/a.rs",
                &[("std::fmt", "use")],
            ))
            .unwrap();
        let mut duplicate = node_with_imports("duplicate", "src/a.rs", &[("std::fmt", "use")]);
        let code = "source-before\n|===\n|Include|Kind\n|std::fmt|use\n|===\nsource-after";
        duplicate.doc.blocks.insert(
            1,
            Block::Listing {
                language: None,
                code: code.to_string(),
            },
        );
        let mut without_metadata = duplicate.clone();
        without_metadata.doc.blocks.pop();
        let second = graph.add_node(duplicate).unwrap();
        graph.graph.add_edge(
            first,
            second,
            AdenEdge {
                edge_type: EdgeType::Calls,
            },
        );
        for llm_mode in [false, true] {
            let opts = AssemblyOptions {
                start_anchor: "first".to_string(),
                llm_mode,
                ..Default::default()
            };
            let output = assemble(&graph, &opts).unwrap();
            let expected_raw = document_to_text(&without_metadata, &[], &[], &[], &[]);
            let expected = if llm_mode {
                strip_asciidoc_markup(&expected_raw)
            } else {
                expected_raw
            };
            let separator = if llm_mode { "\n\n---\n\n" } else { "\n<<<\n" };
            assert_eq!(output.split_once(separator).unwrap().1, expected);
            assert!(output.contains("source-before"));
            assert!(output.contains("source-after"));
            assert_eq!(output.matches("std::fmt").count(), 2);
        }
    }

    #[test]
    fn import_dedup_keeps_unknown_source_paths_distinct() {
        let mut graph = AdenGraph::<DocumentNode, AdenEdge>::new();
        let first = graph
            .add_node(node_with_imports("first", "", &[("std::fmt", "use")]))
            .unwrap();
        let second = graph
            .add_node(node_with_imports("second", "", &[("std::fmt", "use")]))
            .unwrap();
        graph.graph.add_edge(
            first,
            second,
            AdenEdge {
                edge_type: EdgeType::Calls,
            },
        );
        for llm_mode in [false, true] {
            let opts = AssemblyOptions {
                start_anchor: "first".to_string(),
                llm_mode,
                ..Default::default()
            };
            let output = assemble(&graph, &opts).unwrap();
            assert_eq!(output.matches("std::fmt").count(), 2);
        }
    }

    #[test]
    fn import_dedup_filtered_tables_do_not_claim_visibility() {
        let doc = node_with_imports("first", "src/a.rs", &[("std::fmt", "use")]);
        let mut seen = HashMap::new();
        let filtered_opts = AssemblyOptions {
            block_filter: vec![BlockKind::Paragraph],
            ..Default::default()
        };
        let (filtered, imports) = render_assembly_document(&doc, &filtered_opts, &seen);
        assert!(!filtered.contains("std::fmt"));
        record_import_tables(&mut seen, &doc, imports);
        let (unfiltered, _) = render_assembly_document(&doc, &AssemblyOptions::default(), &seen);
        assert_eq!(unfiltered.matches("std::fmt").count(), 1);
    }

    #[test]
    fn import_dedup_clipped_seed_does_not_hide_later_complete_table() {
        let mut graph = AdenGraph::<DocumentNode, AdenEdge>::new();
        let mut seed = node_with_imports("first", "src/a.rs", &[("std::fmt", "use")]);
        seed.doc.blocks[0] = Block::Paragraph(format!("seed {}", "x".repeat(2000)));
        let first = graph.add_node(seed).unwrap();
        let second = graph
            .add_node(node_with_imports(
                "second",
                "src/a.rs",
                &[("std::fmt", "use")],
            ))
            .unwrap();
        graph.graph.add_edge(
            first,
            second,
            AdenEdge {
                edge_type: EdgeType::Calls,
            },
        );
        let opts = AssemblyOptions {
            start_anchor: "first".to_string(),
            token_budget: 128,
            llm_mode: true,
            relevance_select: true,
            relevance: Some(HashMap::new()),
            ..Default::default()
        };
        let (output, anchors) = assemble_with_anchors(&graph, &opts).unwrap();
        assert_eq!(anchors, ["first", "second"]);
        let (clipped, complete) = output.split_once("\n\n---\n\n").unwrap();
        assert!(clipped.ends_with('…'));
        assert!(!clipped.contains("std::fmt"));
        assert_eq!(complete.matches("std::fmt").count(), 1);
        assert!(estimate_tokens(&output) <= opts.token_budget);
    }

    #[test]
    fn import_dedup_skipped_candidate_does_not_claim_visibility() {
        let mut graph = AdenGraph::<DocumentNode, AdenEdge>::new();
        let first = graph
            .add_node(node_with_text("first", "seed summary"))
            .unwrap();
        let mut oversized = node_with_imports("oversized", "src/a.rs", &[("std::fmt", "use")]);
        oversized.doc.blocks[0] = Block::Paragraph("too large ".repeat(1000));
        let skipped = graph.add_node(oversized).unwrap();
        let last = graph
            .add_node(node_with_imports(
                "last",
                "src/a.rs",
                &[("std::fmt", "use")],
            ))
            .unwrap();
        for (from, to) in [(first, skipped), (skipped, last)] {
            graph.graph.add_edge(
                from,
                to,
                AdenEdge {
                    edge_type: EdgeType::Calls,
                },
            );
        }
        for llm_mode in [false, true] {
            let opts = AssemblyOptions {
                start_anchor: "first".to_string(),
                token_budget: 128,
                llm_mode,
                relevance_select: true,
                relevance: Some(HashMap::new()),
                ..Default::default()
            };
            let (output, anchors) = assemble_with_anchors(&graph, &opts).unwrap();
            assert_eq!(anchors, ["first", "last"]);
            assert_eq!(output.matches("std::fmt").count(), 1);
            assert!(estimate_tokens(&output) <= opts.token_budget);
        }
    }

    #[test]
    fn import_dedup_preserves_verified_hydration_with_filtered_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let source = "|===\n|Include|Kind\n|std::fmt|use\n|===\n";
        std::fs::write(dir.path().join("imports.txt"), source).unwrap();
        let mut graph = AdenGraph::<DocumentNode, AdenEdge>::new();
        let mut nodes = Vec::new();
        for anchor in ["first", "second"] {
            let mut node = node_with_imports(anchor, "imports.txt", &[("std::fmt", "use")]);
            node.doc.attributes.extend([
                ("source_file".to_string(), "imports.txt".to_string()),
                ("start_line".to_string(), "1".to_string()),
                ("end_line".to_string(), "4".to_string()),
                ("source_hash".to_string(), aden_core::hash_source(source)),
            ]);
            nodes.push(graph.add_node(node).unwrap());
        }
        graph.graph.add_edge(
            nodes[0],
            nodes[1],
            AdenEdge {
                edge_type: EdgeType::Calls,
            },
        );
        for llm_mode in [false, true] {
            let opts = AssemblyOptions {
                start_anchor: "first".to_string(),
                llm_mode,
                block_filter: vec![BlockKind::Paragraph],
                hydrate_root: Some(dir.path().to_path_buf()),
                ..Default::default()
            };
            let output = assemble(&graph, &opts).unwrap();
            assert_eq!(output.matches(source.trim_end()).count(), 2);
            assert!(estimate_tokens(&output) <= opts.token_budget);
        }
    }

    #[test]
    fn jaccard_and_token_set_behave() {
        let a = token_set("handle the incoming request body");
        assert!((jaccard(&a, &a) - 1.0).abs() < 1e-6, "identical → 1.0");
        let b = token_set("render response cookie session values");
        assert_eq!(jaccard(&a, &b), 0.0, "disjoint → 0.0");
        let c = token_set("handle the incoming request payload");
        let j = jaccard(&a, &c); // share handle/the/incoming/request
        assert!(j > 0.4 && j < 1.0, "partial overlap, got {j}");
        assert_eq!(jaccard(&a, &token_set("")), 0.0, "empty → 0.0");
    }

    /// MMR redundancy pruning: a hub fanning out to two near-duplicate leaves and
    /// one distinct leaf. At a budget that fits the hub + two leaves, plain
    /// assembly spends both leaf slots on the duplicates (anchor order a,b,c) and
    /// drops the distinct leaf; MMR skips the second duplicate, freeing its slot
    /// for the distinct one.
    #[test]
    fn mmr_skips_redundant_and_includes_diverse() {
        let mut graph = AdenGraph::<DocumentNode, AdenEdge>::new();
        let hub = graph
            .add_node(node_with_text("hub", "hub overview of the module"))
            .unwrap();
        let a = graph
            .add_node(node_with_text(
                "leaf-a",
                "parse the incoming http request body and validate the json payload fields",
            ))
            .unwrap();
        let b = graph
            .add_node(node_with_text(
                "leaf-b",
                "parse the incoming http request body and validate the json payload values",
            ))
            .unwrap();
        let c = graph
            .add_node(node_with_text(
                "leaf-c",
                "render a templated response and persist the signed session cookie securely",
            ))
            .unwrap();
        for leaf in [a, b, c] {
            graph.graph.add_edge(
                hub,
                leaf,
                AdenEdge {
                    edge_type: EdgeType::Calls,
                },
            );
        }

        // Budget tuned to fit the hub + two leaves, not all three.
        let opts = AssemblyOptions {
            start_anchor: "hub".to_string(),
            max_depth: 1,
            token_budget: 60,
            ..Default::default()
        };

        let plain = assemble_with_anchors(&graph, &opts).unwrap().1;
        assert!(
            plain.iter().any(|x| x == "leaf-b") && !plain.iter().any(|x| x == "leaf-c"),
            "plain assembly spends the budget on the duplicate leaf-b, dropping leaf-c: {plain:?}"
        );

        let pruned = assemble_with_anchors_mmr(&graph, &opts, Some(0.7))
            .unwrap()
            .1;
        assert!(
            pruned.iter().any(|x| x == "leaf-c") && !pruned.iter().any(|x| x == "leaf-b"),
            "MMR skips redundant leaf-b and includes distinct leaf-c: {pruned:?}"
        );
    }

    /// The graph is a petgraph multigraph: a single (src -> tgt) pair can carry
    /// several parallel edges of different types. FIX B folds them to the BEST
    /// (minimum) priority and visits the target exactly once. Here `tgt` is
    /// reachable from `src` by BOTH a Documents (priority 11) and a Calls
    /// (priority 0) edge — added straight onto the inner `DiGraph` to bypass
    /// `AdenGraph::add_edge`'s `contains_edge` dedup and force a true multigraph.
    /// A separate Documents-only neighbor `late` (priority 11) must rank AFTER
    /// `tgt`, proving `tgt` is ranked by its Calls edge, not its Documents edge,
    /// and that the pre-fix "arbitrary last-inserted edge" bug is gone.
    #[test]
    fn ordered_neighbors_folds_parallel_edges_to_best_priority_once() {
        let mut graph = AdenGraph::<DocumentNode, AdenEdge>::new();
        let src = graph.graph.add_node(node("src"));
        let tgt = graph.graph.add_node(node("a-target"));
        let late = graph.graph.add_node(node("z-late"));

        // Insert the WEAKER (Documents) edge first, then the STRONGER (Calls)
        // edge, on the SAME (src -> tgt) pair — a parallel/multigraph edge. A
        // naive find_edge-based impl would pick whichever it resolves to (often
        // the last-inserted Calls, or the first Documents) and could enqueue the
        // target twice.
        graph.graph.add_edge(
            src,
            tgt,
            AdenEdge {
                edge_type: EdgeType::Documents,
            },
        );
        graph.graph.add_edge(
            src,
            tgt,
            AdenEdge {
                edge_type: EdgeType::Calls,
            },
        );
        // A single-edge Documents-only neighbor to verify ranking, not just dedup.
        graph.graph.add_edge(
            src,
            late,
            AdenEdge {
                edge_type: EdgeType::Documents,
            },
        );

        let ordered = ordered_neighbors(&graph, src, &[], None);
        let anchors: Vec<&str> = ordered
            .iter()
            .map(|&idx| graph.graph[idx].doc.anchor.as_str())
            .collect();

        // `tgt` must appear EXACTLY ONCE (parallel edges deduped) and BEFORE the
        // Documents-only `late` (ranked by its best/Calls priority 0 < 11).
        assert_eq!(
            anchors,
            vec!["a-target", "z-late"],
            "parallel edges must fold to the best (Calls) priority and the target appear once"
        );
        assert_eq!(
            anchors.iter().filter(|&&a| a == "a-target").count(),
            1,
            "the parallel-edge target must be enqueued exactly once, not per parallel edge"
        );
    }

    // ── FIX A: assemble() respects the token budget end-to-end ────────────────

    /// Lock FIX A end-to-end: build a graph of several small docs reachable from
    /// a hub and assert `estimate_tokens(assemble(...))` <= the (tight) budget.
    /// The previously-uncounted inter-doc separators pushed the joined output
    /// past the budget (the ~2.36x overshoot); this guards against a regression
    /// at the public `assemble` boundary, not just the truncate helper.
    #[test]
    fn assemble_output_never_exceeds_token_budget() {
        let mut graph = AdenGraph::<DocumentNode, AdenEdge>::new();
        // A hub plus several small leaf docs, each big enough that the separator
        // accounting matters but small enough that several fit a tight budget.
        let hub = graph
            .add_node(node_with_text(
                "hub",
                "the hub doc body with some words to spend a few tokens",
            ))
            .unwrap();
        let mut leaves = Vec::new();
        for i in 0..12 {
            let n = graph
                .add_node(node_with_text(
                    &format!("leaf-{i:02}"),
                    "leaf paragraph content with several words consuming tokens here",
                ))
                .unwrap();
            leaves.push(n);
        }
        // Fan the hub out to every leaf via Calls edges.
        for &leaf in &leaves {
            graph.add_edge(
                hub,
                leaf,
                AdenEdge {
                    edge_type: EdgeType::Calls,
                },
            );
        }

        // Several tight budgets across the boundary where docs+separators would
        // overflow if the separators were uncounted.
        for &budget in &[8usize, 16, 32, 50, 100] {
            let opts = AssemblyOptions {
                start_anchor: "hub".to_string(),
                max_depth: 3,
                token_budget: budget,
                llm_mode: true,
                ..Default::default()
            };
            let output = assemble(&graph, &opts).expect("assemble must succeed");
            let est = estimate_tokens(&output);
            assert!(
                est <= budget,
                "assemble output estimate_tokens({est}) exceeded budget {budget} (output len {} bytes)",
                output.len()
            );
        }
    }

    /// FIX A for the ADG (compact JSON) path: the `[\n ... \n]` wrapper and the
    /// `,\n` element separators are now charged against the budget, so the full
    /// emitted JSON stays within it.
    #[test]
    fn assemble_adg_output_never_exceeds_token_budget() {
        let mut graph = AdenGraph::<DocumentNode, AdenEdge>::new();
        let hub = graph
            .add_node(node_with_text("hub", "hub body words for tokens"))
            .unwrap();
        for i in 0..10 {
            let n = graph
                .add_node(node_with_text(
                    &format!("leaf-{i:02}"),
                    "leaf body words consuming a few tokens each",
                ))
                .unwrap();
            graph.add_edge(
                hub,
                n,
                AdenEdge {
                    edge_type: EdgeType::Calls,
                },
            );
        }
        for &budget in &[16usize, 32, 64, 128] {
            let opts = AssemblyOptions {
                start_anchor: "hub".to_string(),
                max_depth: 3,
                token_budget: budget,
                ..Default::default()
            };
            let output = assemble_adg(&graph, &opts).expect("assemble_adg must succeed");
            // The full serialized JSON, including wrappers and source hydration,
            // must remain inside the same conservative estimator budget.
            let est = estimate_tokens(&output);
            assert!(
                est <= budget,
                "assemble_adg output len/4 ({est}) exceeded budget {budget} (output len {} bytes)",
                output.len()
            );
        }
    }
    #[test]
    fn assemble_adg_hydrates_only_verified_source_within_budget() {
        let root = tempfile::tempdir().unwrap();
        let source = "fn hydrated() {\n    important_dependency();\n}\n";
        std::fs::write(root.path().join("sample.rs"), source).unwrap();

        let mut node = node_with_text("hydrated", "short summary");
        node.doc
            .attributes
            .insert("source_file".into(), "sample.rs".into());
        node.doc.attributes.insert("start_line".into(), "1".into());
        node.doc.attributes.insert("end_line".into(), "3".into());
        node.doc
            .attributes
            .insert("source_hash".into(), aden_core::hash_source(source));
        let mut graph = AdenGraph::<DocumentNode, AdenEdge>::new();
        graph.add_node(node).unwrap();
        let opts = AssemblyOptions {
            start_anchor: "hydrated".into(),
            token_budget: 256,
            hydrate_root: Some(root.path().to_path_buf()),
            ..Default::default()
        };

        let output = assemble_adg(&graph, &opts).unwrap();
        assert!(estimate_tokens(&output) <= opts.token_budget);
        let value: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value[0]["source"]["hash_verified"], true);
        assert_eq!(value[0]["source"]["state"], "complete");
        assert_eq!(value[0]["source"]["text"], source.trim_end());

        std::fs::write(root.path().join("sample.rs"), "fn changed() {}\n").unwrap();
        let stale = assemble_adg(&graph, &opts).unwrap();
        let stale: serde_json::Value = serde_json::from_str(&stale).unwrap();
        assert!(stale[0].get("source").is_none());
    }

    #[test]
    fn source_spans_match_legacy_line_join_across_sparse_checkpoints() {
        let long = "one\r\n\nαβ\rγ\n".repeat(SOURCE_LINE_STRIDE);
        for text in ["", "\n", "last\r", "α\r\n\r\nβ\n", &long] {
            let lines: Vec<_> = text.lines().collect();
            let file = SourceFile::new(text.to_owned());
            assert_eq!(file.line_count, lines.len());
            for (start, end) in [
                (0, 1),
                (1, 1),
                (1, 3),
                (2, 1),
                (2, 3),
                (SOURCE_LINE_STRIDE - 1, SOURCE_LINE_STRIDE + 2),
                (SOURCE_LINE_STRIDE + 2, usize::MAX),
                (1, usize::MAX),
                (usize::MAX, usize::MAX),
            ] {
                let actual_end = end.min(lines.len());
                let expected = (start != 0 && start <= actual_end)
                    .then(|| (lines[start - 1..actual_end].join("\n"), actual_end));
                assert_eq!(
                    file.span(start, end)
                        .map(|(text, end)| (text.into_owned(), end)),
                    expected,
                    "{start}..={end}"
                );
            }
        }
    }

    #[test]
    fn hydration_threshold_matches_exact_ratio_without_overflow() {
        for budget in [0, 1, 99, 100, 101, usize::MAX - 1, usize::MAX] {
            assert_eq!(
                hydration_threshold(budget) as u128,
                budget as u128 * HYDRATE_TRIGGER_PCT as u128 / 100
            );
        }
    }

    #[test]
    fn source_cache_reuses_bytes_and_hash_across_interleaved_files() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source.rs");
        let original = "fn original() {}\r\n";
        let changed = "fn different() {}\n";
        let expected = aden_core::hash_source(original);
        std::fs::write(&path, original).unwrap();
        std::fs::write(root.path().join("other.rs"), "// other source\n").unwrap();

        let mut cache = SourceCache::new(root.path());
        assert!(cache.reader.is_none());
        let file = cache.get("source.rs").unwrap();
        assert!(file.hash.is_none());
        assert!(file.matches_hash(&expected));
        assert_eq!(file.hash.as_deref(), Some(expected.as_str()));
        assert!(cache.get("other.rs").is_some());
        std::fs::write(&path, changed).unwrap();
        let cached = cache.get("source.rs").unwrap();
        assert!(cached.matches_hash(&expected));
        assert_eq!(cached.span(1, 1).unwrap().0, "fn original() {}");

        let mut next = SourceCache::new(root.path());
        let new_file = next.get("source.rs").unwrap();
        assert!(!new_file.matches_hash(&expected));
        assert!(new_file.matches_hash(&aden_core::hash_source(changed)));
    }

    #[test]
    fn assembly_hydrates_confined_source_and_skips_malformed_spans() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let outside = dir.path().join("outside.rs");
        let outside_text = "fn outside_must_stay_hidden() {}\n";
        std::fs::write(&outside, outside_text).unwrap();
        let source = "fn verified_hydration() {}\r\nfn legacy_hydration() {}\r\n";
        std::fs::write(root.join("inside.rs"), source).unwrap();
        let expected = aden_core::hash_source(source);
        let outside_hash = aden_core::hash_source(outside_text);
        let mut graph = AdenGraph::<DocumentNode, AdenEdge>::new();
        let mut nodes = Vec::new();
        for (anchor, path, start, end, hash) in [
            ("seed", "inside.rs", 1, 1, Some(expected.as_str())),
            ("legacy", "inside.rs", 2, 2, None),
            ("malformed", "inside.rs", 2, 1, Some(expected.as_str())),
            (
                "absolute",
                outside.to_str().unwrap(),
                1,
                1,
                Some(outside_hash.as_str()),
            ),
            ("escape", "../outside.rs", 1, 1, Some(outside_hash.as_str())),
        ] {
            let mut node = node_with_text(anchor, "summary");
            node.doc.attributes.extend([
                ("source_file".to_owned(), path.to_owned()),
                ("start_line".to_owned(), start.to_string()),
                ("end_line".to_owned(), end.to_string()),
            ]);
            if let Some(hash) = hash {
                node.doc
                    .attributes
                    .insert("source_hash".into(), hash.into());
            }
            nodes.push(graph.add_node(node).unwrap());
        }
        for &node in &nodes[1..] {
            graph.graph.add_edge(
                nodes[0],
                node,
                AdenEdge {
                    edge_type: EdgeType::Calls,
                },
            );
        }
        let opts = AssemblyOptions {
            start_anchor: "seed".into(),
            token_budget: 4096,
            hydrate_root: Some(root),
            ..Default::default()
        };
        let (text, anchors) = assemble_with_anchors(&graph, &opts).unwrap();
        assert_eq!(
            anchors,
            ["seed", "absolute", "escape", "legacy", "malformed"]
        );
        assert_eq!(text.matches("fn verified_hydration() {}").count(), 1);
        assert_eq!(text.matches("fn legacy_hydration() {}").count(), 1);
        assert!(!text.contains("outside_must_stay_hidden"));
        assert!(estimate_tokens(&text) <= opts.token_budget);

        let adg = assemble_adg(&graph, &opts).unwrap();
        assert!(estimate_tokens(&adg) <= opts.token_budget);
        let documents: Vec<serde_json::Value> = serde_json::from_str(&adg).unwrap();
        assert_eq!(documents.len(), anchors.len());
        assert_eq!(documents[0]["source"]["text"], "fn verified_hydration() {}");
        assert_eq!(documents[0]["source"]["hash_verified"], true);
        assert!(documents[1..].iter().all(|doc| doc.get("source").is_none()));
    }
}
