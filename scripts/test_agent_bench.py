#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Regression tests for the paired agent benchmark harness."""

from __future__ import annotations

import importlib.util
import json
import shlex
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("agent_bench", ROOT / "scripts/agent_bench.py")
assert SPEC and SPEC.loader
bench = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bench)


class AgentBenchTests(unittest.TestCase):
    @staticmethod
    def events_for(identity: str, **completion) -> list[dict]:
        item = {"id": identity, "type": "mcp_tool_call", "server": "aden", "tool": "asm",
                "arguments": {"from": "target"}}
        return [{"type": "item.started", "item": item},
                {"type": "item.completed", "item": {**item, "status": "completed", **completion}}]

    def test_telemetry_replays_empty_nonempty_duplicates_and_actual_retries(self) -> None:
        events = self.events_for("1", result={"isError": True, "content": [{"type": "text", "text": "error"}]})
        events += self.events_for("2", result={"isError": False, "content": [{"type": "text", "text": ""}]})
        events += self.events_for("3", result={"isError": False, "content": [{"type": "text", "text": "é"}]})
        events.append(events[-1])
        report = bench.telemetry_from_events(events, complete=True, source="stdio_observer")
        self.assertTrue(report["observed"])
        self.assertTrue(report["complete"])
        self.assertEqual(report["counts"]["tool_calls"], 3)
        self.assertEqual(report["counts"]["inferred_retries"], 1)
        self.assertEqual(report["counts"]["duplicate_events"], 1)
        self.assertEqual(report["counts"]["empty_responses"], 1)
        self.assertEqual(report["counts"]["nonempty_responses"], 2)
        self.assertEqual(report["calls"][-1]["output_bytes"], 2)
        self.assertFalse(report["calls"][-1]["inferred_retry"])

    def test_telemetry_separates_errors_from_valid_narrowing(self) -> None:
        events = []
        for kind in ("shell", "transport", "protocol", "tool"):
            events += self.events_for(kind, error={"kind": kind, "message": "observed failure"})
        events += self.events_for("narrow", result={"isError": False, "content": [
            {"type": "text", "text": '{"result_state":"needs_narrowing"}'}]})
        report = bench.telemetry_from_events(events, complete=True, source="stdio_observer")
        for label in ("shell_errors", "transport_errors", "protocol_errors", "tool_errors", "needs_narrowing"):
            self.assertEqual(report["counts"][label], 1)
        self.assertEqual(report["counts"]["successful_calls"], 0)
        self.assertIsNone(report["counts"]["empty_responses"])

    def test_telemetry_rejects_conflicting_identity_and_counts_structured_output(self) -> None:
        structured = self.events_for("structured", result={"isError": False,
                    "content": [{"type": "text", "text": ""}], "structuredContent": {"answer": "present"}})
        report = bench.telemetry_from_events(structured, complete=True)
        self.assertEqual(report["counts"]["nonempty_responses"], 1)
        self.assertEqual(report["counts"]["empty_responses"], 0)
        conflicting = self.events_for("conflict", result={"isError": False, "content": []})
        conflicting[-1]["item"]["tool"] = "grep"
        self.assertIsNone(bench.telemetry_from_events(conflicting, complete=True)["counts"]["tool_calls"])
        one = self.events_for("same-id", result={"isError": False, "content": [{"type": "text", "text": "a"}]})
        other = self.events_for("same-id", result={"isError": False, "content": [{"type": "text", "text": "b"}]})[-1]
        contradictory = bench.telemetry_from_events(one + [other], complete=True)
        self.assertFalse(contradictory["complete"])
        self.assertEqual(contradictory["counts"]["duplicate_events"], 0)
        failed = self.events_for("failed", status="failed", result={"isError": False, "content": []})
        self.assertIsNone(bench.telemetry_from_events(failed, complete=True)["counts"]["successful_calls"])

    def test_missing_and_unknown_telemetry_stays_unknown(self) -> None:
        for events in ([], self.events_for("pending")[:1], self.events_for("missing-start")[1:]):
            report = bench.telemetry_from_events(events, source="stdio_observer")
            self.assertFalse(report["complete"])
            self.assertIsNone(report["counts"]["tool_calls"])
            self.assertIsNone(report["counts"]["shell_errors"])
        unknown = bench.telemetry_from_events(self.events_for("unknown"), complete=True, source="stdio_observer")
        self.assertEqual(unknown["counts"]["tool_calls"], 1)
        self.assertIsNone(unknown["counts"]["successful_calls"])
        self.assertIsNone(unknown["counts"]["nonempty_responses"])
        missing_id = self.events_for("id")
        for event in missing_id:
            del event["item"]["id"]
        self.assertIsNone(bench.telemetry_from_events(missing_id, complete=True)["counts"]["tool_calls"])
        future = [{"type": "item.completed", "item": {"id": "future", "type": "future_tool"}}]
        self.assertIsNone(bench.telemetry_from_events(future, complete=True)["counts"]["tool_calls"])
        no_flag = self.events_for("no-flag", result={"content": [{"type": "text", "text": "result"}]})
        conservative = bench.telemetry_from_events(no_flag, complete=True)
        self.assertIsNone(conservative["counts"]["successful_calls"])
        self.assertEqual(conservative["counts"]["nonempty_responses"], 1)

    def test_shell_exit_code_and_self_reported_provenance(self) -> None:
        item = {"id": "shell", "type": "command_execution", "command": "native-test"}
        events = [{"type": "item.started", "item": item}, {"type": "item.completed", "item": {
            **item, "exit_code": 2, "aggregated_output": "failure", "status": "completed"}}]
        report = bench.telemetry_from_events(events, complete=True, source="adapter_self_reported")
        self.assertFalse(report["observed"])
        self.assertEqual(report["counts"]["shell_errors"], 1)
        self.assertEqual(report["calls"][0]["source"], "adapter_self_reported")

    def test_aggregate_excludes_missing_and_self_reported_counts(self) -> None:
        def row(telemetry: dict) -> dict:
            return {"condition": "aden", "wall_ms": 1, "method_compliant": True,
                    "score": {"grounded_complete": True, "fact_recall": 1, "evidence_recall": 1},
                    "telemetry": telemetry}
        observed = bench.telemetry_from_events([], complete=True, source="stdio_observer")
        unknown = bench.telemetry_from_events([], source="stdio_observer")
        self_reported = bench.telemetry_from_events([], complete=True, source="adapter_self_reported")
        self.assertIsNone(bench.aggregate([row(unknown), row(self_reported)])["aden"]["mean_tool_calls"])
        summary = bench.aggregate([row(observed), row(unknown), row(self_reported)])["aden"]
        self.assertEqual(summary["mean_tool_calls"], 0)
        self.assertEqual(summary["observed_tool_call_runs"], 1)

    def test_mcp_transport_is_neutral_and_isolated(self) -> None:
        prompt = bench.prompt_for({"question": "Explain target"}, "aden", bench.NEUTRAL_PROTOCOL, transport="mcp")
        self.assertIn("configured isolated Aden MCP tools", prompt)
        self.assertNotIn("Do not call Aden MCP", prompt)
        self.assertNotIn("--project", prompt)
        for forbidden in ("ask --", "grep", "first returned", "command first"):
            self.assertNotIn(forbidden, prompt)
        with self.assertRaises(ValueError):
            bench.prompt_for({"question": "question"}, "aden", transport="mcp")
        with self.assertRaisesRegex(ValueError, "different benchmark transports"):
            bench.aggregate([{"transport": "cli"}, {"transport": "mcp"}])
        result = subprocess.run([sys.executable, str(ROOT / "scripts/agent_bench.py"), "--dry-run",
                                 "--protocol", bench.NEUTRAL_PROTOCOL, "--transport", "mcp"],
                                capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("command or fixture", result.stderr)

    def test_neutral_protocol_does_not_prescribe_a_first_tool_or_candidate(self) -> None:
        prompt = bench.prompt_for({"question": "Explain run", "aden_from": "run"}, "aden", bench.NEUTRAL_PROTOCOL)
        self.assertIn("Direct navigation to known symbols is permitted", prompt)
        self.assertIn("exact source reads", prompt)
        self.assertIn("preserve the unresolved alternatives", prompt)
        self.assertIn("source-inspected facts from graph-derived inference", prompt)
        self.assertIn("Supplied target (may require disambiguation): run", prompt)
        for forbidden in ("ask --", "grep", "first returned", "command first", "Synthesize\nimmediately"):
            self.assertNotIn(forbidden, prompt)
        with self.assertRaisesRegex(ValueError, "unknown protocol"):
            bench.prompt_for({"question": "test"}, "aden", "unversioned")

    def test_protocols_cannot_be_pooled(self) -> None:
        with self.assertRaisesRegex(ValueError, "different benchmark protocols"):
            bench.aggregate([{"protocol": protocol} for protocol in bench.PROTOCOLS])

    def test_neutral_dry_run_reports_explicit_protocol_and_settings(self) -> None:
        result = subprocess.run([
            sys.executable, str(ROOT / "scripts/agent_bench.py"), "--dry-run",
            "--protocol", bench.NEUTRAL_PROTOCOL, "--model", "test-model",
            "--model-setting", 'model_reasoning_effort="low"',
        ], capture_output=True, text=True, check=True)
        planned = json.loads(result.stdout)
        self.assertEqual(planned["protocol"], bench.NEUTRAL_PROTOCOL)
        self.assertEqual(planned["model_settings"], ['model_reasoning_effort="low"'])
        self.assertEqual(planned["planned_runs"], 28)

    def test_fixture_report_retains_reproducibility_identity(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            repo = root / "repo"
            repo.mkdir()
            def git(*command: str) -> None:
                subprocess.run(["git", "-C", str(repo), *command], capture_output=True, check=True)
            git("init")
            source = repo / "source.txt"
            source.write_text("before\n", encoding="utf-8")
            git("add", "source.txt")
            git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-m", "fixture")
            source.write_text("after\n", encoding="utf-8")
            (repo / "untracked.txt").write_text("new input", encoding="utf-8")
            tasks_path = root / "tasks.json"
            tasks_path.write_text(json.dumps({
                "schema_version": 1,
                "repositories": {"fixture": {"default_path": str(repo)}},
                "tasks": [{"id": "fixture", "repository": "fixture", "category": "lookup",
                           "question": "What changed?", "required_facts": [{"id": "fact", "any_of": ["after"]}]}],
            }), encoding="utf-8")
            (root / "fixture.aden.1.json").write_text(json.dumps({"answer": "after", "evidence": []}), encoding="utf-8")
            report_path = root / "report.json"
            subprocess.run([
                sys.executable, str(ROOT / "scripts/agent_bench.py"), "--tasks", str(tasks_path),
                "--engine", "fixture", "--fixture-dir", str(root), "--condition", "aden",
                "--protocol", bench.NEUTRAL_PROTOCOL, "--json", str(report_path), "--model", "fixture-model",
            ], capture_output=True, text=True, check=True)
            report = json.loads(report_path.read_text(encoding="utf-8"))
            self.assertEqual(report["schema_version"], bench.REPORT_SCHEMA_VERSION)
            self.assertEqual(report["protocol"], bench.NEUTRAL_PROTOCOL)
            self.assertEqual(report["model_settings"]["requested_model"], "fixture-model")
            self.assertIsNone(report["model_settings"]["effective_settings"])
            self.assertEqual(report["answer_schema"]["definition"], bench.ANSWER_SCHEMA)
            self.assertIn("aden", report["binaries"])
            record = report["records"][0]
            self.assertEqual(record["prompt"]["sha256"], bench.sha256_bytes(record["prompt"]["text"].encode()))
            state = report["repositories"][record["repository_identity"]]
            self.assertTrue(state["dirty"])
            self.assertEqual(state["revision"], record["revision"])
            self.assertIn("+after", state["dirty_diff"])
            self.assertEqual(len(state["untracked_files"]), 1)
            self.assertEqual(state["untracked_files"][0]["sha256"], bench.sha256_bytes(b"new input"))
            self.assertIn(bench.NEUTRAL_PROTOCOL, bench.render_markdown(report))

    def test_committed_corpus_has_fourteen_valid_tasks(self) -> None:
        corpus = bench.load_tasks(bench.DEFAULT_TASKS)
        self.assertEqual(len(corpus["tasks"]), 14)
        self.assertEqual(len({task["id"] for task in corpus["tasks"]}), 14)
        self.assertGreaterEqual(len({task["repository"] for task in corpus["tasks"]}), 5)

        typo_task = next(
            task for task in corpus["tasks"] if task["id"] == "aden-typo-symbol-recovery"
        )
        self.assertEqual(typo_task["aden_from"], "resovle_anchor_detailed")
        self.assertTrue(typo_task["forbidden_claims"])
        prompt = bench.prompt_for(typo_task, "aden")
        self.assertIn("--from resovle_anchor_detailed", prompt)
        self.assertIn("retry the same command exactly once", prompt)
        self.assertIn("canonical anchor substituted", prompt)

    def test_score_requires_facts_evidence_and_no_forbidden_claim(self) -> None:
        task = {
            "required_facts": [
                {"id": "one", "any_of": ["alpha"]},
                {"id": "two", "any_of": ["beta|bravo"]},
            ],
            "expected_evidence": [{"id": "source", "any_of": ["src/main\\.rs"]}],
            "forbidden_claims": ["unsafe claim"],
        }
        complete = bench.score_response(task, {
            "answer": "Alpha and bravo are both present.",
            "evidence": [{"path": "src/main.rs", "line": 3, "anchor": None}],
        })
        self.assertTrue(complete["grounded_complete"])

        forbidden = bench.score_response(task, {
            "answer": "Alpha, beta, and an unsafe claim.",
            "evidence": [{"path": "src/main.rs", "line": 3, "anchor": None}],
        })
        self.assertFalse(forbidden["grounded_complete"])
        self.assertEqual(forbidden["forbidden_claims"], ["unsafe claim"])

    def test_invalid_regex_is_rejected_at_load(self) -> None:
        corpus = {
            "schema_version": 1,
            "repositories": {"repo": {"default_path": "."}},
            "tasks": [{
                "id": "bad",
                "repository": "repo",
                "category": "lookup",
                "question": "question",
                "required_facts": [{"id": "fact", "any_of": ["["]}],
            }],
        }
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "tasks.json"
            path.write_text(json.dumps(corpus), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "invalid regex"):
                bench.load_tasks(path)

    def test_markdown_reports_both_conditions(self) -> None:
        records = [
            {"condition": condition, "wall_ms": 10, "trajectory": [], "score": {
                "grounded_complete": condition == "aden",
                "fact_recall": 1.0 if condition == "aden" else 0.5,
                "evidence_recall": 1.0,
            }, "method_compliant": True}
            for condition in bench.CONDITIONS
        ]
        report = {
            "corpus": "tasks.json",
            "runs_per_condition": 1,
            "engine": "fixture",
            "summary": bench.aggregate(records),
        }
        rendered = bench.render_markdown(report)
        self.assertIn("| baseline |", rendered)
        self.assertIn("| aden |", rendered)
        self.assertIn("100.0%", rendered)

    def test_method_compliance_enforces_condition_boundary(self) -> None:
        conventional = [{"tool": "command_execution", "command": "rg -n symbol src"}]
        graph = [{"tool": "command_execution", "command": "aden locate --symbol symbol"}]
        quoted_graph = [
            {"tool": "command_execution", "command": "/bin/bash -lc 'aden search query'"}
        ]
        self.assertTrue(bench.method_compliance("baseline", conventional))
        self.assertFalse(bench.method_compliance("baseline", graph))
        self.assertTrue(bench.method_compliance("aden", graph))
        self.assertTrue(bench.method_compliance("aden", quoted_graph))
        self.assertFalse(bench.method_compliance("aden", conventional))
        self.assertFalse(bench.method_compliance("aden", []))
        self.assertTrue(bench.method_compliance("aden", conventional, aden_expected=False))

    def test_external_command_engine_uses_provider_neutral_contract(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            adapter = root / "adapter.py"
            adapter.write_text(
                """import json, os
from pathlib import Path
prompt = Path(os.environ["ADEN_BENCH_PROMPT_FILE"]).read_text()
answer = {
    "answer": f"{os.environ['ADEN_BENCH_PROVIDER']}:{os.environ['ADEN_BENCH_MODEL']}:{'Question:' in prompt}:{os.environ['ADEN_BENCH_PROTOCOL']}:{os.environ['ADEN_BENCH_MODEL_SETTINGS']}",
    "evidence": [{"path": "src/lib.rs", "line": 1, "anchor": None}],
}
Path(os.environ["ADEN_BENCH_ANSWER_FILE"]).write_text(json.dumps(answer))
trajectory = [{"tool": "command_execution", "command": "aden ask --project . question"}]
Path(os.environ["ADEN_BENCH_TRAJECTORY_FILE"]).write_text(json.dumps(trajectory))
item = {"id": "adapter-call", "type": "command_execution", "command": "aden ask --project . question"}
events = [{"type": "item.started", "item": item}, {"type": "item.completed", "item": {**item, "exit_code": 0, "aggregated_output": "answer"}}]
Path(os.environ["ADEN_BENCH_EVENTS_FILE"]).write_text(json.dumps({"events": events, "complete": True}))
""",
                encoding="utf-8",
            )
            args = SimpleNamespace(
                agent_command=f"{shlex.quote(sys.executable)} {shlex.quote(str(adapter))}",
                provider="example-provider",
                model="example-model",
                timeout=10,
                protocol=bench.NEUTRAL_PROTOCOL,
                model_setting=['model_reasoning_effort="low"'],
            )
            outcome = bench.run_command(
                root,
                {"question": "Where is the entry point?", "category": "architecture"},
                "aden",
                args,
            )
        self.assertNotIn("error", outcome)
        self.assertEqual(outcome["response"]["answer"],
                         f"example-provider:example-model:True:{bench.NEUTRAL_PROTOCOL}:" + json.dumps(args.model_setting))
        self.assertTrue(bench.method_compliance("aden", outcome["trajectory"]))
        self.assertEqual(outcome["trajectory"][0]["tool"], "command_execution")
        self.assertEqual(outcome["trajectory_source"], "adapter_self_reported")
        self.assertFalse(outcome["telemetry"]["observed"])
        self.assertEqual(outcome["telemetry"]["counts"]["tool_calls"], 1)
        self.assertEqual(len(outcome["raw_events"]), 2)

    def test_deterministic_prompt_pins_route_and_budget(self) -> None:
        normal_prompt = bench.prompt_for(
            {"question": "Who calls it?", "category": "dependency_trace"}, "aden"
        )
        risk_prompt = bench.prompt_for(
            {"question": "How are transaction conflicts handled?", "category": "dependency_trace"},
            "aden",
        )
        self.assertIn("ask --strict --budget 512", normal_prompt)
        self.assertIn("ask --strict --budget 1024", risk_prompt)
        self.assertIn("Do not choose a routing strategy", normal_prompt)
        self.assertIn("Do not run `rg`, `grep`, `find`, `sed`", normal_prompt)
        self.assertIn("retry the same command exactly once", normal_prompt)
        self.assertIn("--project .", normal_prompt)
        self.assertIn("Do not call an Aden MCP tool", normal_prompt)
        from_prompt = bench.prompt_for(
            {
                "question": "Who calls it?",
                "category": "dependency_trace",
                "aden_from": "resolve_anchor_detailed",
            },
            "aden",
        )
        self.assertIn("--from resolve_anchor_detailed", from_prompt)


if __name__ == "__main__":
    unittest.main()
