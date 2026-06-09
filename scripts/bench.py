#!/usr/bin/env python3
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""aden-bench — multi-corpus retrieval benchmark orchestrator (M6).

Runs the M6 corpus through `eval_corpus.py` for BM25 and Hybrid, times index
build separately from query, and emits ONE comparable report (JSON + markdown)
with the honesty discipline baked in: caveats inline, cold-build vs warm kept in
separate rows, spot-check pass-rate published next to each recall, per-repo
memo-risk shown. It measures the *product* path (real binary, real repos); it does
not clone or author queries (see gen_queries.py + the manual spot-check gate).

Mode is decided by which binary is pointed at: the default build is BM25, the
`--features dense` build is Hybrid. Each binary's `gen` writes the index in its own
mode, so we re-gen per mode (and time it).

Usage:
    bench.py --corpora kin-openapi,rustfmt,uno-controls [--mode both]
             [--bm25-bin ./target/debug/aden] [--dense-bin ./target/release/aden]
             [--json out.json] [--md out.md]
"""
import argparse
import json
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
EVAL = os.path.join(HERE, "eval_corpus.py")
HOME = os.path.expanduser("~")

# Corpus manifest. `repo` must already be cloned/prepared (see eval-sets/README.md);
# this orchestrator runs, it does not fetch. memo_risk is published next to numbers.
CORPORA = {
    "kin-openapi": {
        "repo": f"{HOME}/Projects/eval-repos/kin-openapi", "lang": "Go",
        "scope": "openapi3/", "memo_risk": "low",
        "queries": f"{HERE}/eval-sets/kin-openapi.queries.tsv",
        "repo_url": "https://github.com/getkin/kin-openapi",
    },
    "rustfmt": {
        "repo": f"{HOME}/Projects/eval-repos/rustfmt", "lang": "Rust",
        "scope": "src/", "memo_risk": "low",
        "queries": f"{HERE}/eval-sets/rustfmt.queries.tsv",
        "repo_url": "https://github.com/rust-lang/rustfmt",
    },
    "uno-controls": {
        "repo": "/tmp/uno-controls", "lang": "C#",
        "scope": "src/Uno.UI/Controls (isolated copy; basename match)", "memo_risk": "low",
        "queries": f"{HERE}/eval-sets/uno-controls.queries.tsv",
        "repo_url": "https://github.com/unoplatform/uno",
    },
    "flask": {
        "repo": f"{HOME}/Projects/eval-repos/flask", "lang": "Python",
        "scope": "src/flask/", "memo_risk": "high",
        "queries": f"{HERE}/eval-sets/flask.queries.tsv",
        "repo_url": "https://github.com/pallets/flask",
    },
    "tanstack-query": {
        "repo": f"{HOME}/Projects/eval-repos/query", "lang": "TypeScript",
        "scope": "packages/query-core/src", "memo_risk": "med",
        "queries": f"{HERE}/eval-sets/tanstack-query.queries.tsv",
        "repo_url": "https://github.com/TanStack/query",
    },
}

CAVEATS = [
    "external corpora only; aden's own tree is never in the corpus",
    "ground-truth is single-target (one expected file/query) → recall is a LOWER BOUND",
    "labels auto-derived from commit history then manually spot-checked; pass-rate shown per repo",
    "retrieval params (BM25 k1/b, RRF k) are hardcoded per ADR-005, never tuned to these labels",
    "cold index build and warm query are SEPARATE rows; hybrid_cold includes one-time dense embed",
    "self-run on these corpora — indicative, not a standardized leaderboard",
    "retrieval is deterministic as of commit 9da0bcf (gen/index/emit determinism fixed): these recall "
    "numbers are byte-stable run-to-run, so a single decimal is meaningful here (verified: 5 fresh gens "
    "give identical recall).",
]


def git_commit():
    return subprocess.run(["git", "-C", HERE, "rev-parse", "--short", "HEAD"],
                          capture_output=True, text=True).stdout.strip() or "unknown"


def spot_check(tsv):
    """Derive (published, excluded, pass_rate) from the TSV's spot-check comments."""
    pub = exc = 0
    for line in open(tsv, encoding="utf-8"):
        s = line.strip()
        if s.startswith("# [spot-check"):
            exc += 1
        elif s and not s.startswith("#") and "\t" in s:
            pub += 1
    total = pub + exc
    return pub, exc, (round(pub / total, 3) if total else None)


def timed_gen(binary, repo):
    t0 = time.time()
    subprocess.run([binary, "gen", repo, "--auto"], stdout=subprocess.DEVNULL,
                   stderr=subprocess.DEVNULL, check=False)
    return round(time.time() - t0, 1)


def run_recall(binary, repo, queries):
    out = subprocess.run([sys.executable, EVAL, "--bin", binary, "--repo", repo,
                          "--queries", queries, "--json", "--quiet"],
                         capture_output=True, text=True)
    try:
        return json.loads(out.stdout)
    except json.JSONDecodeError:
        sys.stderr.write(out.stderr[-300:] + "\n")
        return None


