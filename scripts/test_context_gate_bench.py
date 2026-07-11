#!/usr/bin/env python3
"""Tests for deterministic context-tier calibration."""

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "context_gate_bench", ROOT / "scripts/context_gate_bench.py"
)
assert SPEC and SPEC.loader
gate = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = gate
SPEC.loader.exec_module(gate)


class ContextGateTests(unittest.TestCase):
    def test_contract_lookup_stays_direct(self) -> None:
        plan = gate.context_plan("When should I run aden gen?", "contract_lookup")
        self.assertEqual((plan.route, plan.tier, plan.budget, plan.ceiling), ("text", "direct", 512, 512))

    def test_dependency_trace_uses_connected_context(self) -> None:
        plan = gate.context_plan("Which path calls run_command?", "dependency_trace")
        self.assertEqual((plan.route, plan.tier, plan.budget, plan.ceiling), ("aden", "connected", 512, 2048))
        self.assertIn("relationship", plan.required_roles)

    def test_risk_signal_raises_compact_task_to_connected(self) -> None:
        plan = gate.context_plan("Explain transaction locking", "symbol_explanation")
        self.assertEqual((plan.tier, plan.budget, plan.ceiling), ("connected", 512, 2048))
        self.assertIn("risk_floor", plan.reasons)

    def test_deep_intent_uses_deep_tier(self) -> None:
        plan = gate.context_plan("Debug the root cause of this architecture failure", "architecture")
        self.assertEqual((plan.tier, plan.budget, plan.ceiling), ("deep", 512, 4096))

    def test_context_scoring_requires_all_facts_and_evidence(self) -> None:
        task = {
            "required_facts": [{"id": "fact", "any_of": ["alpha"]}],
            "expected_evidence": [{"id": "source", "any_of": ["src/main\\.rs"]}],
        }
        self.assertTrue(gate.score_context(task, "alpha from src/main.rs")["sufficient"])
        self.assertFalse(gate.score_context(task, "alpha only")["sufficient"])

    def test_context_scoring_preserves_identifier_underscores(self) -> None:
        task = {
            "required_facts": [{"id": "caller", "any_of": ["call_tool"]}],
            "expected_evidence": [],
        }
        self.assertTrue(gate.score_context(task, "AdenMcpServer::call_tool")["sufficient"])

    def test_explicit_symbol_prefers_code_identifier(self) -> None:
        self.assertEqual(gate.explicit_symbol("What boundaries does run_aden_command enforce?"), "run_aden_command")

    def test_query_terms_drop_generic_question_words(self) -> None:
        self.assertEqual(gate.query_terms("What color pipeline is required?"), {"color", "pipeline"})

    def test_anchor_role_distinguishes_web_layers(self) -> None:
        self.assertNotEqual(
            gate.anchor_role("aden://module/web/src/browser/config.ts#load"),
            gate.anchor_role("aden://module/web/src/components/preview/view.ts#View"),
        )

    def test_intent_queries_decompose_conjoined_facets(self) -> None:
        queries = gate.intent_queries(
            "What guards preview sessions, webview configuration, and queue availability?"
        )
        term_sets = [set(query.split()) for query in queries]
        self.assertTrue(any({"bridge", "config", "configuration", "webview"} <= terms for terms in term_sets))
        self.assertTrue(any({"availability", "available", "closed", "queue"} <= terms for terms in term_sets))

    def test_routing_policy_uses_relationship_traversal(self) -> None:
        task = {"category": "dependency_trace", "question": "Which caller reaches run_command?"}
        self.assertEqual(gate.routing_policy(task), "adaptive")

    def test_routing_policy_uses_graph_for_unanchored_relationship(self) -> None:
        task = {"category": "dependency_trace", "question": "How are transactions committed?"}
        self.assertEqual(gate.routing_policy(task), "ask")

    def test_routing_policy_anchors_explicit_symbols(self) -> None:
        task = {"category": "symbol_explanation", "question": "What does run_command enforce?"}
        self.assertEqual(gate.routing_policy(task), "adaptive")

    def test_dependency_trace_ignores_test_mentions_before_real_callers(self) -> None:
        payload = {
            "matches": [
                {"anchor": "aden://module/app/src/query.rs#tests", "file": "src/query.rs"},
                {"anchor": "aden://module/app/src/lib.rs#Server::call", "file": "src/lib.rs"},
                {"anchor": "aden://module/app/src#run_command", "file": "src/lib.rs"},
            ]
        }
        task = {"category": "dependency_trace", "question": "Who calls run_command?"}
        with mock.patch.object(gate, "run_json", return_value=payload):
            self.assertEqual(
                gate.adaptive_anchors("aden", Path("repo"), task, 5),
                ["aden://module/app/src/lib.rs#Server::call"],
            )


if __name__ == "__main__":
    unittest.main()
