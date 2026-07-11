#!/usr/bin/env python3
"""Score bounded final context against source-grounded required facts."""

from __future__ import annotations

import argparse
import json
import os
import re
import statistics
import subprocess
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SOURCE_FILE_RE = re.compile(r":source_file:\s*(\S+)")


def search_rank(binary: str, task: dict, timeout: int) -> int | None:
    """Rank the expected source on the automatic product path for failure staging."""
    try:
        result = subprocess.run(
            [binary, "search", task["query"], str(Path(task["repo"]).expanduser()),
             "--json", "--limit", "20"],
            capture_output=True, text=True, timeout=timeout,
        )
        results = json.loads(result.stdout).get("results", []) if result.returncode == 0 else []
    except (subprocess.TimeoutExpired, json.JSONDecodeError):
        return None
    expected = [part.strip().casefold() for part in task.get("expected_source", "").split("|") if part.strip()]
    for rank, item in enumerate(results, 1):
        match = SOURCE_FILE_RE.search(item.get("snippet", "") or "")
        path = match.group(1) if match else item.get("anchor", "")
        if expected and any(part in path.casefold() for part in expected):
            return rank
    return None


def run_case(binary: str, task: dict, budget: int, timeout: int, automatic: bool) -> dict:
    env = os.environ.copy()
    if automatic:
        env.pop("ADEN_NAV_FUSION_OFF", None)
    else:
        env["ADEN_NAV_FUSION_OFF"] = "1"
    started = time.monotonic()
    try:
        result = subprocess.run(
            [binary, "ask", "--human", "--strict", "--budget", str(budget), "--project",
             str(Path(task["repo"]).expanduser()), task["query"]],
            capture_output=True, text=True, timeout=timeout, env=env,
        )
        context = result.stdout if result.returncode == 0 else ""
        error = result.stderr[-300:] if result.returncode != 0 else None
    except subprocess.TimeoutExpired:
        context, error = "", f"timeout after {timeout}s"
    elapsed_ms = round((time.monotonic() - started) * 1000)
    facts = [bool(re.search(pattern, context, re.IGNORECASE | re.DOTALL)) for pattern in task["facts"]]
    return {
        "facts": facts,
        "fact_recall": round(sum(facts) / len(facts), 4),
        "sufficient": all(facts),
        "tokens": (len(context.encode("utf-8")) + 3) // 4,
        "elapsed_ms": elapsed_ms,
        "error": error,
    }


def summarize(rows: list[dict]) -> dict:
    return {
        "tasks": len(rows),
        "sufficient": sum(row["sufficient"] for row in rows),
        "mean_fact_recall": round(statistics.mean(row["fact_recall"] for row in rows), 4),
        "median_tokens": statistics.median(row["tokens"] for row in rows),
        "median_elapsed_ms": statistics.median(row["elapsed_ms"] for row in rows),
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tasks", type=Path, default=ROOT / "scripts/passage-context/tasks.json")
    parser.add_argument("--aden-bin", default="aden")
    parser.add_argument("--budget", type=int, default=512)
    parser.add_argument("--timeout", type=int, default=15)
    parser.add_argument("--json", type=Path)
    args = parser.parse_args()
    tasks = json.loads(args.tasks.read_text(encoding="utf-8"))["tasks"]
    records = []
    for task in tasks:
        print(f"[passage-context] {task['id']}", flush=True)
        native = run_case(args.aden_bin, task, args.budget, args.timeout, False)
        automatic = run_case(args.aden_bin, task, args.budget, args.timeout, True)
        records.append({
            "id": task["id"],
            "expected_source": task.get("expected_source"),
            "search_rank": search_rank(args.aden_bin, task, args.timeout),
            "native": native,
            "automatic": automatic,
        })
        if args.json:
            args.json.write_text(json.dumps({"partial": True, "records": records}, indent=2) + "\n")
    report = {
        "schema_version": 1,
        "budget": args.budget,
        "native": summarize([row["native"] for row in records]),
        "automatic": summarize([row["automatic"] for row in records]),
        "records": records,
    }
    rendered = json.dumps(report, indent=2) + "\n"
    if args.json:
        args.json.write_text(rendered)
    print(rendered)


if __name__ == "__main__":
    main()
