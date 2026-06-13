// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use aden_core::{Block, EdgeType};
use aden_graph::{AdenEdge, AdenGraph, DocumentNode};
use petgraph::Direction;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use std::collections::{HashMap, HashSet, VecDeque};

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
    let mut neighbors: Vec<(u8, &str, NodeIndex)> = best
        .into_iter()
        .map(|(target, prio)| (prio, graph.graph[target].doc.anchor.as_str(), target))
        .collect();
    neighbors.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    neighbors.into_iter().map(|(_, _, n)| n).collect()
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
        }
    }
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
/// resolution for token-savings estimates.
pub fn assemble_with_anchors(
    graph: &AdenGraph<DocumentNode, AdenEdge>,
    opts: &AssemblyOptions,
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
    let mut total_tokens = 0usize;

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

    queue.push_back((start_idx, 0usize));

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
        let raw_text = document_to_text(
            doc,
            &opts.block_filter,
            &opts.include_tags,
            &opts.exclude_tags,
            &opts.attributes,
        );

        // In LLM mode, strip AsciiDoc markup so every token is signal.
        let text = if opts.llm_mode {
            strip_asciidoc_markup(&raw_text)
        } else {
            raw_text
        };

        let tokens = estimate_tokens(&text);
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
            result.push(text);
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
        for neighbor in ordered_neighbors(graph, node, &opts.edge_types) {
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
    if *total_tokens >= token_budget * HYDRATE_TRIGGER_PCT / 100 {
        return; // summaries already filled the budget — nothing to pack
    }
    let per_node_cap = (token_budget / HYDRATE_PER_NODE_DIVISOR).max(HYDRATE_MIN_REMAINING);
    // One read per distinct file, shared across the nodes it contains.
    let mut file_cache: HashMap<String, Option<String>> = HashMap::new();

    for (i, &node) in included.iter().enumerate() {
        let remaining = token_budget.saturating_sub(*total_tokens);
        if remaining < HYDRATE_MIN_REMAINING {
            break;
        }
        let attrs = &graph.graph[node].doc.attributes;
        let (Some(file), Some(start), Some(end)) = (
            attrs.get("source_file"),
            attrs
                .get("start_line")
                .and_then(|s| s.parse::<usize>().ok()),
            attrs.get("end_line").and_then(|s| s.parse::<usize>().ok()),
        ) else {
            continue; // synthesized hubs / docs without spans have no source
        };
        let content = file_cache.entry(file.clone()).or_insert_with(|| {
            let p = std::path::Path::new(file);
            let abs = if p.is_absolute() {
                p.to_path_buf()
            } else {
                root.join(p)
            };
            std::fs::read_to_string(abs).ok()
        });
        let Some(content) = content else { continue };

        // Stale-store guard: the span's line numbers are only meaningful for
        // the file content that was hashed at gen time.
        if let Some(expected) = attrs.get("source_hash")
            && aden_core::hash_source(content) != *expected
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

        let lines: Vec<&str> = content.lines().collect();
        if start == 0 || start > lines.len() {
            continue;
        }
        let span = lines[start - 1..end.min(lines.len())].join("\n");
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
        let adg_json = emit_adg(&doc.doc).map_err(|e| AssemblyError::Graph(e.to_string()))?;
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

        for neighbor in ordered_neighbors(graph, node, &opts.edge_types) {
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
                return filtered_out;
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
                return filtered_out;
            }
        }

        out
    } else {
        doc.parsed
            .as_ref()
            .map(|p| p.raw_content.trim().to_string())
            .unwrap_or_default()
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
    let mut out: Vec<String> = Vec::new();
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
    // Accumulated rows of the current Signature property table, collapsed into a
    // single dense line on close.
    let mut sig_acc: Vec<(String, String)> = Vec::new();
    // The document's title (`= Title` heading) — the synthesis fallback for
    // the signature name (see `flush_signature`).
    let mut doc_title: Option<String> = None;

    // Cap on inlined callee names: a hub's Callee/Line table would otherwise
    // spend hundreds of tokens on call-graph scaffolding before any neighbor's
    // prose is reached. The first MAX_CALLEES (source order) are kept and the
    // rest summarized as `(+K more)`.
    const MAX_CALLEES: usize = 12;
    let flush_callees = |calls: &mut Vec<String>, out: &mut Vec<String>| {
        if calls.is_empty() {
            return;
        }
        let total = calls.len();
        let mut line = format!("calls: {}", calls[..total.min(MAX_CALLEES)].join(", "));
        if total > MAX_CALLEES {
            line.push_str(&format!(" (+{} more)", total - MAX_CALLEES));
        }
        out.push(line);
        calls.clear();
    };

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
        out: &mut Vec<String>,
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
            out.push(line);
        }
        for (k, v) in extras {
            out.push(format!("{}: {}", k.to_lowercase().replace(' ', "_"), v));
        }
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
            flush_callees(&mut callee_calls, &mut out);
            flush_signature(&mut sig_acc, &mut out, doc_title.as_deref());
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
                let h0 = cells[0].to_lowercase();
                let h1 = cells[1].to_lowercase();

                if h1 == "value" && (h0 == "property" || h0 == "name") {
                    table_mode = TableMode::Property;
                    continue;
                }
                if h0 == "callee" && h1 == "line" {
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
                        let lineno = cells.get(1).map(|s| s.trim()).unwrap_or("");
                        if !callee.is_empty() {
                            if lineno.is_empty() || lineno == "0" {
                                callee_calls.push(callee.to_string());
                            } else {
                                callee_calls.push(format!("{}({})", callee, lineno));
                            }
                        }
                        continue;
                    }
                    TableMode::None => {
                        // Generic table row — keep as compact space-separated values
                        let row = cells.join("  ").trim().to_string();
                        if !row.is_empty() {
                            out.push(row);
                            prev_blank = false;
                        }
                        continue;
                    }
                }
            }
        } else if table_mode != TableMode::None {
            // Non-table line encountered — flush accumulated table state
            flush_callees(&mut callee_calls, &mut out);
            flush_signature(&mut sig_acc, &mut out, doc_title.as_deref());
            table_mode = TableMode::None;
        }

        // --- Headings: convert = Title → Title, == Section → Section: ---
        let processed = if let Some(rest) = trimmed.strip_prefix("= ") {
            if doc_title.is_none() {
                doc_title = Some(rest.trim().to_string());
            }
            rest.to_string()
        } else if trimmed.starts_with("== ")
            || trimmed.starts_with("=== ")
            || trimmed.starts_with("==== ")
        {
            let stripped = trimmed.trim_start_matches('=').trim();
            format!("{}:", stripped)
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
                out.push(String::new());
            }
            prev_blank = true;
            continue;
        }
        prev_blank = false;
        out.push(processed);
    }

    // Final flush of any pending callee list / signature
    flush_callees(&mut callee_calls, &mut out);
    flush_signature(&mut sig_acc, &mut out, doc_title.as_deref());

    // Drop a trailing empty section header. A header line (ends with ':' and
    // has no value after the colon, e.g. "Relationships:") with no real content
    // after it carries no signal — common once the parent-module boilerplate is
    // suppressed and the section is left dangling at a node's end. Only the
    // trailing case is removed so genuine heading hierarchies stay intact.
    let is_empty_header = |s: &str| {
        let t = s.trim_end();
        t.ends_with(':') && !t[..t.len() - 1].contains(' ') && !t[..t.len() - 1].contains(':')
    };
    let mut compact: Vec<String> = Vec::with_capacity(out.len());
    for i in 0..out.len() {
        if is_empty_header(&out[i])
            && out[i + 1..].iter().all(|l| l.trim().is_empty())
            // Only when real content precedes it — never collapse a genuine
            // heading hierarchy (a header whose previous line is also a header).
            && compact
                .iter()
                .rev()
                .find(|l| !l.trim().is_empty())
                .is_some_and(|prev| !is_empty_header(prev))
        {
            continue; // dangling section header after real content
        }
        compact.push(out[i].clone());
    }

    compact.join("\n").trim().to_string()
}

