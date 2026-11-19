#!/usr/bin/env python3
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later

"""Compare bounded graph reads on an isolated, deterministic Rust scale fixture.

Each repetition starts with empty per-arm Aden data. Arms share identical source
paths and bytes; requests run serially in seeded order. OS caches are uncontrolled.
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

from cold_gen_bench import isolated_environment, peak_working_set
from read_path_bench import binary_identity


BEFORE = (
    "pub fn bench_leaf() -> i32 { 1 }\n"
    "pub fn bench_middle() -> i32 { bench_leaf() }\n"
    "pub fn bench_entry() -> i32 { bench_middle() }\n"
)
AFTER = BEFORE.replace("bench_leaf", "bench_twig").replace("{ 1 }", "{ 2 }")
PREFIX = "aden://module/graph-read-bench/chain.rs#"
ENTRY = PREFIX + "bench_entry"


def cpu_time_ms(process):
    """OS-recorded CPU of the main process; no descendant or wall-time estimate."""
    if os.name != "nt":
        return None
    import ctypes
    from ctypes import wintypes
    kernel, user, created, ended = (wintypes.FILETIME() for _ in range(4))
    query = ctypes.WinDLL("kernel32", use_last_error=True).GetProcessTimes
    query.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(wintypes.FILETIME)] * 4
    query.restype = wintypes.BOOL
    if not query(int(process._handle), ctypes.byref(created), ctypes.byref(ended), ctypes.byref(kernel), ctypes.byref(user)):
        return None
    ticks = sum((value.dwHighDateTime << 32) + value.dwLowDateTime for value in (kernel, user))
    return ticks / 10_000


def execute(binary, arguments, project, env, timeout):
    started = time.perf_counter()
    try:
        process = subprocess.Popen([str(binary), *arguments], cwd=project, env=env,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    except OSError as error:
        return dict(success=False, error=type(error).__name__, elapsed_ms=(time.perf_counter() - started) * 1000)
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
    elapsed = (time.perf_counter() - started) * 1000
    row = dict(success=process.returncode == 0 and not timed_out, returncode=process.returncode,
               elapsed_ms=elapsed, cpu_time_ms=cpu_time_ms(process),
               peak_cli_working_set_bytes=peak_working_set(process), traces=[])
    for line in stderr.decode("utf-8", errors="replace").splitlines():
        try:
            value = json.loads(line)
            if isinstance(value, dict) and value.get("type") in ("aden_gen_trace", "aden_read_trace"):
                row["traces"].append(value)
        except ValueError:
            pass
    if timed_out:
        row.update(error="timeout", cleanup_errors=cleanup_errors)
        if process.poll() is None:
            row["cleanup_incomplete_pid"] = process.pid
    elif process.returncode:
        row["error"] = "nonzero_exit"
    try:
        row["value"] = json.loads(stdout)
    except ValueError:
        pass
    return row


def fixture(project: Path, files: int, functions: int) -> str:
    project.mkdir(parents=True)
    entries = {
        "Cargo.toml": b"[package]\nname='graph-read-bench'\nversion='0.1.0'\n",
        "chain.rs": BEFORE.encode(),
    }
    for file in range(files):
        entries[f"unit_{file:04}.rs"] = "".join(
            f"pub fn filler_{file:04}_{function:03}() -> usize {{ {function} }}\n"
            for function in range(functions)
        ).encode()
    digest = hashlib.sha256()
    for name, content in sorted(entries.items()):
        (project / name).write_bytes(content)
        encoded = name.encode()
        digest.update(len(encoded).to_bytes(8, "big"))
        digest.update(encoded)
        digest.update(len(content).to_bytes(8, "big"))
        digest.update(content)
    return digest.hexdigest()


def receipt_error(value):
    if not isinstance(value, dict) or "error" in value:
        return "missing successful JSON object"
    receipt = value.get("context_receipt")
    if not isinstance(receipt, dict) or receipt.get("freshness") != "current":
        return "graph is not current"
    fingerprint = receipt.get("observed_source_fingerprint")
    if not isinstance(fingerprint, str) or not fingerprint:
        return "missing source observation"
    return None


def validate_query(value, edited=False, reverse=False):
    error = receipt_error(value)
    if error:
        return error
    if value.get("truncated") is not False or not isinstance(value.get("items"), list):
        return "missing or truncated query"
    leaf = "bench_twig" if edited else "bench_leaf"
    expected = ({(PREFIX + leaf, 0), (PREFIX + "bench_middle", 1), (ENTRY, 2)} if reverse else
                {(ENTRY, 0), (PREFIX + "bench_middle", 1), (PREFIX + leaf, 2)})
    observed = []
    for item in value["items"]:
        if not isinstance(item, dict) or not isinstance(item.get("anchor"), str):
            return "invalid query item"
        depth = item.get("depth")
        if type(depth) is not int or depth not in (0, 1, 2):
            return "invalid query depth"
        if depth and item.get("via_edge_types") != ["Calls"]:
            return "wrong query relationship"
        observed.append((item["anchor"], depth))
    if len(observed) != len(expected) or set(observed) != expected:
        return "query did not return the exact typed chain"
    return None


def definition_metadata_error(value, symbol, line):
    if not isinstance(value, dict):
        return "missing definition metadata"
    expected = dict(anchor=PREFIX + symbol, file="chain.rs", node_type="function",
                    start_line=str(line), end_line=str(line))
    if any(value.get(field) != expected_value for field, expected_value in expected.items()):
        return "definition metadata did not match the fixture"
    return None


def validate_understand(value, edited=False, middle=False):
    error = receipt_error(value)
    if error:
        return error
    leaf = "bench_twig" if edited else "bench_leaf"
    symbol, line = ("bench_middle", 2) if middle else (leaf, 1)
    content = AFTER if edited else BEFORE
    source = value.get("source")
    if not isinstance(source, dict) or source.get("state") != "complete" or source.get("hash_verified") is not True:
        return "source is unavailable, clipped, or unverified"
    if source.get("text") != content.splitlines(keepends=True)[line - 1]:
        return "source did not match the fixture definition"
    if value.get("anchor") != PREFIX + symbol or source.get("file") != "chain.rs":
        return "source has wrong anchor or file"
    if any(type(source.get(field)) is not int or source[field] != line
           for field in ("start_line", "end_line", "indexed_start_line", "indexed_end_line")):
        return "source has wrong line range"
    if source.get("last_line_complete") is not True:
        return "source has an incomplete final line"
    error = definition_metadata_error(value.get("definition"), symbol, line)
    if error:
        return error
    relationships = value.get("relationships")
    if not isinstance(relationships, dict):
        return "missing relationship directions"
    for field, direction, depth in (("backlinks", "incoming", 1), ("impact", "outgoing", 2)):
        relation = relationships.get(field)
        if (not isinstance(relation, dict) or relation.get("direction") != direction
                or type(relation.get("max_depth")) is not int or relation["max_depth"] != depth):
            return "wrong relationship direction or depth"
    backlinks, impact = value.get("backlinks"), value.get("impact")
    if not isinstance(backlinks, list) or not isinstance(impact, list):
        return "missing relationship arrays"
    expected_backlinks = {("mod-graph-read-bench", "Module", "Contains"),
                          (ENTRY if middle else PREFIX + "bench_middle", "Function", "Calls")}
    observed = []
    for node in backlinks:
        if (not isinstance(node, dict) or not isinstance(node.get("anchor"), str)
                or not isinstance(node.get("node_type"), str)
                or type(node.get("depth")) is not int or node["depth"] != 1
                or node.get("inferred") is not False
                or not isinstance(node.get("via_edge_types"), list)
                or len(node["via_edge_types"]) != 1 or not isinstance(node["via_edge_types"][0], str)):
            return "invalid incoming relationship"
        observed.append((node["anchor"], node["node_type"], node["via_edge_types"][0]))
    if len(observed) != len(expected_backlinks) or set(observed) != expected_backlinks:
        return "wrong incoming relationships"
    expected_impact = [(PREFIX + leaf, "Function", 1)] if middle else []
    observed = []
    for node in impact:
        if (not isinstance(node, dict) or not isinstance(node.get("anchor"), str)
                or not isinstance(node.get("node_type"), str) or type(node.get("depth")) is not int):
            return "invalid outgoing relationship"
        observed.append((node["anchor"], node["node_type"], node["depth"]))
    if len(observed) != len(expected_impact) or set(observed) != set(expected_impact):
        return "wrong outgoing relationships"
    context = value.get("context")
    if not isinstance(context, str) or f"{symbol}() -> i32" not in context:
        return "context omitted the requested definition"
    if middle and (f"{leaf}() -> i32" not in context or f"calls: {leaf}(2)" not in context):
        return "context omitted the dependency or call evidence"
    if edited and "bench_leaf" in context:
        return "context retained the deleted symbol"
    return None


def validate_grep(value):
    error = receipt_error(value)
    if error:
        return error
    if value.get("truncated") is not False or not isinstance(value.get("matches"), list):
        return "missing or truncated grep"
    expected = {(PREFIX + "bench_leaf", 1, BEFORE.splitlines()[0]),
                (PREFIX + "bench_middle", 2, BEFORE.splitlines()[1])}
    matches = value["matches"]
    if any(not isinstance(match, dict)
           or not isinstance(match.get("anchor"), str)
           or type(match.get("line")) is not int
           or not isinstance(match.get("text"), str) for match in matches):
        return "invalid grep match"
    observed = [(match.get("anchor"), match.get("line"), match.get("text")) for match in matches]
    if len(observed) != len(expected) or set(observed) != expected:
        return "grep did not return the exact definition and call"
    return None


def validate_search(value, edited=False):
    error = receipt_error(value)
    if error:
        return error
    results = value.get("results")
    if value.get("truncated") is not False or not isinstance(results, list):
        return "missing or truncated search"
    if any(not isinstance(result, dict) or not isinstance(result.get("anchor"), str) for result in results):
        return "invalid search result"
    anchors = [result["anchor"] for result in results]
    expected = PREFIX + ("bench_twig" if edited else "bench_leaf")
    if expected not in anchors or len(set(anchors)) != len(anchors):
        return "search omitted the current definition or duplicated results"
    if edited and PREFIX + "bench_leaf" in anchors:
        return "search retained the deleted definition"
    return None


def validate_definition(value, edited):
    error = receipt_error(value)
    if error:
        return error
    if type(value.get("total")) is not int:
        return "missing definition count"
    if edited:
        if value.get("total") != 1 or value.get("match_kind") != "definition":
            return "new definition missing or ambiguous"
        definitions = value.get("items")
        if (not isinstance(definitions, list) or len(definitions) != 1
                or not isinstance(definitions[0], dict)
                or definitions[0].get("anchor") != PREFIX + "bench_twig"):
            return "new definition has wrong anchor"
    elif value.get("total") != 0 or value.get("items") != []:
        return "deleted definition remains"
    return None


def validate_locate(value, edited=False, callers=False):
    error = receipt_error(value)
    if error:
        return error
    if (value.get("truncated") is not False or type(value.get("total")) is not int
            or type(value.get("returned")) is not int
            or value["total"] != 1 or value["returned"] != 1):
        return "wrong or incomplete locate counts"
    items = value.get("items")
    if not isinstance(items, list) or len(items) != 1 or not isinstance(items[0], dict):
        return "invalid locate items"
    if callers:
        if value.get("mode") != "callers" or value.get("match_kind") != "call_edges":
            return "locate did not use recorded call relationships"
        expected = dict(anchor=PREFIX + "bench_middle", file="chain.rs", start_line="2")
        if any(items[0].get(field) != expected_value for field, expected_value in expected.items()):
            return "locate returned the wrong caller"
    else:
        if value.get("mode") != "symbol" or value.get("match_kind") != "definition":
            return "locate did not return an exact definition"
        symbol = "bench_twig" if edited else "bench_leaf"
        error = definition_metadata_error(items[0], symbol, 1)
        if error:
            return error
        resolution = value.get("resolution")
        if (not isinstance(resolution, dict) or resolution.get("complete") is not True
                or resolution.get("state") not in ("exact", "unique")
                or resolution.get("anchor") != PREFIX + symbol):
            return "definition resolution is incomplete or has the wrong target"
    return None


def request(binary, arguments, project, env, timeout, validator):
    row = execute(binary, arguments, project, env, timeout)
    value = row.pop("value", None)
    try:
        error = validator(value)
    except (TypeError, ValueError, KeyError, AttributeError) as exception:
        # Unexpected JSON shapes are failed evidence, not a reason to discard
        # earlier measurements. Keep the original payload for diagnosis.
        error = f"validation error: {type(exception).__name__}"
    if error:
        row.setdefault("error", error)
    row["success"] = bool(row.get("success") and not error)
    row["evidence"] = value
    return row


def query_arguments(seed, mode="--from"):
    return ["query", mode, seed, "--edge-type", "Calls", "--depth", "2", "--max-results", "10"]


def rotated_order(arms, repetition):
    offset = repetition % len(arms)
    return arms[offset:] + arms[:offset]


def summarize(rows):
    groups = {}
    for row in rows:
        groups.setdefault((row["arm"], row["workload"]), []).append(row)
    summary = []
    for (arm, workload), attempts in sorted(groups.items()):
        good = [row for row in attempts if row["success"]]
        times = [row["elapsed_ms"] for row in good]
        memory = [row["peak_cli_working_set_bytes"] for row in good if row.get("peak_cli_working_set_bytes") is not None]
        cpu = [row["cpu_time_ms"] for row in good if row.get("cpu_time_ms") is not None]
        summary.append(dict(arm=arm, workload=workload, started=len(attempts), successful=len(good),
                            failures=len(attempts) - len(good), median_ms=statistics.median(times) if times else None,
                            min_ms=min(times) if times else None, max_ms=max(times) if times else None,
                            all_started_median_ms=statistics.median(row["elapsed_ms"] for row in attempts),
                            cpu_median_ms=statistics.median(cpu) if cpu else None,
                            peak_cli_working_set_median_bytes=statistics.median(memory) if memory else None))
    return summary


def evidence_parity(rows):
    """Compare source observations across arms within each fixture state."""
    groups = {}
    for row in rows:
        if not row["success"]:
            continue
        state = "after" if row["workload"].startswith(("post_edit", "warm_after_edit")) else "before"
        fingerprint = row["evidence"]["context_receipt"]["observed_source_fingerprint"]
        groups.setdefault((row["repetition"], state), set()).add(fingerprint)
    return bool(groups) and all(len(values) == 1 for values in groups.values())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", action="append", required=True, metavar="ARM=PATH")
    parser.add_argument("--files", type=int, default=512, help="Filler files, in addition to the chain")
    parser.add_argument("--functions", type=int, default=24, help="Functions per filler file")
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--timeout", type=float, default=120)
    parser.add_argument("--seed", type=int, default=17)
    parser.add_argument("--trace", action="store_true")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if min(args.files, args.functions, args.repetitions) < 1 or args.timeout <= 0:
        parser.error("files, functions, repetitions, and timeout must be positive")
    binaries = {}
    for declaration in args.binary:
        arm, separator, path = declaration.partition("=")
        if not separator or not arm or arm in binaries:
            parser.error("each binary needs a unique ARM=PATH")
        binaries[arm] = Path(path).resolve(strict=True)
    identities = {arm: binary_identity(binary) for arm, binary in binaries.items()}
    rng = random.Random(args.seed)
    initial_arms = list(binaries)
    rng.shuffle(initial_arms)
    samples, setup, hashes = [], [], []
    with tempfile.TemporaryDirectory(prefix="aden-graph-read-bench-") as scratch:
        base = Path(scratch)
        for repetition in range(args.repetitions):
            project = base / f"run-{repetition}" / "project"
            hashes.append(fixture(project, args.files, args.functions))
            environments = {}
            for arm_index, arm in enumerate(binaries):
                env = isolated_environment()
                env["ADEN_DATA_DIR"] = str(base / f"run-{repetition}" / f"data-{arm_index}")
                if args.trace:
                    env.update(ADEN_READ_TRACE="1", ADEN_GEN_TRACE="1")
                environments[arm] = env

            def measure(arm, workload, arguments, validator, destination=samples):
                row = request(binaries[arm], arguments, project, environments[arm], args.timeout, validator)
                row.update(arm=arm, workload=workload, repetition=repetition)
                destination.append(row)

            arms = rotated_order(initial_arms, repetition)
            for arm in arms:
                measure(arm, "cold_symbol", query_arguments("bench_entry"), validate_query)
            workloads = {
                "warm_symbol": (query_arguments("bench_entry"), validate_query),
                "warm_anchor": (query_arguments(ENTRY), validate_query),
                "warm_impact": (query_arguments(ENTRY, "--impact"), validate_query),
                "warm_backlinks": (query_arguments(PREFIX + "bench_leaf", "--backlinks"), lambda value: validate_query(value, reverse=True)),
                "warm_understand": (["understand", PREFIX + "bench_leaf"], validate_understand),
                "warm_understand_middle": (["understand", PREFIX + "bench_middle"], lambda value: validate_understand(value, middle=True)),
                "warm_locate_anchor": (["locate", "--symbol", PREFIX + "bench_leaf", "--json"], validate_locate),
                "warm_locate_symbol": (["locate", "--symbol", "bench_leaf", "--json"], validate_locate),
                "warm_caller_of": (["locate", "--caller-of", "bench_leaf", "--json"], lambda value: validate_locate(value, callers=True)),
                "warm_grep": (["grep", "bench_leaf", "--limit", "5", "--json"], validate_grep),
                "warm_search": (["search", "bench_leaf", "--json", "--limit", "10"], validate_search),
            }
            cells = [(arm, workload) for arm in binaries for workload in workloads]
            rng.shuffle(cells)
            for arm, workload in cells:
                arguments, validator = workloads[workload]
                measure(arm, workload, arguments, validator, setup)
            rng.shuffle(cells)
            for arm, workload in cells:
                arguments, validator = workloads[workload]
                measure(arm, workload, arguments, validator)
            source = project / "chain.rs"
            original = source.stat()
            source.write_bytes(AFTER.encode())
            os.utime(source, ns=(original.st_atime_ns, original.st_mtime_ns))
            for arm in reversed(arms):
                measure(arm, "post_edit_anchor", query_arguments(ENTRY), lambda value: validate_query(value, True))
                measure(arm, "warm_after_edit_impact", query_arguments(ENTRY, "--impact"), lambda value: validate_query(value, True))
                measure(arm, "warm_after_edit_backlinks", query_arguments(PREFIX + "bench_twig", "--backlinks"), lambda value: validate_query(value, True, reverse=True))
                measure(arm, "post_edit_search", ["search", "bench_twig", "--json", "--limit", "10"], lambda value: validate_search(value, True))
                measure(arm, "warm_after_edit_understand", ["understand", PREFIX + "bench_twig"], lambda value: validate_understand(value, edited=True))
                measure(arm, "warm_after_edit_understand_middle", ["understand", PREFIX + "bench_middle"], lambda value: validate_understand(value, edited=True, middle=True))
                measure(arm, "warm_after_edit_locate_anchor", ["locate", "--symbol", PREFIX + "bench_twig", "--json"], lambda value: validate_locate(value, edited=True))
                measure(arm, "warm_after_edit_locate_symbol", ["locate", "--symbol", "bench_twig", "--json"], lambda value: validate_locate(value, edited=True))
                measure(arm, "warm_after_edit_caller_of", ["locate", "--caller-of", "bench_twig", "--json"], lambda value: validate_locate(value, edited=True, callers=True))
                measure(arm, "post_edit_old_definition", ["locate", "--symbol", "bench_leaf", "--json"], lambda value: validate_definition(value, False), setup)
                measure(arm, "post_edit_new_definition", ["locate", "--symbol", "bench_twig", "--json"], lambda value: validate_definition(value, True), setup)
    parity = evidence_parity(samples + setup)
    report = dict(schema_version=1, kind="graph_read_comparison", binary_identities=identities,
                  environment=dict(platform=platform.platform(), cpu_count=os.cpu_count()),
                  fixture=dict(files=args.files, functions_per_file=args.functions, source_sha256=hashes,
                               source_files=args.files + 1, chain_before=BEFORE, chain_after=AFTER),
                  repetitions=args.repetitions, seed=args.seed, trace_enabled=args.trace,
                  ordering="Cold arms rotate a seeded starting order across repetitions; post-edit arms reverse that order. Warm cells are seeded shuffles.",
                  harness_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                  equal_source_observations=parity, setup=setup, samples=samples, summary=summarize(samples),
                  limits="Synthetic isolated CLI fixture. OS caches uncontrolled. All failures retained. Cold query includes automatic generation; warm requests each use a fresh CLI process. Windows peak memory is the main CLI process only. Trace overhead is separate from untraced timing. This does not establish LLM productivity, tail latency, or real-repository scalability.")
    args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"equal_source_observations": parity, "summary": report["summary"]}, indent=2))
    return int(not parity or any(not row["success"] for row in samples + setup))


if __name__ == "__main__":
    raise SystemExit(main())
