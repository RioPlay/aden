#!/usr/bin/env python3
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Semi-automated retrieval-eval query authoring (M6, Stage 1).

Derive `query <TAB> expected_path <TAB> note` rows from a repo's OWN git history:
each commit that touches EXACTLY ONE in-scope source file becomes a query whose
natural-language text is the (cleaned) commit subject and whose ground-truth target
is that file. The intent comes from the project's developers, not from inspecting
aden's output — so the labels are not fit to aden (see the M6 scope doc / ADR-005).

This produces *candidate* labels. They are NOT publishable until a human spot-checks
a sample (the honesty gate): read the generating commit, confirm the file is the
genuine primary answer and not one-of-many-valid. Spot-check pass-rate is published
next to the recall. This script never runs aden, so it cannot reverse-fit.

Usage:
    gen_queries.py --repo <path> [--scope <subdir>] [--ext .go] [--max 40] --out <tsv>
"""
import argparse
import re
import subprocess
import sys
from collections import defaultdict

# Conventional-commit / housekeeping prefixes to strip from the query text so it
# reads like a natural request ("Add X" not "feat(api): add X").
CC_PREFIX = re.compile(r"^(feat|fix|chore|docs|test|refactor|perf|style|build|ci|revert)"
                       r"(\([^)]*\))?!?:\s*", re.I)
# A bare `module:` / `pkg/sub:` prefix some projects use (e.g. "openapi3: ...").
MODULE_PREFIX = re.compile(r"^[a-z][a-z0-9_./-]{1,20}:\s+", re.I)
# Trailing PR/issue ref like "(#237)" — pure noise in a retrieval query.
PR_REF = re.compile(r"\s*\(#\d+\)\s*$")
# A single leading changelog verb that BM25 matches as a common term across the
# whole repo (every "Add ..." commit), drowning the distinctive nouns. Strip ONE.
LEAD_VERB = re.compile(
    r"^(add|adds|added|fix|fixes|fixed|fixing|remove|removes|update|updates|make|makes|use|uses|"
    r"allow|allows|support|supports|implement|implements|introduce|introduces|improve|improves|"
    r"refactor|rework|reworks|handle|handles|avoid|prevent|ensure|ensures|enable|enables|disable|"
    r"move|moves|rename|renames|drop|drops|correct|corrects|resolve|resolves|expose|exposes|"
    r"return|returns|set|sets|stop|stops|skip|skips|honor|honour|respect)\s+", re.I)
# Subjects that are pure housekeeping with little content get dropped.
GENERIC = re.compile(r"^(fix|update|bump|chore|merge|revert|cleanup|tidy|wip|misc)\b", re.I)
WORD = re.compile(r"[A-Za-z][A-Za-z0-9_]+")


def git(repo, *args):
    out = subprocess.run(["git", "-C", repo, *args], capture_output=True, text=True)
    return out.stdout if out.returncode == 0 else ""


def clean_subject(subj):
    s = subj.strip()
    s = PR_REF.sub("", s)            # drop trailing (#237)
    s = CC_PREFIX.sub("", s)         # drop feat(scope): / fix:
    s = MODULE_PREFIX.sub("", s)     # drop "openapi3: "
    # NB: leaving leading changelog verbs ("Add"/"Fix") IN — the pilot showed stripping
    # them loses more BM25 signal than the boilerplate noise it removes (LEAD_VERB kept
    # only for reference). The real label-quality fix is the manual spot-check gate.
    s = s.strip().rstrip(".")
    # Re-capitalise so it reads as a phrase, not mid-sentence.
    return (s[:1].upper() + s[1:])[:120] if s else s


def content_tokens(s):
    # Meaningful words (≥3 chars), lowercased, for the generic/dup filters.
    return [w.lower() for w in WORD.findall(s) if len(w) >= 3]


def jaccard(a, b):
    sa, sb = set(a), set(b)
    return len(sa & sb) / len(sa | sb) if (sa | sb) else 0.0


def collect(repo, scope, ext, want_tests):
    """Yield (sha, subject, file) for commits touching exactly one in-scope file."""
    fmt = "%x01%H%x09%s"  # record sep \x01, then sha \t subject
    raw = git(repo, "log", "--no-merges", f"--format={fmt}", "--name-only",
              "--", scope or ".")
    in_scope = lambda f: (
        f.endswith(ext)
        and (want_tests or not re.search(r"(_test\.|\.test\.|/tests?/|/testdata/)", f))
        and (not scope or f.startswith(scope.rstrip("/") + "/") or f.startswith(scope))
    )
    for rec in raw.split("\x01"):
        rec = rec.strip("\n")
        if not rec:
            continue
        head, _, rest = rec.partition("\n")
        sha, _, subject = head.partition("\t")
        files = [f for f in rest.splitlines() if f.strip()]
        scoped = [f for f in files if in_scope(f)]
        # Exactly one in-scope source file, and the commit didn't sprawl across many
        # other files (a focused change → the subject describes that one file well).
        if len(scoped) == 1 and len(files) <= 4:
            yield sha[:7], subject.strip(), scoped[0]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True)
    ap.add_argument("--scope", default="", help="restrict to a sub-tree (e.g. openapi3/)")
    ap.add_argument("--ext", default=".go", help="source extension (default .go)")
    ap.add_argument("--max", type=int, default=40, help="cap published rows")
    ap.add_argument("--with-tests", action="store_true", help="allow test files as targets")
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    # One candidate per file: keep the most descriptive (longest content) subject,
    # so churny files don't dominate and each row targets a distinct file.
    best = {}  # file -> (sha, cleaned_subject, ntokens)
    seen_commits = 0
    for sha, subject, f in collect(args.repo, args.scope, args.ext, args.with_tests):
        seen_commits += 1
        q = clean_subject(subject)
        toks = content_tokens(q)
        if GENERIC.match(subject) and len(toks) < 3:
            continue
        if len(toks) < 3 or len(q) < 12:        # too thin to be a real query
            continue
        cur = best.get(f)
        if cur is None or len(toks) > cur[2]:
            best[f] = (sha, q, len(toks))

    rows = [(q, f, sha) for f, (sha, q, _) in best.items()]
    # Drop near-duplicate queries (different files, near-identical subject text).
    kept = []
    for q, f, sha in sorted(rows, key=lambda r: -len(r[0])):
        toks = content_tokens(q)
        if any(jaccard(toks, content_tokens(k[0])) > 0.6 for k in kept):
            continue
        kept.append((q, f, sha))
    kept.sort(key=lambda r: r[1])  # stable by path
    published = kept[: args.max]

    with open(args.out, "w", encoding="utf-8") as out:
        out.write("# Auto-generated candidate queries (M6 Stage 1: commit -> single file).\n")
        out.write("# UNVALIDATED: spot-check before publishing recall. query<TAB>expected_path<TAB>note\n")
        for q, f, sha in published:
            out.write(f"{q}\t{f}\tcommit {sha}\n")

    print(f"[gen_queries] single-file commits seen: {seen_commits}", file=sys.stderr)
    print(f"[gen_queries] distinct files w/ a usable subject: {len(best)}", file=sys.stderr)
    print(f"[gen_queries] after dedup: {len(kept)}  ->  published (cap {args.max}): {len(published)}",
          file=sys.stderr)
    print(f"[gen_queries] wrote {args.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
