#!/usr/bin/env python3
"""Time-boxed held-out breadth regression for native Aden routing."""

from __future__ import annotations

import argparse
import json
import subprocess
import time
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_MANIFEST = ROOT / "scripts" / "heldout-context" / "repos.json"


def run_mode(
    aden_bin: Path, repo: Path, queries: Path, mode: str, timeout: int
) -> tuple[dict[str, Any], int]:
    started = time.monotonic()
    command = [
        "python3",
        str(ROOT / "scripts" / "eval_corpus.py"),
        "--bin",
        str(aden_bin),
        "--repo",
        str(repo),
        "--queries",
        str(queries),
        "--mode",
        mode,
        "--quiet",
        "--json",
    ]
    try:
        result = subprocess.run(
            command, capture_output=True, text=True, timeout=timeout
        )
    except subprocess.TimeoutExpired:
        elapsed = round((time.monotonic() - started) * 1000)
        return {
            "timed_out": True,
            "timeout_seconds": timeout,
            "error": f"{mode} exceeded the {timeout}s per-mode limit",
        }, elapsed
    # eval_corpus exits 1 when accuracy is below its gate; valid JSON is still
    # the authoritative measurement and must not be discarded.
    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        return {
            "timed_out": False,
            "error": result.stderr[-500:] or str(error),
            "returncode": result.returncode,
        }, round((time.monotonic() - started) * 1000)
    return payload, round((time.monotonic() - started) * 1000)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--aden-bin", type=Path, default=ROOT / "target" / "release" / "aden")
    parser.add_argument("--repo", action="append", help="Run only this repository id")
    parser.add_argument("--timeout", type=int, default=180, help="Per repo/mode timeout")
    parser.add_argument("--json", type=Path)
    args = parser.parse_args()

    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    records = []
    for item in manifest["repositories"]:
        if args.repo and item["id"] not in args.repo:
            continue
        repo = Path(item["path"]).expanduser()
        queries = ROOT / item["queries"]
        print(f"[heldout] {item['id']} search", flush=True)
        search, search_ms = run_mode(args.aden_bin, repo, queries, "search", args.timeout)
        print(f"[heldout] {item['id']} ask", flush=True)
        ask, ask_ms = run_mode(args.aden_bin, repo, queries, "ask", args.timeout)
        print(f"[heldout] {item['id']} ask-context", flush=True)
        context, context_ms = run_mode(
            args.aden_bin, repo, queries, "ask-context", args.timeout
        )
        records.append(
            {
                **item,
                "search": search,
                "ask": ask,
                "context": context,
                "search_wall_ms": search_ms,
                "ask_wall_ms": ask_ms,
                "context_wall_ms": context_ms,
            }
        )
        if args.json:
            args.json.parent.mkdir(parents=True, exist_ok=True)
            args.json.write_text(
                json.dumps({"schema_version": 1, "partial": True, "records": records}, indent=2)
                + "\n",
                encoding="utf-8",
            )

    report = {"schema_version": 1, "repositories": len(records), "records": records}
    rendered = json.dumps(report, indent=2) + "\n"
    if args.json:
        args.json.write_text(rendered, encoding="utf-8")
    print(rendered)


if __name__ == "__main__":
    main()
