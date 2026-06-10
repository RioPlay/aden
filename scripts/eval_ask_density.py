#!/usr/bin/env python3
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""End-to-end `ask` DENSITY & IMMEDIACY eval: drive the real `aden` binary.

The routing-side sibling (`eval_corpus.py --mode ask`) judges WHERE `ask` lands;
this harness judges WHAT it delivers once it gets there. Per the design record
(research: ask-density-and-routing.adoc), `ask` was observed routing correctly
and then assembling 22 of 4115 budgeted tokens (0.5% density). These metrics
are acceptance criteria, not diagnostics — the gates below must hold post-fix.

Per query it runs `aden ask "<q>" <repo> --explain`, parses the routed anchor
(`// Aden Ask:` header), the final anchor + token/budget figures (summary
trailer), and the assembled body, then scores:

  routed-hit   the routed (pre-fallback) anchor matches the expected set
  anchor-hit   the FINAL (post-fallback) anchor matches the expected set;
               `routed-hit - anchor-hit` makes fallback damage visible
  density      est_tokens(substantive body) / effective_budget, with the same
               bytes.div_ceil(4) estimator the assembler budgets by.
               *Substantive* = body minus blank lines, `---` separators,
               `//` header/meta lines, bare-title lines (a line that is only a
               symbol/module name), and `calls:`-only lines.
  immediacy    binary: every must_contain string (identifiers/constants/match
               arms drawn from the ground-truth answer) appears in the body.
               Deterministic substring judging — no LLM in the loop.
  honesty      est_tokens(full body) <= effective_budget

Gates (from the design record): mean density >= 0.50, per-query density floor
>= 0.15, immediacy >= 12/15, budget honesty 15/15, anchor-hit >= 12/15.

The query set is embedded (not TSV) because must_contain items carry nested
accept-alternatives (e.g. `32_000` OR `AUTO_BUDGET_CAP`), which the 3-column
TSV convention cannot express without inventing a sub-separator dialect.

Usage:
    eval_ask_density.py --bin <aden> --repo <path> [--budget N] [--json] [--quiet]
