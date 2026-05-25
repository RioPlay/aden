use std::collections::{HashSet, VecDeque};
use std::path::Path;

use aden_graph::Direction;

use crate::types::QueryIntent;
use crate::util::{
    load_or_build_index, node_to_json, parse_single_edge_type, perform_check, sanitize_anchor, sanitize_source_file,
};

pub fn cmd_check(path: &Path, severity: &str) -> Result<(), Box<dyn std::error::Error>> {
    if !path.is_dir() {
        return Err("check requires a directory path".into());
    }

    let min_severity = match severity.to_lowercase().as_str() {
        "suggest" => 0,
        "warn" => 1,
        "forbid" => 2,
        _ => return Err(format!("Invalid severity '{}': use Suggest, Warn, or Forbid", severity).into()),
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

pub fn cmd_graph(path: &Path, from: &str, depth: usize) -> Result<(), Box<dyn std::error::Error>> {
    if !path.is_dir() {
        return Err("graph requires a directory path".into());
    }

    let graph = aden_graph::cache::build_from_directory_cached(path)?;
    let start_idx = graph.get_index(from).ok_or_else(|| format!("Anchor '{}' not found", from))?;

    println!("Graph neighborhood from anchor '{}' (depth <= {})", from, depth);
    println!("| Anchor | Depth | |\n|=== |");

    let mut visited = std::collections::HashSet::new();
    let mut queue = std::collections::VecDeque::new();
    queue.push_back((start_idx, 0usize));

    while let Some((node, d)) = queue.pop_front() {
        if visited.contains(&node) || d > depth {
            continue;
        }
        visited.insert(node);
        println!("| {} | {} |", graph.graph[node].anchor, d);
        for neighbor in graph.graph.neighbors_directed(node, Direction::Outgoing) {
            if !visited.contains(&neighbor) {
                queue.push_back((neighbor, d + 1));
            }
        }
    }
    Ok(())
}

pub fn cmd_asm(
    path: &Path,
    from: &str,
    depth: usize,
    budget: usize,
    edge_types: Vec<aden_core::EdgeType>,
    out: Option<&Path>,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use aden_asm::traverse::{assemble, assemble_adg, AssemblyOptions};

    if !path.is_dir() {
        return Err("asm requires a directory path".into());
    }

    let graph = aden_graph::cache::build_from_directory_cached(path)?;
    let opts = AssemblyOptions {
        start_anchor: from.to_string(),
        max_depth: depth,
        token_budget: budget,
        edge_types,
        block_filter: Vec::new(),
    };

    let output = match format {
        "adg" => assemble_adg(&graph, &opts)?,
        "aden" => assemble(&graph, &opts)?,
        _ => return Err(format!("Unknown format '{}': use 'aden' or 'adg'", format).into()),
    };

    if let Some(out_path) = out {
        std::fs::write(out_path, output)?;
        println!("Written assembly to {}", out_path.display());
    } else {
        println!("{output}");
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
        let start_idx = graph
            .get_index(anchor)
            .ok_or_else(|| format!("Anchor '{}' not found", anchor))?;
        let filter_type = if let Some(et) = edge_type {
            Some(parse_single_edge_type(et).ok_or_else(|| format!("invalid edge type: {}", et))?)
        } else {
            None
        };

        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        visited.insert(start_idx);
        queue.push_back((start_idx, 0usize));
        results.push(node_to_json(&graph.graph[start_idx], 0));

        while let Some((node, d)) = queue.pop_front() {
            if d >= depth {
                continue;
            }
            for neighbor in graph.graph.neighbors_directed(node, Direction::Outgoing) {
                let weight = graph.graph.find_edge(node, neighbor)
                    .and_then(|e| graph.graph.edge_weight(e))
                    .copied()
                    .unwrap_or(aden_core::EdgeType::Uses);
                if let Some(ft) = filter_type
                    && weight != ft {
                        continue;
                    }
                if visited.insert(neighbor) {
                    results.push(node_to_json(&graph.graph[neighbor], d + 1));
                    queue.push_back((neighbor, d + 1));
                }
            }
        }
    } else if let Some(anchor) = backlinks {
        let target_idx = graph
            .get_index(anchor)
            .ok_or_else(|| format!("Anchor '{}' not found", anchor))?;
        for neighbor in graph.graph.neighbors_directed(target_idx, Direction::Incoming) {
            results.push(node_to_json(&graph.graph[neighbor], 1));
        }
    } else if let Some(anchor) = impact {
        let start_idx = graph
            .get_index(anchor)
            .ok_or_else(|| format!("Anchor '{}' not found", anchor))?;
        let impact_types = [aden_core::EdgeType::Uses,
            aden_core::EdgeType::Calls,
            aden_core::EdgeType::Constrains,
            aden_core::EdgeType::Invokes];

        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        visited.insert(start_idx);
        queue.push_back((start_idx, 0usize));
        results.push(node_to_json(&graph.graph[start_idx], 0));

        while let Some((node, d)) = queue.pop_front() {
            for neighbor in graph.graph.neighbors_directed(node, Direction::Outgoing) {
                let weight = graph.graph.find_edge(node, neighbor)
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

    println!("{}", serde_json::to_string_pretty(&results)?);
    Ok(())
}

/// Intent classification helpers.
pub fn classify_intent(question: &str) -> QueryIntent {
    let q = question.to_lowercase();
    if q.contains("fail") || q.contains("error") || q.contains("panic") || q.contains("crash") || q.contains("broken") {
        QueryIntent::Debug
    } else if q.contains("how do i") || q.contains("how to") || q.contains("usage") || q.contains("example") {
        QueryIntent::Usage
    } else if q.contains("refactor") || q.contains("rewrite") || q.contains("rename") {
        QueryIntent::Refactor
    } else if q.contains("depend") || q.contains("blast radius") || q.contains("what uses") || q.contains("who calls") {
        QueryIntent::Impact
    } else if q.contains("what is") || q.contains("what does") || q.contains("explain") || q.contains("how does") {
        QueryIntent::Explain
    } else {
        QueryIntent::General
    }
}

pub fn edge_types_for_intent(intent: &QueryIntent) -> Vec<aden_core::EdgeType> {
    use aden_core::EdgeType::*;
    match intent {
        QueryIntent::Debug => vec![Constrains, Documents, Calls, Invokes, Requires],
        QueryIntent::Usage => vec![Uses, Invokes, Requires, Documents],
        QueryIntent::Explain => vec![Uses, Calls, Implements, Documents],
        QueryIntent::Refactor => vec![Calls, Uses, Mutates, Supersedes, Amends],
        QueryIntent::Impact => vec![Uses, Calls, Constrains],
        QueryIntent::General => vec![Uses, Documents, Constrains],
    }
}

pub fn depth_for_intent(intent: &QueryIntent) -> usize {
    match intent {
        QueryIntent::Debug => 3,
        QueryIntent::Usage => 2,
        QueryIntent::Explain => 2,
        QueryIntent::Refactor => 4,
        QueryIntent::Impact => 3,
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
    use aden_asm::traverse::{assemble, AssemblyOptions};

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
            println!("Tips:\n  - Use more specific keywords from the codebase.\n  - Try `aden search <term>` to see available anchors.\n  - Or pin an anchor with --from <anchor>.");
            return Ok(());
        }
        results[0].anchor.clone()
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

    println!("// Strategy: {:?} | Depth: {} | Edges: {:?}", intent, depth,
             edge_types.iter().map(|e| format!("{:?}", e)).collect::<Vec<_>>().join(", "));
    println!();

    // Step 3: Build graph and assemble context
    let graph = aden_graph::cache::build_from_directory_cached(path)?;

    // Verify anchor exists, fallback if not found
    if !graph.anchor_to_index.contains_key(&start_anchor) {
        println!("WARNING: Anchor '{}' not found. Falling back to 'readme'.", start_anchor);
        println!("         Use 'aden list .' to see available anchors.\n");
        let fallback = "readme";
        if graph.anchor_to_index.contains_key(fallback) {
            start_anchor = fallback.to_string();
        } else {
            return Err("No valid anchors found. Run 'aden list .' to see available anchors.".into());
        }
    }

    let block_filter = block_filter_for_intent(&intent);
    let opts = AssemblyOptions {
        start_anchor: start_anchor.clone(),
        max_depth: depth,
        token_budget: budget,
        edge_types,
        block_filter,
    };
    let assembled = assemble(&graph, &opts)?;

    // Step 4: Send to LLM or print raw context
    if let Some(model_spec) = model {
        query_llm(model_spec, question, &assembled, &start_anchor)?;
    } else {
        let consumed = assembled.len();
        let budget_label = if consumed > budget { "OVER BUDGET" } else { "on budget" };
        let page_breaks = assembled.matches("\n<<<\n").count();
        let node_count = page_breaks + 1;

        println!("{}", assembled);
        println!();
        println!("// ────────────────────────────────────────────────");
        println!("// Aden Ask Summary");
        println!("//   Question: {}", question);
        println!("//   Anchor  : [[{}]]", start_anchor);
        println!("//   Strategy: {:?} | Depth: {}", intent, depth);
        println!("//   Nodes   : {} | Bytes: {} / {} ({})", node_count, consumed, budget, budget_label);
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
        if std::process::Command::new("ollama").arg("list").output().is_ok() {
            ("ollama", model_spec)
        } else {
            return Err("No LLM provider prefix given (e.g., ollama:llama3) and ollama is not available".into());
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
                    "-sS", "https://api.openai.com/v1/chat/completions",
                    "-H", &format!("Authorization: Bearer {}", api_key),
                    "-H", "Content-Type: application/json",
                    "-d", &payload.to_string(),
                ])
                .output()?;

            if output.status.success() {
                let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;
                if let Some(content) = json["choices"][0]["message"]["content"].as_str() {
                    println!("\n=== LLM Response ===\n{}", content);
                } else {
                    println!("Unexpected OpenAI response: {}", String::from_utf8_lossy(&output.stdout));
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

pub fn cmd_search(path: &Path, query: &str) -> Result<(), Box<dyn std::error::Error>> {
    if !path.is_dir() {
        return Err("search requires a directory path".into());
    }

    let index = load_or_build_index(path)?;
    let results = index.query(query);

    if results.is_empty() {
        println!("No results for '{}'", query);
        return Ok(());
    }

    println!("| Anchor | Score | Snippet |");
    println!("|=== |");
    for r in &results {
        let snippet = if r.snippet.len() > 80 {
            format!("{}...", &r.snippet[..80])
        } else {
            r.snippet.clone()
        };
        println!("| {} | {:.1} | {} |", r.anchor, r.score, snippet);
    }
    Ok(())
}

pub fn cmd_list(
    path: &Path,
    filter: Option<&str>,
    verbose: bool,
    limit: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    if !path.is_dir() {
        return Err("list requires a directory path".into());
    }

    let graph = aden_graph::cache::build_from_directory_cached(path)?;
    let anchors: Vec<_> = graph.graph.node_indices().filter_map(|idx| {
        graph.graph.node_weight(idx).map(|n| n.anchor.clone())
    }).collect();

    let filtered: Vec<_> = match filter {
        Some(f) => anchors.iter().filter(|a| a.contains(f)).cloned().collect(),
        None => anchors,
    };

    let limited: Vec<_> = filtered.into_iter().take(limit).collect();

    println!("Anchors in {} (showing {}/total)", path.display(), limited.len());
    println!();

    if verbose {
        println!("| Anchor | Type | Source File |");
        println!("|=== |");
        for anchor in &limited {
            if let Some(idx) = graph.anchor_to_index.get(anchor)
                && let Some(n) = graph.graph.node_weight(*idx) {
                let node_type = n.doc.attributes.get("node-type").cloned().unwrap_or_else(|| "unknown".to_string());
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

    if limited.len() >= limit {
        println!("\n... {} more (use --limit to see more)", limited.len() - limit);
    }

    Ok(())
}

fn print_locate_results(hits: &[serde_json::Value], format: &str) {
    if format == "json" {
        println!("{}", serde_json::to_string_pretty(&hits).unwrap_or_default());
        return;
    }
    // Token-efficient output: compact format
    for h in hits {
        let file = h["file"].as_str().unwrap_or("");
        let start = h["start_line"].as_str().unwrap_or("");
        let anchor = h["anchor"].as_str().unwrap_or("");
        let nt = h["node_type"].as_str().unwrap_or("");

        // Extract symbol name from anchor for brevity
        let symbol = anchor.split('#').last().unwrap_or(anchor);

        if file.is_empty() || start.is_empty() {
            println!("{} {} [{}]", symbol, nt, anchor);
        } else {
            println!("{} {} {}:{}", symbol, nt, file, start);
        }
    }
}

pub fn cmd_locate(
    path: &Path,
    symbol: Option<&str>,
    caller_of: Option<&str>,
    format: &str,
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
                if anchor_lower.contains(&search_term) || anchor_lower.split('#').any(|p| p.contains(&search_term)) {
                    let attrs = &graph.graph[node].doc.attributes;
                    let file = attrs.get("source_file").cloned().unwrap_or_default();
                    let start_line = attrs.get("start_line").cloned().unwrap_or_default();
                    let end_line = attrs.get("end_line").cloned().unwrap_or_default();
                    let node_type = attrs.get("node-type").cloned().unwrap_or_else(|| format!("{:?}", graph.graph[node].doc.node_type));
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
                println!("No symbol found matching '{}'", sym);
                println!("Hint: Try 'aden search \"{}\"' to find related anchors", sym);
                return Ok(());
            }
            println!("Found {} fuzzy match(es) for '{}':", fuzzy_hits.len(), sym);
            print_locate_results(&fuzzy_hits, format);
            return Ok(());
        }

        println!("Found {} match(es) for '{}':", hits.len(), sym);
        if format == "json" {
            println!("{}", serde_json::to_string_pretty(&hits)?);
        } else {
            print_locate_results(&hits, format);
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
pub fn cmd_watch(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
    use std::sync::mpsc::channel;

    if !path.is_dir() {
        return Err("watch requires a directory path".into());
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
    println!("Watching {} for changes... Press Ctrl+C to stop.", path.display());

    // Supported source extensions that parse_file can handle
    let source_exts = [
        "rs", "py", "js", "ts", "tsx", "jsx", "mjs", "cjs", "go",
        "java", "c", "cpp", "cc", "cxx", "h", "hpp", "rb", "cs",
        "swift", "kt", "scala", "zig", "lua", "hs", "ml", "php",
        "ex", "exs", "erl", "gleam", "sh", "bash", "dockerfile",
        "html", "css", "scss", "vue", "svelte", "proto", "tf",
        "cmake",
    ];

    // Contracts directory
    let contracts_dir = path.join("contracts");
    std::fs::create_dir_all(&contracts_dir)?;

    for event in rx {
        for p in &event.paths {
            if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                let ext = ext.to_lowercase();
                if source_exts.contains(&ext.as_str()) {
                    println!("INFO: Source change detected in {}", p.display());
                    if let Ok(source) = std::fs::read_to_string(p) {
                        match aden_parse::parse_file(p, &source) {
                            Ok(mut docs) if !docs.is_empty() => {
                                for doc in &mut docs {
                                    sanitize_source_file(doc);
                                    let safe_anchor = sanitize_anchor(&doc.anchor);
                                    let out_path = contracts_dir.join(format!("{}.adoc", safe_anchor));
                                    if let Err(e) = std::fs::write(&out_path, aden_emit::emit_document(doc)) {
                                        eprintln!("ERROR: Failed to write {}: {}", out_path.display(), e);
                                    } else {
                                        println!("INFO: Regenerated {}", out_path.display());
                                    }
                                }
                            }
                            Ok(_) => {}
                            Err(aden_core::Error::UnsupportedLanguage(_)) => {
                                // Silently skip; may be a file extension we don't support yet.
                            }
                            Err(e) => eprintln!("ERROR: Parse failed for {}: {}", p.display(), e),
                        }
                    }
                } else if matches!(ext.as_str(), "adoc" | "aden") {
                    println!("INFO: Doc change detected in {}", p.display());
                    // Validate
                    match perform_check(path) {
                        Ok(messages) => {
                            for msg in messages {
                                println!("{}", msg);
                            }
                        }
                        Err(e) => eprintln!("ERROR: Check failed: {}", e),
                    }
                }
            }
        }
    }
    Ok(())
}