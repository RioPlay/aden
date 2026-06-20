// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

pub fn cmd_suggest(intent: &str) -> Result<(), Box<dyn std::error::Error>> {
    let intent_lower = intent.to_lowercase();

    let suggestions = vec![
        (
            vec!["generate", "doc", "contract", "parse", "extract"],
            "gen",
            "aden gen . --auto",
            "Generate contracts from source code",
        ),
        (
            vec!["search", "find", "look"],
            "search",
            "aden search '<query>'",
            "Search for text in contracts",
        ),
        (
            vec!["list", "show", "all", "anchors", "contracts"],
            "list",
            "aden list .",
            "List all anchors in the graph",
        ),
        (
            vec!["ask", "question", "explain", "how", "what"],
            "ask",
            "aden ask '<question>'",
            "Ask a natural language question",
        ),
        (
            vec!["fix", "heal", "drift", "stale", "update"],
            "heal",
            "aden heal . --fix",
            "Auto-fix stale contracts",
        ),
        (
            vec!["check", "validate", "reference", "link"],
            "check",
            "aden check .",
            "Validate all cross-references",
        ),
        (
            vec!["graph", "depend", "neighbor", "related"],
            "graph",
            "aden graph --from <anchor> --depth 2",
            "Show graph neighborhood",
        ),
        (
            vec!["assemble", "context", "prompt", "token"],
            "asm",
            "aden asm --from <anchor> --budget 4096",
            "Assemble context within token budget",
        ),
        (
            vec!["locate", "symbol", "function", "where"],
            "locate",
            "aden locate --symbol <name> .",
            "Find symbol definition",
        ),
        (
            vec![
                "rename",
                "refactor",
                "blast",
                "impact",
                "before i change",
                "before i rename",
                "safe to change",
                "downstream",
            ],
            "understand",
            "aden understand <symbol>",
            "One-shot: definition + callers (backlinks) + downstream impact for a symbol",
        ),
        (
            vec![
                "caller",
                "callers",
                "who calls",
                "called by",
                "backlink",
                "references",
                "used by",
                "usages",
                "dependents",
            ],
            "query",
            "aden query . --backlinks <anchor>",
            "List everything that references a symbol (blast radius)",
        ),
        (
            vec!["init", "scaffold", "setup"],
            "init",
            "aden init",
            "Scaffold .agent/ templates",
        ),
        (
            vec!["watch", "auto", "regenerate"],
            "watch",
            "aden watch .",
            "Watch for changes and auto-regenerate",
        ),
        (
            vec!["clean", "gc", "garbage", "orphan"],
            "gc",
            "aden heal . --gc",
            "Garbage collect orphaned contracts",
        ),
        (
            vec!["doctor", "diagnose", "health", "check environment"],
            "doctor",
            "aden doctor .",
            "Check environment health",
        ),
    ];

    let mut matches: Vec<_> = suggestions
        .iter()
        .filter(|(keywords, _, _, _)| keywords.iter().any(|k| intent_lower.contains(k)))
        .collect();

    matches.sort_by_key(|a| std::cmp::Reverse(a.0.len()));

    println!("Aden Suggestion for: \"{}\"", intent);
    println!("====================");
    println!();

    if matches.is_empty() {
        println!("No exact match found. Try:");
        println!("  aden gen . --auto         # Generate contracts");
        println!("  aden search '<query>'    # Search contracts");
        println!("  aden ask '<question>'    # Ask a question");
        println!("  aden list .              # List all anchors");
        println!("  aden heal . --fix         # Fix drift");
    } else {
        println!("Try one of these commands:\n");
        for (_, cmd, example, desc) in &matches {
            println!("  {}: {}", cmd, desc);
            println!("    Example: {}\n", example);
        }
    }

    Ok(())
}
