#!/usr/bin/env python3
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Paired end-to-end agent benchmark for conventional and Aden navigation.

The benchmark runs the same repository question through two read-only Codex
conditions, captures the complete JSONL trajectory, and scores the final answer
against deterministic fact/evidence patterns.  A fixture engine keeps the
harness itself cheap and deterministic to test.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shlex
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_TASKS = ROOT / "scripts" / "agent-bench" / "tasks.json"
CONDITIONS = ("baseline", "aden")
ADEN_EXPECTED_CATEGORIES = {
    "architecture",
    "concept_discovery",
    "dependency_trace",
    "symbol_explanation",
}

ANSWER_SCHEMA = {
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "type": "object",
    "additionalProperties": False,
    "required": ["answer", "evidence"],
    "properties": {
        "answer": {"type": "string"},
        "evidence": {
            "type": "array",
            "items": {
                "type": "object",
                "additionalProperties": False,
                "required": ["path", "line", "anchor"],
                "properties": {
                    "path": {"type": "string"},
                    "line": {"type": ["integer", "null"]},
                    "anchor": {"type": ["string", "null"]},
                },
            },
        },
    },
}


def load_tasks(path: Path) -> dict[str, Any]:
    data = json.loads(path.read_text(encoding="utf-8"))
    if data.get("schema_version") != 1:
        raise ValueError("task corpus schema_version must be 1")
    repos = data.get("repositories")
    tasks = data.get("tasks")
    if not isinstance(repos, dict) or not isinstance(tasks, list) or not tasks:
        raise ValueError("task corpus requires repositories and a non-empty tasks list")
    seen: set[str] = set()
    for task in tasks:
        task_id = task.get("id")
        if not isinstance(task_id, str) or not task_id or task_id in seen:
            raise ValueError(f"invalid or duplicate task id: {task_id!r}")
        seen.add(task_id)
        if task.get("repository") not in repos:
            raise ValueError(f"{task_id}: unknown repository {task.get('repository')!r}")
        if not task.get("question") or not task.get("required_facts"):
            raise ValueError(f"{task_id}: question and required_facts are required")
        for fact in task["required_facts"]:
            if not fact.get("id") or not fact.get("any_of"):
                raise ValueError(f"{task_id}: every fact needs id and any_of")
            compile_patterns(fact["any_of"], f"{task_id}:{fact['id']}")
        for evidence in task.get("expected_evidence", []):
            compile_patterns(evidence.get("any_of", []), f"{task_id}:evidence")
        compile_patterns(task.get("forbidden_claims", []), f"{task_id}:forbidden")
    return data


def compile_patterns(patterns: list[str], label: str) -> list[re.Pattern[str]]:
    try:
        return [re.compile(pattern, re.IGNORECASE | re.MULTILINE) for pattern in patterns]
    except re.error as error:
        raise ValueError(f"{label}: invalid regex: {error}") from error


def resolve_repo(repo: dict[str, Any]) -> Path:
    env_key = repo.get("path_env")
    raw = os.environ.get(env_key, "") if env_key else ""
    raw = raw or repo.get("default_path", "")
    return Path(os.path.expandvars(os.path.expanduser(raw))).resolve()


def git_revision(path: Path) -> str | None:
    result = subprocess.run(
        ["git", "-C", str(path), "rev-parse", "HEAD"],
        capture_output=True,
        text=True,
    )
    return result.stdout.strip() if result.returncode == 0 else None


