#!/usr/bin/env python3
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# A1 - blast-radius / impact accuracy. The graph's flagship capability is "what breaks if I
# change X" (the callers/users of a symbol), which dense embeddings cannot answer. This measures
# whether the graph's incoming CODE edges match the real call sites in source.
#
# Graph side: incoming Calls/Uses/Invokes edges per symbol (from the dumped edges.json).
# Ground truth: a PARSE-INDEPENDENT text scan of the source for `NAME(` call sites (so the eval
# does not just re-check aden's own parser against itself). File-level recall/precision over the
# 40 gold symbols. Caveats: file-level (coarse); text scan counts the def line + comments as
# weak false positives; src only (tests excluded, matching the clean dump).
#
# Run: python3 scripts/blast_radius_eval.py

import json
import re
import statistics
from collections import defaultdict
from pathlib import Path

ROOT = Path("/home/unknown/Projects/aden")
EVAL = Path.home() / ".cache/aden/dict/eval"
CALL_TYPES = {"Calls", "Uses", "Invokes"}

edges = json.loads((EVAL / "edges.json").read_text())
probes = json.loads((EVAL / "probes.json").read_text())


def anchor_file(a):
    if "aden://module/" not in a:
        return None
    p = "crates/" + a.split("aden://module/")[1].split("#")[0]
    return p if p.endswith(".rs") else p.rstrip("/") + "/lib.rs"


# graph: target-anchor -> set of caller source files (incoming code edges)
callers = defaultdict(set)
for s, t, et in edges:
    if et in CALL_TYPES:
        f = anchor_file(s)
        if f:
            callers[t].add(f)

# load src once (exclude tests/, matching the clean dump)
src = {}
for p in ROOT.glob("crates/**/*.rs"):
    rp = str(p.relative_to(ROOT))
    if "/tests/" in rp:
        continue
    src[rp] = p.read_text(errors="ignore")


def truth_files(name):
    # A file is a real caller if it has a `NAME(` call that is NOT the `fn NAME(` definition or
    # trait declaration (those contain `NAME(` but are not call sites).
    call = re.compile(r"\b" + re.escape(name) + r"\s*\(")
    deff = re.compile(r"\bfn\s+" + re.escape(name) + r"\s*\(")
    out = set()
    for rp, txt in src.items():
        if any(call.search(l) and not deff.search(l) for l in txt.splitlines()):
            out.add(rp)
    return out


gold = sorted({acc for _q, accs, _e in probes for acc in accs})
print(f"{'symbol':<34} truth graph hit  recall  prec")
recs, pres = [], []
for name in gold:
    g = set()
    for t, cf in callers.items():
        sym = t.split("#")[-1]
        if sym == name or sym.endswith("::" + name):
            g |= cf
    tf = truth_files(name)
    if not tf:
        continue  # not call-measurable (a type, or only defined/declared, never called)
    inter = g & tf
    recall = len(inter) / len(tf) if tf else None
    prec = len(inter) / len(g) if g else None
    if recall is not None:
        recs.append(recall)
    if prec is not None:
        pres.append(prec)
    rr = f"{recall:.2f}" if recall is not None else "  - "
    pp = f"{prec:.2f}" if prec is not None else "  - "
    print(f"{name:<34} {len(tf):>5} {len(g):>5} {len(inter):>3}  {rr:>5}  {pp:>5}")

print(
    f"\nmeasured {len(recs)} callable symbols | mean recall {statistics.mean(recs):.2f} "
    f"| mean precision {statistics.mean(pres):.2f}"
)
