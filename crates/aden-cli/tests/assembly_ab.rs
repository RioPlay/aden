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
use aden_graph::Direction;
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

fn cases(repo: &str) -> Vec<Case> {
    if repo == "kin-openapi" {
        // CROSS-LANGUAGE (Go): unlike Rust's edge-less types, a Go struct hub HAS
        // outgoing Uses edges to its field-types (confirmed: `understand Schema` ->
        // 7 downstream nodes), but methods attach as INCOMING backlinks (like Rust).
        // So reachable golds are downstream Uses-targets (field-types), not methods.
        // Tests whether gather-then-select reorders a Uses frontier toward a
        // query-relevant deep field-type better than the structural walk.
        return vec![
            Case {
                hub: "Components openapi components container",
                query: "security scheme definitions",
                gold: "SecuritySchemes",
            },
            Case {
                hub: "Components openapi components container",
                query: "request body definitions",
                gold: "RequestBodies",
            },
            Case {
                hub: "Operation openapi path operation",
                query: "external documentation links",
                gold: "ExternalDocs",
            },
            Case {
                hub: "Schema openapi schema object",
                query: "source location of a field",
                gold: "Location",
            },
        ];
    }
    if repo == "rustfmt" {
        // CROSS-LANGUAGE FINDING (kept as reproducible evidence, not a passing eval):
        // these Flask-shaped class-hub cases come back UNREACHABLE on Rust, and that IS
        // the result. In aden's Rust graph a type node has ZERO outgoing edges
        // (confirmed: `understand FmtVisitor` -> downstream 0 nodes); impl methods attach
        // to the module/impl and only REFERENCE the type (incoming backlinks). So a
        // class-hub seed cannot reach its methods via outgoing Contains the way it does
        // in Python. Rust navigation is Calls-based (priority 0, already top of the
        // walk), so gather-then-select's low-priority-Contains RESCUE has no analogue
        // here. Conclusion: the mechanism is validated for the Contains-heavy regime
        // (Python classes, prose sections), NOT a universal uplift. See the devlog.
        return vec![
            Case {
                hub: "FmtVisitor source formatting visitor",
                query: "format the statements inside a code block",
                gold: "walk_block_stmts",
            },
            Case {
                hub: "FmtVisitor source formatting visitor",
                query: "format a macro invocation",
                gold: "visit_mac",
            },
            Case {
                hub: "FmtVisitor source formatting visitor",
                query: "format an item declared inside a trait",
                gold: "visit_trait_item",
            },
            Case {
                hub: "FmtVisitor source formatting visitor",
                query: "format a type alias definition",
                gold: "visit_ty_alias_kind",
            },
            Case {
                hub: "FmtVisitor source formatting visitor",
                query: "walk and format the items of a module",
                gold: "walk_mod_items",
            },
        ];
    }
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

/// Precision controls: off-topic queries on real hubs. No member is the answer, so
/// relevance is noise — promotion should NOT disturb the structural assembly here.
/// Low structural retention under these = over-aggressive promotion (pulling in
/// topically-adjacent-but-wrong members when there is no clear winner).
fn negatives(repo: &str) -> Vec<(&'static str, &'static str)> {
    if repo == "kin-openapi" {
        return vec![
            (
                "Components openapi components container",
                "numeric tensor matrix multiplication kernels",
            ),
            (
                "Schema openapi schema object",
                "establish a tcp network socket connection",
            ),
            (
                "Operation openapi path operation",
                "rotating file log handler buffering",
            ),
        ];
    }
    if repo == "rustfmt" {
        return vec![
            (
                "ast visitor that formats source code items",
                "numeric tensor matrix multiplication kernels",
            ),
            (
                "ast visitor that formats source code items",
                "establish a tcp network socket connection",
            ),
        ];
    }
    vec![
        (
            "Flask application object class",
            "sqlite database migration and schema versioning",
        ),
        (
            "Blueprint class for modular routes",
            "rotating file log handler buffering",
        ),
        (
            "request object incoming data",
            "command line argument parsing subcommands",
        ),
        (
            "session cookie interface",
            "numeric tensor matrix multiplication kernels",
        ),
    ]
}

#[test]
#[ignore = "measurement harness, not a CI gate; reads an external repo"]
fn assembly_ab_report() {
    let Some(repo) = repo() else {
        eprintln!("SKIP: eval repo not found (set ADEN_ASM_REPO)");
        return;
    };
    // Select the case set by repo dir name (flask | rustfmt), so the same harness
    // validates the gate across languages: run once per repo via ADEN_ASM_REPO.
    let repo_name = repo
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
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

    // Relevance signal: HYBRID (dense + BM25) when built with `--features dense`
    // and the bge model is present — the SAME signal production `ask` uses, so
    // this measures the real feature rather than the BM25 floor. The harness
    // embeds the graph nodes itself, so the repo need not be gen'd with dense.
    // Falls back to BM25 when dense is absent so the harness still runs.
    let embedder: Option<Box<dyn aden_index::EmbeddingProvider>> = {
        #[cfg(feature = "dense")]
        {
            let dir = PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join(".cache/aden-models/bge-small-en-v1.5");
            match aden_index::TractEmbedder::from_dir(&dir) {
                Ok(e) => Some(Box::new(e) as Box<dyn aden_index::EmbeddingProvider>),
                Err(e) => {
                    eprintln!("dense model unavailable ({e}); falling back to BM25");
                    None
                }
            }
        }
        #[cfg(not(feature = "dense"))]
        {
            None
        }
    };
    if let Some(e) = &embedder {
        index.embed_documents(e.as_ref());
    }
    let relevance_mode = if embedder.is_some() {
        "HYBRID (dense+BM25)"
    } else {
        "BM25"
    };
    // Single query path: hybrid when an embedder is loaded, BM25 otherwise.
    let do_query = |q: &str| -> Vec<aden_index::SearchResult> {
        match &embedder {
            Some(e) => index.hybrid_query(q, e.as_ref()),
            None => index.query(q),
        }
    };

    let includes_gold = |opts: &AssemblyOptions, gold: &str| -> bool {
        assemble_with_anchors(&graph, opts)
            .map(|(_, inc)| inc.iter().any(|a| a.contains(gold)))
            .unwrap_or(false)
    };
    // Resolve a prose hub to the most-connected node among its top BM25 matches
    // (the real structural hub); shared by the positive and precision-control loops.
    let resolve_hub = |hub: &str| -> Option<String> {
        index
            .query(hub)
            .iter()
            .take(8)
            // A hub is a SYMBOL (class/type), not a whole-file module node, which
            // out-ranks everything on out-degree and isn't what we seed assembly at.
            .filter(|r| r.anchor.contains('#') && !r.anchor.starts_with("mod-"))
            .filter_map(|r| {
                graph.get_index(&r.anchor).map(|ix| {
                    let deg = graph
                        .graph
                        .neighbors_directed(ix, Direction::Outgoing)
                        .count();
                    (r.anchor.clone(), deg)
                })
            })
            .max_by_key(|(_, deg)| *deg)
            .map(|(anchor, _)| anchor)
    };
    let included_anchors = |opts: &AssemblyOptions| -> Vec<String> {
        assemble_with_anchors(&graph, opts)
            .map(|(_, inc)| inc)
            .unwrap_or_default()
    };

    let budgets = [128usize, 256, 512, 1024];
    const REACH_BUDGET: usize = 16384; // generous: is gold reachable from the seed at all?

    println!(
        "\n=== Assembly A/B: structural vs query-aware ordering ===\n\
         Repo: {} | {} nodes, {} edges | budgets {:?} | relevance: {}",
        repo.display(),
        graph.node_count(),
        graph.edge_count(),
        budgets,
        relevance_mode
    );

    let mut struct_hits = vec![0usize; budgets.len()];
    let mut aware_hits = vec![0usize; budgets.len()];
    let mut select_hits = vec![0usize; budgets.len()]; // relevance_select (gather-then-select)
    let mut scored = 0usize;
    // Gather-then-select proxy: how often gold lands in the relevance-rank top-K of
    // the generously-gathered set (would survive a relevance-driven compression).
    let topk = [5usize, 10, 25];
    let mut rank_top = [0usize; 3];

    for c in cases(&repo_name) {
        // Seed resolution stays on BM25 deliberately: the seed is experimental SETUP,
        // held fixed across all arms AND across the BM25-vs-hybrid comparison, so the
        // only thing that varies is the relevance treatment. (Hybrid seed resolution
        // drifts the hub onto test helpers here, a separate routing question.)
        let Some(seed) = resolve_hub(c.hub) else {
            println!("  [skip] hub '{}' resolved nothing", c.hub);
            continue;
        };

        let rel: HashMap<String, f32> = do_query(c.query)
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

        // Gather-then-select proxy (the "lift the budget, then compress" idea): at
        // the reach budget gold IS gathered, so rank the gathered nodes by relevance
        // — gold's rank is where a relevance-driven compression would place it. A low
        // rank means gather-then-select keeps gold even at a tiny output budget,
        // without any ordering trick during the walk.
        let (_, gathered) =
            assemble_with_anchors(&graph, &mk(REACH_BUDGET, Some(rel.clone()))).unwrap_or_default();
        let mut ranked: Vec<&String> = gathered.iter().collect();
        ranked.sort_by(|a, b| {
            let ra = rel.get(*a).copied().unwrap_or(0.0);
            let rb = rel.get(*b).copied().unwrap_or(0.0);
            rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal)
        });
        let gold_rank = ranked.iter().position(|a| a.contains(c.gold));
        for (ti, &k) in topk.iter().enumerate() {
            if gold_rank.is_some_and(|r| r < k) {
                rank_top[ti] += 1;
            }
        }

        let mut row_s = String::new();
        let mut row_a = String::new();
        let mut row_sel = String::new();
        for (bi, &b) in budgets.iter().enumerate() {
            let s = includes_gold(&mk(b, None), c.gold);
            let a = includes_gold(&mk(b, Some(rel.clone())), c.gold);
            // Gather-then-select arm: same seed/budget/relevance, but the new mode.
            let sel = includes_gold(
                &AssemblyOptions {
                    relevance_select: true,
                    ..mk(b, Some(rel.clone()))
                },
                c.gold,
            );
            if s {
                struct_hits[bi] += 1;
            }
            if a {
                aware_hits[bi] += 1;
            }
            if sel {
                select_hits[bi] += 1;
            }
            row_s.push(if s { 'Y' } else { '.' });
            row_a.push(if a { 'Y' } else { '.' });
            row_sel.push(if sel { 'Y' } else { '.' });
        }
        let tail = seed.rsplit('#').next().unwrap_or(&seed);
        let rank_str = match gold_rank {
            Some(r) => format!("rel-rank #{}/{}", r + 1, gathered.len()),
            None => "rel-rank NA".to_string(),
        };
        println!(
            "  seed={tail:<26} gold={:<18} struct[{row_s}] aware[{row_a}] select[{row_sel}] {rank_str}",
            c.gold
        );
    }

    println!("\n  reachable cases scored: {scored}");
    println!("  structural    gold-inclusion by budget {budgets:?}: {struct_hits:?}");
    println!("  query-aware   gold-inclusion by budget {budgets:?}: {aware_hits:?}");
    println!("  gather-select gold-inclusion by budget {budgets:?}: {select_hits:?}");

    // PRECISION control: on off-topic queries, gather-select should preserve the
    // structural assembly (retention ~ 1.00). A low number means promotion is firing
    // on noise — the cost of leaning in, which recall alone cannot see.
    println!(
        "\n  Precision control (off-topic queries; retention = overlap with structural, ~1.00 = safe):"
    );
    let mut retain_sum = vec![0.0f64; budgets.len()];
    let mut neg_scored = 0usize;
    for (hub, q) in negatives(&repo_name) {
        let Some(seed) = resolve_hub(hub) else {
            continue;
        };
        let rel: HashMap<String, f32> = do_query(q)
            .into_iter()
            .map(|r| (r.anchor, r.score as f32))
            .collect();
        let mk_neg = |b: usize, relevance: Option<HashMap<String, f32>>| AssemblyOptions {
            start_anchor: seed.clone(),
            max_depth: 3,
            token_budget: b,
            relevance,
            ..Default::default()
        };
        neg_scored += 1;
        let mut row = String::new();
        for (bi, &b) in budgets.iter().enumerate() {
            let s_set: std::collections::HashSet<String> =
                included_anchors(&mk_neg(b, None)).into_iter().collect();
            let sel = included_anchors(&AssemblyOptions {
                relevance_select: true,
                ..mk_neg(b, Some(rel.clone()))
            });
            let kept = sel.iter().filter(|a| s_set.contains(*a)).count();
            let ret = if s_set.is_empty() {
                1.0
            } else {
                kept as f64 / s_set.len() as f64
            };
            retain_sum[bi] += ret;
            row.push_str(&format!("{ret:.2} "));
        }
        let tail = seed.rsplit(['#', '/']).next().unwrap_or(&seed);
        println!("    seed={tail:<26} retention[{}]", row.trim_end());
    }
    if neg_scored > 0 {
        let avg: Vec<String> = retain_sum
            .iter()
            .map(|s| format!("{:.2}", s / neg_scored as f64))
            .collect();
        println!(
            "    mean structural retention by budget {budgets:?}: [{}]",
            avg.join(", ")
        );
    }
    println!(
        "  gather-then-select: gold in relevance-rank top-{topk:?} of the gathered set: {rank_top:?} (of {scored})"
    );
}
