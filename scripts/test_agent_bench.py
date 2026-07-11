#!/usr/bin/env python3
"""Regression tests for the paired agent benchmark harness."""

from __future__ import annotations

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("agent_bench", ROOT / "scripts/agent_bench.py")
assert SPEC and SPEC.loader
bench = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bench)


class AgentBenchTests(unittest.TestCase):
    def test_committed_corpus_has_twelve_valid_tasks(self) -> None:
        corpus = bench.load_tasks(bench.DEFAULT_TASKS)
        self.assertEqual(len(corpus["tasks"]), 12)
        self.assertEqual(len({task["id"] for task in corpus["tasks"]}), 12)
        self.assertGreaterEqual(len({task["repository"] for task in corpus["tasks"]}), 5)

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
        self.assertTrue(bench.method_compliance("aden", conventional, aden_expected=False))

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
        self.assertIn("Do not run `rg`", normal_prompt)
        self.assertIn("--project .", normal_prompt)
        self.assertIn("Do not call an Aden MCP tool", normal_prompt)


if __name__ == "__main__":
    unittest.main()
