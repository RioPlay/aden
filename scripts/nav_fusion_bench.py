#!/usr/bin/env python3
"""Measure a deterministic Aden + conventional-navigation recall backstop.

The gate is label-blind: conventional file ranking participates only when the
top native candidates are predominantly prose-document anchors. Expected paths
are used solely after selection to score the held-out result.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import re
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]


def load_module(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


eval_corpus = load_module("eval_corpus", ROOT / "scripts" / "eval_corpus.py")
conventional = load_module(
    "conventional_nav_bench", ROOT / "scripts" / "conventional_nav_bench.py"
)

SOURCE_PATH = re.compile(
    r"(.+?\.(?:adoc|aden|md|rst|txt|rs|go|py|ts|tsx|js|jsx|cs|c|h))(?:/.*)?$",
    re.IGNORECASE,
)


def canonical_path(path: str) -> str:
    value = (path or "").removeprefix("./")
    match = SOURCE_PATH.match(value)
    return (match.group(1) if match else value).casefold()


def predominantly_prose(results: list[dict[str, Any]], window: int = 10) -> bool:
    votes: list[bool] = []
    prose_extensions = {".adoc", ".aden", ".md", ".rst", ".txt"}
    code_extensions = {".rs", ".go", ".py", ".ts", ".tsx", ".js", ".jsx", ".cs", ".c", ".h"}
    for result in results[:window]:
        anchor = result.get("anchor", "")
        path = canonical_path(eval_corpus.result_path(result))
        suffix = Path(path).suffix
        if anchor.startswith("aden://doc/") or suffix in prose_extensions:
            votes.append(True)
        elif (
            anchor.startswith(("aden://module/", "aden://symbol/"))
            or suffix in code_extensions
        ):
            votes.append(False)
        # Synthetic aliases without a resolvable scheme/path carry no corpus vote.
    if not votes:
        return False
    return sum(votes) / len(votes) >= 0.8


def native_top_file_consensus(results: list[dict[str, Any]], window: int = 5) -> int:
    paths = [canonical_path(eval_corpus.result_path(result)) for result in results[:window]]
    if not paths or not paths[0]:
        return 0
    return sum(path == paths[0] for path in paths)


def predominantly_chronological(results: list[dict[str, Any]], window: int = 10) -> bool:
    paths = [canonical_path(eval_corpus.result_path(result)) for result in results[:window]]
    stems = [Path(path).stem for path in paths if path]
    if not stems:
        return False
    dated = sum(bool(re.fullmatch(r"\d{4}-\d{2}-\d{2}", stem)) for stem in stems)
    return dated / len(stems) >= 0.8


def reciprocal_rank_fusion(
    aden_paths: list[str], conventional_paths: list[str], conventional_weight: float = 1.5,
    offset: int = 60,
) -> list[str]:
    scores: dict[str, float] = {}
    for rank, path in enumerate(aden_paths, 1):
        key = canonical_path(path)
        if key:
            scores[key] = scores.get(key, 0.0) + 1.0 / (offset + rank)
    for rank, path in enumerate(conventional_paths, 1):
        key = canonical_path(path)
        if key:
            scores[key] = scores.get(key, 0.0) + conventional_weight / (offset + rank)
    return sorted(scores, key=lambda path: (scores[path], path), reverse=True)


def top_hit(paths: list[str], expected: str) -> bool:
    return bool(
        paths
        and any(
            target in paths[0]
            for target in eval_corpus.expected_alternatives(expected)
        )
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--manifest", type=Path,
        default=ROOT / "scripts" / "heldout-context" / "repos.json",
    )
    parser.add_argument("--aden-bin", default="aden")
    parser.add_argument("--timeout", type=int, default=10)
    parser.add_argument("--weight", type=float, default=1.5)
    parser.add_argument("--json", type=Path)
    args = parser.parse_args()

    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    records: list[dict[str, Any]] = []
    for item in manifest["repositories"]:
        repo = Path(item["path"]).expanduser()
        queries = eval_corpus.load_queries(ROOT / item["queries"])
        for query, expected, note in queries:
            prior_off = os.environ.get("ADEN_NAV_FUSION_OFF")
            os.environ["ADEN_NAV_FUSION_OFF"] = "1"
            try:
                aden_results = eval_corpus.run_search(
                    args.aden_bin, str(repo), query, 20, args.timeout
                )
            finally:
                if prior_off is None:
                    os.environ.pop("ADEN_NAV_FUSION_OFF", None)
                else:
                    os.environ["ADEN_NAV_FUSION_OFF"] = prior_off
            aden_paths = [
                eval_corpus.result_path(result) for result in aden_results
            ]
            product_results = eval_corpus.run_search(
                args.aden_bin, str(repo), query, 20, args.timeout
            )
            product_paths = [
                eval_corpus.result_path(result) for result in product_results
            ]
            conventional_paths = conventional.rank_files(repo, query, args.timeout)[:20]
            prose_gate = (
                predominantly_prose(aden_results)
                and not predominantly_chronological(aden_results)
                and native_top_file_consensus(aden_results) < 3
            )
            fused_paths = reciprocal_rank_fusion(
                aden_paths, conventional_paths, args.weight
            )
            selected_paths = fused_paths if prose_gate else [canonical_path(p) for p in aden_paths]
            records.append(
                {
                    "repository": item["id"],
                    "query": query,
                    "expected": expected,
                    "prose_gate": prose_gate,
                    "aden_top": canonical_path(aden_paths[0]) if aden_paths else None,
                    "fused_top": fused_paths[0] if fused_paths else None,
                    "aden_pass": top_hit([canonical_path(p) for p in aden_paths], expected),
                    "fused_pass": top_hit(fused_paths, expected),
                    "gated_pass": top_hit(selected_paths, expected),
                    "product_pass": top_hit(
                        [canonical_path(path) for path in product_paths], expected
                    ),
                    "note": note,
                }
            )
            print(
                f"[nav-fusion] {item['id']} {len(records)} gate={'prose' if prose_gate else 'native'}",
                flush=True,
            )
            if args.json:
                args.json.parent.mkdir(parents=True, exist_ok=True)
                args.json.write_text(
                    json.dumps({"schema_version": 1, "partial": True, "records": records}, indent=2)
                    + "\n",
                    encoding="utf-8",
                )

    by_repo = {}
    for item in manifest["repositories"]:
        rows = [row for row in records if row["repository"] == item["id"]]
        by_repo[item["id"]] = {
            "queries": len(rows),
            "prose_gated": sum(row["prose_gate"] for row in rows),
            "aden_pass": sum(row["aden_pass"] for row in rows),
            "fused_pass": sum(row["fused_pass"] for row in rows),
            "gated_pass": sum(row["gated_pass"] for row in rows),
            "product_pass": sum(row["product_pass"] for row in rows),
        }
    report = {
        "schema_version": 1,
        "queries": len(records),
        "weight": args.weight,
        "summary": {
            "aden_pass": sum(row["aden_pass"] for row in records),
            "global_fused_pass": sum(row["fused_pass"] for row in records),
            "gated_pass": sum(row["gated_pass"] for row in records),
            "product_pass": sum(row["product_pass"] for row in records),
        },
        "repositories": by_repo,
        "records": records,
    }
    rendered = json.dumps(report, indent=2) + "\n"
    if args.json:
        args.json.write_text(rendered, encoding="utf-8")
    print(rendered)


if __name__ == "__main__":
    main()