def score_response(task: dict[str, Any], response: dict[str, Any]) -> dict[str, Any]:
    answer = response.get("answer", "")
    evidence = response.get("evidence", [])
    evidence_text = "\n".join(
        " ".join(str(value or "") for value in (item.get("path"), item.get("anchor")))
        for item in evidence
        if isinstance(item, dict)
    )

    facts = []
    for fact in task["required_facts"]:
        matched = any(pattern.search(answer) for pattern in compile_patterns(fact["any_of"], fact["id"]))
        facts.append({"id": fact["id"], "matched": matched})

    evidence_scores = []
    for index, expected in enumerate(task.get("expected_evidence", []), start=1):
        matched = any(
            pattern.search(evidence_text)
            for pattern in compile_patterns(expected["any_of"], f"evidence-{index}")
        )
        evidence_scores.append({"id": expected.get("id", str(index)), "matched": matched})

    forbidden = [
        pattern.pattern
        for pattern in compile_patterns(task.get("forbidden_claims", []), "forbidden")
        if pattern.search(answer)
    ]
    fact_recall = sum(item["matched"] for item in facts) / len(facts)
    evidence_recall = (
        sum(item["matched"] for item in evidence_scores) / len(evidence_scores)
        if evidence_scores
        else 1.0
    )
    return {
        "fact_recall": round(fact_recall, 4),
        "evidence_recall": round(evidence_recall, 4),
        "forbidden_claims": forbidden,
        "grounded_complete": fact_recall == 1.0 and evidence_recall == 1.0 and not forbidden,
        "facts": facts,
        "evidence": evidence_scores,
    }


def prompt_for(task: dict[str, Any], condition: str) -> str:
    shared = f"""Answer this repository question using read-only investigation.

Question: {task['question']}

Return only the JSON object required by the provided output schema. Keep the answer concise but
complete. Evidence must cite repository-relative paths, line numbers when available, and Aden
anchors when used. Do not modify files, run tests, install dependencies, or access the network.
"""
    if condition == "baseline":
        return shared + """
Condition: conventional navigation. Do not invoke `aden` or any Aden MCP tool. Use normal
repository discovery such as `rg`, file reads, and read-only Git commands.
"""
    risk_terms = re.compile(r"\b(concurren|transaction|conflict|lock|security)\w*\b", re.IGNORECASE)
    budget = 1024 if risk_terms.search(task["question"]) else 512
    default_aden = ROOT / "target" / "release" / "aden"
    aden_bin = os.environ.get(
        "ADEN_BENCH_BIN", str(default_aden) if default_aden.is_file() else "aden"
    )
    aden_command = shlex.quote(aden_bin)
    return shared + f"""
Condition: Aden deterministic context navigation. Run this exact retrieval command first:
`{aden_command} ask --strict --budget {budget} --project . "{task['question']}"`.
Do not choose a routing strategy: Aden performs evidence-role routing, facet expansion, and source
arbitration internally. Treat its source bodies, paths, and anchors as verified evidence. Synthesize
immediately from that result. Do not run `rg`, `sed`, `aden asm`, or another retrieval command unless
the first command returns an explicit empty/error response; missing confidence alone is not a reason
to add tools. Mention no methodology in the answer.
Use the Aden CLI with an explicit `--project .` argument. Do not call an Aden MCP tool; benchmark
runs disable user MCP configuration so each task stays isolated to its assigned repository.
"""


def summarize_event(event: dict[str, Any]) -> dict[str, Any] | None:
    event_type = event.get("type", "unknown")
    if event_type != "item.completed":
        return None
    item = event.get("item") if isinstance(event.get("item"), dict) else {}
    item_type = item.get("type")
    if item_type in {"command_execution", "mcp_tool_call", "web_search"}:
        record: dict[str, Any] = {"event": event_type, "tool": item_type}
        if item.get("command"):
            record["command"] = item["command"]
        if item.get("server") or item.get("tool"):
            record["name"] = ".".join(filter(None, (item.get("server"), item.get("tool"))))
        return record
    return None


def method_compliance(
    condition: str, trajectory: list[dict[str, Any]], aden_expected: bool = True
) -> bool:
    invocations = "\n".join(
        " ".join(str(value or "") for value in (item.get("name"), item.get("command")))
        for item in trajectory
    )
    used_aden = bool(
        re.search(r"(^|[\s/'\"])aden(?:-mcp)?(?=[\s.]|$)", invocations, re.IGNORECASE)
    )
    return not used_aden if condition == "baseline" else (used_aden or not aden_expected)


def usage_from_events(events: list[dict[str, Any]]) -> dict[str, int]:
    usage: dict[str, int] = {}
    for event in events:
        candidate = event.get("usage")
        if not isinstance(candidate, dict):
            candidate = event.get("turn", {}).get("usage") if isinstance(event.get("turn"), dict) else None
        if isinstance(candidate, dict):
            for key, value in candidate.items():
                if isinstance(value, int):
                    usage[key] = max(usage.get(key, 0), value)
    return usage


