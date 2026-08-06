#!/usr/bin/env python3
"""Read-only product gauntlet against diverse external repositories.

The default profile uses Python, Go, Rust, and TypeScript projects under
~/Projects/upstream. --large adds Uno and a Linux source subset and may require
roughly 2 GiB of peak memory during a cold index. All Aden state is isolated in
a temporary ADEN_DATA_DIR; repository Git state and Aden marker files must be
unchanged after every case.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time


CASES = (
    ("eval-repos/flask", "class Flask", "Flask", "src", True),
    ("eval-repos/kin-openapi", "type Loader", "Loader", "openapi3", True),
    ("eval-repos/rustfmt", "format_input_inner", "format_input_inner", "src", True),
    ("pi", "rewriteSessionCwd", "rewriteSessionCwd", "packages", True),
)
LARGE_CASES = (
    ("eval-repos/uno", "class App", "App", "src", False),
    ("linux-aden-subset", "acpi_debugger_init", "acpi_debugger_init", "kernel", True),
)
MARKERS = (".aden", ".agent", ".adenignore", "AGENTS.md")


def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", default="~/Projects/upstream")
    parser.add_argument("--aden-bin", default="target/debug/aden")
    parser.add_argument("--large", action="store_true")
    parser.add_argument("--timeout", type=float, default=300.0)
    parser.add_argument("--max-conceptual-seconds", type=float, default=30.0)
    return parser.parse_args()


def git_state(repo: Path) -> bytes | None:
    result = subprocess.run(
        ["git", "-C", str(repo), "status", "--porcelain=v1", "-z", "--untracked-files=all"],
        capture_output=True,
        check=False,
    )
    return result.stdout if result.returncode == 0 else None


def marker_state(repo: Path) -> dict[str, tuple[bool, int, int]]:
    state: dict[str, tuple[bool, int, int]] = {}
    for marker in MARKERS:
        path = repo / marker
        if path.exists():
            stat = path.stat()
            state[marker] = (True, stat.st_size, stat.st_mtime_ns)
        else:
            state[marker] = (False, 0, 0)
    return state


def main() -> None:
    args = arguments()
    upstream = Path(args.root).expanduser().resolve()
    aden = Path(args.aden_bin).resolve()
    if not upstream.is_dir():
        raise SystemExit(f"upstream directory not found: {upstream}")
    if not aden.is_file():
        raise SystemExit(f"aden binary not found: {aden}")

    cases = list(CASES)
    if args.large:
        cases.extend(LARGE_CASES)
    missing = [relative for relative, *_ in cases if not (upstream / relative).is_dir()]
    if missing:
        raise SystemExit(f"required upstream repositories are missing: {', '.join(missing)}")

    work = Path(tempfile.mkdtemp(prefix="aden-upstream-gauntlet-"))
    env = os.environ.copy()
    env["ADEN_DATA_DIR"] = str(work / "data")

    def run(repo: Path, *argv: str) -> tuple[dict, float]:
        started = time.monotonic()
        result = subprocess.run(
            [str(aden), *argv],
            cwd=repo,
            env=env,
            text=True,
            capture_output=True,
            timeout=args.timeout,
            check=False,
        )
        elapsed = time.monotonic() - started
        if result.returncode:
            raise AssertionError((repo, argv, result.returncode, result.stdout, result.stderr))
        return json.loads(result.stdout), elapsed

    try:
        for index, (relative, pattern, symbol, scope, check_ask) in enumerate(cases, 1):
            repo = upstream / relative
            before_git = git_state(repo)
            before_markers = marker_state(repo)

            outline, elapsed = run(repo, "tree", "--symbols", str(repo))
            assert outline["format"] == "symbol-outline-v1", outline
            assert outline["context_receipt"]["freshness"] == "current", outline
            assert len(outline["outline"].encode("utf-8")) <= 96 * 1024, outline
            assert outline["returned_symbol_count"] <= outline["symbol_count"], outline
            if outline["truncated"]:
                assert outline["result_state"] == "truncated", outline
                assert outline["returned_symbol_count"] < outline["symbol_count"], outline
                assert outline.get("next_action"), outline
                scoped, _ = run(repo, "tree", "--symbols", str(repo / scope))
                assert scoped["symbol_count"] < outline["symbol_count"], (outline, scoped)
                assert len(scoped["outline"].encode("utf-8")) <= 96 * 1024, scoped
            else:
                assert outline["result_state"] == "complete", outline
                assert outline["returned_symbol_count"] == outline["symbol_count"], outline

            matches, _ = run(repo, "grep", pattern, str(repo))
            assert matches.get("returned", 0) > 0, matches
            assert matches["context_receipt"]["freshness"] == "current", matches
            located, _ = run(repo, "locate", "--symbol", symbol, str(repo))
            assert located.get("returned", 0) > 0, located
            assert located["context_receipt"]["freshness"] == "current", located
            if check_ask:
                answer, _ = run(repo, "ask", f"Where is {symbol} defined?", str(repo))
                assert answer["result_state"] == "bounded", answer
                assert answer["question_fit"] == "bounded", answer
                assert answer.get("context"), answer
                fragment = answer["anchor"].rsplit("#", 1)[-1]
                final_component = fragment.replace("::", ".").rsplit(".", 1)[-1]
                assert final_component == symbol, answer

            assert marker_state(repo) == before_markers, f"Aden marker footprint changed in {repo}"
            assert git_state(repo) == before_git, f"working tree changed in {repo}"
            print(
                f"[{index}/{len(cases)}] {relative}: {outline['file_count']} files, "
                f"{outline['symbol_count']} symbols, state={outline['result_state']}, "
                f"cold_tree={elapsed:.2f}s"
            )

        if args.large:
            linux = upstream / "linux-aden-subset"
            before_git = git_state(linux)
            before_markers = marker_state(linux)
            conceptual, elapsed = run(
                linux,
                "ask",
                "How does ACPI debugger initialization work?",
                str(linux),
            )
            assert conceptual["result_state"] == "bounded", conceptual
            assert conceptual.get("context"), conceptual
            assert elapsed <= args.max_conceptual_seconds, (
                f"large-repo conceptual ask took {elapsed:.2f}s; "
                f"ceiling is {args.max_conceptual_seconds:.2f}s"
            )
            assert marker_state(linux) == before_markers
            assert git_state(linux) == before_git
            print(
                f"[perf] linux conceptual ask: {elapsed:.2f}s "
                f"(ceiling {args.max_conceptual_seconds:.2f}s)"
            )
        print("upstream read-only gauntlet: PASS")
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
