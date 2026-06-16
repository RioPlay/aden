// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
// Assembly-quality A/B (measurement harness, #[ignore]d, real repo).
//
// Validates the query-aware assembly ordering committed in 173ea06: from a hub
// seed, focused by a query, at a TIGHT budget — does ordering the frontier by
// query relevance pull the gold answer node into the bundle better than the
// structural (edge_priority) order? Fair A/B: identical seed, budget, depth and
// cases; only the `relevance` toggle differs between the two arms.
//
// Real repo graph (default ~/Projects/eval-repos/flask, override ADEN_ASM_REPO).
// Hand-authored hub→gold cases (overfit caveat: small, illustrative). A
// reachability gate (large-budget inclusion) drops cases the seed cannot reach,
// so only cases where ordering can actually matter are scored.
//
// Run: cargo test -p aden-cli --test assembly_ab -- --include-ignored --nocapture

use aden_asm::traverse::{AssemblyOptions, assemble_with_anchors};
use aden_core::Block;
use aden_index::Index;
use std::collections::HashMap;
use std::path::PathBuf;

fn repo() -> Option<PathBuf> {
    let p = std::env::var("ADEN_ASM_REPO")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join("Projects/eval-repos/flask")
        });
    p.is_dir().then_some(p)
}

/// Searchable prose of a node (docstrings) for relevance scoring. The symbol
/// name itself reaches the index via the `[[anchor]]` line.
fn node_text(doc: &aden_core::Document) -> String {
    let mut s = String::new();
    for b in &doc.blocks {
        if let Block::Paragraph(t) = b {
            s.push_str(t);
            s.push(' ');
        }
    }
    s
}

/// Seed the assembly at `hub`, focus it with `query`, expect a node whose anchor
/// contains `gold`.
struct Case {
    hub: &'static str,
    query: &'static str,
    gold: &'static str,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            hub: "Flask application object class",
            query: "handle errors and exceptions raised in a view",
            gold: "handle_exception",
        },
        Case {
            hub: "Flask application object class",
            query: "register a blueprint on the application",
            gold: "register_blueprint",
        },
        Case {
            hub: "Flask application object class",
            query: "the wsgi application entry point callable",
            gold: "wsgi_app",
        },
        Case {
            hub: "Flask application object class",
            query: "add a url rule for routing requests",
            gold: "add_url_rule",
        },
        Case {
            hub: "Flask application object class",
            query: "run the local development server",
            gold: "run",
        },
        Case {
            hub: "Flask application object class",
            query: "create the configuration object",
            gold: "config",
        },
        Case {
            hub: "Blueprint class for modular routes",
            query: "register a url rule on the blueprint",
            gold: "add_url_rule",
        },
        Case {
            hub: "request object incoming data",
            query: "parse json from the request body",
            gold: "json",
        },
        Case {
            hub: "config configuration object",
            query: "load configuration from a python file",
            gold: "from_pyfile",
        },
        Case {
            hub: "session cookie interface",
            query: "save the session into a secure cookie",
            gold: "save_session",
        },
    ]
}

#[test]
#[ignore = "measurement harness, not a CI gate; reads an external repo"]
fn assembly_ab_report() {
    let Some(repo) = repo() else {
        eprintln!("SKIP: eval repo not found (set ADEN_ASM_REPO)");
        return;
    };
    // The store-backed loader: the edge-rich graph the gen pipeline wrote (Calls,
    // Uses, Contains …), the same one `asm`/`ask` load. Requires `aden gen <repo>`
    // to have run. (`AdenGraph::build_from_directory` would give a doc-only,
    // edge-less graph — assembly traverses edges, so it must be the store graph.)
    let graph = match aden_graph::cache::build_from_directory_cached(&repo) {
        Ok(g) => g,
        Err(e) => {
            eprintln!(
                "SKIP: could not load store graph for {}: {e}",
                repo.display()
            );
            return;
        }
    };

    // BM25 index over the graph's own nodes, so relevance anchors == graph
    // anchors. `[[anchor]]` carries the symbol name; node_text adds the docstring.
    let mut index = Index::default();
    let entries: Vec<(PathBuf, String)> = graph
        .all_nodes()
        .into_iter()
        .enumerate()
        .map(|(i, (anchor, node))| {
            (
                PathBuf::from(format!("n{i}.adoc")),
                format!("[[{anchor}]]\n{}\n", node_text(&node.doc)),
            )
        })
        .collect();
    index.ingest(entries);
    index.finalize();

    let includes_gold = |opts: &AssemblyOptions, gold: &str| -> bool {
        assemble_with_anchors(&graph, opts)
            .map(|(_, inc)| inc.iter().any(|a| a.contains(gold)))
            .unwrap_or(false)
    };

    let budgets = [128usize, 256, 512, 1024];
    const REACH_BUDGET: usize = 16384; // generous: is gold reachable from the seed at all?

    println!(
        "\n=== Assembly A/B: structural vs query-aware ordering ===\n\
         Repo: {} | {} nodes, {} edges | budgets {:?}",
        repo.display(),
        graph.node_count(),
        graph.edge_count(),
        budgets
    );

    let mut struct_hits = vec![0usize; budgets.len()];
    let mut aware_hits = vec![0usize; budgets.len()];
    let mut scored = 0usize;

    for c in cases() {
        let Some(seed) = index.query(c.hub).first().map(|r| r.anchor.clone()) else {
            println!("  [skip] hub '{}' resolved nothing", c.hub);
            continue;
        };

        let rel: HashMap<String, f32> = index
            .query(c.query)
            .into_iter()
            .map(|r| (r.anchor, r.score as f32))
            .collect();

        let mk = |budget: usize, relevance: Option<HashMap<String, f32>>| AssemblyOptions {
            start_anchor: seed.clone(),
            max_depth: 3,
            token_budget: budget,
            relevance,
            ..Default::default()
        };

        // Reachability gate: if the seed can't reach gold even at a huge budget,
        // ordering is irrelevant — drop the case rather than score a structural
        // dead end as a loss for both arms.
        if !includes_gold(&mk(REACH_BUDGET, None), c.gold) {
            let tail = seed.rsplit('#').next().unwrap_or(&seed);
            println!(
                "  [unreachable] seed={tail:<28} gold={:<20} (seed cannot reach gold; skipped)",
                c.gold
            );
            continue;
        }
        scored += 1;

        let mut row_s = String::new();
        let mut row_a = String::new();
        for (bi, &b) in budgets.iter().enumerate() {
            let s = includes_gold(&mk(b, None), c.gold);
            let a = includes_gold(&mk(b, Some(rel.clone())), c.gold);
            if s {
                struct_hits[bi] += 1;
            }
            if a {
                aware_hits[bi] += 1;
            }
            row_s.push(if s { 'Y' } else { '.' });
            row_a.push(if a { 'Y' } else { '.' });
        }
        let tail = seed.rsplit('#').next().unwrap_or(&seed);
        println!(
            "  seed={tail:<28} gold={:<20} struct[{row_s}] aware[{row_a}]",
            c.gold
        );
    }

    println!("\n  reachable cases scored: {scored}");
    println!("  structural gold-inclusion by budget {budgets:?}: {struct_hits:?}");
    println!("  query-aware gold-inclusion by budget {budgets:?}: {aware_hits:?}");
}