def run_codex(repo: Path, task: dict[str, Any], condition: str, args: argparse.Namespace) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="aden-agent-bench-") as tmp:
        tmp_path = Path(tmp)
        schema_path = tmp_path / "answer.schema.json"
        answer_path = tmp_path / "answer.json"
        schema_path.write_text(json.dumps(ANSWER_SCHEMA), encoding="utf-8")
        command = [
            args.codex_bin,
            "exec",
            "--ephemeral",
            "--ignore-user-config",
            "--json",
            "--color",
            "never",
            "--sandbox",
            "read-only",
            "--output-schema",
            str(schema_path),
            "--output-last-message",
            str(answer_path),
            "--cd",
            str(repo),
        ]
        if args.model:
            command.extend(["--model", args.model])
        command.append(prompt_for(task, condition))
        started = time.monotonic()
        result = subprocess.run(command, capture_output=True, text=True, timeout=args.timeout)
        wall_ms = round((time.monotonic() - started) * 1000)
        events = []
        for line in result.stdout.splitlines():
            try:
                value = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(value, dict):
                events.append(value)
        if result.returncode != 0:
            return {
                "error": f"codex exited {result.returncode}: {result.stderr[-500:]}",
                "wall_ms": wall_ms,
                "trajectory": [record for event in events if (record := summarize_event(event))],
                "usage": usage_from_events(events),
            }
        try:
            response = json.loads(answer_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            return {"error": f"invalid structured answer: {error}", "wall_ms": wall_ms}
        return {
            "response": response,
            "wall_ms": wall_ms,
            "trajectory": [record for event in events if (record := summarize_event(event))],
            "usage": usage_from_events(events),
        }


def run_fixture(task: dict[str, Any], condition: str, run: int, args: argparse.Namespace) -> dict[str, Any]:
    path = args.fixture_dir / f"{task['id']}.{condition}.{run}.json"
    return {"response": json.loads(path.read_text(encoding="utf-8")), "wall_ms": 0, "trajectory": [], "usage": {}}


def aggregate(records: list[dict[str, Any]]) -> dict[str, Any]:
    by_condition: dict[str, Any] = {}
    for condition in CONDITIONS:
        rows = [row for row in records if row["condition"] == condition and not row.get("error")]
        by_condition[condition] = {
            "runs": len(rows),
            "grounded_completion": round(sum(row["score"]["grounded_complete"] for row in rows) / len(rows), 4) if rows else None,
            "mean_fact_recall": round(sum(row["score"]["fact_recall"] for row in rows) / len(rows), 4) if rows else None,
            "mean_evidence_recall": round(sum(row["score"]["evidence_recall"] for row in rows) / len(rows), 4) if rows else None,
            "median_wall_ms": sorted(row["wall_ms"] for row in rows)[len(rows) // 2] if rows else None,
            "mean_tool_calls": round(sum(len(row["trajectory"]) for row in rows) / len(rows), 2) if rows else None,
            "method_compliance": round(sum(row["method_compliant"] for row in rows) / len(rows), 4) if rows else None,
            "errors": sum(1 for row in records if row["condition"] == condition and row.get("error")),
        }
    return by_condition


def render_markdown(report: dict[str, Any]) -> str:
    lines = [
        "# Aden paired agent benchmark",
        "",
        f"Corpus: `{report['corpus']}`  ",
        f"Runs per task/condition: {report['runs_per_condition']}  ",
        f"Engine: `{report['engine']}`",
        "",
        "| Condition | Successful runs | Grounded completion | Fact recall | Evidence recall | Method compliance | Median wall | Tool calls | Errors |",
        "|---|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for condition in CONDITIONS:
        row = report["summary"][condition]
        percent = lambda value: "—" if value is None else f"{value:.1%}"
        wall = "—" if row["median_wall_ms"] is None else f"{row['median_wall_ms']} ms"
        calls = "—" if row["mean_tool_calls"] is None else str(row["mean_tool_calls"])
        lines.append(
            f"| {condition} | {row['runs']} | {percent(row['grounded_completion'])} | "
            f"{percent(row['mean_fact_recall'])} | {percent(row['mean_evidence_recall'])} | "
            f"{percent(row['method_compliance'])} | {wall} | {calls} | {row['errors']} |"
        )
    lines += [
        "",
        "Deterministic scoring checks required fact patterns, expected evidence, and forbidden claims. It does not judge prose quality.",
        "Repository revisions are pinned by the corpus; use `--allow-revision-mismatch` only for exploratory runs.",
    ]
    return "\n".join(lines) + "\n"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tasks", type=Path, default=DEFAULT_TASKS)
    parser.add_argument("--task", action="append", help="run only this task id (repeatable)")
    parser.add_argument("--condition", choices=[*CONDITIONS, "both"], default="both")
    parser.add_argument("--runs", type=int, default=1)
    parser.add_argument("--engine", choices=["codex", "fixture"], default="codex")
    parser.add_argument("--fixture-dir", type=Path)
    parser.add_argument("--codex-bin", default="codex")
    parser.add_argument("--model")
    parser.add_argument("--timeout", type=int, default=300)
    parser.add_argument("--allow-revision-mismatch", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--json", type=Path, help="write the full result JSON")
    parser.add_argument("--md", type=Path, help="write the summary Markdown")
    args = parser.parse_args()
    if args.runs < 1:
        parser.error("--runs must be positive")
    if args.engine == "fixture" and not args.fixture_dir:
        parser.error("--fixture-dir is required for fixture engine")

    corpus = load_tasks(args.tasks)
    selected = [task for task in corpus["tasks"] if not args.task or task["id"] in args.task]
    unknown = sorted(set(args.task or []) - {task["id"] for task in selected})
    if unknown:
        parser.error(f"unknown task ids: {', '.join(unknown)}")
    conditions = CONDITIONS if args.condition == "both" else (args.condition,)

    prepared = []
    for task in selected:
        repo_cfg = corpus["repositories"][task["repository"]]
        repo = resolve_repo(repo_cfg)
        actual = git_revision(repo)
        expected = repo_cfg.get("revision")
        if not repo.is_dir() or actual is None:
            raise SystemExit(f"{task['id']}: repository unavailable at {repo}")
        if expected and actual != expected and not args.allow_revision_mismatch:
            raise SystemExit(f"{task['id']}: revision mismatch at {repo}: expected {expected}, got {actual}")
        prepared.append((task, repo, actual))

    if args.dry_run:
        print(json.dumps({
            "valid": True,
            "tasks": len(prepared),
            "conditions": list(conditions),
            "planned_runs": len(prepared) * len(conditions) * args.runs,
            "repositories": sorted({str(repo) for _, repo, _ in prepared}),
        }, indent=2))
        return

    records = []
    for task, repo, revision in prepared:
        for run in range(1, args.runs + 1):
            for condition in conditions:
                print(f"[agent-bench] {task['id']} {condition} run {run}", file=sys.stderr)
                try:
                    outcome = (
                        run_codex(repo, task, condition, args)
                        if args.engine == "codex"
                        else run_fixture(task, condition, run, args)
                    )
                except (OSError, subprocess.TimeoutExpired, json.JSONDecodeError) as error:
                    outcome = {"error": str(error), "wall_ms": args.timeout * 1000}
                record = {
                    "task_id": task["id"],
                    "category": task["category"],
                    "repository": task["repository"],
                    "revision": revision,
                    "condition": condition,
                    "run": run,
                    **outcome,
                }
                if "response" in record:
                    record["score"] = score_response(task, record["response"])
                    record["method_compliant"] = method_compliance(
                        condition,
                        record.get("trajectory", []),
                        task["category"] in ADEN_EXPECTED_CATEGORIES,
                    )
                records.append(record)

    report = {
        "schema_version": 1,
        "corpus": str(args.tasks),
        "engine": args.engine,
        "runs_per_condition": args.runs,
        "records": records,
        "summary": aggregate(records),
    }
    rendered = render_markdown(report)
    if args.json:
        args.json.parent.mkdir(parents=True, exist_ok=True)
        args.json.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    if args.md:
        args.md.parent.mkdir(parents=True, exist_ok=True)
        args.md.write_text(rendered, encoding="utf-8")
    if not args.json:
        print(json.dumps(report, indent=2))
    print(rendered, file=sys.stderr if not args.md else sys.stdout)


if __name__ == "__main__":
    main()
