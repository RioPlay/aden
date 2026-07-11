#!/usr/bin/env python3
"""Benchmark conventional recursive-text navigation on the held-out labels."""

from __future__ import annotations

import argparse
import importlib.util
import json
import re
import statistics
import subprocess
import time
from collections import defaultdict
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("eval_corpus", ROOT / "scripts" / "eval_corpus.py")
assert SPEC and SPEC.loader
eval_corpus = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(eval_corpus)

STOP = {
    "add", "allow", "and", "better", "change", "code", "does", "error", "file", "fix",
    "fixing", "for", "from", "function", "handle", "into", "make", "method", "more", "not",
    "only", "same", "should", "support", "that", "the", "this", "use", "using", "value",
    "when", "where", "which", "with",
}


def query_terms(query: str) -> list[str]:
    return sorted(
        {
            token for token in re.findall(r"[a-z0-9_]+", query.casefold())
            if len(token) >= 3 and token not in STOP
        },
        key=lambda token: (-len(token), token),
    )[:12]


def rank_files(repo: Path, query: str, timeout: int) -> list[str]:
    terms = query_terms(query)
    if not terms:
        return []
    pattern = "|".join(re.escape(term) for term in terms)
    result = subprocess.run(
        [
            "rg", "--json", "--ignore-case", "--glob", "!.git/**", "--glob", "!target/**",
            "--glob", "!node_modules/**", pattern, ".",
        ],
        cwd=repo,
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    if result.returncode not in {0, 1}:
        return []
    matched_terms: dict[str, set[str]] = defaultdict(set)
    match_count: dict[str, int] = defaultdict(int)
    for line in result.stdout.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event.get("type") != "match":
            continue
        data = event.get("data", {})
        path = data.get("path", {}).get("text", "").removeprefix("./")
        text = data.get("lines", {}).get("text", "").casefold()
        if not path:
            continue
        hits = {term for term in terms if term in text}
        matched_terms[path].update(hits)
        match_count[path] += len(data.get("submatches", [])) or 1
    query_set = set(terms)
    return sorted(
        matched_terms,
        key=lambda path: (
            len(matched_terms[path]),
            len(query_set & set(re.findall(r"[a-z0-9_]+", path.casefold()))),
            match_count[path],
            -len(path),
            path,
        ),
        reverse=True,
    )


def rank_of(paths: list[str], expected: str) -> int | None:
    alternatives = eval_corpus.expected_alternatives(expected)
    return next(
        (
            index for index, path in enumerate(paths, 1)
            if any(target in path.casefold() for target in alternatives)
        ),
        None,
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--queries", type=Path, required=True)
    parser.add_argument("--timeout", type=int, default=20)
    parser.add_argument("--json", type=Path)
    args = parser.parse_args()

    records: list[dict[str, Any]] = []
    for query, expected, note in eval_corpus.load_queries(args.queries):
        started = time.monotonic()
        paths = rank_files(args.repo, query, args.timeout)
        elapsed_ms = round((time.monotonic() - started) * 1000)
        rank = rank_of(paths, expected)
        sizes = []
        for path in paths[:5]:
            try:
                sizes.append((args.repo / path).stat().st_size)
            except OSError:
                sizes.append(0)
        records.append(
            {
                "query": query,
                "expected": expected,
                "rank": rank,
                "top_path": paths[0] if paths else None,
                "elapsed_ms": elapsed_ms,
                "top1_context_tokens": (sizes[0] + 3) // 4 if sizes else 0,
                "top5_context_tokens": (sum(sizes) + 3) // 4,
                "note": note,
            }
        )
    total = len(records)
    recall = lambda limit: round(sum(r["rank"] is not None and r["rank"] <= limit for r in records) / total, 4)
    report = {
        "schema_version": 1,
        "queries": total,
        "recall@1": recall(1),
        "recall@5": recall(5),
        "recall@10": recall(10),
        "recall@20": recall(20),
        "median_elapsed_ms": statistics.median(r["elapsed_ms"] for r in records),
        "median_top1_context_tokens": statistics.median(
            r["top1_context_tokens"] for r in records
        ),
        "median_top5_context_tokens": statistics.median(
            r["top5_context_tokens"] for r in records
        ),
        "records": records,
    }
    rendered = json.dumps(report, indent=2) + "\n"
    if args.json:
        args.json.write_text(rendered, encoding="utf-8")
    print(rendered)


if __name__ == "__main__":
    main()
