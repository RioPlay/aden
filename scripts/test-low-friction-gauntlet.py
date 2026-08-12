#!/usr/bin/env python3
"""DEPRECATED: product gauntlet lives in Rust integration tests.

Use:
  cargo test -p aden-cli --test low_friction_gauntlet
  ADEN_BIN=target/debug/aden cargo test -p aden-mcp --test mcp_live_gauntlet

This script remains for local/offline experiments only; Lean CI no longer runs it.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--aden-bin", default="target/debug/aden")
    parser.add_argument("--mcp-bin", default="target/debug/aden-mcp")
    parser.add_argument("--skip-mcp", action="store_true")
    return parser.parse_args()


def main() -> None:
    args = arguments()
    root = Path(__file__).resolve().parents[1]
    aden = Path(args.aden_bin).resolve()
    mcp = Path(args.mcp_bin).resolve()
    for binary in (aden, mcp):
        if not binary.is_file():
            raise SystemExit(f"binary not found: {binary}")

    work = Path(tempfile.mkdtemp(prefix="aden-low-friction-gauntlet-"))
    project = work / "project"
    project.mkdir()
    source = project / "main.rs"
    source.write_text(
        "/// First main entry point.\nfn main() { helper(); }\nfn helper() {}\n",
        encoding="utf-8",
    )
    (project / "Cargo.toml").write_text(
        '[package]\nname="gauntlet"\nversion="0.1.0"\nedition="2024"\n',
        encoding="utf-8",
    )
    env = os.environ.copy()
    env["ADEN_DATA_DIR"] = str(work / "data")

    def run(*argv: str, ok: bool = True) -> subprocess.CompletedProcess[str]:
        result = subprocess.run(
            [str(aden), *argv],
            text=True,
            capture_output=True,
            env=env,
            cwd=project,
            check=False,
        )
        if ok and result.returncode:
            raise AssertionError((argv, result.returncode, result.stdout, result.stderr))
        return result

    def ask() -> dict:
        # Normal model calls intentionally omit budget and strict boilerplate.
        result = run("ask", "Where is the main entry point?", ".")
        value = json.loads(result.stdout)
        assert value.get("anchor") and value.get("context"), value
        receipt = value["context_receipt"]
        for field in (
            "freshness",
            "graph_revision",
            "observed_source_fingerprint",
            "refresh_cause",
        ):
            assert receipt.get(field), (field, value)
        return value

    try:
        print("[gauntlet 1/6] cold-start defaults and zero project footprint")
        first = ask()
        assert first["budget"] == 4096 and first["expanded"] is False, first
        assert not (project / ".aden").exists()
        assert not (project / ".agent").exists()
        anchor = first["anchor"]
        assembly = json.loads(run("asm", "--from", anchor, ".").stdout)
        assert assembly.get("documents"), assembly
        assert (
            assembly["context_receipt"]["graph_revision"]
            == first["context_receipt"]["graph_revision"]
        )

        print("[gauntlet 2/6] compact outline and unsafe-question guard")
        outline_payload = json.loads(run("tree", "--symbols", ".").stdout)
        outline = outline_payload["outline"]
        assert outline_payload["format"] == "symbol-outline-v1"
        assert outline_payload["result_state"] == "complete"
        assert outline_payload["truncated"] is False
        assert outline_payload["symbol_count"] == outline_payload["returned_symbol_count"]
        assert "main.rs:" in outline and " main" in outline and " helper" in outline
        assert "helper();" not in outline
        broad_raw = run("ask", "Find all security issues across the repository", ".").stdout
        broad = json.loads(broad_raw)
        assert broad["result_state"] == "needs_narrowing" and broad["context"] == ""
        assert len(broad_raw.encode("utf-8")) < 2_000

        top_help = run("--help").stdout
        assert "  tree " in top_help and "  understand " in top_help
        assert "  overlay " not in top_help and "  audit " not in top_help
        assert "Optional Aden methodology" in run("commands").stdout

        print("[gauntlet 3/6] output-mode and strict-boundary matrix")
        llm = run("asm", "--format", "llm", "--from", anchor, ".").stdout
        assert "main" in llm and not llm.lstrip().startswith("{"), llm
        human = run("--human", "asm", "--from", anchor, ".").stdout
        assert "main" in human and not human.lstrip().startswith("{"), human
        help_text = run("asm", "--help").stdout
        assert "[default: json]" in help_text and "[default: llm]" not in help_text

        strict = json.loads(
            run("ask", "--strict", "Where is the main entry point?", ".").stdout
        )
        assert strict["context_receipt"].get("graph_revision"), strict
        tiny_raw = run(
            "ask",
            "--strict",
            "--budget",
            "15",
            "Where is the main entry point?",
            ".",
        ).stdout
        assert (len(tiny_raw.encode("utf-8")) + 3) // 4 <= 15, tiny_raw
        tiny = json.loads(tiny_raw)
        assert tiny.get("incomplete") or tiny.get("truncated"), tiny

        print("[gauntlet 4/6] no-change and same-size edit refresh")
        unchanged = ask()
        assert (
            unchanged["context_receipt"]["graph_revision"]
            == first["context_receipt"]["graph_revision"]
        )
        source.write_text(
            "/// Other main entry point.\nfn main() { helper(); }\nfn helper() {}\n",
            encoding="utf-8",
        )
        changed = ask()
        assert changed["context_receipt"]["refresh_cause"] == "source_changed", changed
        assert (
            changed["context_receipt"]["graph_revision"]
            != first["context_receipt"]["graph_revision"]
        )
        assert (
            changed["context_receipt"]["observed_source_fingerprint"]
            != first["context_receipt"]["observed_source_fingerprint"]
        )

        print("[gauntlet 5/6] huge-outline bound, explicit escape, and subtree recovery")
        (project / "large.rs").write_text(
            "".join(f"fn generated_symbol_{index}() {{}}\n" for index in range(4_200)),
            encoding="utf-8",
        )
        focused = project / "focused"
        focused.mkdir()
        (focused / "focus.rs").write_text("fn only_this_subtree() {}\n", encoding="utf-8")
        bounded_outline = json.loads(run("tree", "--symbols", ".").stdout)
        assert bounded_outline["result_state"] == "truncated", bounded_outline
        assert bounded_outline["truncated"] is True, bounded_outline
        assert bounded_outline["returned_symbol_count"] < bounded_outline["symbol_count"]
        assert len(bounded_outline["outline"].encode("utf-8")) <= 96 * 1024
        assert "subtree" in bounded_outline["next_action"].lower()

        full_outline = json.loads(run("--unlimited", "tree", "--symbols", ".").stdout)
        assert full_outline["result_state"] == "complete", full_outline
        assert full_outline["returned_symbol_count"] == full_outline["symbol_count"]
        assert full_outline["symbol_count"] == bounded_outline["symbol_count"]

        scoped_outline = json.loads(run("tree", "--symbols", "focused").stdout)
        assert scoped_outline["result_state"] == "complete", scoped_outline
        assert scoped_outline["file_count"] == 1, scoped_outline
        assert "only_this_subtree" in scoped_outline["outline"]
        assert "generated_symbol" not in scoped_outline["outline"]

        print("[gauntlet 6/6] standalone MCP client, no explicit budget")
        if not args.skip_mcp:
            subprocess.run(
                [
                    os.sys.executable,
                    str(root / "scripts/test-mcp-live.py"),
                    "--mcp-bin",
                    str(mcp),
                ],
                check=True,
                env=env,
                cwd=root,
            )
        print("low-friction gauntlet: PASS")
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
