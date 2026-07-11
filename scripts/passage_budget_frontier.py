#!/usr/bin/env python3
"""Measure passage-context quality across strict token budgets with hard timeouts."""

from __future__ import annotations

import argparse
import json
from pathlib import Path

from passage_context_bench import ROOT, run_case, summarize


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--tasks", type=Path, default=ROOT / "scripts/passage-context/tasks.json"
    )
    parser.add_argument("--aden-bin", default="aden")
    parser.add_argument(
        "--budgets", type=int, nargs="+", default=[128, 192, 256, 320, 384, 448, 512]
    )
    parser.add_argument("--timeout", type=int, default=15)
    parser.add_argument("--json", type=Path)
    args = parser.parse_args()

    tasks = json.loads(args.tasks.read_text(encoding="utf-8"))["tasks"]
    reports = []
    for budget in args.budgets:
        print(f"[passage-frontier] budget={budget}", flush=True)
        rows = []
        for index, task in enumerate(tasks, 1):
            row = run_case(args.aden_bin, task, budget, args.timeout, True)
            rows.append(row)
            print(
                f"  {index:02d}/{len(tasks):02d} {task['id']}: "
                f"recall={row['fact_recall']:.4f}",
                flush=True,
            )
        report = {
            "budget": budget,
            **summarize(rows),
            "misses": [
                task["id"] for task, row in zip(tasks, rows) if not row["sufficient"]
            ],
        }
        reports.append(report)
        if args.json:
            args.json.write_text(
                json.dumps({"partial": True, "frontier": reports}, indent=2) + "\n"
            )
        print(
            f"[passage-frontier] result={json.dumps(report, sort_keys=True)}",
            flush=True,
        )

    payload = {"schema_version": 1, "frontier": reports}
    if args.json:
        args.json.write_text(json.dumps(payload, indent=2) + "\n")
    print(json.dumps(payload, indent=2))


if __name__ == "__main__":
    main()