"""
import argparse
import json
import re
import subprocess
import sys

# Each case: question, expected anchors (any substring match passes, mirroring
# eval_corpus.ask_case_passes), must_contain (list of items; each item is a
# list of accept-alternatives — ANY alternative satisfies the item), and an
# optional ci flag for case-insensitive judging of that case's strings.
CASES = [
    {
        "q": "Where are the MCP tool definitions registered and how does the tool list get built?",
        "expected": ["tool_from_spec", "TOOLS", "tool_arg_specs"],
        "must": [["tool_from_spec"], ["list_tools"], ["ToolSpec"]],
    },
    {
        "q": "How does heal decide the severity of a drift event?",
        "expected": ["DriftEvent::severity"],
        "must": [["BrokenReference"], ["Critical"], ["StaleHash"], ["High"]],
    },
    {
        "q": "How does ask classify the intent of a question?",
        "expected": ["classify_intent"],
        "must": [["Scores EVERY"], ["tie"], ["Debug"]],
    },
    {
        "q": "What happens when an ask anchor assembles almost nothing?",
        "expected": ["cmd_ask", "community_seed_for"],
        "must": [["thin"], ["mod-project"], ["community"]],
    },
    {
        "q": "How does the assembler decide which neighbors to spend the budget on first?",
        "expected": ["ordered_neighbors", "edge_priority"],
        "must": [["priority"], ["Calls"], ["anchor"]],
    },
    {
        "q": "How are callee names resolved into Calls edges at gen time?",
        "expected": ["resolve_callee", "link_store_edges"],
        "must": [["locality"], ["name_index"], ["ambiguous"]],
    },
    {
        "q": "How does the auto relevance boost scale the ask budget?",
        "expected": ["auto_boosted_budget"],
        "must": [["AUTO_BOOST_MAX"], ["clamp"], ["32_000", "AUTO_BUDGET_CAP"]],
    },
    {
        # The picker was `community_seed_for` when the design record was
        # written; the F3 fix renamed it `community_seeds_for` (relevance-
        # ranked candidates). The prefix matches both.
        "q": "How does the community fallback pick a richer seed for a thin anchor?",
        "expected": ["community_seed"],
        "must": [["degree"], ["mod-"], ["hub"]],
    },
    {
        "q": "Which edge types count as downstream impact in query --impact?",
        "expected": ["cmd_query"],
        "must": [["Uses"], ["Constrains"], ["Invokes"], ["Mutates"]],
    },
    {
        "q": "How is the BM25 relevance score computed?",
        "expected": ["add_bm25_scores"],
        "must": [["idf"], ["avg_doc_length", "k1", "b ="]],
    },
    {
        "q": "How does truncation guarantee the assembled output stays within the token budget?",
        "expected": ["truncate_to_tokens"],
        "must": [["4"], ["char boundary", "ELLIPSIS"]],
    },
    {
        "q": "What stops a module hub from exploding the neighborhood build?",
        "expected": ["build_neighborhood_cached", "is_hub_anchor"],
        "must": [["MAX_NODES"], ["hub"], ["d > 0", "intermediate"]],
    },
    {
        "q": "What counts as a test anchor when routing ask away from fixtures?",
        "expected": ["is_test_anchor", "is_test_result"],
        "must": [["/tests/"], [".spec."], ["markers"]],
        "ci": True,
    },
    {
        "q": "How does gen turn a symbol's signature into Uses edges for types?",
        "expected": ["link_store_edges", "extract_type_idents"],
        "must": [["Uses"], ["dead code", "use_records"]],
    },
    {
        "q": "What does understand bundle into one report?",
        "expected": ["cmd_understand"],
        "must": [["backlinks"], ["impact"], ["assemble", "locate"]],
    },
]

# `ask` output markers (same regexes as eval_corpus.py where shared).
ROUTED_RE = re.compile(r"^// Aden Ask: .* → \[\[(.+?)\]\]\s*$", re.MULTILINE)
FINAL_ANCHOR_RE = re.compile(r"^//\s+Anchor\s*:\s*\[\[(.+?)\]\]", re.MULTILINE)
# Summary trailer: `Nodes : N | ~T tokens (B bytes) / <budget...> budget (<label>)`
NODES_RE = re.compile(
    r"^//\s+Nodes\s*:\s*\d+\s*\|\s*~(\d+) tokens \((\d+) bytes\) / (\d+)[^/]*budget \((.+?)\)",
    re.MULTILINE,
)
EDGE_TYPES_LINE = "<!-- Edge Types:"
EXPLAIN_HEADER = "// ── Ask Routing Explain"
SUMMARY_HEADER = "// Aden Ask Summary"

# A bare-title line: a single symbol/module-name token, nothing else.
BARE_TITLE_RE = re.compile(r"^[A-Za-z0-9_.:#/\-]+$")


def est_tokens(text):
    """The assembler's estimator: bytes.div_ceil(4)."""
    n = len(text.encode("utf-8"))
    return max((n + 3) // 4, 1) if n else 0


def substantive_lines(body):
    """Body minus blank / `---` / `//`-meta / bare-title / `calls:`-only lines."""
    keep = []
    for line in body.splitlines():
        t = line.strip()
        if not t or t == "---":
            continue
        if t.startswith("//"):
            continue
        if t.startswith("calls:"):
            continue
        if BARE_TITLE_RE.fullmatch(t):
            continue
        keep.append(line)
    return "\n".join(keep)


def extract_body(stdout):
    """The assembled body: after the `<!-- Edge Types -->` header comment, up to
    the explain block (if present) or the summary rule. Forward scan: the
    explain block PRECEDES the summary, so the first trailer marker wins —
    a backwards scan would hit the summary first and swallow the explain
    block into the body. Hydrated source can indent these strings but never
    emits them at column 0, so the startswith match is unambiguous."""
    lines = stdout.splitlines()
    start = 0
    for i, ln in enumerate(lines):
        if ln.startswith(EDGE_TYPES_LINE):
            start = i + 1
            break
    end = len(lines)
    for i in range(start, len(lines)):
        if lines[i].startswith(EXPLAIN_HEADER):
            end = i
            break
        if lines[i].startswith(SUMMARY_HEADER):
            # the box rule line directly above the summary header belongs to it
            end = i - 1 if i > start else i
            break
    return "\n".join(lines[start:end]).strip("\n")


def anchor_hits(anchor, expected):
    if not anchor:
        return False
    a = anchor.lower()
    return any(e.lower() in a for e in expected)


def run_case(bin_path, repo, case, budget):
    cmd = [bin_path, "ask", case["q"], repo, "--explain"]
    if budget:
        cmd += ["--budget", str(budget)]
    out = subprocess.run(cmd, capture_output=True, text=True)
    if out.returncode != 0:
        print(f"  ! ask failed for {case['q']!r}: {out.stderr.strip()[:200]}",
              file=sys.stderr)
        return None
    stdout = out.stdout
    routed = ROUTED_RE.search(stdout)
    final = FINAL_ANCHOR_RE.search(stdout)
    nodes = NODES_RE.search(stdout)
    body = extract_body(stdout)
    eff_budget = int(nodes.group(3)) if nodes else 0

    hay = body if not case.get("ci") else body.lower()
    fold = (lambda s: s) if not case.get("ci") else (lambda s: s.lower())
    missing = [
        item for item in case["must"]
        if not any(fold(alt) in hay for alt in item)
    ]

    sub = substantive_lines(body)
    density = (est_tokens(sub) / eff_budget) if eff_budget else 0.0
    return {
        "q": case["q"],
        "routed": routed.group(1) if routed else None,
        "final": final.group(1) if final else None,
        "routed_hit": anchor_hits(routed.group(1) if routed else None, case["expected"]),
        "anchor_hit": anchor_hits(final.group(1) if final else None, case["expected"]),
        "budget": eff_budget,
        "body_tokens": est_tokens(body),
        "substantive_tokens": est_tokens(sub),
        "density": round(density, 4),
        "immediate": not missing,
        "missing": ["|".join(m) for m in missing],
        "honest": est_tokens(body) <= eff_budget if eff_budget else False,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True, help="path to the aden binary")
    ap.add_argument("--repo", required=True, help="path to the target repository")
    ap.add_argument("--budget", type=int, default=1024,
                    help="base --budget passed to ask (default 1024, which the "
                         "relevance boost scales to ~4096 effective — the same "
                         "conditions as the design record's evidence runs; "
                         "0 = ask's own default)")
    ap.add_argument("--json", action="store_true", help="emit metrics as JSON")
    ap.add_argument("--quiet", action="store_true", help="suppress per-query lines")
    args = ap.parse_args()

    rows = []
    for case in CASES:
        r = run_case(args.bin, args.repo, case, args.budget)
        if r is None:
            r = {"q": case["q"], "routed": None, "final": None, "routed_hit": False,
                 "anchor_hit": False, "budget": 0, "body_tokens": 0,
                 "substantive_tokens": 0, "density": 0.0, "immediate": False,
                 "missing": ["|".join(m) for m in case["must"]], "honest": False}
        rows.append(r)
        if not args.quiet:
            mark = "PASS" if (r["anchor_hit"] and r["immediate"] and r["honest"]
                              and r["density"] >= 0.15) else "FAIL"
            print(f"  [{mark}] {r['q'][:64]!r}")
            print(f"         routed={r['routed']}  final={r['final']}"
                  f"  hit(r/f)={int(r['routed_hit'])}/{int(r['anchor_hit'])}")
            print(f"         density={r['density']:.3f}"
                  f"  ({r['substantive_tokens']}/{r['budget']} tok)"
                  f"  immediacy={'yes' if r['immediate'] else 'NO ' + str(r['missing'])}"
                  f"  honest={'yes' if r['honest'] else 'NO'}")

    n = len(rows)
    densities = [r["density"] for r in rows]
    metrics = {
        "queries": n,
        "routed_hit": sum(r["routed_hit"] for r in rows),
        "anchor_hit": sum(r["anchor_hit"] for r in rows),
        "mean_density": round(sum(densities) / n, 4),
        "min_density": round(min(densities), 4),
        "immediacy": sum(r["immediate"] for r in rows),
        "budget_honest": sum(r["honest"] for r in rows),
        "per_query": rows,
    }
    gates = {
        "anchor_hit>=12": metrics["anchor_hit"] >= 12,
        "mean_density>=0.50": metrics["mean_density"] >= 0.50,
        "min_density>=0.15": metrics["min_density"] >= 0.15,
        "immediacy>=12": metrics["immediacy"] >= 12,
        f"honesty=={n}": metrics["budget_honest"] == n,
    }
    metrics["gates"] = gates

    if args.json:
        print(json.dumps(metrics, indent=2))
    else:
        print(f"\n  N={n}  anchor-hit={metrics['anchor_hit']}/{n}"
              f" (routed {metrics['routed_hit']}/{n})"
              f"  density mean={metrics['mean_density']:.3f}"
              f" min={metrics['min_density']:.3f}"
              f"  immediacy={metrics['immediacy']}/{n}"
              f"  honesty={metrics['budget_honest']}/{n}")
        for g, ok in gates.items():
            print(f"  gate {g}: {'PASS' if ok else 'FAIL'}")
    sys.exit(0 if all(gates.values()) else 1)


if __name__ == "__main__":
    main()
