#!/usr/bin/env python3
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Measure real Flask, Express, and ripgrep workflows, retaining raw receipts.

Use clean checkouts named flask, express, and ripgrep under --root. The report
records their commits. Index state is isolated, and a temporary Flask edit is
restored byte-for-byte. No upstream code is executed. Established conceptual
queries are regression gates; new usage probes remain explicit observations.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import queue
import statistics
import subprocess
import tempfile
import threading
import time

REPOS = {
    "flask": ("full_dispatch_request", "How does Flask dispatch a request and handle exceptions?", "Flask.full_dispatch_request"),
    "express": ("res.json", "Where does Express send a JSON response?", "res.json"),
    "ripgrep": ("search_parallel", "Where does ripgrep choose between parallel and single-threaded search?", "run"),
}
EXPECTED_BACKLINKS = {
    "flask": {"/src/flask/app.py#Flask.wsgi_app"},
    "express": {"/lib/response.js#res.send"},
    "ripgrep": {"/crates/core/main.rs#run", "/crates/core/index/enabled.rs#read"},
}
USAGE_QUESTIONS = {
    "flask": "How do I configure logging in Flask?",
    "express": "How do I install Express?",
    "ripgrep": "How do I choose between parallel and single-threaded search in ripgrep?",
}


def mcp_locate(binary, repo, env, symbol):
    proc = subprocess.Popen([str(binary), "mcp", "stdio"], cwd=repo, env=env,
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, text=True, encoding="utf-8")
    messages = queue.Queue()
    def pump():
        for line in proc.stdout:
            try:
                messages.put(json.loads(line))
            except ValueError:
                pass
    threading.Thread(target=pump, daemon=True).start()
    threading.Thread(target=lambda: proc.stderr.read(), daemon=True).start()
    root_requests = 0
    def send(message):
        proc.stdin.write(json.dumps(message) + "\n")
        proc.stdin.flush()
    def call(identifier, method, params):
        nonlocal root_requests
        send(dict(jsonrpc="2.0", id=identifier, method=method, params=params))
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            message = messages.get(timeout=max(.1, deadline-time.monotonic()))
            if message.get("method") == "roots/list":
                root_requests += 1
                send(dict(jsonrpc="2.0", id=message["id"], result={"roots": [{"uri": repo.as_uri(), "name": repo.name}]}))
            if message.get("id") == identifier and ("result" in message or "error" in message):
                if "error" in message:
                    raise RuntimeError(message["error"])
                return message["result"]
        raise TimeoutError(method)
    try:
        call(1, "initialize", {"protocolVersion": "2024-11-05", "clientInfo": {"name": "aden-real-world", "version": "1"}, "capabilities": {"roots": {"listChanged": False}}})
        send(dict(jsonrpc="2.0", method="notifications/initialized"))
        names = [t["name"] for t in call(2, "tools/list", {})["tools"]]
        result = call(3, "tools/call", {"name": "locate", "arguments": {"symbol": symbol}})
        if result.get("isError"):
            raise RuntimeError(result)
        payload = json.loads(next(c["text"] for c in result["content"] if c["type"] == "text"))
        return dict(tools=names, roots_requests=root_requests, output=payload)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--aden-bin", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--large-repo", type=Path, help="Optional additional clean checkout for a scalability smoke test")
    parser.add_argument("--large-symbol", default="DisposableStore", help="Known unique symbol in --large-repo")
    parser.add_argument("--large-source", help="Expected project-relative definition file when the symbol is ambiguous")
    args = parser.parse_args()
    root, binary = args.root.resolve(), args.aden_bin.resolve()
    repo_paths = {name: root/name for name in REPOS}
    if args.large_repo:
        repo_paths["large"] = args.large_repo.resolve()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix="aden-trial-", dir=args.output.parent))
    env = dict(os.environ, ADEN_DATA_DIR=str(work / "data"))
    env.pop("ADEN_SKIP_AUTO_GEN", None)
    report = dict(
        started_at=datetime.now(timezone.utc).isoformat(), platform=platform.platform(),
        binary=str(binary), binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
        version=subprocess.check_output([str(binary), "--version"], text=True),
        work=str(work), repositories={}, checks=[],
    )
    def save():
        args.output.write_text(json.dumps(report, indent=2, ensure_ascii=False), encoding="utf-8")
    def check(repo, name, passed, required=True):
        report["checks"].append(dict(repo=repo, check=name, passed=bool(passed), required=required))
        print(f"{repo}: {name}: {'PASS' if passed else 'MISS'}", flush=True)
    def run(repo, label, *command):
        started = time.perf_counter()
        try:
            result = subprocess.run([str(binary), *command], cwd=repo_paths[repo], env=env,
                                    capture_output=True, timeout=900 if label == "large-gen" else 180)
        except subprocess.TimeoutExpired as error:
            report["repositories"][repo]["runs"][label] = dict(
                command=list(command), seconds=round(time.perf_counter()-started, 3),
                returncode=None, timed_out=True,
                stdout=(error.stdout or b"").decode("utf-8", "replace"),
                stderr=(error.stderr or b"").decode("utf-8", "replace"),
            )
            check(repo, f"{label} completes within its time limit", False)
            save()
            raise
        record = dict(command=list(command), seconds=round(time.perf_counter()-started, 3),
                      bytes=len(result.stdout), returncode=result.returncode,
                      stderr=result.stderr.decode("utf-8", "replace"))
        try:
            record["output"] = json.loads(result.stdout)
        except ValueError:
            record["output"] = result.stdout.decode("utf-8", "replace")
        report["repositories"][repo]["runs"][label] = record
        save()
        if result.returncode:
            raise RuntimeError((repo, command, record))
        return record["output"]
    def git(repo, *command):
        return subprocess.check_output(["git", "-C", str(repo_paths[repo]), *command]).decode("utf-8").strip()
    try:
        for repo, (symbol, question, conceptual_target) in REPOS.items():
            before = git(repo, "status", "--porcelain=v1", "--untracked-files=all")
            if before:
                raise RuntimeError(f"use a clean checkout: {repo}")
            record = report["repositories"][repo] = dict(commit=git(repo, "rev-parse", "HEAD"), tracked_files=len(git(repo, "ls-files").splitlines()), runs={})
            outline = run(repo, "cold-tree", "tree", "--symbols")
            check(repo, "cold index is current", outline.get("freshness") == "current")
            expected = "Flask.full_dispatch_request" if repo == "flask" else symbol
            for iteration in range(3):
                located = run(repo, f"locate-{iteration}", "locate", symbol)
                check(repo, f"exact definition {iteration+1}", located.get("resolution", {}).get("anchor", "").endswith("#"+expected))
            record["warm_locate_median_seconds"] = statistics.median(record["runs"][f"locate-{i}"]["seconds"] for i in range(3))
            answer = run(repo, "named-ask", "ask", f"Where is {symbol} defined?", "--json", "--explain")
            check(repo, "named question resolves implementation", (answer.get("anchor") or "").endswith("#"+expected))
            conceptual = run(repo, "conceptual-ask", "ask", question)
            check(repo, "conceptual question resolves implementation", (conceptual.get("anchor") or "").endswith("#"+conceptual_target))
            usage = run(repo, "usage-ask", "ask", USAGE_QUESTIONS[repo])
            check(repo, "usage question routes to documentation", (usage.get("anchor") or "").startswith("aden://doc/"), required=False)
            understood = run(repo, "understand", "understand", symbol)
            backlinks = [item.get("anchor", "") for item in understood.get("backlinks", [])]
            check(repo, "relationship context includes source-verified callers",
                  all(any(anchor.endswith(suffix) for anchor in backlinks)
                      for suffix in EXPECTED_BACKLINKS[repo]))
            if repo == "flask":
                callers = run(repo, "direct-callers", "locate", "--caller-of", symbol)
                check(repo, "class is not reported as its method's caller",
                      not any(item.get("anchor", "").endswith("#Flask") for item in callers.get("items", [])))
            missing = run(repo, "missing", "ask", "Where is aden_nonexistent_validation_symbol defined?")
            check(repo, "missing definition fails small", missing.get("anchor") is None and missing.get("result_state") == "empty")
            started = time.perf_counter()
            mcp = mcp_locate(binary, root/repo, env, symbol)
            mcp["seconds"] = round(time.perf_counter()-started, 3)
            record["mcp"] = mcp
            check(repo, "MCP workspace and definition", mcp["output"].get("resolution", {}).get("anchor", "").endswith("#"+expected) and mcp["output"].get("freshness") == "current")
            view = work/f"{repo}.html"
            run(repo, "view", "view", "--no-open", "--out", str(view))
            html = view.read_text(encoding="utf-8")
            data = json.JSONDecoder().raw_decode(html.split("const DATA = ", 1)[1])[0]
            record["viewer"] = dict(path=str(view), bytes=view.stat().st_size, nodes=len(data["nodes"]), edges=len(data["edges"]))
            check(repo, "simple viewer retains target without simulation", any(n.get("anchor", "").endswith("#"+expected) for n in data["nodes"]) and "ForceGraph" not in html and "requestAnimationFrame" not in html)
            check(repo, "checkout unchanged", git(repo, "status", "--porcelain=v1", "--untracked-files=all") == before)
            save()

        # One deliberate edit in a known source file, always restored exactly.
        source = root/"flask/src/flask/app.py"
        original = source.read_bytes()
        try:
            source.write_bytes(original + b"\n\ndef aden_validation_added_symbol():\n    return 'fresh'\n")
            changed = run("flask", "freshness-edit", "locate", "aden_validation_added_symbol")
            check("flask", "ordinary read sees source edit", changed.get("resolution", {}).get("state") == "unique" and changed.get("freshness") == "current")
        finally:
            source.write_bytes(original)
        restored = run("flask", "freshness-restore", "locate", "aden_validation_added_symbol")
        check("flask", "ordinary read prunes restored symbol", restored.get("resolution", {}).get("state") == "not_found" and restored.get("freshness") == "current")
        check("flask", "edit restored byte-for-byte", source.read_bytes() == original and not git("flask", "status", "--porcelain=v1", "--untracked-files=all"))

        if args.large_repo:
            if git("large", "status", "--porcelain=v1", "--untracked-files=all"):
                raise RuntimeError("use a clean large checkout")
            large = report["repositories"]["large"] = dict(
                path=str(repo_paths["large"]), commit=git("large", "rev-parse", "HEAD"),
                tracked_files=len(git("large", "ls-files").splitlines()), runs={},
            )
            # Explicit generation is appropriate for this large external clone.
            run("large", "large-gen", "gen")
            outline = run("large", "tree", "tree", "--symbols")
            check("large", "indexed outline is current and bounded", outline.get("freshness") == "current" and outline.get("truncated") is True)
            for iteration in range(3):
                located = run("large", f"locate-{iteration}", "locate", args.large_symbol)
                if args.large_source:
                    definitions = [item for item in located.get("items", [])
                                   if item.get("file", "").replace("\\", "/") == args.large_source.replace("\\", "/")
                                   and item.get("anchor", "").endswith("#" + args.large_symbol)]
                    expected_anchor = definitions[0]["anchor"] if len(definitions) == 1 else None
                    resolution = located.get("resolution", {})
                    resolved = expected_anchor is not None and (
                        resolution.get("anchor") == expected_anchor or
                        expected_anchor in resolution.get("candidates", []))
                else:
                    expected_anchor = located.get("resolution", {}).get("anchor")
                    resolved = located.get("resolution", {}).get("state") == "unique"
                check("large", f"expected definition {iteration+1}", resolved)
            large["warm_locate_median_seconds"] = statistics.median(large["runs"][f"locate-{i}"]["seconds"] for i in range(3))
            pin = ("--from", expected_anchor) if args.large_source and expected_anchor else ()
            answer = run("large", "named-ask", "ask", f"Where is {args.large_symbol} defined?", "--json", "--explain", *pin)
            check("large", "named question matches exact definition", expected_anchor is not None and answer.get("anchor") == expected_anchor)
            started = time.perf_counter()
            large["mcp"] = mcp_locate(binary, repo_paths["large"], env, expected_anchor or args.large_symbol)
            large["mcp"]["seconds"] = round(time.perf_counter()-started, 3)
            check("large", "MCP matches exact definition", expected_anchor is not None and large["mcp"]["output"].get("resolution", {}).get("anchor") == expected_anchor)
            check("large", "checkout unchanged", not git("large", "status", "--porcelain=v1", "--untracked-files=all"))
            save()
    finally:
        save()
    failures = [c for c in report["checks"] if c["required"] and not c["passed"]]
    print(f"Report: {args.output}; required failures: {len(failures)}", flush=True)
    raise SystemExit(bool(failures))


if __name__ == "__main__":
    main()
