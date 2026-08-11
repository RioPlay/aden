#!/usr/bin/env python3
"""Regression tests for the paired agent benchmark harness."""

from __future__ import annotations

import importlib.util
import json
import shlex
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
    "answer": f"{os.environ['ADEN_BENCH_PROVIDER']}:{os.environ['ADEN_BENCH_MODEL']}:{'Question:' in prompt}",
    "evidence": [{"path": "src/lib.rs", "line": 1, "anchor": None}],
}
Path(os.environ["ADEN_BENCH_ANSWER_FILE"]).write_text(json.dumps(answer))
trajectory = [{"tool": "command_execution", "command": "aden ask --project . question"}]
Path(os.environ["ADEN_BENCH_TRAJECTORY_FILE"]).write_text(json.dumps(trajectory))
""",
                encoding="utf-8",
            )
            args = SimpleNamespace(
                agent_command=f"{shlex.quote(sys.executable)} {shlex.quote(str(adapter))}",
                provider="example-provider",
                model="example-model",
                timeout=10,
            )
            outcome = bench.run_command(
                root,
                {"question": "Where is the entry point?", "category": "architecture"},
                "aden",
                args,
            )
        self.assertNotIn("error", outcome)
        self.assertEqual(outcome["response"]["answer"], "example-provider:example-model:True")
        self.assertTrue(bench.method_compliance("aden", outcome["trajectory"]))
        self.assertEqual(outcome["trajectory"][0]["tool"], "command_execution")

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
