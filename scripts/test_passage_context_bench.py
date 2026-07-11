import importlib.util
import pathlib
import unittest
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "passage_context_bench", ROOT / "scripts/passage_context_bench.py"
)
assert SPEC and SPEC.loader
bench = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bench)


class PassageContextBenchTests(unittest.TestCase):
    def test_summary_reports_quality_cost_and_latency_separately(self):
        rows = [
            {"sufficient": True, "fact_recall": 1.0, "tokens": 500, "elapsed_ms": 20},
            {"sufficient": False, "fact_recall": 0.5, "tokens": 300, "elapsed_ms": 40},
        ]
        summary = bench.summarize(rows)
        self.assertEqual(summary["sufficient"], 1)
        self.assertEqual(summary["mean_fact_recall"], 0.75)
        self.assertEqual(summary["median_tokens"], 400)
        self.assertEqual(summary["median_elapsed_ms"], 30)

    def test_search_rank_stages_correct_file_without_dropping_rank_one(self):
        payload = {
            "results": [
                {"anchor": "unresolved", "snippet": ""},
                {"anchor": "target", "snippet": ":source_file: docs/target.adoc"},
            ]
        }
        completed = mock.Mock(returncode=0, stdout=__import__("json").dumps(payload))
        with mock.patch.object(bench.subprocess, "run", return_value=completed):
            rank = bench.search_rank(
                "aden",
                {"repo": "/repo", "query": "q", "expected_source": "target.adoc"},
                1,
            )
        self.assertEqual(rank, 2)

    def test_search_rank_accepts_multiple_grounded_sources(self):
        payload = {
            "results": [
                {"anchor": "dense", "snippet": ":source_file: docs/dense.adoc"},
            ]
        }
        completed = mock.Mock(returncode=0, stdout=__import__("json").dumps(payload))
        with mock.patch.object(bench.subprocess, "run", return_value=completed):
            rank = bench.search_rank(
                "aden",
                {
                    "repo": "/repo",
                    "query": "q",
                    "expected_source": "rag.adoc|dense.adoc",
                },
                1,
            )
        self.assertEqual(rank, 1)


if __name__ == "__main__":
    unittest.main()
