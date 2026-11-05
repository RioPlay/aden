#!/usr/bin/env python3
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Bounded CLI retrieval microbenchmark; no LLM/productivity claims.

Index states are controlled; OS caches are not. Each sample uses a fresh isolated
fixture. Arms are deterministically shuffled per cell to reduce order bias.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import random
import statistics
import subprocess
import tempfile
import time


BEFORE = "pub fn leaf() -> i32 { 1 }\npub fn middle() -> i32 { leaf() }\npub fn entry() -> i32 { middle() }\n"
AFTER = BEFORE.replace("leaf", "twig").replace("{ 1 }", "{ 2 }")
WORKLOADS = {
    "source": ["read", "lib.rs", "--start-line", "1", "--end-line", "3"],
    "symbol": ["understand", "leaf", "--profile", "compact-v2", "--json"],
    "incoming": ["query", "--backlinks", "leaf", "--edge-type", "Calls", "--depth", "2", "--max-results", "10"],
    "outgoing": ["query", "--impact", "entry", "--edge-type", "Calls", "--depth", "2", "--max-results", "10"],
    "search": ["grep", "leaf", "--limit", "5", "--json"],
}
STATES = ("unindexed", "primed", "post_edit")


def workload_args(workload: str, edited: bool = False) -> list[str]:
    return [arg.replace("leaf", "twig") if edited else arg for arg in WORKLOADS[workload]]


def run(binary: Path, arguments: list[str], project: Path, env: dict[str, str], timeout: float) -> dict:
    started = time.perf_counter()
    try:
        result = subprocess.run(
            [str(binary), *arguments], cwd=project, env=env, capture_output=True,
            timeout=timeout, check=False,
        )
    except subprocess.TimeoutExpired:
        return {"elapsed_ms": (time.perf_counter() - started) * 1000, "exit_code": None, "error": "timeout"}
    except OSError as error:
        return {"elapsed_ms": (time.perf_counter() - started) * 1000, "exit_code": None, "error": f"execution failed: {type(error).__name__}"}
    elapsed = (time.perf_counter() - started) * 1000
    traces = []
    stderr = result.stderr.decode("utf-8", errors="replace")
    for line in stderr.splitlines():
        try:
            value = json.loads(line)
        except ValueError:
            continue
        if isinstance(value, dict) and value.get("type") == "aden_read_trace":
            traces.append(value)
    row = {"elapsed_ms": elapsed, "exit_code": result.returncode, "traces": traces}
    if result.returncode:
        row["error"] = stderr.replace(str(project.parent), "<fixture>")[:1000]
        return row
    try:
        row["value"] = json.loads(result.stdout)
    except ValueError:
        row["error"] = "successful command did not return JSON"
    return row


def validate(workload: str, value: object, content: str) -> str | None:
    """Behavioral smoke checks; the Rust suites establish full query semantics."""
    if not isinstance(value, dict) or "error" in value:
        return "missing successful JSON object"
    if workload == "source":
        if value.get("source", {}).get("text") != content:
            return "source did not match the actual fixture bytes"
        if value.get("observation") != "file_bytes_read" or "freshness" in value:
            return "source observation misrepresented graph freshness"
    else:
        if value.get("context_receipt", {}).get("freshness") != "current":
            return "graph request did not establish currentness"
        if workload == "symbol":
            source = value.get("source", {})
            if source.get("hash_verified") is not True or content.splitlines()[0] not in source.get("text", ""):
                return "symbol source was missing, stale, or unverified"
        elif workload == "search":
            if not any(content.splitlines()[0] in match.get("text", "") for match in value.get("matches", [])):
                return "search omitted the current definition"
        else:
            anchors = [item.get("anchor", "") for item in value.get("items", []) if isinstance(item, dict)]
            leaf = "twig" if content == AFTER else "leaf"
            symbols = [anchor.rsplit("#", 1)[-1] for anchor in anchors if isinstance(anchor, str)]
            if len(symbols) != 3 or set(symbols) != {"middle", "entry", leaf}:
                return "query did not return exactly the requested seed and typed relationship chain"
    return None


