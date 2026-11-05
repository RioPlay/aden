# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later

import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location("read_path_bench", Path(__file__).with_name("read_path_bench.py"))
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)


class ReadPathBenchTests(unittest.TestCase):
    def test_execution_failures_are_recorded_instead_of_aborting_the_matrix(self):
        for failure in (FileNotFoundError(), bench.subprocess.TimeoutExpired("binary", 1)):
            with patch.object(bench.subprocess, "run", side_effect=failure):
                row = bench.run(Path("binary"), [], Path("."), {}, 1)
            self.assertIsNone(row["exit_code"])
            self.assertTrue(row["error"])
            self.assertGreaterEqual(row["elapsed_ms"], 0)

    def test_failed_attempts_remain_in_denominator_and_elapsed_accounting(self):
        rows = [
            {"arm": "a", "workload": "source", "state": "primed", "phase": "request", "elapsed_ms": 10, "success": True},
            {"arm": "a", "workload": "source", "state": "primed", "phase": "request", "elapsed_ms": 100, "success": False},
            {"arm": "a", "workload": "source", "state": "primed", "phase": "setup", "elapsed_ms": 300, "success": False},
        ]
        summary = bench.summarize(rows)[0]
        self.assertEqual((summary["started"], summary["successful"], summary["failures"]), (3, 1, 2))
        self.assertEqual(summary["all_started_elapsed_median_ms"], 100)
        self.assertEqual(summary["successful_request_median_ms"], 10)

    def test_all_failures_do_not_fabricate_success_latency(self):
        rows = [{"arm": "a", "workload": "source", "state": "unindexed", "phase": "request", "elapsed_ms": 30, "success": False}]
        self.assertIsNone(bench.summarize(rows)[0]["successful_request_median_ms"])

    def test_stale_or_unverified_evidence_is_rejected(self):
        self.assertIsNotNone(bench.validate("source", {"source": {"text": bench.BEFORE}, "observation": "file_bytes_read"}, bench.AFTER))
        self.assertIsNotNone(bench.validate("symbol", {"context_receipt": {"freshness": "current"}, "source": {"text": bench.AFTER, "hash_verified": False}}, bench.AFTER))
        self.assertIsNotNone(bench.validate("incoming", {"context_receipt": {"freshness": "snapshot"}, "items": []}, bench.BEFORE))

    def test_source_cannot_borrow_graph_freshness(self):
        value = {"source": {"text": bench.BEFORE}, "observation": "file_bytes_read", "freshness": "current"}
        self.assertIsNotNone(bench.validate("source", value, bench.BEFORE))
        value.pop("freshness")
        self.assertIsNone(bench.validate("source", value, bench.BEFORE))

    def test_query_must_prove_the_measured_seed_and_exclude_deleted_symbols(self):
        def response(*names):
            return {"context_receipt": {"freshness": "current"}, "items": [{"anchor": f"fixture#{name}"} for name in names]}
        self.assertIsNotNone(bench.validate("incoming", response("middle", "entry"), bench.AFTER))
        self.assertIsNotNone(bench.validate("incoming", response("leaf", "middle", "entry"), bench.AFTER))
        self.assertIsNotNone(bench.validate("outgoing", response("twig", "middle", "entry", "leaf"), bench.AFTER))
        self.assertIsNone(bench.validate("incoming", response("twig", "middle", "entry"), bench.AFTER))


if __name__ == "__main__":
    unittest.main()