/// Replace AsciiDoc cross-reference macros with their display text or target.
/// `<<anchor,display text>>` → `display text`
/// `<<anchor>>` → `anchor`
fn replace_xrefs(line: &str) -> String {
    let mut result = String::with_capacity(line.len());
    let mut remaining = line;

    while let Some(start) = remaining.find("<<") {
        result.push_str(&remaining[..start]);
        let after = &remaining[start + 2..];
        if let Some(end) = after.find(">>") {
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
            result.push_str("<<");
            remaining = after;
        }
    }
    result.push_str(remaining);
    result
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
        let ordered = ordered_neighbors(&graph, src, &[]);
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

        let ordered = ordered_neighbors(&graph, src, &[EdgeType::Calls]);
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

        let ordered = ordered_neighbors(&graph, src, &[]);
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
        let hub = graph.add_node(node_with_text(
            "hub",
            "the hub doc body with some words to spend a few tokens",
        ));
        let mut leaves = Vec::new();
        for i in 0..12 {
            let n = graph.add_node(node_with_text(
                &format!("leaf-{i:02}"),
                "leaf paragraph content with several words consuming tokens here",
            ));
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
        let hub = graph.add_node(node_with_text("hub", "hub body words for tokens"));
        for i in 0..10 {
            let n = graph.add_node(node_with_text(
                &format!("leaf-{i:02}"),
                "leaf body words consuming a few tokens each",
            ));
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
            // assemble_adg uses the floor estimator (len/4) per doc but counts the
            // wrapper/separator via estimate_tokens; the full output's byte/4 must
            // still fit the budget.
            let est = output.len() / 4;
            assert!(
                est <= budget,
                "assemble_adg output len/4 ({est}) exceeded budget {budget} (output len {} bytes)",
                output.len()
            );
        }
    }
}
