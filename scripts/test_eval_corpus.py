#!/usr/bin/env python3
"""Regression tests for bounded product-path corpus evaluation."""

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("eval_corpus", ROOT / "scripts/eval_corpus.py")
assert SPEC and SPEC.loader
evaluation = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = evaluation
SPEC.loader.exec_module(evaluation)


class EvalCorpusTests(unittest.TestCase):
    def test_routing_explain_uses_compact_budget(self) -> None:
        completed = mock.Mock(
            returncode=0,
            stdout="//   Primary  : anchor (source: src/lib.rs)\n//   Anchor  : [[anchor]]\n",
            stderr="",
        )
        with mock.patch.object(evaluation, "run_bounded", return_value=completed) as run:
            final, primary, source = evaluation.run_ask("aden", "/repo", "question", 5)
        command = run.call_args.args[0]
        self.assertIn("--explain", command)
        self.assertEqual(command[command.index("--budget") + 1], "512")
        self.assertEqual((final, primary, source), ("anchor", "anchor", "src/lib.rs"))


if __name__ == "__main__":
    unittest.main()
