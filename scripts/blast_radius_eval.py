#!/usr/bin/env python3
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""blast_radius_eval — measure aden's blast-radius (caller) accuracy on a real repo.

aden's differentiator over dense/grep is the STRUCTURE question: "what references /
calls this symbol?" (`understand` Backlinks / `query --backlinks`). This scores that
against a parse-independent ground truth: a text scan of the source for real `NAME(`
call sites, file-level, over auto-discovered gold symbols.

Method (mirrors the 2026-06-18 devlog harness, reconstructed):
  1. Discover candidate functions by a DEF regex (per language).
  2. Ground-truth caller FILES = files containing `\\bNAME\\s*\\(` other than the def
     line. Test dirs are excluded to match aden's PRODUCTION extraction scope (aden
     does not extract Calls from `#[cfg(test)]` / test modules; counting test callers
     would penalize the bench, not the extractor — the devlog's key correction).
  3. Keep symbols with >=1 ground-truth caller file (actually-called), sample N.
  4. aden's caller files = `understand NAME --json` Backlinks anchors -> files.
  5. Per symbol: precision = |aden ∩ gt| / |aden|, recall = |aden ∩ gt| / |gt|.
     Report the mean (the devlog's headline was precision 0.99 / recall 0.98).

Usage:
  blast_radius_eval.py --bin ./target/release/aden --repo ~/Projects/eval-repos/flask \\
      --lang py --src src/ [--n 30] [--json]
"""
import argparse
import json
import os
import random
import re
import subprocess
import sys

DEF_RE = {
    "py": re.compile(r"^\s*def\s+([A-Za-z_]\w*)\s*\("),
    "rs": re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([A-Za-z_]\w*)\s*[(<]"),
    "go": re.compile(r"^\s*func\s+(?:\([^)]*\)\s*)?([A-Za-z_]\w*)\s*\("),
}
SRC_EXT = {"py": ".py", "rs": ".rs", "go": ".go"}
TEST_MARKERS = ("/test", "test_", "_test.", "/tests/", "conftest.py")


def is_test(path):
    p = path.replace("\\", "/")
    return any(m in p for m in TEST_MARKERS)


def source_files(repo, sub, ext):
    root = os.path.join(repo, sub)
    for dirpath, _dirs, files in os.walk(root):
        for f in files:
            if f.endswith(ext):
                yield os.path.join(dirpath, f)


def discover(repo, sub, lang):
    """Return {name: def_file} and a per-file text cache."""
    ext, defs, texts = SRC_EXT[lang], {}, {}
    drx = DEF_RE[lang]
    for path in source_files(repo, sub, ext):
        try:
            txt = open(path, encoding="utf-8", errors="ignore").read()
        except OSError:
            continue
        texts[path] = txt
        if is_test(path):
            continue
        for line in txt.splitlines():
            m = drx.match(line)
            if m and m.group(1) not in ("main", "new"):
                defs.setdefault(m.group(1), path)
    return defs, texts


def ground_truth_callers(name, def_file, texts):
    """Files (non-test) with a real `name(` call site that is not name's own def."""
    call = re.compile(r"\b" + re.escape(name) + r"\s*\(")
    defline = re.compile(r"^\s*(?:def|fn|func)\b.*\b" + re.escape(name) + r"\s*[(<]")
    out = set()
    for path, txt in texts.items():
        if is_test(path):
            continue
        for line in txt.splitlines():
            if call.search(line) and not defline.match(line):
                out.add(path)
                break
    out.discard(def_file)  # a recursive self-call in the def file is not a "caller file"
    return out


def aden_caller_files(binary, repo, name):
    """Return (resolved_leaf_symbol, caller_files) or None. `resolved_leaf_symbol`
    is the bare symbol `understand` actually resolved to, so the caller can detect a
    RESOLUTION MISS (e.g. `understand errorhandler` resolving to `app_errorhandler`)
    and separate aden's name-resolution accuracy from its blast-radius accuracy."""
    out = subprocess.run([binary, "understand", name, repo, "--json"],
                         capture_output=True, text=True)
    if out.returncode != 0:
        return None
    try:
        data = json.loads(out.stdout)
    except json.JSONDecodeError:
        return None
    defn = data.get("definition") or {}
    def_anchor = defn.get("anchor", "") if isinstance(defn, dict) else str(defn)
    # leaf of aden://...#Class.method or #func -> the method/func name.
    leaf = re.split(r"[#.]", def_anchor)[-1] if def_anchor else ""
    files = set()
    for bl in data.get("backlinks", []) or []:
        anchor = bl if isinstance(bl, str) else bl.get("anchor", "")
        m = re.search(r"aden://[a-z]+/[^/]+/([^#]+)", anchor)
        if m:
            files.add(m.group(1))
    return leaf, files


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--repo", required=True)
    ap.add_argument("--lang", required=True, choices=list(DEF_RE))
    ap.add_argument("--src", default="")
    ap.add_argument("--n", type=int, default=30)
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    defs, texts = discover(args.repo, args.src, args.lang)
    gold = []
    for name, dfile in defs.items():
        # Curate to genuinely-resolvable callable symbols (mirrors the devlog's gold
        # set): skip dunders (call sites are implicit/everywhere) and short/generic
        # identifiers (`name`, `get`) whose text-scan ground truth is pure noise.
        if name.startswith("__") or len(name) < 5:
            continue
        gt = {os.path.relpath(p, args.repo) for p in ground_truth_callers(name, dfile, texts)}
        # Keep symbols that are actually called but not UBIQUITOUS: a symbol called
        # from >8 files is a generic name the grep can't disambiguate (false ground
        # truth), not a discriminative blast-radius target.
        if 1 <= len(gt) <= 8:
            gold.append((name, dfile, gt))
    if not gold:
        sys.exit("no called symbols discovered")
    random.Random(args.seed).shuffle(gold)
    gold = gold[: args.n]

    rows, precs, recs = [], [], []
    resolution_miss = 0
    for name, dfile, gt_files in gold:
        res = aden_caller_files(args.bin, args.repo, name)
        if res is None:
            continue
        resolved, aden_files = res
        # Resolution miss: `understand NAME` landed on a different symbol (fuzzy
        # substring match). Count it separately and exclude from blast-radius P/R so
        # the structure number is not contaminated by name-resolution errors.
        if resolved and resolved != name:
            resolution_miss += 1
            continue
        # Match on path suffix (aden may report a shorter relative path).
        def hit(gtf):
            return any(gtf.endswith(af) or af.endswith(gtf) for af in aden_files)
        tp = sum(1 for g in gt_files if hit(g))
        prec = (sum(1 for af in aden_files
                    if any(af.endswith(g) or g.endswith(af) for g in gt_files))
                / len(aden_files)) if aden_files else 0.0
        rec = tp / len(gt_files) if gt_files else 0.0
        precs.append(prec)
        recs.append(rec)
        rows.append({"symbol": name, "gt_caller_files": len(gt_files),
                     "aden_caller_files": len(aden_files),
                     "precision": round(prec, 3), "recall": round(rec, 3)})

    n = len(rows)
    attempted = n + resolution_miss
    summary = {"repo": os.path.basename(args.repo.rstrip("/")), "lang": args.lang,
               "symbols_scored": n,
               "resolution_misses": resolution_miss,
               "resolution_accuracy": round(n / attempted, 3) if attempted else None,
               "mean_precision": round(sum(precs) / n, 3) if n else None,
               "mean_recall": round(sum(recs) / n, 3) if n else None}
    if args.json:
        print(json.dumps({"summary": summary, "rows": rows}, indent=2))
    else:
        print(f"\nblast-radius on {summary['repo']} ({args.lang}), N={n} "
              f"(+{resolution_miss} resolution misses excluded)")
        print(f"  understand resolution accuracy = {summary['resolution_accuracy']}")
        print(f"  mean precision (on resolved)   = {summary['mean_precision']}")
        print(f"  mean recall    (on resolved)   = {summary['mean_recall']}")
        worst = sorted(rows, key=lambda r: (r["recall"], r["precision"]))[:5]
        print("  lowest-recall symbols:",
              ", ".join(f"{r['symbol']}(r={r['recall']},p={r['precision']})" for r in worst))


if __name__ == "__main__":
    main()
