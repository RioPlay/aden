#!/usr/bin/env python3
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""lexicon_ab_bench — A/B the dual-substrate lexicon levers (PR #38/#39).

The levers are query-time only (`query_index` reads `ADEN_LEXICON_OFF` at search
time), so the index is gen'd ONCE per corpus and `aden search` is run twice over
the same store: once with the levers ON (default) and once with them OFF
(`ADEN_LEXICON_OFF=1`). This isolates the retrieval delta the merge buys, holding
the binary, the index, and the labels fixed.

It does NOT re-implement metric math: it shells `eval_corpus.py` (the validated
product-path harness) per mode and just diffs the two JSON reports. Same single-
target, lower-bound recall caveats apply — see scripts/eval-sets/README.md.

Usage:
    lexicon_ab_bench.py --corpora rustfmt,flask,...  [--gen] [--bin PATH]
                        [--json out.json] [--md out.md]
"""
import argparse
import copy
import json
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
EVAL = os.path.join(HERE, "eval_corpus.py")
HOME = os.path.expanduser("~")
DEFAULT_BIN = os.path.join(HOME, ".local/bin/aden")

# `repo` is the path passed to `gen`/`search` (gen scope). `expected_path` labels
# in each TSV are substrings resolved against that scope.
CORPORA = {
    "rustfmt": {
        "repo": f"{HOME}/Projects/eval-repos/rustfmt", "lang": "Rust",
        "queries": f"{HERE}/eval-sets/rustfmt.queries.tsv"},
    "kin-openapi": {
        "repo": f"{HOME}/Projects/eval-repos/kin-openapi", "lang": "Go",
        "queries": f"{HERE}/eval-sets/kin-openapi.queries.tsv"},
    "flask": {
        "repo": f"{HOME}/Projects/eval-repos/flask", "lang": "Python",
        "queries": f"{HERE}/eval-sets/flask.queries.tsv"},
    "tanstack-query": {
        "repo": f"{HOME}/Projects/eval-repos/query", "lang": "TypeScript",
        "queries": f"{HERE}/eval-sets/tanstack-query.queries.tsv"},
    "uno-controls": {
        "repo": "/tmp/uno-controls", "lang": "C#",
        "queries": f"{HERE}/eval-sets/uno-controls.queries.tsv"},
    "linux-subset": {
        "repo": "/tmp/linux-subset", "lang": "C",
        "queries": f"{HERE}/eval-sets/linux-subset.queries.tsv"},
    # prose corpora are added by the caller via --extra-manifest if desired
}

METRIC_KEYS = ("recall@1", "recall@5", "recall@10", "recall@20", "mrr@20")


def git_commit():
    return subprocess.run(["git", "-C", HERE, "rev-parse", "--short", "HEAD"],
                          capture_output=True, text=True).stdout.strip() or "unknown"


def run_metrics(binary, repo, queries, lexicon_on):
    env = dict(os.environ)
    # Levers are opt-in (ADEN_LEXICON_ON) as of the additive-guard change. ON sets the
    # opt-in flag; OFF leaves it unset (the default = baseline ranking).
    if lexicon_on:
        env["ADEN_LEXICON_ON"] = "1"
        env.pop("ADEN_LEXICON_OFF", None)
    else:
        env.pop("ADEN_LEXICON_ON", None)
        env["ADEN_LEXICON_OFF"] = "1"
    out = subprocess.run([sys.executable, EVAL, "--bin", binary, "--repo", repo,
                          "--queries", queries, "--json", "--quiet"],
                         capture_output=True, text=True, env=env)
    try:
        return json.loads(out.stdout)
    except json.JSONDecodeError:
        sys.stderr.write(out.stderr[-400:] + "\n")
        return None


def timed_gen(binary, repo):
    t0 = time.time()
    subprocess.run([binary, "gen", repo, "--auto"], stdout=subprocess.DEVNULL,
                   stderr=subprocess.DEVNULL, check=False)
    return round(time.time() - t0, 1)


def bench(name, cfg, binary, do_gen):
    rec = {"name": name, "lang": cfg["lang"], "repo": cfg["repo"]}
    if not os.path.isdir(cfg["repo"]):
        rec["error"] = f"repo not prepared at {cfg['repo']}"
        return rec
    if do_gen:
        print(f"  [{name}] gen …", file=sys.stderr)
        rec["gen_time_sec"] = timed_gen(binary, cfg["repo"])
    print(f"  [{name}] eval OFF …", file=sys.stderr)
    off = run_metrics(binary, cfg["repo"], cfg["queries"], lexicon_on=False)
    print(f"  [{name}] eval ON …", file=sys.stderr)
    on = run_metrics(binary, cfg["repo"], cfg["queries"], lexicon_on=True)
    if not off or not on:
        rec["error"] = "eval failed (see stderr)"
        return rec
    rec["queries"] = on.get("queries")
    rec["off"], rec["on"] = off, on
    rec["delta"] = {k: round(on[k] - off[k], 4) for k in METRIC_KEYS}
    return rec


def render_md(report):
    L = [f"# Lexicon A/B (dual-substrate) — commit `{report['aden_commit']}`  ",
         "_OFF = `ADEN_LEXICON_OFF=1`; ON = default (PPMI rerank for code, OEWN "
         "expansion for prose). Same binary, same index, query-time only._\n",
         "| Corpus (lang) | N | Mode | R@1 | R@5 | R@10 | R@20 | MRR@20 |",
         "|---|---|---|---|---|---|---|---|"]
    for c in report["corpora"]:
        if "error" in c:
            L.append(f"| {c['name']} ({c['lang']}) | — | — | _{c['error']}_ |||||")
            continue
        for mode, m in (("OFF", c["off"]), ("ON", c["on"])):
            L.append(f"| {c['name']} ({c['lang']}) | {c['queries']} | {mode} | "
                     + " | ".join(f"{m[k]:.3f}" for k in METRIC_KEYS) + " |")
        d = c["delta"]
        L.append(f"| **Δ ON−OFF** | | | "
                 + " | ".join(f"**{d[k]:+.3f}**" for k in METRIC_KEYS) + " |")
    return "\n".join(L) + "\n"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpora", default=",".join(CORPORA))
    ap.add_argument("--bin", default=DEFAULT_BIN)
    ap.add_argument("--gen", action="store_true", help="gen each corpus first")
    ap.add_argument("--extra-manifest", help="JSON file of extra {name: cfg} corpora")
    ap.add_argument("--json")
    ap.add_argument("--md")
    args = ap.parse_args()

    manifest = copy.deepcopy(CORPORA)
    if args.extra_manifest:
        manifest.update(json.load(open(args.extra_manifest)))
    names = [n.strip() for n in args.corpora.split(",") if n.strip()]
    unknown = [n for n in names if n not in manifest]
    if unknown:
        sys.exit(f"unknown corpora: {unknown}; known: {list(manifest)}")

    report = {"version": "1.0", "aden_commit": git_commit(), "corpora": []}
    for n in names:
        report["corpora"].append(bench(n, manifest[n], args.bin, args.gen))

    if args.json:
        open(args.json, "w").write(json.dumps(report, indent=2) + "\n")
        print(f"[bench] wrote {args.json}", file=sys.stderr)
    if args.md:
        open(args.md, "w").write(render_md(report))
        print(f"[bench] wrote {args.md}", file=sys.stderr)
    print(render_md(report))


if __name__ == "__main__":
    main()
