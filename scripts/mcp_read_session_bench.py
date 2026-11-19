#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Compare real MCP read sequences with a selected optimization off/on.

Every attempt is retained. This is an isolated synthetic latency diagnostic,
not evidence of agent productivity, cold disk performance, or tail latency.
"""
from __future__ import annotations

import argparse
from collections import deque
import hashlib
import json
import os
from pathlib import Path
import queue
import statistics
import subprocess
import tempfile
import threading
import time


SOURCE = "pub fn entry() { middle(); }\npub fn middle() { before(); }\npub fn before() {}\n"


def validate_deleted(payload):
    receipt = payload.get("context_receipt", {})
    if (payload.get("items") != [] or type(payload.get("returned")) is not int or payload["returned"] != 0
        or type(payload.get("total")) is not int or payload["total"] != 0
        or payload.get("match_kind") != "full_text_fallback"
        or payload.get("resolution", {}).get("state") != "not_found"
        or payload.get("truncated") is not False or receipt.get("freshness") != "current"
        or not receipt.get("graph_revision") or not receipt.get("observed_source_fingerprint")):
        raise ValueError("deleted definition is present or removal evidence is invalid")


def validate(tool, payload, symbol):
    receipt = payload.get("context_receipt", {})
    if receipt.get("freshness") != "current" or not receipt.get("graph_revision") or (tool != "understand" and not receipt.get("observed_source_fingerprint")):
        raise ValueError("missing authoritative current source observation")
    observation = {key: receipt.get(key) for key in ["freshness", "observed_source_fingerprint"]}
    prefix = "aden://module/read-session-fixture/lib.rs#"
    if tool == "locate":
        items = payload.get("items")
        if not isinstance(items, list) or len(items) != 1 or payload.get("match_kind") != "definition":
            raise ValueError("locate did not return one definition")
        anchors = [item.get("anchor", "") for item in items]
        if anchors != [prefix + symbol] or items[0].get("file") != "lib.rs" or int(items[0].get("start_line", 0)) != 3 or int(items[0].get("end_line", 0)) != 3:
            raise ValueError("wrong definition")
        return {"anchors": anchors, "observation": observation}
    if tool == "understand":
        source = payload.get("source", {})
        if (payload.get("anchor") != prefix + symbol or source.get("file") != "lib.rs"
            or source.get("start_line") != 3 or source.get("end_line") != 3
            or source.get("state") != "complete" or source.get("hash_verified") is not True
            or source.get("text", "").splitlines() != [f"pub fn {symbol}() {{}}"]):
            raise ValueError("understand lacks verified expected source")
        callers = payload.get("backlinks")
        if not isinstance(callers, list) or payload.get("impact") != []:
            raise ValueError("understand lacks expected leaf relationships")
        call_edges = [item for item in callers if "Calls" in item.get("via_edge_types", [])]
        if len(call_edges) != 1 or call_edges[0].get("anchor") != prefix + "middle" or call_edges[0].get("depth") != 1:
            raise ValueError("understand lacks the expected direct caller")
        relationships = sorted([{key: item.get(key) for key in ["anchor", "depth", "via_edge_types"]} for item in callers], key=lambda item: item["anchor"])
        return {"anchor": payload.get("anchor"), "source": source.get("text"), "backlinks": relationships, "observation": observation}
    items = payload.get("results", payload.get("items"))
    if not isinstance(items, list):
        raise ValueError("query has no result list")
    anchors = sorted(item.get("anchor", "") for item in items)
    expected = {prefix + name: depth for depth, name in enumerate(["entry", "middle", symbol])}
    if len(anchors) != 3 or set(anchors) != set(expected) or payload.get("truncated") is not False:
        raise ValueError("query did not return the exact complete call chain")
    for item in items:
        if item.get("depth") != expected[item["anchor"]] or (item["depth"] > 0 and "Calls" not in item.get("via_edge_types", [])):
            raise ValueError("wrong call depth or edge type")
    return {"items": sorted([{key: item.get(key) for key in ["anchor", "depth", "via_edge_types"]} for item in items], key=lambda item: item["anchor"]), "observation": observation}


def read_environment(binary, data, enabled, optimization="resident"):
    if optimization not in {"resident", "symbols"}:
        raise ValueError("unknown optimization")
    env = {k: v for k, v in os.environ.items() if not k.startswith("ADEN_")}
    env.update(ADEN_DATA_DIR=str(data), ADEN_BIN=str(binary),
        ADEN_READ_SESSION=str(int(enabled or optimization == "symbols")))
    if optimization == "symbols":
        env["ADEN_SYMBOL_LOOKUP_CACHE"] = str(int(enabled))
    return env


class Client:
    def __init__(self, binary, project, data, enabled, optimization="resident"):
        env = read_environment(binary, data, enabled, optimization)
        self.process = subprocess.Popen([str(binary), "mcp", "stdio", "--surface", "essential", str(project)],
            cwd=project, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, encoding="utf-8", bufsize=1)
        self.lines = queue.Queue()
        self.errors = deque(maxlen=32)
        self.next_id = 0
        def pump():
            for line in self.process.stdout:
                self.lines.put(line)
            self.lines.put(None)
        def drain():
            for line in self.process.stderr:
                self.errors.append(line[:1024])
        threading.Thread(target=pump, daemon=True).start()
        threading.Thread(target=drain, daemon=True).start()
        try:
            self.rpc("initialize", {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "read-session-bench", "version": "1"}})
            self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        except Exception:
            self.close()
            raise

    def send(self, message):
        self.process.stdin.write(json.dumps(message) + "\n")
        self.process.stdin.flush()

    def rpc(self, method, params):
        self.next_id += 1
        request_id = self.next_id
        self.send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        deadline = time.monotonic() + 60
        while True:
            try:
                line = self.lines.get(timeout=max(0.01, deadline - time.monotonic()))
            except queue.Empty:
                raise TimeoutError("MCP response deadline exceeded")
            if line is None:
                raise RuntimeError("MCP closed stdout: " + "".join(self.errors))
            message = json.loads(line)
            if message.get("id") == request_id:
                if "error" in message:
                    raise RuntimeError(str(message["error"]))
                return message["result"]
            if time.monotonic() >= deadline:
                raise TimeoutError("MCP response deadline exceeded")

    def call(self, tool, arguments):
        result = self.rpc("tools/call", {"name": tool, "arguments": arguments})
        if result.get("isError"):
            raise RuntimeError(str(result))
        return json.loads(result["content"][0]["text"])

    def close(self):
        if self.process.stdin:
            self.process.stdin.close()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)


def sequence(client, symbol, records, state, identity=None):
    projections = []
    start = time.perf_counter()
    for tool, arguments in [
        ("locate", {"symbol": symbol}),
        ("understand", {"symbol": symbol}),
        ("query", {"from": "entry", "depth": 2, "edge_type": "Calls", "max_results": 10}),
    ]:
        record = {**(identity or {}), "state": state, "tool": tool, "arguments": arguments, "valid": False}
        records.append(record)
        begin = time.perf_counter()
        try:
            payload = client.call(tool, arguments)
            record["response"] = payload
            projection = validate(tool, payload, symbol)
            record["valid"] = True
            projections.append(projection)
        except Exception as error:
            record["error"] = str(error)
            raise
        finally:
            record["elapsed_ms"] = (time.perf_counter() - begin) * 1000
    return {"elapsed_ms": (time.perf_counter() - start) * 1000, "projection": projections}


def memory_observation(client):
    """Absolute parent/live-child memory outside the timed requests, Windows only."""
    if os.name != "nt":
        return None
    pid = client.process.pid
    command = (f"$children = @(Get-CimInstance Win32_Process -Filter 'ParentProcessId = {pid}'); "
        f"$ids = @({pid}) + @($children | ForEach-Object {{ [int]$_.ProcessId }}); "
        "Get-Process -Id $ids -ErrorAction SilentlyContinue | Select-Object Id,WorkingSet64,PeakWorkingSet64 | ConvertTo-Json -Compress")
    try:
        result = subprocess.run(["powershell.exe", "-NoProfile", "-Command", command],
            capture_output=True, text=True, timeout=15, check=True)
        return {"parent_pid": pid, "processes": json.loads(result.stdout), "limit": "Live processes only. Exited children are unavailable; compare resident child observations only when both arms retain a worker."}
    except Exception as error:
        return {"unavailable": str(error)}


def live_child_ids(observation, parent_pid):
    if not observation or "processes" not in observation:
        return None
    processes = observation["processes"]
    if isinstance(processes, dict):
        processes = [processes]
    return sorted(row["Id"] for row in processes if row["Id"] != parent_pid)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--files", type=int, default=128)
    parser.add_argument("--functions", type=int, default=12)
    parser.add_argument("--repetitions", type=int, default=4)
    parser.add_argument("--optimization", choices=["resident", "symbols"], default="resident")
    args = parser.parse_args()
    binary = args.binary.resolve()
    report = {"binary": str(binary), "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "optimization": args.optimization, "harness_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "files": args.files, "functions": args.functions, "repetitions": args.repetitions,
        "attempts": [], "arms": [], "success": False,
        "limits": "Synthetic real MCP sequence; uncontrolled OS caches, small samples, no productivity/tail latency or memory comparison."}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    try:
        if args.files < 0 or args.functions < 1 or args.repetitions < 1:
            raise ValueError("invalid fixture/repetition size")
        with tempfile.TemporaryDirectory(prefix="aden-session-bench-") as work:
            work = Path(work)
            project = work / "project"
            project.mkdir()
            (project / "Cargo.toml").write_text('[package]\nname="read-session-fixture"\nversion="0.1.0"\nedition="2024"\n', encoding="utf-8")
            source = project / "lib.rs"
            for index in range(args.files):
                (project / f"filler_{index}.rs").write_text("".join(f"pub fn filler_{index}_{n}() {{}}\n" for n in range(args.functions)), encoding="utf-8")
            for repetition in range(args.repetitions):
                arms = [False, True] if repetition % 2 == 0 else [True, False]
                pairs = {}
                for enabled in arms:
                    source.write_text(SOURCE, encoding="utf-8")
                    resident_enabled = enabled or args.optimization == "symbols"
                    arm = {"repetition": repetition, "enabled": enabled, "resident_enabled": resident_enabled, "sequences": []}
                    report["arms"].append(arm)
                    client = None
                    try:
                        client = Client(binary, project, work / f"data-{repetition}-{int(enabled)}", enabled, args.optimization)
                        for state in ["cold", "warm_1", "warm_2"]:
                            seq = sequence(client, "before", report["attempts"], state, {"enabled": enabled, "repetition": repetition})
                            arm["sequences"].append({"state": state, **seq})
                        arm["warm_memory_observation"] = memory_observation(client)
                        old_stat = source.stat()
                        source.write_text(SOURCE.replace("before", "afterr"), encoding="utf-8")
                        os.utime(source, ns=(old_stat.st_atime_ns, old_stat.st_mtime_ns))
                        for state in ["post_edit", "warm_after_edit"]:
                            seq = sequence(client, "afterr", report["attempts"], state, {"enabled": enabled, "repetition": repetition})
                            arm["sequences"].append({"state": state, **seq})
                        deleted = client.call("locate", {"symbol": "before"})
                        arm["deleted_definition_check"] = deleted
                        validate_deleted(deleted)
                        arm["memory_observation"] = memory_observation(client)
                        before_ids = live_child_ids(arm["warm_memory_observation"], client.process.pid)
                        after_ids = live_child_ids(arm["memory_observation"], client.process.pid)
                        if before_ids is not None and after_ids is not None:
                            if before_ids != after_ids or len(after_ids) != int(resident_enabled):
                                raise ValueError("resident process was not retained across calls/edits or disabled arm retained a child")
                            arm["worker_identity_verified"] = True
                            arm["worker_pids"] = after_ids
                        else:
                            arm["worker_identity_verified"] = None
                        pairs[enabled] = [seq["projection"] for seq in arm["sequences"]]
                    except Exception as error:
                        arm["error"] = str(error)
                        raise
                    finally:
                        if client:
                            client.close()
                if pairs[False] != pairs[True]:
                    raise ValueError("enabled and disabled result/source projections differ")
            report["success"] = True
            report["summary_ms"] = {
                str(enabled): {state: statistics.median(seq["elapsed_ms"] for arm in report["arms"] if arm["enabled"] == enabled for seq in arm["sequences"] if seq["state"] == state)
                    for state in ["cold", "warm_1", "warm_2", "post_edit", "warm_after_edit"]}
                for enabled in [False, True]}
    except Exception as error:
        report["error"] = str(error)
    finally:
        args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({key: value for key, value in report.items() if key in {"success", "summary_ms", "error"}}))
    raise SystemExit(0 if report["success"] else 1)


if __name__ == "__main__":
    main()
