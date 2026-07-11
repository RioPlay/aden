#!/usr/bin/env python3
"""Tests for bounded held-out benchmark execution."""

from __future__ import annotations

import importlib.util
import subprocess
import sys
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "heldout_context_bench", ROOT / "scripts/heldout_context_bench.py"
)
assert SPEC and SPEC.loader
bench = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = bench
SPEC.loader.exec_module(bench)


class HeldoutContextBenchTests(unittest.TestCase):
    def test_timeout_is_recorded_instead_of_aborting_the_matrix(self) -> None:
        with mock.patch.object(
            bench.subprocess,
            "run",
            side_effect=subprocess.TimeoutExpired(["eval"], 7),
        ):
            payload, elapsed = bench.run_mode(
                Path("aden"), Path("repo"), Path("queries.tsv"), "ask", 7
            )
        self.assertTrue(payload["timed_out"])
        self.assertEqual(payload["timeout_seconds"], 7)
        self.assertGreaterEqual(elapsed, 0)


if __name__ == "__main__":
    unittest.main()
