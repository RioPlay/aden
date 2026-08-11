#!/usr/bin/env python3
"""Drive the standalone aden-mcp stdio server as a minimal LLM client.

The journey deliberately omits budget and strict arguments. Transport defaults
must remain bounded without forcing every model call to repeat boilerplate.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import select
import shutil
import subprocess
import tempfile
import time


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--mcp-bin", default="target/debug/aden-mcp")
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    mcp_bin = Path(args.mcp_bin).resolve()
    if not mcp_bin.is_file():
        raise SystemExit(f"aden-mcp binary not found: {mcp_bin}")

    work = Path(tempfile.mkdtemp(prefix="aden-mcp-live-qa-"))
    project = work / "project"
    data = work / "data"
    project.mkdir()
    (project / "Cargo.toml").write_text(
        '[package]\nname = "mcp-live-qa"\nversion = "0.1.0"\nedition = "2024"\n',
        encoding="utf-8",
    )
    (project / "main.rs").write_text(
        "/// Entry point exercised over the real MCP transport.\n"
        "fn main() { helper(); }\n"
        "fn helper() {}\n",
        encoding="utf-8",
    )

    env = os.environ.copy()
    env["ADEN_DATA_DIR"] = str(data)
    process = subprocess.Popen(
        [str(mcp_bin), str(project)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
        env=env,
    )
    assert process.stdin and process.stdout and process.stderr

    def send(message: dict[str, object]) -> None:
        process.stdin.write(json.dumps(message, separators=(",", ":")) + "\n")
        process.stdin.flush()

    def receive(request_id: int, timeout: float = 60.0) -> dict[str, object]:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            ready, _, _ = select.select(
                [process.stdout, process.stderr], [], [], 0.5
            )
            for stream in ready:
                line = stream.readline()
                if stream is process.stderr:
                    if line.strip():
                        print(f"aden-mcp: {line.rstrip()}", file=os.sys.stderr)
                    continue
                if not line:
                    raise RuntimeError("aden-mcp closed stdout")
                message = json.loads(line)
                if message.get("id") == request_id:
                    return message
            if process.poll() is not None:
                raise RuntimeError(f"aden-mcp exited with {process.returncode}")
        raise TimeoutError(f"timed out waiting for MCP response {request_id}")

    def tool_call(request_id: int, name: str, arguments: dict[str, object]) -> dict:
        send(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "method": "tools/call",
                "params": {"name": name, "arguments": arguments},
            }
        )
        rpc = receive(request_id)
        if "error" in rpc:
            raise AssertionError(rpc)
        text = rpc["result"]["content"][0]["text"]
        return json.loads(text)

    try:
        send(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": {"name": "aden-live-qa", "version": "1.0"},
                },
            }
        )
        initialized = receive(1)
        assert "result" in initialized, initialized
        instruction_bytes = len(initialized["result"].get("instructions", ""))
        assert instruction_bytes < 1_600, instruction_bytes
        send(
            {
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {},
            }
        )

        send({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})
        listed = receive(2)
        tool_list = listed["result"]["tools"]
        tools = {tool["name"] for tool in tool_list}
        required = {"tree", "ask", "asm", "grep", "locate", "query", "understand"}
        assert required <= tools, (required, tools)
        schemas = {tool["name"]: tool["inputSchema"] for tool in tool_list}
        assert "budget" not in schemas["ask"]["properties"]
        assert "strict" not in schemas["ask"]["properties"]
        assert "budget" not in schemas["asm"]["properties"]
        assert "format" not in schemas["asm"]["properties"]
        registry_bytes = len(json.dumps(tool_list, separators=(",", ":")))
        assert registry_bytes < 5_000, registry_bytes

        # No budget or strict field: these are transport policy, not model
        # boilerplate. A normal response still has to carry authoritative proof.
        ask = tool_call(3, "ask", {"question": "Where is the main entry point?"})
        assert ask.get("anchor"), ask
        assert ask.get("context"), ask
        ask_receipt = ask["context_receipt"]
        for field in (
            "freshness",
            "graph_revision",
            "observed_source_fingerprint",
            "refresh_cause",
        ):
            assert ask_receipt.get(field), (field, ask)

        assembly = tool_call(4, "asm", {"from": ask["anchor"]})
        assert assembly.get("documents"), assembly
        asm_receipt = assembly["context_receipt"]
        assert asm_receipt.get("freshness") == "current", assembly
        assert asm_receipt.get("graph_revision") == ask_receipt["graph_revision"]
        assert (
            asm_receipt.get("observed_source_fingerprint")
            == ask_receipt["observed_source_fingerprint"]
        )

        outline = tool_call(5, "tree", {"symbols": True})
        assert outline.get("format") == "symbol-outline-v1", outline
        assert outline.get("result_state") == "complete", outline
        assert outline.get("truncated") is False, outline
        assert outline.get("symbol_count") == outline.get("returned_symbol_count"), outline
        assert "main.rs:" in outline.get("outline", ""), outline
        assert " main" in outline.get("outline", ""), outline
        assert outline.get("symbol_count", 0) >= 1, outline

        broad = tool_call(
            6,
            "ask",
            {"question": "Find all security issues across the repository"},
        )
        assert broad.get("result_state") == "needs_narrowing", broad
        assert broad.get("question_fit") == "repository_wide", broad
        assert broad.get("context") == "", broad
        assert len(json.dumps(broad)) < 2_000, broad

        escaped = tool_call(7, "grep", {"pattern": "root", "path": "/etc"})
        assert escaped.get("error", {}).get("code") == "path_outside_workspace", escaped
        assert escaped["error"].get("recovery"), escaped

        print("live MCP bounded ask -> asm, broad guard, and path confinement passed")
        print("explicit budget sent: no")
        print(f"essential tool registry: {registry_bytes} bytes")
        print(f"startup guidance: {instruction_bytes} bytes")
        print(f"total recurring MCP context: {registry_bytes + instruction_bytes} bytes")
        print(f"anchor: {ask['anchor']}")
        print(f"graph revision: {ask_receipt['graph_revision']}")
    finally:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
