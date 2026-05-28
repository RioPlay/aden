use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

use aden_core::AdenConfig;
use aden_graph::Direction;

use crate::types::{AnchorPattern, QueryIntent};
use crate::util::{
    load_or_build_index, node_to_json, parse_single_edge_type, perform_check, sanitize_anchor,
    sanitize_source_file, valid_edge_types,
};
use aden_index::SearchResult;

/// Words too common in natural-language questions to be treated as symbol names.
/// "include", "output", "context", "decide" would match struct/function names
/// spuriously in many codebases.
const SYMBOL_STOP_WORDS: &[&str] = &[
    // question words
    "how", "what", "why", "when", "where", "which", "does", "do", "did",
    "is", "are", "was", "were", "will", "would", "can", "could", "should",
    // connectives
    "the", "an", "in", "to", "of", "and", "or", "not", "that", "this",
    "with", "for", "from", "into", "on", "by", "at", "its", "it", "a",
    // common verbs that collide with symbol names in many codebases
    "include", "output", "input", "get", "set", "new", "add", "find",
    "build", "make", "run", "use", "put", "take", "call", "handle",
    "process", "check", "update", "create", "delete", "remove", "read",
    "write", "send", "receive", "parse", "emit", "render", "load", "save",
    "open", "close", "start", "stop", "init", "reset", "fetch", "log",
    "print", "format", "encode", "decode", "next", "map", "list", "count",
    // generic nouns that are often symbol names *and* common English words
    "context", "result", "error", "data", "value", "node", "graph", "block",
    "item", "type", "name", "path", "file", "line", "text", "token", "key",
    "time", "index", "state", "kind", "source", "target", "mode", "level",
];

/// Extract explicit `func()` or `Type::method()` references from a query.
/// These are unambiguous intent signals — the user told us exactly what they
/// want to know about.
fn extract_explicit_symbols(query: &str) -> Vec<String> {
    let mut symbols = Vec::new();
    // Match word characters immediately followed by `(`
    let mut chars = query.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '(' {
            // Walk backwards to collect the symbol name
            let before = &query[..i];
            let sym: String = before
                .chars()
                .rev()
                .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            if sym.len() >= 2 {
                symbols.push(sym.to_lowercase());
            }
        }
    }
    symbols
}

/// Resolve a natural-language query to the best matching anchor.
///
/// Strategy (in order):
///
/// 1. **Explicit call syntax** — `func()` or `Type::method()` in the query
///    is an unambiguous signal.  Match against `#symbol` anchors first.
///
/// 2. **Qualified symbol token match** — query tokens that are ≥3 chars,
///    not in the stop-word list, and exactly match a `#symbol` name in the
///    top results.  Requires the match to appear in a top-20 result.
///
/// 3. **Score-driven selection with tiebreaker** — pick the highest-scoring
///    result.  Within a 5-point noise band, prefer by `AnchorPattern`
///    (Symbol > Adr > Plan > Module > …).
///
/// No hardcoded word→module mappings.  The search index is the source of
/// truth; this function only applies generic structural preferences on top.
fn resolve_anchor_fuzzy(query: &str, results: &[SearchResult]) -> String {
    if results.is_empty() {
        return "readme".to_string();
    }

    // Step 1: explicit `func()` syntax — highest confidence.
    let explicit = extract_explicit_symbols(query);
    if !explicit.is_empty() {
        // Search all results (not just top-10) for an exact symbol name match.
        for sym in &explicit {
            if let Some(hit) = results.iter().find(|r| {
                r.anchor
                    .rsplit('#')
                    .next()
                    .map(|s| s.to_lowercase() == *sym)
                    .unwrap_or(false)
            }) {
                return hit.anchor.clone();
            }
        }
    }

    // Step 2: qualified token match — tokens that are specific enough to be
    // symbol names (≥3 chars, not a stop word, not a single common letter).
    let query_tokens: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| {
            s.len() >= 3
                && !SYMBOL_STOP_WORDS.contains(&s.to_lowercase().as_str())
        })
        .map(|s| s.to_lowercase())
        .collect();

    for result in results.iter().take(20) {
        if let Some(sym) = result.anchor.rsplit('#').next() {
            if sym.len() < 3 {
                continue;
            }
            let sym_lower = sym.to_lowercase();
            if SYMBOL_STOP_WORDS.contains(&sym_lower.as_str()) {
                continue;
            }
            if query_tokens.iter().any(|t| *t == sym_lower) {
                return result.anchor.clone();
            }
        }
    }

    // Step 3: score-driven selection with structural tiebreaker.
    // Within a 5-point noise band of the top score, prefer Symbol over Module.
    // Exception: do NOT select a symbol anchor whose bare name is a stop word
    // (e.g. the query "How does error handling work?" must not route to `#Error`
    // just because BM25 ranked it highest — "error" is a stop word and the user
    // was asking a general question, not asking about a specific Error type).
    let top_score = results[0].score;
    let noise_band = 5.0_f64;

    // Helper: true if anchor is a symbol whose bare name is a stop word.
    let is_stopword_symbol = |anchor: &str| -> bool {
        if let Some(sym) = anchor.rsplit('#').next() {
            if anchor.contains('#') {
                return SYMBOL_STOP_WORDS.contains(&sym.to_lowercase().as_str());
            }
        }
        false
    };

    // First pass: pick best within noise band, excluding stop-word symbols.
    let best = results
        .iter()
        .filter(|r| (top_score - r.score) <= noise_band)
        .filter(|r| !is_stopword_symbol(&r.anchor))
        .max_by_key(|r| AnchorPattern::from_anchor(&r.anchor).tiebreak());

    // Fallback: if every candidate was a stop-word symbol, relax and take top score.
    let best = best.unwrap_or_else(|| {
        results
            .iter()
            .max_by_key(|r| AnchorPattern::from_anchor(&r.anchor).tiebreak())
            .unwrap_or(&results[0])
    });

    best.anchor.clone()
}

