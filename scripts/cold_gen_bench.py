#!/usr/bin/env python3
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Isolated release cold-generation benchmark; snapshots sources, never cleans live data.

Peak memory is the main CLI process's OS-recorded peak working set on Windows,
not the sum of descendant memory. OS file caches are uncontrolled.
"""
from __future__ import annotations
import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import platform
import random
import shutil
import statistics
import subprocess
import tempfile
import time

from read_path_bench import binary_identity


def isolated_environment():
    git_scope = {"GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR", "GIT_INDEX_FILE", "GIT_PREFIX",
                 "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_NAMESPACE"}
    return {k: v for k, v in os.environ.items()
            if not k.startswith(("ADEN_", "GIT_TRACE")) and k not in git_scope}


def git_root_lookups(trace_file):
    if not trace_file.is_file():
        return None
    total = 0
    for line in trace_file.read_bytes().splitlines():
        try:
            event = json.loads(line)
            if event.get("event") == "start" and event.get("argv", [])[-2:] == ["rev-parse", "--show-toplevel"]:
                total += 1
        except (ValueError, AttributeError):
            pass
    return total


def peak_working_set(process):
    if os.name != "nt":
        return None
    from ctypes import wintypes
    class Counters(ctypes.Structure):
        _fields_ = [("cb", wintypes.DWORD), ("faults", wintypes.DWORD)] + [
            (name, ctypes.c_size_t) for name in (
                "peak", "working", "peak_paged", "paged", "peak_nonpaged",
                "nonpaged", "pagefile", "peak_pagefile", "private")]
    counters = Counters()
    counters.cb = ctypes.sizeof(counters)
    query = ctypes.WinDLL("psapi", use_last_error=True).GetProcessMemoryInfo
    query.argtypes = [wintypes.HANDLE, ctypes.POINTER(Counters), wintypes.DWORD]
    query.restype = wintypes.BOOL
    if query(int(process._handle), ctypes.byref(counters), counters.cb):
        return counters.peak
    return None


def execute(binary, arguments, project, env, timeout):
    started = time.perf_counter()
    try:
        process = subprocess.Popen([str(binary), *arguments], cwd=project, env=env,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    except OSError as error:
        return dict(success=False, error=type(error).__name__, elapsed_ms=(time.perf_counter()-started)*1000)
    timed_out = False
    cleanup_errors = []
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired as error:
        timed_out = True
        stdout, stderr = error.output or b"", error.stderr or b""
        if os.name == "nt" and process.poll() is None:
            try:
                subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"], capture_output=True, timeout=5)
            except (OSError, subprocess.SubprocessError) as error:
                cleanup_errors.append(type(error).__name__)
        try:
            if process.poll() is None:
                process.kill()
            stdout, stderr = process.communicate(timeout=5)
        except (OSError, subprocess.SubprocessError) as error:
            cleanup_errors.append(type(error).__name__)
            # A Windows pipe reader may still hold the stream lock while a
            # descendant owns its write handle; closing here could block.
    elapsed = (time.perf_counter()-started)*1000
    traces = []
    for line in stderr.decode("utf-8", errors="replace").splitlines():
        try:
            value = json.loads(line)
            if isinstance(value, dict) and value.get("type") in ("aden_gen_trace", "aden_read_trace"):
                traces.append(value)
        except ValueError:
            pass
    row = dict(success=process.returncode == 0 and not timed_out, returncode=process.returncode,
               elapsed_ms=elapsed, peak_cli_working_set_bytes=peak_working_set(process), traces=traces)
    if timed_out:
        row["error"] = "timeout"
        row["cleanup_errors"] = cleanup_errors
        if process.poll() is None:
            row["cleanup_incomplete_pid"] = process.pid
    elif process.returncode:
        row["error"] = "nonzero_exit"
    try:
        row["value"] = json.loads(stdout)
    except ValueError:
        pass  # Human gen output is intentionally not retained.
    return row


def query_evidence(value, seed):
    if not isinstance(value, dict) or value.get("truncated") is not False:
        raise ValueError("missing_or_truncated_query")
    receipt = value.get("context_receipt", {})
    if not isinstance(receipt, dict) or receipt.get("freshness") != "current" or not isinstance(receipt.get("observed_source_fingerprint"), str) or not receipt["observed_source_fingerprint"]:
        raise ValueError("unverified_query")
    items = value.get("items", [])
    if not isinstance(items, list):
        raise ValueError("invalid_query_items")
    pairs = []
    for item in items:
        if not isinstance(item, dict):
            raise ValueError("invalid_query_item")
        anchor, depth = item.get("anchor"), item.get("depth")
        if not isinstance(anchor, str) or type(depth) is not int or depth not in (0, 1):
            raise ValueError("invalid_query_item")
        via = item.get("via_edge_types")
        if depth == 1 and (not isinstance(via, list) or "Calls" not in via):
            raise ValueError("wrong_relationship")
        pairs.append((anchor, depth))
    seeds = [anchor.rsplit("#", 1)[-1] for anchor, depth in pairs if depth == 0]
    if seeds != [seed]:
        raise ValueError("missing_seed")
    if len({anchor for anchor, _ in pairs}) != len(pairs):
        raise ValueError("duplicate_query_item")
    return dict(anchors=sorted(pairs), source_fingerprint=receipt["observed_source_fingerprint"])


def summarize(rows):
    groups = {}
    for row in rows:
        groups.setdefault((row["fixture"], row["mode"], row["arm"]), []).append(row)
    result = []
    for (fixture, mode, arm), attempts in sorted(groups.items()):
        good = [r for r in attempts if r["success"]]
        times = [r["ready_ms"] for r in good]
        memory = [r["peak_cli_working_set_bytes"] for r in good if r.get("peak_cli_working_set_bytes") is not None]
        result.append(dict(fixture=fixture, mode=mode, arm=arm, started=len(attempts),
            successful=len(good), failures=len(attempts)-len(good),
            ready_median_ms=statistics.median(times) if times else None,
            ready_min_ms=min(times) if times else None, ready_max_ms=max(times) if times else None,
            peak_cli_working_set_median_bytes=statistics.median(memory) if memory else None,
            all_started_median_ms=statistics.median(r["attempt_ms"] for r in attempts)))
    return result


def snapshot_repo(source, destination):
    env = isolated_environment()
    subprocess.run(["git", "clone", "--shared", "--no-checkout", str(source), str(destination)],
                   env=env, capture_output=True, check=True, timeout=30)
    listing = subprocess.run(["git", "ls-files", "-c", "-o", "--exclude-standard", "-z"],
                             cwd=source, env=env, capture_output=True, check=True, timeout=30)
    for name in sorted(set(os.fsdecode(n) for n in listing.stdout.split(b"\0") if n)):
        relative = Path(name)
        if relative.is_absolute() or ".." in relative.parts:
            raise ValueError("unsafe_repository_file")
        original = source / relative
        if original.is_file() and not original.is_symlink():
            target = destination / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(original, target)


def scaling_fixture(destination, files, functions):
    (destination / "src").mkdir(parents=True)
    (destination / "Cargo.toml").write_bytes(b"[package]\nname='cold-gen-scaling'\nversion='0.1.0'\n")
    (destination / "src/lib.rs").write_bytes(b"pub fn bench_entry() { bench_leaf(); }\npub fn bench_leaf() {}\n")
    for file in range(files):
        lines = []
        for function in range(functions):
            previous = f"u{file:04}_f{function-1:03}();" if function else ""
            lines.append(f"pub fn u{file:04}_f{function:03}() {{ {previous} }}\n")
        (destination / f"src/unit_{file:04}.rs").write_bytes("".join(lines).encode())
    for arguments in (["init", "-q"], ["add", "."], ["-c", "user.name=Cold benchmark", "-c", "user.email=benchmark@example.invalid", "commit", "-q", "-m", "fixture"]):
        subprocess.run(["git", *arguments], cwd=destination, env=isolated_environment(), capture_output=True, check=True, timeout=30)


def sample(binary, validator, project, data, seed, mode, timeout, trace):
    env = isolated_environment()
    env["ADEN_DATA_DIR"] = str(data)
    if trace:
        env.update(ADEN_GEN_TRACE="1", ADEN_READ_TRACE="1")
    def measured(step, arguments):
        trace_file = data.with_suffix(f".{step}.git-events")
        if trace:
            env["GIT_TRACE2_EVENT"] = str(trace_file)
        result = execute(binary, arguments, project, env, timeout)
        if trace:
            result["git_root_lookups"] = git_root_lookups(trace_file)
        return result
    started = time.perf_counter()
    steps = []
    if mode == "manual":
        generation = measured("generation", ["gen", "."])
        generation.pop("value", None)
        steps.append(dict(step="generation", **generation))
        if not generation["success"]:
            return dict(success=False, error="generation_failed", steps=steps)
    request = measured("first_graph_read", ["query", "--from", seed, "--edge-type", "Calls", "--depth", "1", "--max-results", "100"])
    ready_ms = (time.perf_counter()-started)*1000
    value = request.pop("value", None)
    steps.append(dict(step="first_graph_read", **request))
    if not request["success"]:
        return dict(success=False, error="query_failed", steps=steps)
    try:
        evidence = query_evidence(value, seed)
        if seed == "bench_entry" and {(a.rsplit("#", 1)[-1], d) for a, d in evidence["anchors"]} != {("bench_entry", 0), ("bench_leaf", 1)}:
            raise ValueError("incorrect_scaling_fixture_neighborhood")
        snapshots = list(data.glob("projects/*/graph.snapshot"))
        if len(snapshots) != 1:
            raise ValueError("missing_snapshot")
        checked = execute(validator, [str(snapshots[0])], project, env, timeout)
        canonical = checked.pop("value", None)
        if not checked["success"] or not isinstance(canonical, dict) or not canonical.get("graph_sha256") or not canonical.get("cache_sha256"):
            raise ValueError("canonical_validation_failed")
        evidence.update(canonical)
        memories = [step["peak_cli_working_set_bytes"] for step in steps if step.get("peak_cli_working_set_bytes") is not None]
        return dict(success=True, ready_ms=ready_ms, peak_cli_working_set_bytes=max(memories) if memories else None,
                    evidence=evidence, steps=steps, validation_ms=checked["elapsed_ms"])
    except (ValueError, TypeError, KeyError) as error:
        return dict(success=False, error=str(error), steps=steps)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", action="append", required=True, metavar="ARM=PATH")
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--source", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--fixture", action="append", choices=["repo", "many-small"])
    parser.add_argument("--mode", action="append", choices=["manual", "automatic"])
    parser.add_argument("--files", type=int, default=256)
    parser.add_argument("--functions", type=int, default=24)
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--timeout", type=float, default=90)
    parser.add_argument("--trace", action="store_true")
    parser.add_argument("--seed", type=int, default=17)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if min(args.files, args.functions, args.repetitions) < 1 or args.timeout <= 0:
        parser.error("sizes, repetitions and timeout must be positive")
    binaries = {}
    for declaration in args.binary:
        arm, separator, path = declaration.partition("=")
        if not separator or not arm or arm in binaries:
            parser.error("each binary needs a unique ARM=PATH")
        binaries[arm] = Path(path).resolve(strict=True)
    validator = args.evidence.resolve(strict=True)
    source = args.source.resolve(strict=True)
    identities = {arm: binary_identity(binary) for arm, binary in binaries.items()}
    rows = []
    setup_error = None
    with tempfile.TemporaryDirectory(prefix="aden-cold-gen-") as work:
        base = Path(work)
        fixtures = {}
        try:
            for fixture in args.fixture or ["repo", "many-small"]:
                project = base / fixture
                if fixture == "repo":
                    snapshot_repo(source, project)
                    seed = "ensure_fresh_with_policy"
                else:
                    scaling_fixture(project, args.files, args.functions)
                    seed = "bench_entry"
                fixtures[fixture] = (project, seed)
        except (OSError, ValueError, subprocess.SubprocessError) as error:
            setup_error = type(error).__name__
        rng = random.Random(args.seed)
        mode_names = args.mode or ["manual", "automatic"]
        arm_orders = {}
        for fixture in fixtures:
            for mode in mode_names:
                arms = list(binaries)
                rng.shuffle(arms)
                arm_orders[fixture, mode] = arms
        if not setup_error:
            for repetition in range(args.repetitions):
                for fixture, (project, seed) in fixtures.items():
                    for mode in mode_names:
                        arms = arm_orders[fixture, mode]
                        for arm in (arms if repetition % 2 == 0 else reversed(arms)):
                            started = time.perf_counter()
                            try:
                                result = sample(binaries[arm], validator, project, base / f"data-{len(rows)}", seed, mode, args.timeout, args.trace)
                            except Exception as error:
                                # Retain unexpected per-attempt failures without
                                # logging exception messages/source contents.
                                result = dict(success=False, error=type(error).__name__)
                            rows.append(dict(fixture=fixture, mode=mode, arm=arm, repetition=repetition,
                                             attempt_ms=(time.perf_counter()-started)*1000, **result))
        parity = {}
        for fixture in fixtures:
            good = [r for r in rows if r["fixture"] == fixture and r["success"]]
            signatures = {json.dumps(r["evidence"], sort_keys=True) for r in good}
            parity[fixture] = len(signatures) == 1
        report = dict(schema_version=1, kind="cold_generation_to_first_query", binary_identities=identities,
            validator_identity=dict(sha256=hashlib.sha256(validator.read_bytes()).hexdigest()),
            environment=dict(platform=platform.platform(), cpu_count=os.cpu_count()),
            repetitions=args.repetitions, seed=args.seed, trace_enabled=args.trace, files=args.files, functions=args.functions,
            setup_error=setup_error, semantic_parity=parity, summary=summarize(rows), samples=rows,
            limits="Release builds selected explicitly; new isolated ADEN_DATA_DIR per attempt. Fixed source snapshots including dirty source. CLI automatic mode exercises the same silent generation path used by MCP; transport excluded. OS caches uncontrolled. Peak memory is main CLI process working set only (Windows), excludes Git/other children and the untimed semantic validator. Stages may overlap; do not sum inclusive read traces. No population tail or LLM-productivity claim.")
        args.output.write_text(json.dumps(report, indent=2)+"\n", encoding="utf-8")
        print(json.dumps({k: report[k] for k in ("setup_error", "semantic_parity", "summary")}, indent=2))
        return 0 if rows and not setup_error and all(parity.values()) and all(r["success"] for r in rows) else 1


if __name__ == "__main__":
    raise SystemExit(main())
