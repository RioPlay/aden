#!/usr/bin/env python3
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Compare bounded, equivalent Calls queries on an existing indexed repository.

Uses normal per-user Aden storage, clearing inherited ADEN overrides. Priming may
refresh derived graph data; source files are never edited. Builds/scans must be idle.
"""

import argparse
import json
import os
from pathlib import Path
import platform
import random
import statistics

from read_path_bench import binary_identity, run


def validate(value, anchor):
    if not isinstance(value, dict):
        return "missing JSON object", []
    receipt = value.get("context_receipt")
    if not isinstance(receipt, dict) or receipt.get("freshness") != "current":
        return "graph is not current", []
    if not isinstance(receipt.get("observed_source_fingerprint"), str) or not receipt["observed_source_fingerprint"]:
        return "missing source observation", []
    items = value.get("items")
    if not isinstance(items, list) or value.get("truncated") is not False:
        return "missing or truncated result", []
    pairs = []
    for item in items:
        if not isinstance(item, dict) or not isinstance(item.get("anchor"), str):
            return "invalid result item", []
        depth = item.get("depth")
        if type(depth) is not int or depth not in (0, 1):
            return "invalid depth", []
        if depth == 0 and item["anchor"] != anchor:
            return "wrong seed", []
        via = item.get("via_edge_types")
        if depth == 1 and (not isinstance(via, list) or "Calls" not in via):
            return "wrong edge type", []
        pairs.append((item["anchor"], depth))
    if (anchor, 0) not in pairs or len({a for a, _ in pairs}) != len(pairs):
        return "missing seed or duplicate result", []
    return None, sorted(pairs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", action="append", required=True, metavar="ARM=PATH")
    parser.add_argument("--project", type=Path, required=True)
    parser.add_argument("--anchor", required=True, help="Exact canonical symbol anchor")
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--timeout", type=float, default=30)
    parser.add_argument("--seed", type=int, default=17)
    parser.add_argument("--trace", action="store_true")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.repetitions < 1 or args.timeout <= 0:
        parser.error("repetitions and timeout must be positive")
    project = args.project.resolve(strict=True)
    binaries = {}
    for declaration in args.binary:
        arm, separator, path = declaration.partition("=")
        if not separator or not arm or arm in binaries:
            parser.error("each binary needs a unique ARM=PATH")
        binaries[arm] = Path(path).resolve(strict=True)
    identities = {arm: binary_identity(binary) for arm, binary in binaries.items()}
    env = {k: v for k, v in os.environ.items() if not k.startswith("ADEN_")}
    if args.trace:
        env["ADEN_READ_TRACE"] = "1"
    modes = ("--from", "--impact")

    def request(arm, mode):
        command = ["query", mode, args.anchor, "--edge-type", "Calls", "--depth", "1", "--max-results", "100"]
        result = run(binaries[arm], command, project, env, args.timeout)
        value = result.pop("value", None)
        error, pairs = validate(value, args.anchor)
        result.update(arm=arm, mode=mode, success=not (result.get("error") or error), anchors=pairs)
        if error:
            result.setdefault("error", error)
        if isinstance(value, dict):
            result["receipt"] = value.get("context_receipt")
        return result

    # Keep warm-up outcomes/time in the report, but out of measured medians.
    setup = [request(arm, mode) for arm in binaries for mode in modes]
    rows = []
    rng = random.Random(args.seed)
    for repetition in range(args.repetitions):
        cells = [(arm, mode) for arm in binaries for mode in modes]
        rng.shuffle(cells)
        for arm, mode in cells:
            rows.append({"repetition": repetition, **request(arm, mode)})
    valid_sets = {tuple(row["anchors"]) for row in setup + rows if row["success"]}
    fingerprints = {row.get("receipt", {}).get("observed_source_fingerprint") for row in setup + rows if row["success"]}
    equal = len(valid_sets) == 1 and len(fingerprints) == 1 and None not in fingerprints
    summary = []
    for arm in binaries:
        for mode in modes:
            attempts = [r for r in rows if r["arm"] == arm and r["mode"] == mode]
            times = [r["elapsed_ms"] for r in attempts if r["success"]]
            summary.append(dict(arm=arm, mode=mode, started=len(attempts), successful=len(times), failures=len(attempts)-len(times), median_ms=statistics.median(times) if times else None, min_ms=min(times) if times else None, max_ms=max(times) if times else None))
    report = dict(schema_version=1, kind="existing_repo_query_comparison", project_name=project.name, anchor=args.anchor, edge_type="Calls", depth=1, max_results=100, trace_enabled=args.trace, repetitions=args.repetitions, seed=args.seed, environment=dict(platform=platform.platform(), cpu_count=os.cpu_count()), binary_identities=identities, equal_result_sets_and_source=equal, setup=setup, samples=rows, summary=summary, limits="Warm CLI index state; OS caches uncontrolled. Times include request subprocess startup. All failures retained. Traced runs separate from untraced comparisons. No LLM task productivity or universal latency claim.")
    args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"equal_result_sets_and_source": equal, "summary": summary}, indent=2))
    return 0 if equal and all(r["success"] for r in setup + rows) else 1


if __name__ == "__main__":
    raise SystemExit(main())