pub fn cmd_check(path: &Path, severity: &str) -> Result<(), Box<dyn std::error::Error>> {
    if !path.is_dir() {
        return Err("check requires a directory path".into());
    }

    let min_severity = match severity.to_lowercase().as_str() {
        "suggest" => 0,
        "warn" => 1,
        "forbid" => 2,
        _ => {
            return Err(format!(
                "Invalid severity '{}': use Suggest, Warn, or Forbid",
                severity
            )
            .into());
        }
    };

    let messages = perform_check(path)?;
    let mut exit_code = 0i32;
    for msg in &messages {
        if msg.starts_with("ERROR:") {
            // ERROR maps to Forbid (severity 2)
            if min_severity <= 2 {
                eprintln!("{msg}");
                exit_code = 1;
            } else {
                println!("{msg}");
            }
        } else if msg.starts_with("WARNING:") {
            // WARNING maps to Warn (severity 1)
            if min_severity <= 1 {
                eprintln!("{msg}");
                exit_code = 1;
            } else {
                println!("{msg}");
            }
        } else {
            println!("{msg}");
        }
    }

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

#[derive(Clone)]
pub struct AsmOptions {
    pub path: PathBuf,
    pub from: String,
    pub depth: usize,
    pub budget: usize,
    pub edge_types: Vec<aden_core::EdgeType>,
    pub out: Option<PathBuf>,
    pub format: String,
    pub silent: bool,
    pub auto: bool,
    pub strict: bool,
    pub inspect: bool,
    pub include_tags: Vec<String>,
    pub exclude_tags: Vec<String>,
    pub attributes: Vec<String>,
}

pub fn cmd_asm(opts: AsmOptions) -> Result<(), Box<dyn std::error::Error>> {
    use aden_asm::traverse::{AssemblyOptions, assemble, assemble_adg};

    if !opts.path.is_dir() {
        return Err("asm requires a directory path".into());
    }

let graph = aden_graph::cache::build_from_directory_cached(&opts.path)?;

    let (mut resolved_anchor, effective_budget) = if opts.auto && !opts.strict {
        let index = load_or_build_index(&opts.path)?;
        let results = index.query(&opts.from);
        let resolved = resolve_anchor_fuzzy(&opts.from, &results);
        if resolved != opts.from {
            eprintln!("INFO: Resolved '{}' → '{}'", opts.from, resolved);
        }
        let budget = if results.is_empty() {
            opts.budget
        } else {
            let avg_score: f64 =
                results.iter().map(|r| r.score).sum::<f64>() / results.len() as f64;
            let boost = (avg_score * 2.0).min(3.0) as usize;
            (opts.budget * (1 + boost)).min(32000)
        };
        (resolved, budget)
    } else {
        (opts.from.clone(), opts.budget)
    };

    // Verify anchor exists, fallback if not found
    if !graph.anchor_to_index.contains_key(&resolved_anchor) {
        let anchor_lower = resolved_anchor.to_lowercase();
        let suggestions: Vec<String> = graph.anchor_to_index.keys()
            .filter(|a| {
                let lower = a.to_lowercase();
                lower.contains(&anchor_lower) || anchor_lower.len() >= 3 && 
                anchor_lower.chars().all(|c| lower.contains(c))
            })
            .take(5)
            .cloned()
            .collect();

        let mut final_suggestions = suggestions.clone();
        if final_suggestions.is_empty() {
            for fallback in &["readme", "index", "overview", "readme.md"] {
                if graph.anchor_to_index.contains_key(*fallback) {
                    final_suggestions.push(fallback.to_string());
                    break;
                }
            }
        }

        if !final_suggestions.is_empty() {
            println!(
                "WARNING: Anchor '{}' not found. Did you mean: {}?",
                resolved_anchor,
                final_suggestions.join(", ")
            );
            println!("         Use 'aden list .' to see available anchors.\n");
            resolved_anchor = final_suggestions[0].clone();
        } else {
            return Err(
                "No valid anchors found. Run 'aden list .' to see available anchors.".into(),
            );
        }
    }

    if opts.inspect {
        println!("=== Context Assembly Inspection ===");
        println!("Start: {}", resolved_anchor.clone());
        println!("Depth: {}", opts.depth);
        println!("Budget: {} tokens (auto={}, strict={})", effective_budget, opts.auto, opts.strict);
        println!("\n=== Nodes to be included ===");

        let mut visited = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        if let Some(start_idx) = graph.get_index(&resolved_anchor) {
            queue.push_back((start_idx, 0usize));
            while let Some((node, d)) = queue.pop_front() {
                if visited.contains(&node) || d > opts.depth {
                    continue;
                }
                visited.insert(node);
                println!("  [{}] {}", d, graph.graph[node].anchor);
                for neighbor in graph.graph.neighbors_directed(node, Direction::Outgoing) {
                    if !visited.contains(&neighbor) {
                        queue.push_back((neighbor, d + 1));
                    }
                }
            }
        }
        return Ok(());
    }

    // llm_mode=true is the default. Only raw AsciiDoc (--format aden) disables it.
    let llm_mode = opts.format != "aden";
    let asm_opts = AssemblyOptions {
        start_anchor: resolved_anchor,
        max_depth: opts.depth,
        token_budget: effective_budget,
        edge_types: opts.edge_types.clone(),
        block_filter: Vec::new(),
        include_tags: opts.include_tags.clone(),
        exclude_tags: opts.exclude_tags.clone(),
        attributes: opts.attributes.clone(),
        llm_mode,
    };

    let output = match opts.format.as_str() {
        "adg" => assemble_adg(&graph, &asm_opts)?,
        "aden" | "llm" => assemble(&graph, &asm_opts)?,
        _ => return Err(format!("Unknown format: '{}'. Use 'llm' (default), 'adg', or 'aden' (raw AsciiDoc).", opts.format).into()),
    };

    if let Some(out_path) = &opts.out {
        std::fs::write(out_path, output)?;
        println!("Written assembly to {}", out_path.display());
    } else {
        if opts.silent {
            print!("{}", output);
        } else {
            println!("{}", output);
        }
    }
    Ok(())
}

pub fn cmd_query(
    path: &Path,
    from: Option<&str>,
    edge_type: Option<&str>,
    depth: usize,
    backlinks: Option<&str>,
    impact: Option<&str>,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if !path.is_dir() {
        return Err("query requires a directory path".into());
    }

    let graph = aden_graph::cache::build_from_directory_cached(path)?;

    let mode_count = from.is_some() as u8 + backlinks.is_some() as u8 + impact.is_some() as u8;
    if mode_count != 1 {
        return Err("exactly one of --from, --backlinks, or --impact must be specified".into());
    }

    let mut results = Vec::new();

    if let Some(anchor) = from {
        let start_idx = graph.get_index(anchor).ok_or_else(|| {
            format!(
                "Anchor '{}' not found. Run 'aden list .' to see available anchors.",
                anchor
            )
        })?;
        let filter_type = if let Some(et) = edge_type {
            let valid = valid_edge_types().join(", ");
            Some(
                parse_single_edge_type(et)
                    .ok_or_else(|| format!("invalid edge type: '{}'. Valid: {}", et, valid))?,
            )
        } else {
            None
        };

        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        visited.insert(start_idx);
        queue.push_back((start_idx, 0usize));
        results.push(node_to_json(&graph.graph[start_idx], 0));

        while let Some((node, d)) = queue.pop_front() {
            if d > depth {
                continue;
            }
            for neighbor in graph.graph.neighbors_directed(node, Direction::Outgoing) {
                let weight = graph
                    .graph
                    .find_edge(node, neighbor)
                    .and_then(|e| graph.graph.edge_weight(e))
                    .copied()
                    .unwrap_or(aden_core::EdgeType::Uses);
                if let Some(ft) = filter_type
                    && weight != ft
                {
                    continue;
                }
                if visited.insert(neighbor) {
                    results.push(node_to_json(&graph.graph[neighbor], d + 1));
                    queue.push_back((neighbor, d + 1));
                }
            }
        }
    } else if let Some(anchor) = backlinks {
        let target_idx = graph.get_index(anchor).ok_or_else(|| {
            format!(
                "Anchor '{}' not found. Run 'aden list .' to see available anchors.",
                anchor
            )
        })?;
        for neighbor in graph
            .graph
            .neighbors_directed(target_idx, Direction::Incoming)
        {
            results.push(node_to_json(&graph.graph[neighbor], 1));
        }
    } else if let Some(anchor) = impact {
        let start_idx = graph.get_index(anchor).ok_or_else(|| {
            format!(
                "Anchor '{}' not found. Run 'aden list .' to see available anchors.",
                anchor
            )
        })?;
        let impact_types = [
            aden_core::EdgeType::Uses,
            aden_core::EdgeType::Calls,
            aden_core::EdgeType::Constrains,
            aden_core::EdgeType::Invokes,
        ];

        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        visited.insert(start_idx);
        queue.push_back((start_idx, 0usize));
        results.push(node_to_json(&graph.graph[start_idx], 0));

        while let Some((node, d)) = queue.pop_front() {
            for neighbor in graph.graph.neighbors_directed(node, Direction::Outgoing) {
                let weight = graph
                    .graph
                    .find_edge(node, neighbor)
                    .and_then(|e| graph.graph.edge_weight(e))
                    .copied()
                    .unwrap_or(aden_core::EdgeType::Uses);
                if !impact_types.contains(&weight) {
                    continue;
                }
                if visited.insert(neighbor) {
                    results.push(node_to_json(&graph.graph[neighbor], d + 1));
                    queue.push_back((neighbor, d + 1));
                }
            }
        }
    }

    match format {
        "table" => {
            println!("| Anchor | Depth | Node Type |\n|=== |");
            for r in results {
                println!("| {} | {} | {} |", r["anchor"], r["depth"], r["node_type"]);
            }
        }
        _ => {
            println!("{}", serde_json::to_string_pretty(&results)?);
        }
    }
    Ok(())
}

/// Intent classification helpers.
pub fn classify_intent(question: &str) -> QueryIntent {
    let q = question.to_lowercase();
    if q.contains("fail")
        || q.contains("error")
        || q.contains("panic")
        || q.contains("crash")
        || q.contains("broken")
    {
        QueryIntent::Debug
    } else if q.contains("how do i")
        || q.contains("how to")
        || q.contains("usage")
        || q.contains("example")
    {
        QueryIntent::Usage
    } else if q.contains("refactor") || q.contains("rewrite") || q.contains("rename") {
        QueryIntent::Refactor
    } else if q.contains("depend")
        || q.contains("blast radius")
        || q.contains("what uses")
        || q.contains("who calls")
    {
        QueryIntent::Impact
    } else if q.contains("what is")
        || q.contains("what does")
        || q.contains("explain")
        || q.contains("how does")
    {
        QueryIntent::Explain
    } else if q.contains("list")
        || q.contains("show me all")
        || q.contains("give me a list")
        || q.contains("what are all")
    {
        QueryIntent::List
    } else if q.contains("compare")
        || q.contains("difference between")
        || q.contains("versus")
        || q.contains("vs ")
    {
        QueryIntent::Compare
    } else if q.contains("how many")
        || q.contains("count ")
        || q.contains("number of")
        || q.contains("total ")
    {
        QueryIntent::Count
    } else {
        QueryIntent::General
    }
}

pub fn edge_types_for_intent(intent: &QueryIntent) -> Vec<aden_core::EdgeType> {
    use aden_core::EdgeType::*;
    // Include both code edges AND semantic edges for all intents
    let semantic = vec![IsA, PartOf, RelatesTo, SimilarTo, AssociatedWith, Explains];
    match intent {
        QueryIntent::Debug => vec![Constrains, Documents, Calls, Invokes, Requires].into_iter().chain(semantic.clone()).collect(),
        QueryIntent::Usage => vec![Uses, Invokes, Requires, Documents].into_iter().chain(semantic.clone()).collect(),
        QueryIntent::Explain => vec![Uses, Calls, Implements, Documents].into_iter().chain(semantic.clone()).collect(),
        QueryIntent::Refactor => vec![Calls, Uses, Mutates, Supersedes, Amends].into_iter().chain(semantic.clone()).collect(),
        QueryIntent::Impact => vec![Uses, Calls, Constrains].into_iter().chain(semantic.clone()).collect(),
        QueryIntent::List => vec![Uses, Documents].into_iter().chain(semantic.clone()).collect(),
        QueryIntent::Compare => vec![Uses, Documents, Constrains].into_iter().chain(semantic.clone()).collect(),
        QueryIntent::Count => vec![Documents, Uses].into_iter().chain(semantic.clone()).collect(),
        QueryIntent::General => vec![Uses, Documents, Constrains].into_iter().chain(semantic).collect(),
    }
}

pub fn depth_for_intent(intent: &QueryIntent) -> usize {
    match intent {
        QueryIntent::Debug => 3,
        QueryIntent::Usage => 2,
        QueryIntent::Explain => 2,
        QueryIntent::Refactor => 4,
        QueryIntent::Impact => 3,
        QueryIntent::List => 1,
        QueryIntent::Compare => 2,
        QueryIntent::Count => 1,
        QueryIntent::General => 2,
    }
}

pub fn block_filter_for_intent(intent: &QueryIntent) -> Vec<aden_asm::traverse::BlockKind> {
    use aden_asm::traverse::BlockKind::*;
    match intent {
        QueryIntent::Debug => vec![Table, Admonition, Paragraph],
        QueryIntent::Usage => vec![Listing, Table, DescriptionList],
        QueryIntent::Explain => vec![Paragraph, Table, Listing],
        QueryIntent::Refactor => vec![Table, Admonition, Paragraph],
        QueryIntent::Impact => vec![Table, Listing],
        QueryIntent::List => vec![Table, Listing, DescriptionList],
        QueryIntent::Compare => vec![Paragraph, Table],
        QueryIntent::Count => vec![Table, Listing],
        QueryIntent::General => vec![Paragraph, Table, Listing, Admonition, DescriptionList],
    }
}

pub fn cmd_ask(
    path: &Path,
    question: &str,
    from_override: Option<&str>,
    budget: usize,
    model: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    use aden_asm::traverse::{AssemblyOptions, assemble};

    if !path.is_dir() {
        return Err("ask requires a directory path".into());
    }

    // Step 1: Resolve question to an anchor via search, or use override
    let mut start_anchor = if let Some(anchor) = from_override {
        anchor.to_string()
    } else {
        let idx = load_or_build_index(path)?;
        let results = idx.query(question);
        if results.is_empty() {
            println!("No relevant documents found for: {}", question);
            println!(
                "Tips:\n  - Use more specific keywords from the codebase.\n  - Try `aden search <term>` to see available anchors.\n  - Or pin an anchor with --from <anchor>."
            );
            return Ok(());
        }
        resolve_anchor_fuzzy(question, &results)
    };

    println!("// Aden Ask: '{}' → [[{}]]", question, start_anchor);
    if from_override.is_some() {
        println!("// (pinned by --from)");
    }
    println!();

    // Step 2: Classify intent and route assembly strategy
    let intent = classify_intent(question);
    let edge_types = edge_types_for_intent(&intent);
    let depth = depth_for_intent(&intent);

    println!(
        "// Strategy: {:?} | Depth: {} | Edges: {:?}",
        intent,
        depth,
        edge_types
            .iter()
            .map(|e| format!("{:?}", e))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!();

    // Step 3: Build graph and assemble context
    let graph = aden_graph::cache::build_from_directory_cached(path)?;

    // Verify anchor exists, fallback if not found
    if !graph.anchor_to_index.contains_key(&start_anchor) {
        // Try fuzzy matching to find similar anchor
        let anchor_lower = start_anchor.to_lowercase();
        let suggestions: Vec<String> = graph.anchor_to_index.keys()
            .filter(|a| {
                let lower = a.to_lowercase();
                lower.contains(&anchor_lower) || anchor_lower.len() >= 3 && 
                anchor_lower.chars().all(|c| lower.contains(c))
            })
            .take(5)
            .cloned()
            .collect();

        let mut final_suggestions = suggestions.clone();
        if final_suggestions.is_empty() {
            // Try common fallbacks
            for fallback in &["readme", "index", "overview", "readme.md"] {
                if graph.anchor_to_index.contains_key(*fallback) {
                    final_suggestions.push(fallback.to_string());
                    break;
                }
            }
        }

        if !final_suggestions.is_empty() {
            println!(
                "WARNING: Anchor '{}' not found. Did you mean: {}?",
                start_anchor,
                final_suggestions.join(", ")
            );
            println!("         Use 'aden list .' to see available anchors.\n");
            start_anchor = final_suggestions[0].clone();
        } else {
            return Err(
                "No valid anchors found. Run 'aden list .' to see available anchors.".into(),
            );
        }
    }

    let block_filter = block_filter_for_intent(&intent);
    let edge_types_str = edge_types.iter().map(|e| format!("{:?}", e)).collect::<Vec<_>>().join(", ");
    let opts = AssemblyOptions {
        start_anchor: start_anchor.clone(),
        max_depth: depth,
        token_budget: budget,
        edge_types,
        block_filter,
        include_tags: Vec::new(),
        exclude_tags: Vec::new(),
        attributes: Vec::new(),
        llm_mode: true, // aden ask always targets an LLM — emit clean prose
    };
    let assembled = assemble(&graph, &opts)?;

    // Step 4: Send to LLM or print raw context
    if let Some(model_spec) = model {
        query_llm(model_spec, question, &assembled, &start_anchor)?;
    } else {
        // Show context with metadata for LLMs
        println!("<!-- ADEN CONTEXT ASSEMBLY -->");
        println!("<!-- Question: {} -->", question);
        println!("<!-- Anchor: {} | Depth: {} | Budget: {} -->", start_anchor, depth, budget);
        println!("<!-- Strategy: {:?} -->", intent);
        println!("<!-- Edge Types: {} -->", edge_types_str);
        println!();
        
        let consumed = assembled.len();
        let budget_label = if consumed > budget {
            "OVER BUDGET"
        } else {
            "on budget"
        };
        let page_breaks = assembled.matches("\n<<<\n").count();
        let node_count = page_breaks + 1;

        println!("{}", assembled);
        println!();
        println!("// ────────────────────────────────────────────────");
        println!("// Aden Ask Summary");
        println!("//   Question: {}", question);
        println!("//   Anchor  : [[{}]]", start_anchor);
        println!("//   Strategy: {:?} | Depth: {}", intent, depth);
        println!(
            "//   Nodes   : {} | Bytes: {} / {} ({})",
            node_count, consumed, budget, budget_label
        );
        println!("// ────────────────────────────────────────────────");
    }

    Ok(())
}

fn query_llm(
    model_spec: &str,
    question: &str,
    context: &str,
    anchor: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let system_prompt = format!(
        r#"You are an expert software engineering assistant analyzing a codebase.
The user asked: "{}"
I have retrieved the relevant context starting from anchor [[{}]].
Please answer the question based ONLY on the provided context. If the context does not contain enough information, say so explicitly.

Context begins below (--- separates different documents):
"#,
        question, anchor
    );

    let full_prompt = format!("{}\n{}\n", system_prompt, context);

    let (provider, model_name) = if let Some(pos) = model_spec.find(':') {
        (&model_spec[..pos], &model_spec[pos + 1..])
    } else {
        // Auto-detect: try ollama first
        if std::process::Command::new("ollama")
            .arg("list")
            .output()
            .is_ok()
        {
            ("ollama", model_spec)
        } else {
            return Err(
                "No LLM provider prefix given (e.g., ollama:llama3) and ollama is not available"
                    .into(),
            );
        }
    };

    match provider {
        "ollama" => {
            println!("Asking ollama ({}) via stdin...", model_name);
            let mut child = std::process::Command::new("ollama")
                .args(["run", model_name])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()?;

            if let Some(stdin) = child.stdin.take() {
                use std::io::Write;
                let mut stdin = stdin;
                stdin.write_all(full_prompt.as_bytes())?;
                // drop stdin to signal EOF
            }

            let output = child.wait_with_output()?;
            if output.status.success() {
                let response = String::from_utf8_lossy(&output.stdout);
                println!("\n=== LLM Response ===\n{}", response);
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(format!("ollama run failed: {}", stderr).into());
            }
        }
        "openai" => {
            let api_key = std::env::var("OPENAI_API_KEY")
                .map_err(|_| "OPENAI_API_KEY not set. Export it to use --model openai:<name>")?;
            println!("QueryingOpenAI ({})...", model_name);

            let payload = serde_json::json!({
                "model": model_name,
                "messages": [
                    { "role": "system", "content": &system_prompt },
                    { "role": "user", "content": context }
                ],
                "temperature": 0.3,
                "max_tokens": 2048
            });

            let output = std::process::Command::new("curl")
                .args([
                    "-sS",
                    "https://api.openai.com/v1/chat/completions",
                    "-H",
                    &format!("Authorization: Bearer {}", api_key),
                    "-H",
                    "Content-Type: application/json",
                    "-d",
                    &payload.to_string(),
                ])
                .output()?;

            if output.status.success() {
                let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
                if let Some(content) = json["choices"][0]["message"]["content"].as_str() {
                    println!("\n=== LLM Response ===\n{}", content);
                } else {
                    println!(
                        "Unexpected OpenAI response: {}",
                        String::from_utf8_lossy(&output.stdout)
                    );
                }
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(format!("OpenAI API call failed: {}", stderr).into());
            }
        }
        other => {
            return Err(format!(
                "Unknown LLM provider '{}'. Supported: ollama:<model>, openai:<model>",
                other
            )
            .into());
        }
    }

    Ok(())
}

pub fn cmd_query_adq(path: &Path, script: &str) -> Result<(), Box<dyn std::error::Error>> {
    if !path.is_dir() {
        return Err("query-adq requires a directory path".into());
    }

    let graph = aden_graph::cache::build_from_directory_cached(path)?;
    let result = aden_graph::query::execute_adq(&graph, script)?;

    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

pub fn cmd_search(
    path: &Path,
    query: &str,
    limit: usize,
    offset: usize,
    doc_type: Option<&str>,
    include_semantics: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !path.is_dir() {
        return Err("search requires a directory path".into());
    }

    // Load config to check for private patterns (ADRs, retros, etc.)
    let config = AdenConfig::load(path);

    let index = load_or_build_index(path)?;
    let mut results = index.query(query);

    // Filter out private anchors (ADRs, retros, kickoffs, etc.) in public mode
    let is_public = matches!(config.profile.mode, aden_core::ProfileMode::Public);
    if is_public {
        results.retain(|r| !config.is_private_anchor(&r.anchor));
    }

    // Filter by document type if specified
    if let Some(dt) = doc_type {
        let dt_pattern = match dt.to_lowercase().as_str() {
            "module" | "mod" => "mod-",
            "adr" => "adr-",
            "plan" => "plan-",
            "use-case" | "usecase" => "use-case-",
            "agent" => "agent-",
            _ => {
                eprintln!("Warning: Unknown doc type '{}'. Valid: module, adr, plan, use-case, agent", dt);
                return Err(format!("Invalid --type '{}'. Use: module, adr, plan, use-case, agent", dt).into());
            }
        };
        results.retain(|r| r.anchor.starts_with(dt_pattern));
    }

    // If --semantics, also search the graph for semantic relationships
    let mut semantic_results: Vec<(String, String)> = Vec::new();
    if include_semantics {
        if let Ok(graph) = aden_graph::cache::build_from_directory_cached(path) {
    let query_lower = query.to_lowercase();
            for edge_idx in graph.graph.edge_indices() {
                let (src, tgt) = graph.graph.edge_endpoints(edge_idx).expect("valid edge");
                let edge_type = &graph.graph[edge_idx];
                if edge_type.is_semantic() {
                    let src_anchor = graph.graph[src].anchor.to_lowercase();
                    let tgt_anchor = graph.graph[tgt].anchor.to_lowercase();
                    if src_anchor.contains(&query_lower) || tgt_anchor.contains(&query_lower) {
                        semantic_results.push((
                            graph.graph[tgt].anchor.clone(),
                            format!("{:?} via {:?}", edge_type, graph.graph[src].anchor),
                        ));
                    }
                }
            }
        }
    }

    if results.is_empty() && semantic_results.is_empty() {
        println!("No results for '{}'", query);
        return Ok(());
    }

    let total = results.len();
    let limited: Vec<_> = results.into_iter().skip(offset).take(limit).collect();

    println!("Showing {}/{} results (offset={})", limited.len(), total, offset);
    println!("| Anchor | Score | Snippet |");
    println!("|=== |");
    for r in &limited {
        let snippet = if r.snippet.len() > 80 {
            format!("{}...", &r.snippet[..80])
        } else {
            r.snippet.clone()
        };
        println!("| {} | {:.1} | {} |", r.anchor, r.score, snippet);
    }

    // Print semantic results if any
    if !semantic_results.is_empty() {
        println!();
        println!("Semantic relationships (--semantics):");
        println!("| Anchor | Relationship |");
        println!("|=== |");
        for (anchor, rel) in &semantic_results {
            println!("| {} | {} |", anchor, rel);
        }
    }
    Ok(())
}

pub fn cmd_list(
    path: &Path,
    filter: Option<&str>,
    verbose: bool,
    limit: usize,
    offset: usize,
    semantics_only: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !path.is_dir() {
        return Err("list requires a directory path".into());
    }

    let graph = aden_graph::cache::build_from_directory_cached(path)?;

    // If semantics_only, collect only nodes that are part of semantic relationships
    let anchors: Vec<String> = if semantics_only {
        let mut semantic_anchors: std::collections::HashSet<String> = std::collections::HashSet::new();
        for edge_idx in graph.graph.edge_indices() {
            let edge_type = &graph.graph[edge_idx];
            if edge_type.is_semantic() {
                let (src, tgt) = graph.graph.edge_endpoints(edge_idx).expect("valid edge");
                semantic_anchors.insert(graph.graph[src].anchor.clone());
                semantic_anchors.insert(graph.graph[tgt].anchor.clone());
            }
        }
        semantic_anchors.into_iter().collect()
    } else {
        graph
            .graph
            .node_indices()
            .filter_map(|idx| graph.graph.node_weight(idx).map(|n| n.anchor.clone()))
            .collect()
    };

    let filtered: Vec<_> = match filter {
        Some(f) => anchors.iter().filter(|a| a.contains(f)).cloned().collect(),
        None => anchors,
    };
    let total_count = filtered.len();
    let limited: Vec<_> = filtered.into_iter().skip(offset).take(limit).collect();

    let offset_info = if offset > 0 {
        format!(" (offset={})", offset)
    } else {
        String::new()
    };
    println!(
        "Anchors in {}{} (showing {}/total {})",
        path.display(),
        offset_info,
        limited.len(),
        total_count
    );
    println!();

    if verbose {
        println!("| Anchor | Type | Source File |");
        println!("|=== |");
        for anchor in &limited {
            if let Some(idx) = graph.anchor_to_index.get(anchor)
                && let Some(n) = graph.graph.node_weight(*idx)
            {
                let node_type = n
                    .doc
                    .attributes
                    .get("node-type")
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string());
                let source = n.source_path.to_string_lossy().to_string();
                println!("| {} | {} | {} |", anchor, node_type, source);
            }
        }
    } else {
        println!("| Anchor |");
        println!("|=== |");
        for anchor in &limited {
            println!("| {} |", anchor);
        }
    }

    if limited.len() == limit && total_count > limit {
        println!(
            "\n... {} more (use --limit or --offset to see more)",
            total_count - limit
        );
    }

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
        if ctx > 0 && !file.is_empty() {
            if let Ok(lines) = std::fs::read_to_string(file) {
                let start_num: usize = start.parse().unwrap_or(1);
                let end_num: usize = end.parse().unwrap_or(start_num);
                let before = start_num.saturating_sub(ctx);
                let after = end_num + ctx;
                let all_lines: Vec<&str> = lines.lines().collect();
                if before < all_lines.len() && before < after {
                    println!("  Context (lines {}-{}):", before + 1, after.min(all_lines.len()));
                    for (i, line) in all_lines.iter().enumerate().take(after).skip(before) {
                        let line_num = i + 1;
                        let marker = if line_num >= start_num && line_num <= end_num { ">" } else { " " };
                        println!("{}{:4}: {}", marker, line_num, line);
                    }
                }
            }
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
) -> Result<(), Box<dyn std::error::Error>> {
    use serde_json::json;

    if !path.is_dir() {
        return Err("locate requires a directory path".into());
    }

    let graph = aden_graph::cache::build_from_directory_cached(path)?;

    // If --symbol is given, find the definition.
    if let Some(sym) = symbol {
        let sym_lower = sym.to_lowercase();
        let mut hits = Vec::new();

        for node in graph.graph.node_indices() {
            let anchor = &graph.graph[node].anchor;
            let anchor_lower = anchor.to_lowercase();

            // Case-insensitive match: exact suffix, #suffix, or partial
            if anchor_lower.ends_with(&sym_lower)
                || anchor_lower.contains(&format!("#{}", &sym_lower))
                || anchor_lower.contains(&sym_lower)
            {
                let attrs = &graph.graph[node].doc.attributes;
                let file = attrs.get("source_file").cloned().unwrap_or_default();
                let start_line = attrs.get("start_line").cloned().unwrap_or_default();
                let end_line = attrs.get("end_line").cloned().unwrap_or_default();
                let node_type = attrs
                    .get("node-type")
                    .cloned()
                    .unwrap_or_else(|| format!("{:?}", graph.graph[node].doc.node_type));
                hits.push(json!({
                    "anchor": anchor,
                    "node_type": node_type,
                    "file": file,
                    "start_line": start_line,
                    "end_line": end_line,
                }));
            }
        }

        if hits.is_empty() {
            // Try fuzzy search - match any part of the anchor
            let mut fuzzy_hits = Vec::new();
            let search_term = sym.to_lowercase();
            for node in graph.graph.node_indices() {
                let anchor = &graph.graph[node].anchor;
                let anchor_lower = anchor.to_lowercase();
                if anchor_lower.contains(&search_term)
                    || anchor_lower.split('#').any(|p| p.contains(&search_term))
                {
                    let attrs = &graph.graph[node].doc.attributes;
                    let file = attrs.get("source_file").cloned().unwrap_or_default();
                    let start_line = attrs.get("start_line").cloned().unwrap_or_default();
                    let end_line = attrs.get("end_line").cloned().unwrap_or_default();
                    let node_type = attrs
                        .get("node-type")
                        .cloned()
                        .unwrap_or_else(|| format!("{:?}", graph.graph[node].doc.node_type));
                    fuzzy_hits.push(json!({
                        "anchor": anchor,
                        "node_type": node_type,
                        "file": file,
                        "start_line": start_line,
                        "end_line": end_line,
                    }));
                }
            }

            if fuzzy_hits.is_empty() {
                // Fall back to full-text search index
                let index = load_or_build_index(path)?;
                let search_results = index.query(sym);

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
                        println!("| {} | {:.1} | {} |", r.anchor, r.score, snippet);
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
            println!("Found {} fuzzy match(es) for '{}':", fuzzy_hits.len(), sym);
            let fuzzy_limited: Vec<_> = fuzzy_hits.iter().take(limit).cloned().collect();
            print_locate_results(&fuzzy_limited, format, context);
            return Ok(());
        }

        println!("Found {} match(es) for '{}':", hits.len(), sym);
        let hits_limited: Vec<_> = hits.iter().take(limit).cloned().collect();
        if format == "json" {
            println!("{}", serde_json::to_string_pretty(&hits_limited)?);
        } else {
            print_locate_results(&hits_limited, format, context);
        }
        return Ok(());
    }

    // If --caller-of is given, show call sites (requires call-graph edges with span metadata).
    if let Some(_target) = caller_of {
        println!("caller-of requires call-graph edges with line metadata (not yet implemented)");
        println!("Use 'aden graph --from <anchor> --depth 1' for module-level callers instead.");
        return Ok(());
    }

    Err("locate requires one of --symbol or --caller-of".into())
}

#[cfg(feature = "watch")]
pub fn cmd_watch(
    path: &Path,
    graph_sync: bool,
    restore: bool,
    sync_all: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
    use std::sync::mpsc::channel;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use std::collections::HashSet;

    if !path.is_dir() {
        return Err("watch requires a directory path".into());
    }

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    // Setup ctrl-c handler
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    }).expect("Error setting Ctrl-C handler");

    // Optional: Restore graph from cache for faster startup
    let mut graph: Option<aden_graph::graph::AdenGraph> = None;
    if graph_sync && restore {
        println!("Restoring graph from cache...");
        match aden_graph::cache::build_from_directory_cached(path) {
            Ok(g) => {
                let anchor_count = g.graph.node_indices().count();
                println!("Restored graph ({} anchors)", anchor_count);
                graph = Some(g);
            }
            Err(e) => {
                println!("Note: Could not restore graph (will build fresh): {}", e);
            }
        }
    }

    let (tx, rx) = channel();
    let mut watcher = RecommendedWatcher::new(
        move |res: Result<notify::Event, notify::Error>| {
            if let Ok(event) = res {
                let _ = tx.send(event);
            }
        },
        Config::default(),
    )?;

    watcher.watch(path, RecursiveMode::Recursive)?;
    println!(
        "Watching {} for changes... Press Ctrl+C to stop.",
        path.display()
    );

    // Supported source extensions that parse_file can handle
    let source_exts = [
        "rs",
        "py",
        "js",
        "ts",
        "tsx",
        "jsx",
        "mjs",
        "cjs",
        "go",
        "java",
        "c",
        "cpp",
        "cc",
        "cxx",
        "h",
        "hpp",
        "rb",
        "cs",
        "swift",
        "kt",
        "scala",
        "zig",
        "lua",
        "hs",
        "ml",
        "php",
        "ex",
        "exs",
        "erl",
        "gleam",
        "sh",
        "bash",
        "dockerfile",
        "html",
        "css",
        "scss",
        "vue",
        "svelte",
        "proto",
        "tf",
        "cmake",
    ];

    // Contracts directory
    let contracts_dir = path.join("contracts");
    std::fs::create_dir_all(&contracts_dir)?;

    // Debounce state
    let debounce_duration = Duration::from_millis(100);
    let mut pending_paths: HashSet<std::path::PathBuf> = HashSet::new();
    let mut last_process_time = Instant::now();

    // Graph sync state
    let _graph_arc = if graph_sync {
        graph.map(|g| std::sync::Arc::new(std::sync::Mutex::new(g)))
    } else {
        None
    };

    println!("Watching {} for changes... Press Ctrl+C to stop.", path.display());
    if graph_sync {
        println!("Graph sync enabled - contracts and graph stay current.");
    }

    // Main event loop
    while running.load(Ordering::SeqCst) {
        // Process events with debouncing
        for event in rx.try_iter() {
            for p in &event.paths {
                if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                    let ext = ext.to_lowercase();
                    if source_exts.contains(&ext.as_str()) || ext == "adoc" || ext == "aden" {
                        pending_paths.insert(p.clone());
                    }
                }
            }
        }

        // Only process if debounce window passed
        if !pending_paths.is_empty() && last_process_time.elapsed() >= debounce_duration {
            let paths_to_process: Vec<_> = pending_paths.drain().collect();
            last_process_time = Instant::now();
            let mut contracts_regenerated = 0usize;

            // Process each changed file
            for p in &paths_to_process {
                if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                    let ext = ext.to_lowercase();

                    if source_exts.contains(&ext.as_str()) {
                        // Source file change - regenerate contract
                        println!("INFO: Source change: {}", p.file_name().unwrap_or_default().to_string_lossy());
                        if let Ok(source) = std::fs::read_to_string(p) {
                            match aden_parse::parse_file(p, &source) {
                                Ok(mut docs) if !docs.is_empty() => {
                                    for doc in &mut docs {
                                        sanitize_source_file(doc);
                                        let safe_anchor = sanitize_anchor(&doc.anchor);
                                        let out_path = contracts_dir.join(format!("{}.adoc", safe_anchor));
                                        if std::fs::write(&out_path, aden_emit::emit_document(doc)).is_ok() {
                                            contracts_regenerated += 1;
                                        }
                                    }
                                }
                                Ok(_) => {}
                                Err(aden_core::Error::UnsupportedLanguage(_)) => {}
                                Err(e) => eprintln!("ERROR: Parse failed: {}", e),
                            }
                        }
                    } else if ext == "adoc" || ext == "aden" {
                        // Doc file change - validate
                        if let Err(e) = perform_check(path) {
                            eprintln!("ERROR: Check failed: {}", e);
                        }
                    }
                }
            }

            // Summary
            if contracts_regenerated > 0 {
                println!("INFO: Regenerated {} contract(s)", contracts_regenerated);
                
                // Optional: Update graph incrementally
                if graph_sync {
                    // For now, just rebuild graph (incremental coming soon)
                    // TODO: Implement incremental graph update
                }
            }

            // Unified sync mode: run gen + check + heal
            if sync_all && !paths_to_process.is_empty() {
                println!("INFO: Running unified sync...");
                
                // Run check
                if let Err(e) = crate::util::perform_check(path) {
                    let has_errors = format!("{:?}", e).contains("ERROR:");
                    if has_errors {
                        eprintln!("CHECK: {}", e);
                    }
                } else {
                    println!("CHECK: All references valid");
                }

                // Run heal scan (summary only)
                #[cfg(feature = "watch")]
                {
                    use aden_heal::{Scanner, generate};
                    let scanner = Scanner::new(path);
                    if let Ok(events) = scanner.scan() {
                        let report = generate(events.clone(), path);
                        println!("HEAL: Health score = {:.2}", report.overall_score);
                        if !events.is_empty() {
                            println!("HEAL: {} drift event(s) detected", events.len());
                        }
                    }
                }
            }
        }

        // Small sleep to prevent CPU spinning
        std::thread::sleep(Duration::from_millis(10));
    }

    Ok(())
}
