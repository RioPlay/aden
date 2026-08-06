// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::io::Read;
use std::path::Path;

use aden_asm::traverse::{AssemblyOptions, assemble};
use aden_graph::cache::build_from_directory_cached;
use walkdir::WalkDir;

/// Scan incomplete contracts and preview the context/prompts needed to complete them.
///
/// This command is currently report-only: it never invokes an LLM or writes contract
/// files. The model argument is reserved for a future explicit integration.
pub fn cmd_complete(
    path: &Path,
    dry_run: bool,
    model: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    if !path.is_dir() {
        return Err("complete requires a directory path".into());
    }

    let incomplete = find_incomplete_contracts(path)?;

    if incomplete.is_empty() {
        println!("No incomplete contracts found.");
        return Ok(());
    }

    println!("Found {} incomplete contract(s):", incomplete.len());
    for (file, anchor) in &incomplete {
        println!("  - {} (anchor: {})", file.display(), anchor);
    }
    println!();

    if dry_run {
        println!("Dry-run mode: no changes made.");
        println!("Use without --dry-run to preview the context and prompt.");
        return Ok(());
    }

    if model.is_none() {
        println!("Note: LLM completion requires --model flag (e.g., --model=ollama:llama3)");
        println!("Currently running in scan-only mode.");
        return Ok(());
    }

    let _model_spec = model.unwrap();
    let graph = build_from_directory_cached(path)?;

    for (_file, anchor) in &incomplete {
        println!("Processing: {}", anchor);

        let asm_opts = AssemblyOptions {
            start_anchor: anchor.clone(),
            max_depth: 5,
            token_budget: 4096,
            edge_types: Vec::new(),
            block_filter: Vec::new(),
            include_tags: Vec::new(),
            exclude_tags: Vec::new(),
            attributes: Vec::new(),
            llm_mode: true, // complete targets an LLM — emit clean prose
            hydrate_root: None,
            relevance: None,
            relevance_select: false,
            relevance_confidence: None,
        };

        let context = match assemble(&graph, &asm_opts) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("  Warning: Could not assemble context: {}", e);
                continue;
            }
        };

        let prompt = format!(
            r#"You are a documentation assistant. Complete the following contract.

The contract "{}" has a [must-complete] block that requires:
1. A description explaining what this symbol does
2. Relationships - links to parent modules, related symbols, or semantic connections

Contract anchor: {}

Current context:
{}

Please provide:
## Description
[Write a clear description of what this symbol does]

## Relationships  
[Add <<refs>> to related symbols. Format: <<anchor,display text>>
If this symbol belongs to a larger module or package, cross-reference it
using the module's anchor (e.g. <<mod-mypackage,mypackage>>).]
"#,
            anchor, anchor, context
        );

        // Report-only by design: no model invocation or file mutation occurs.
        println!(
            "  Prompt preview: {} chars (no files changed)",
            prompt.len()
        );
    }

    println!(
        "\nPreview complete: inspected {} contract(s); no files changed",
        incomplete.len()
    );
    Ok(())
}

fn find_incomplete_contracts(
    path: &Path,
) -> Result<Vec<(std::path::PathBuf, String)>, Box<dyn std::error::Error>> {
    let mut incomplete = Vec::new();

    for entry in WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let p = entry.path().to_path_buf();
        if !p.is_file() {
            continue;
        }
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "adoc" && ext != "aden" {
            continue;
        }
        let mut text = String::new();
        if let Ok(mut file) = std::fs::File::open(&p) {
            let _ = file.read_to_string(&mut text);
            if text.contains("[must-complete]") {
                // Reverse the filesystem-safe encoding used by aden-gen:
                // "aden---module-foo-bar.rs-MyStruct" → "aden://module/foo/bar.rs#MyStruct"
                // For anchors in other schemes, use the stem as-is.
                let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown");
                let anchor = if stem.contains("---module-") {
                    stem.replacen("---module-", "://module/", 1)
                        .replacen("---", "/", 1)
                } else {
                    stem.to_string()
                };
                incomplete.push((p, anchor));
            }
        }
    }

    Ok(incomplete)
}
