# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later

import copy
import os
from pathlib import Path
import sys
import unittest
from unittest.mock import Mock, patch

import cold_gen_bench as bench


class ColdGenerationBenchTests(unittest.TestCase):
    def test_failures_stay_in_denominator_and_all_started_elapsed(self):
        rows = [dict(fixture="repo", mode="manual", arm="a", success=True, ready_ms=10,
                     peak_cli_working_set_bytes=100, attempt_ms=12),
                dict(fixture="repo", mode="manual", arm="a", success=False, attempt_ms=90)]
        result = bench.summarize(rows)[0]
        self.assertEqual((result["started"], result["successful"], result["failures"]), (2, 1, 1))
        self.assertEqual(result["ready_median_ms"], 10)
        self.assertEqual(result["all_started_median_ms"], 51)
        rows[0].update(success=False)
        self.assertIsNone(bench.summarize(rows)[0]["ready_median_ms"])

    def test_readiness_requires_current_seed_and_correct_typed_edges(self):
        value = dict(truncated=False, context_receipt=dict(freshness="current", observed_source_fingerprint="abc"),
                     items=[dict(anchor="aden://module/a#seed", depth=0),
                            dict(anchor="aden://module/a#leaf", depth=1, via_edge_types=["Calls"])])
        self.assertEqual(len(bench.query_evidence(value, "seed")["anchors"]), 2)
        for broken in (dict(value, truncated=True), dict(value, items=[]),
                       dict(value, context_receipt=dict(freshness="lagging"))):
            with self.assertRaises(ValueError):
                bench.query_evidence(broken, "seed")
        changed = copy.deepcopy(value)
        changed["items"][1]["via_edge_types"] = ["Contains"]
        with self.assertRaises(ValueError):
            bench.query_evidence(changed, "seed")

    def test_spawn_failure_is_a_recorded_attempt(self):
        with patch.object(bench.subprocess, "Popen", side_effect=FileNotFoundError):
            row = bench.execute(Path("missing"), [], Path("."), {}, 1)
        self.assertFalse(row["success"])
        self.assertEqual(row["error"], "FileNotFoundError")

    def test_timeout_cleanup_failures_remain_failed_rows(self):
        process = Mock(returncode=None, pid=123)
        process.poll.return_value = None
        process.communicate.side_effect = [bench.subprocess.TimeoutExpired("cli", 1), bench.subprocess.TimeoutExpired("cli", 5)]
        with patch.object(bench.subprocess, "Popen", return_value=process), \
             patch.object(bench.subprocess, "run", side_effect=bench.subprocess.TimeoutExpired("taskkill", 5)), \
             patch.object(bench, "peak_working_set", return_value=None):
            row = bench.execute(Path("cli"), [], Path("."), {}, 1)
        self.assertFalse(row["success"])
        self.assertEqual(row["error"], "timeout")
        self.assertTrue(row["cleanup_errors"])
        self.assertEqual(row["cleanup_incomplete_pid"], 123)

    @unittest.skipUnless(os.name == "nt", "Windows native peak working set")
    def test_os_peak_retains_memory_of_exited_process(self):
        row = bench.execute(Path(sys.executable), ["-c", "allocation=bytearray(16*1024*1024)"],
                            Path("."), os.environ.copy(), 10)
        self.assertTrue(row["success"])
        self.assertGreater(row["peak_cli_working_set_bytes"], 16*1024*1024)


if __name__ == "__main__":
    unittest.main()