def sample(binary: Path, workload: str, state: str, timeout: float, trace: bool) -> dict:
    with tempfile.TemporaryDirectory(prefix="aden-read-bench-") as scratch:
        base = Path(scratch)
        project = base / "project"
        project.mkdir()
        (project / "Cargo.toml").write_text("[package]\nname='read-bench'\nversion='0.1.0'\n", encoding="utf-8")
        source = project / "lib.rs"
        source.write_bytes(BEFORE.encode())
        env = {key: value for key, value in os.environ.items() if not key.upper().startswith("ADEN_")}
        env["ADEN_DATA_DIR"] = str(base / "data")
        if trace:
            env["ADEN_READ_TRACE"] = "1"
        setup = []
        if state != "unindexed":
            started = time.perf_counter()
            try:
                generation = subprocess.run(
                    [str(binary), "gen", ".", "--quiet"], cwd=project, env=env,
                    capture_output=True, timeout=timeout, check=False,
                )
            except subprocess.TimeoutExpired:
                return {"success": False, "phase": "setup", "error": "generation timeout", "elapsed_ms": (time.perf_counter() - started) * 1000}
            except OSError as error:
                return {"success": False, "phase": "setup", "error": f"generation failed: {type(error).__name__}", "elapsed_ms": (time.perf_counter() - started) * 1000}
            setup.append({"phase": "generation", "elapsed_ms": (time.perf_counter() - started) * 1000, "exit_code": generation.returncode})
            if generation.returncode:
                return {"success": False, "phase": "setup", "error": "generation failed", "setup": setup, "elapsed_ms": sum(step["elapsed_ms"] for step in setup)}
            prime = run(binary, WORKLOADS[workload], project, env, timeout)
            prime_error = prime.get("error") or validate(workload, prime.get("value"), BEFORE)
            setup.append({"phase": "prime", **{key: value for key, value in prime.items() if key != "value"}})
            if prime_error:
                return {"success": False, "phase": "setup", "error": prime_error, "setup": setup, "elapsed_ms": sum(step["elapsed_ms"] for step in setup)}
        content = BEFORE
        if state == "post_edit":
            modified = source.stat()
            source.write_bytes(AFTER.encode())
            os.utime(source, ns=(modified.st_atime_ns, modified.st_mtime_ns))
            content = AFTER
        result = run(binary, workload_args(workload, state == "post_edit"), project, env, timeout)
        value = result.pop("value", None)
        error = result.get("error") or validate(workload, value, content)
        if error:
            result["error"] = error
        result.update(success=not error, phase="request", setup=setup)
        if state == "post_edit" and workload != "source" and not error:
            verification = []
            for symbol, count in (("leaf", 0), ("twig", 1)):
                probe = run(binary, ["locate", "--symbol", symbol, "--json"], project, env, timeout)
                evidence = probe.pop("value", {})
                verification.append(probe)
                if probe.get("error") or evidence.get("total") != count or (count and evidence.get("match_kind") != "definition"):
                    result.update(success=False, error="post-edit stored definitions did not replace leaf with twig")
            result["verification"] = verification
        if isinstance(value, dict):
            result["receipt"] = value.get("context_receipt")
            result["result_state"] = value.get("result_state")
            if workload in ("incoming", "outgoing"):
                result["returned_symbols"] = [item.get("anchor", "").rsplit("#", 1)[-1] for item in value.get("items", []) if isinstance(item, dict) and isinstance(item.get("anchor"), str)]
        return result


def summarize(samples: list[dict]) -> list[dict]:
    groups = {}
    for row in samples:
        groups.setdefault((row["arm"], row["workload"], row["state"]), []).append(row)
    summary = []
    for (arm, workload, state), rows in sorted(groups.items()):
        requests = [row for row in rows if row["phase"] == "request"]
        successes = [row["elapsed_ms"] for row in requests if row["success"]]
        all_times = [row.get("attempt_elapsed_ms", row["elapsed_ms"]) for row in rows]
        summary.append({
            "arm": arm, "workload": workload, "state": state,
            "started": len(rows), "successful": sum(row["success"] for row in rows),
            "failures": sum(not row["success"] for row in rows),
            "all_started_elapsed_median_ms": statistics.median(all_times),
            "successful_request_median_ms": statistics.median(successes) if successes else None,
            "successful_request_min_ms": min(successes) if successes else None,
            "successful_request_max_ms": max(successes) if successes else None,
        })
    return summary


def binary_identity(binary: Path) -> dict:
    digest = hashlib.sha256()
    with binary.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            digest.update(chunk)
    version = subprocess.run([str(binary), "--version"], capture_output=True, timeout=30, check=True)
    return {"name": binary.name, "sha256": digest.hexdigest(), "version": version.stdout.decode("utf-8").splitlines()}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", action="append", required=True, metavar="ARM=PATH")
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--timeout", type=float, default=30)
    parser.add_argument("--seed", type=int, default=17)
    parser.add_argument("--trace", action="store_true", help="Diagnostic run; tracing adds overhead. Do not pool with untraced timings.")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if args.repetitions < 1 or args.timeout <= 0:
        parser.error("repetitions and timeout must be positive")
    binaries = {}
    for declaration in args.binary:
        arm, separator, path = declaration.partition("=")
        if not separator or not arm or arm in binaries:
            parser.error("each binary needs a unique ARM=PATH")
        binaries[arm] = Path(path).resolve(strict=True)
    identities = {arm: binary_identity(binary) for arm, binary in binaries.items()}
    rows = []
    rng = random.Random(args.seed)
    for repetition in range(args.repetitions):
        for state in STATES:
            for workload in WORKLOADS:
                arms = list(binaries)
                rng.shuffle(arms)
                for arm in arms:
                    attempt_started = time.perf_counter()
                    result = sample(binaries[arm], workload, state, args.timeout, args.trace)
                    result["attempt_elapsed_ms"] = (time.perf_counter() - attempt_started) * 1000
                    rows.append({"arm": arm, "workload": workload, "state": state, "repetition": repetition, **result})
    report = {
        "schema_version": 1, "kind": "read_path_microbenchmark", "binary_identities": identities,
        "environment": {"platform": platform.platform(), "architecture": platform.machine(), "cpu_count": os.cpu_count(), "python": platform.python_version()},
        "repetitions": args.repetitions, "seed": args.seed, "trace_enabled": args.trace,
        "fixture_mutation": "Equal-size leaf-to-twig definition/call rename with preserved mtime; graph requests verify old definition absent and new definition present.",
        "limits": "Synthetic CLI fixtures; index states controlled, OS caches not controlled. No LLM/task-productivity or population-level latency claim. Request latency excludes separately recorded setup/verification. All-started elapsed includes entire attempts, including failures. Traced runs add overhead and must not be pooled with untraced timings.",
        "summary": summarize(rows), "samples": rows,
    }
    encoded = json.dumps(report, indent=2) + "\n"
    if args.output:
        args.output.write_text(encoded, encoding="utf-8")
    else:
        print(encoded, end="")
    return int(any(not row["success"] for row in rows))


if __name__ == "__main__":
    raise SystemExit(main())
