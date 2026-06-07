#!/usr/bin/env python3
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""End-to-end retrieval eval: drive the real `aden` binary over a real repo.

Unlike crates/aden-index/tests/eval_corpus.rs (which scores the in-process index
over text fixtures), this measures the *product* path a user actually hits:

    aden gen <repo>            # build graph + index (+ embeddings, dense build)
    aden search "<q>" --json   # per query; mode is BM25 or hybrid depending on
                               # which binary you point at (default vs --features dense)

It reads a TSV of `query <TAB> expected <TAB> note` where `expected` is a substring
of the target file path (e.g. `installDependencies.ts` or `mm/slab.c`), runs each
query, finds the rank of the first result whose source file matches, and reports
Recall@{1,5,10,20} and MRR@20 — the CQS-style numbers.

Usage:
    eval_corpus.py --bin <aden> --repo <path> --queries <tsv> [--limit 20] [--gen] [--json]

To compare BM25 vs hybrid, run twice with the two binaries (default vs dense build).
"""
import argparse
import json
import re
import subprocess
import sys

SOURCE_FILE_RE = re.compile(r":source_file:\s*(\S+)")
# aden://module/<crate>/<file>#<symbol>  -> recover <file> as a fallback key
ANCHOR_FILE_RE = re.compile(r"aden://[a-z]+/[^/]+/([^#]+)")


def result_path(result):
    """Best-effort source path for a result: prefer the snippet's :source_file:,
    fall back to the file component of the anchor."""
    snippet = result.get("snippet", "") or ""
    m = SOURCE_FILE_RE.search(snippet)
    if m:
        return m.group(1)
    m = ANCHOR_FILE_RE.search(result.get("anchor", "") or "")
    return m.group(1) if m else ""


def run_search(bin_path, repo, query, limit):
    out = subprocess.run(
        [bin_path, "search", query, repo, "--json", "--limit", str(limit)],
        capture_output=True, text=True,
    )
    if out.returncode != 0:
        print(f"  ! search failed for {query!r}: {out.stderr.strip()[:200]}", file=sys.stderr)
        return []
    try:
        return json.loads(out.stdout).get("results", [])
    except json.JSONDecodeError:
        return []


def rank_of(results, expected):
    exp = expected.lower()
    for i, r in enumerate(results, start=1):
        if exp in result_path(r).lower():
            return i
    return None


def load_queries(path):
    cases = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.rstrip("\n")
            if not line.strip() or line.lstrip().startswith("#"):
                continue
            cols = line.split("\t")
            if len(cols) < 2 or not cols[0].strip() or not cols[1].strip():
                continue
            cases.append((cols[0].strip(), cols[1].strip(),
                          cols[2].strip() if len(cols) > 2 else ""))
    return cases


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True, help="path to the aden binary")
    ap.add_argument("--repo", required=True, help="path to the target repository")
    ap.add_argument("--queries", required=True, help="TSV: query<TAB>expected_path<TAB>note")
    ap.add_argument("--limit", type=int, default=20)
    ap.add_argument("--gen", action="store_true", help="run `aden gen` first")
    ap.add_argument("--json", action="store_true", help="emit metrics as JSON")
    ap.add_argument("--quiet", action="store_true", help="suppress per-query lines")
    args = ap.parse_args()

    if args.gen:
        print(f"[eval] aden gen {args.repo} …", file=sys.stderr)
        subprocess.run([args.bin, "gen", args.repo, "--auto"], check=False,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    cases = load_queries(args.queries)
    if not cases:
        sys.exit(f"no queries in {args.queries}")

    ranks = []
    for q, expected, note in cases:
        results = run_search(args.bin, args.repo, q, args.limit)
        rank = rank_of(results, expected)
        ranks.append(rank)
        if not args.quiet:
            mark = "OK  " if rank == 1 else (f"@{rank:<3}" if rank else "MISS")
            print(f"  [{mark}] {q!r} -> {expected}  ({note})")

    n = len(ranks)
    def recall(k):
        return sum(1 for r in ranks if r and r <= k) / n
    mrr = sum((1.0 / r) for r in ranks if r) / n
    metrics = {
        "queries": n,
        "recall@1": round(recall(1), 4),
        "recall@5": round(recall(5), 4),
        "recall@10": round(recall(10), 4),
        "recall@20": round(recall(20), 4),
        "mrr@20": round(mrr, 4),
        "misses": sum(1 for r in ranks if not r),
    }
    if args.json:
        print(json.dumps(metrics, indent=2))
    else:
        print(f"\n  N={n}  R@1={metrics['recall@1']:.3f}  R@5={metrics['recall@5']:.3f}  "
              f"R@10={metrics['recall@10']:.3f}  R@20={metrics['recall@20']:.3f}  "
              f"MRR@20={metrics['mrr@20']:.3f}  (misses={metrics['misses']})")


if __name__ == "__main__":
    main()
