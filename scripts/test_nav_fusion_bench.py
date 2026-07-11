import importlib.util
import pathlib
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "nav_fusion_bench", ROOT / "scripts" / "nav_fusion_bench.py"
)
assert SPEC and SPEC.loader
nav = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(nav)


class NavFusionBenchTests(unittest.TestCase):
    def test_canonical_path_removes_anchor_suffix(self):
        self.assertEqual(
            nav.canonical_path("book/guide.adoc/h3topic"), "book/guide.adoc"
        )

    def test_prose_gate_uses_candidate_schemes(self):
        prose = [{"anchor": f"aden://doc/repo/{i}"} for i in range(8)]
        code = [{"anchor": f"aden://module/repo/{i}"} for i in range(3)]
        self.assertTrue(nav.predominantly_prose(prose + code))
        self.assertFalse(nav.predominantly_prose(code + prose[:7]))

    def test_fusion_rewards_cross_method_agreement(self):
        ranked = nav.reciprocal_rank_fusion(
            ["native.rs", "shared.rs"], ["shared.rs", "other.rs"], 1.5
        )
        self.assertEqual(ranked[0], "shared.rs")

    def test_unresolved_rank_one_is_not_silently_skipped(self):
        self.assertFalse(nav.top_hit(["", "target.adoc"], "target.adoc"))

    def test_multiple_acceptable_targets_are_scored(self):
        self.assertTrue(nav.top_hit(["second.adoc"], "first.adoc|second.adoc"))
        results = [
            {"snippet": ":source_file: unrelated.adoc", "anchor": "u"},
            {"snippet": ":source_file: second.adoc", "anchor": "s"},
        ]
        self.assertEqual(
            nav.eval_corpus.rank_of(results, "first.adoc|second.adoc"), 2
        )
        self.assertEqual(
            nav.conventional.rank_of(
                ["unrelated.adoc", "second.adoc"], "first.adoc|second.adoc"
            ),
            2,
        )

    def test_native_consensus_counts_only_top_file(self):
        results = [
            {"anchor": "a", "snippet": ":source_file: guide.adoc"},
            {"anchor": "b", "snippet": ":source_file: other.adoc"},
            {"anchor": "c", "snippet": ":source_file: guide.adoc"},
            {"anchor": "d", "snippet": ":source_file: guide.adoc"},
            {"anchor": "e", "snippet": ":source_file: third.adoc"},
            {"anchor": "f", "snippet": ":source_file: guide.adoc"},
        ]
        self.assertEqual(nav.native_top_file_consensus(results), 3)

    def test_chronological_gate_recognizes_date_named_results(self):
        results = [
            {"anchor": "a", "snippet": f":source_file: log/2026-06-{day:02}.adoc"}
            for day in range(1, 9)
        ] + [
            {"anchor": "b", "snippet": ":source_file: roadmap.adoc"},
            {"anchor": "c", "snippet": ":source_file: README.adoc"},
        ]
        self.assertTrue(nav.predominantly_chronological(results))


if __name__ == "__main__":
    unittest.main()
