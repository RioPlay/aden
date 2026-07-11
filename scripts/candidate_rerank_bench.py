#!/usr/bin/env python3
"""Prototype label-blind candidate-body reranking for held-out queries."""

from __future__ import annotations

import argparse
import importlib.util
import json
import math
import re
import subprocess
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("eval_corpus", ROOT / "scripts" / "eval_corpus.py")
assert SPEC and SPEC.loader
eval_corpus = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(eval_corpus)

STOP = {
    "add", "allow", "better", "change", "code", "does", "error", "file", "fix", "fixing",
    "for", "from", "function", "handle", "into", "make", "method", "more", "not", "only",
    "same", "should", "support", "that", "the", "this", "use", "using", "value", "when",
    "where", "which", "with",
}


def terms(text: str) -> set[str]:
    return {
        token for token in re.findall(r"[a-z0-9_]+", text.casefold())
        if len(token) >= 3 and token not in STOP
    }


def probe(aden_bin: str, repo: Path, anchor: str, budget: int, timeout: int) -> str:
    result = subprocess.run(
        [aden_bin, "asm", "--human", "--silent", "--strict", "--budget", str(budget),
         "--project", str(repo), "--from", anchor],
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    return result.stdout if result.returncode == 0 else ""


def choose(
    aden_bin: str, repo: Path, query: str, results: list[dict[str, Any]], budget: int, timeout: int
) -> tuple[dict[str, Any] | None, list[dict[str, Any]]]:
    if not results:
        return None, []
    with ThreadPoolExecutor(max_workers=min(8, len(results))) as pool:
        bodies = list(
            pool.map(
                lambda result: probe(aden_bin, repo, result.get("anchor", ""), budget, timeout),
                results,
            )
        )
    query_terms = terms(query)
    body_terms = [terms(body) for body in bodies]
    document_frequency = {
        term: sum(term in candidate for candidate in body_terms) for term in query_terms
    }
    weights = {
        term: 1.0 + math.log((len(results) + 1) / (document_frequency[term] + 1))
        for term in query_terms
    }
    denominator = sum(weights.values()) or 1.0
    top_score = float(results[0].get("score", 0.0)) or 1.0
    scored = []
    for result, candidate_terms in zip(results, body_terms):
        anchor_terms = terms(result.get("anchor", ""))
        body_coverage = sum(weights[t] for t in query_terms & candidate_terms) / denominator
        anchor_coverage = len(query_terms & anchor_terms) / max(1, len(query_terms))
        search_score = float(result.get("score", 0.0)) / top_score
        score = 0.55 * body_coverage + 0.25 * anchor_coverage + 0.20 * search_score
        scored.append({**result, "rerank_score": score, "body_coverage": body_coverage})
    scored.sort(key=lambda result: (result["rerank_score"], result["score"]), reverse=True)
    return scored[0], scored


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--aden-bin", default=str(ROOT / "target" / "release" / "aden"))
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--queries", type=Path, required=True)
    parser.add_argument("--candidates", type=int, default=8)
    parser.add_argument("--probe-budget", type=int, default=128)
    parser.add_argument("--timeout", type=int, default=20)
    parser.add_argument("--json", type=Path)
    args = parser.parse_args()

    records = []
    for query, expected, note in eval_corpus.load_queries(args.queries):
        results = eval_corpus.run_search(
            args.aden_bin, str(args.repo), query, args.candidates, args.timeout
        )
        selected, scored = choose(
            args.aden_bin, args.repo, query, results, args.probe_budget, args.timeout
        )
        selected_path = eval_corpus.result_path(selected or {})
        passed = any(
            target in selected_path.casefold()
            for target in eval_corpus.expected_alternatives(expected)
        )
        records.append(
            {
                "query": query,
                "expected": expected,
                "selected": selected_path,
                "passed": passed,
                "search_rank": eval_corpus.rank_of(results, expected),
                "top_candidates": [
                    {
                        "path": eval_corpus.result_path(result),
                        "score": round(result["rerank_score"], 4),
                        "coverage": round(result["body_coverage"], 4),
                    }
                    for result in scored[:3]
                ],
                "note": note,
            }
        )
        print(f"[rerank] {len(records)} {query[:60]}", flush=True)
        if args.json:
            args.json.write_text(json.dumps({"partial": True, "records": records}, indent=2) + "\n")

    report = {
        "schema_version": 1,
        "queries": len(records),
        "passed": sum(record["passed"] for record in records),
        "accuracy": round(sum(record["passed"] for record in records) / len(records), 4),
        "records": records,
    }
    rendered = json.dumps(report, indent=2) + "\n"
    if args.json:
        args.json.write_text(rendered)
    print(rendered)


if __name__ == "__main__":
    main()