def bench_corpus(name, cfg, modes, bins):
    pub, exc, rate = spot_check(cfg["queries"])
    rec = {"name": name, "repo_url": cfg["repo_url"], "scope": cfg["scope"],
           "language": cfg["lang"], "memo_risk": cfg["memo_risk"],
           "query_count": pub, "spot_check_excluded": exc, "spot_check_pass_rate": rate,
           "gen_time_sec": {}, "accurate": {}}
    if not os.path.isdir(cfg["repo"]):
        rec["error"] = f"repo not prepared at {cfg['repo']} (clone/isolate per README)"
        return rec
    for mode in modes:
        binary = bins[mode]
        if not os.path.exists(binary):
            rec["accurate"][mode] = {"error": f"binary missing: {binary}"}
            continue
        print(f"  [{name}] {mode}: gen …", file=sys.stderr)
        gen_t = timed_gen(binary, cfg["repo"])
        rec["gen_time_sec"]["hybrid_cold" if mode == "hybrid" else mode] = gen_t
        print(f"  [{name}] {mode}: eval ({pub} queries) …", file=sys.stderr)
        m = run_recall(binary, cfg["repo"], cfg["queries"])
        rec["accurate"][mode] = m or {"error": "eval failed"}
    return rec


def render_md(report):
    L = [f"# aden retrieval benchmark — commit `{report['aden_commit']}`  ",
         f"_build features: {', '.join(report['build_features']) or 'bm25 (default)'}_\n",
         "## Accuracy (Recall@K, MRR) — single-target lower bound\n",
         "| Corpus (lang, memo-risk, spot-check%) | N | Mode | R@1 | R@5 | R@10 | R@20 | MRR@20 |",
         "|---|---|---|---|---|---|---|---|"]
    for c in report["corpora"]:
        tag = f"{c['name']} ({c['language']}, {c['memo_risk']}, " \
              f"{int(c['spot_check_pass_rate']*100) if c['spot_check_pass_rate'] else '—'}%)"
        for mode in ("bm25", "hybrid"):
            a = c.get("accurate", {}).get(mode)
            if not a or "error" in a:
                continue
            L.append(f"| {tag} | {c['query_count']} | {mode} | {a['recall@1']:.3f} | "
                     f"{a['recall@5']:.3f} | {a['recall@10']:.3f} | {a['recall@20']:.3f} | "
                     f"{a['mrr@20']:.3f} |")
    L += ["", "## Speed — cold index build (warm query measured separately, not here)\n",
          "| Corpus | BM25 gen | Hybrid cold (incl. 1-time embed) |", "|---|---|---|"]
    for c in report["corpora"]:
        g = c.get("gen_time_sec", {})
        L.append(f"| {c['name']} | {g.get('bm25','—')}s | {g.get('hybrid_cold','—')}s |")
    L += ["", "## Caveats (read with every number)\n"] + [f"- {x}" for x in report["caveats"]]
    L.append("\n> Task-success axis (T1/T2/T3 vs no-context/grep baselines) is the next M6 build "
             "and not yet in this report.")
    return "\n".join(L) + "\n"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpora", default=",".join(CORPORA),
                    help="comma list (default all): " + ",".join(CORPORA))
    ap.add_argument("--mode", choices=["bm25", "hybrid", "both"], default="both")
    ap.add_argument("--bm25-bin", default="./target/debug/aden")
    ap.add_argument("--dense-bin", default="./target/release/aden")
    ap.add_argument("--json", help="write JSON report here")
    ap.add_argument("--md", help="write markdown report here")
    args = ap.parse_args()

    modes = ["bm25", "hybrid"] if args.mode == "both" else [args.mode]
    bins = {"bm25": args.bm25_bin, "hybrid": args.dense_bin}
    names = [n.strip() for n in args.corpora.split(",") if n.strip()]
    unknown = [n for n in names if n not in CORPORA]
    if unknown:
        sys.exit(f"unknown corpora: {unknown}; known: {list(CORPORA)}")

    report = {"version": "1.0", "aden_commit": git_commit(),
              "build_features": ["dense"] if "hybrid" in modes else [],
              "corpora": [], "caveats": CAVEATS}
    for n in names:
        report["corpora"].append(bench_corpus(n, CORPORA[n], modes, bins))

    out = json.dumps(report, indent=2)
    if args.json:
        open(args.json, "w").write(out + "\n")
        print(f"[bench] wrote {args.json}", file=sys.stderr)
    if args.md:
        open(args.md, "w").write(render_md(report))
        print(f"[bench] wrote {args.md}", file=sys.stderr)
    if not args.json and not args.md:
        print(out)
    else:
        print(render_md(report))


if __name__ == "__main__":
    main()
