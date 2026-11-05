# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later

import copy
import unittest

from repo_query_bench import validate


class RepoQueryEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.anchor = "aden://module/example.rs#entry"
        self.value = {
            "context_receipt": {"freshness": "current", "observed_source_fingerprint": "abc"},
            "truncated": False,
            "items": [
                {"anchor": self.anchor, "depth": 0},
                {"anchor": "aden://module/example.rs#leaf", "depth": 1, "via_edge_types": ["Calls"]},
            ],
        }

    def test_valid_typed_neighborhood_keeps_exact_seed(self):
        error, pairs = validate(self.value, self.anchor)
        self.assertIsNone(error)
        self.assertIn((self.anchor, 0), pairs)
        self.assertEqual(len(pairs), 2)

    def test_stale_incomplete_or_unobserved_results_cannot_count_as_success(self):
        for field, replacement in [("context_receipt", None), ("context_receipt", {"freshness": "lagging"}), ("context_receipt", {"freshness": "current"}), ("truncated", True), ("items", None)]:
            value = copy.deepcopy(self.value)
            value[field] = replacement
            self.assertIsNotNone(validate(value, self.anchor)[0])

    def test_wrong_seed_edge_depth_and_duplicates_are_rejected(self):
        variants = [
            [{"anchor": "wrong", "depth": 0}],
            [{"anchor": self.anchor, "depth": True}],
            [{"anchor": self.anchor, "depth": 0}, {"anchor": "leaf", "depth": 1, "via_edge_types": ["Contains"]}],
            [{"anchor": self.anchor, "depth": 0}, {"anchor": "leaf", "depth": 1, "via_edge_types": "Calls"}],
            [{"anchor": self.anchor, "depth": 0}, {"anchor": self.anchor, "depth": 0}],
        ]
        for items in variants:
            value = copy.deepcopy(self.value)
            value["items"] = items
            self.assertIsNotNone(validate(value, self.anchor)[0])


if __name__ == "__main__":
    unittest.main()
