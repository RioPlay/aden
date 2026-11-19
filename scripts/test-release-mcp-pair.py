#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Smoke the installed Aden pair without Cargo, the build tree, or MCP setup."""

import argparse
import json
import os
from pathlib import Path
from queue import Empty, Queue
import subprocess
from tempfile import TemporaryDirectory
from threading import Thread
from time import monotonic


def read_over_mcp(command, project, data, version):
    environment = os.environ.copy()
    for name in ("ADEN_BIN", "ADEN_PROJECT"):
        environment.pop(name, None)
    environment["ADEN_DATA_DIR"] = str(data)
    # The standalone server must use this installation's sibling CLI.
    environment["PATH"] = ""
    with subprocess.Popen(
        [*command, str(project), "--surface", "essential"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        text=True,
        encoding="utf-8",
        env=environment,
        cwd=project,
    ) as server:
        replies = Queue()

        def collect():
            try:
                for line in server.stdout:
                    replies.put(json.loads(line))
            except (ValueError, OSError) as error:
                replies.put(error)
            finally:
                replies.put(None)

        reader = Thread(target=collect, daemon=True)
        reader.start()

        def request(method, params, request_id=None):
            message = {"jsonrpc": "2.0", "method": method, "params": params}
            if request_id is not None:
                message["id"] = request_id
            server.stdin.write(json.dumps(message) + "\n")
            server.stdin.flush()
            if request_id is None:
                return None
            deadline = monotonic() + 20
            while True:
                remaining = deadline - monotonic()
                if remaining <= 0:
                    raise RuntimeError(f"Timed out waiting for {method}")
                try:
                    reply = replies.get(timeout=remaining)
                except Empty as error:
                    raise RuntimeError(f"Timed out waiting for {method}") from error
                if not isinstance(reply, dict):
                    raise RuntimeError(f"Invalid stdio response to {method}: {reply}")
                if reply.get("id") == request_id:
                    if not isinstance(reply.get("result"), dict):
                        raise RuntimeError(f"{method} failed: {reply}")
                    return reply["result"]

        try:
            initialized = request(
                "initialize",
                {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": {"name": "aden-release-smoke", "version": "1"},
                },
                1,
            )
            if initialized.get("protocolVersion") != "2025-03-26":
                raise RuntimeError(f"Unexpected negotiated MCP protocol: {initialized}")
            if initialized.get("serverInfo", {}).get("version") != version:
                raise RuntimeError(f"Unexpected MCP server version: {initialized}")
            request("notifications/initialized", {})
            tools = request("tools/list", {}, 2)
            if "read" not in {tool["name"] for tool in tools["tools"]}:
                raise RuntimeError("Essential MCP surface did not advertise read")
            read = request(
                "tools/call",
                {"name": "read", "arguments": {"file": "lib.rs", "start_line": 2, "end_line": 2}},
                3,
            )
            if read.get("isError"):
                raise RuntimeError(f"Installed MCP source read failed: {read}")
            evidence = json.loads(read["content"][0]["text"])
            server.stdin.close()
            exit_code = server.wait(timeout=5)
            if exit_code:
                raise RuntimeError(f"Installed MCP server exited with status {exit_code}")
            return evidence["source"]
        finally:
            if not server.stdin.closed:
                server.stdin.close()
            if server.poll() is None:
                server.kill()
                server.wait()
            reader.join(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("install_dir", type=Path)
    parser.add_argument("version")
    args = parser.parse_args()
    suffix = ".exe" if os.name == "nt" else ""
    installed = args.install_dir.resolve()
    cli = installed / f"aden{suffix}"
    bridge = installed / f"aden-mcp{suffix}"
    for executable in (cli, bridge):
        output = subprocess.check_output([executable, "--version"], text=True, timeout=20)
        expected = f"{executable.stem} {args.version}"
        if output.splitlines()[:1] != [expected]:
            raise SystemExit(f"Expected {expected!r} from installed {executable.name}")

    with TemporaryDirectory(prefix="aden-release-mcp-") as temp:
        root = Path(temp)
        project = root / "fixture"
        project.mkdir()
        expected = "pub fn paired_release_definition() -> u32 { 42 }"
        (project / "lib.rs").write_text("// source fixture\n" + expected + "\n", encoding="utf-8")
        for label, command in (
            ("embedded", [cli, "mcp", "stdio"]),
            ("standalone", [bridge]),
        ):
            source = read_over_mcp(command, project, root / label, args.version)
            if (
                source.get("file") != "lib.rs"
                or source.get("start_line") != 2
                or source.get("end_line") != 2
                or source.get("text", "").rstrip("\r\n") != expected
                or not source.get("source_hash")
            ):
                raise SystemExit(f"{label} MCP did not return the requested source evidence: {source}")
    print(f"Installed Aden {args.version} CLI and both MCP entry points passed the source smoke")


if __name__ == "__main__":
    main()
