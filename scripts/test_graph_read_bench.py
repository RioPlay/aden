# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later

import copy
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import graph_read_bench as bench


class GraphReadBenchTests(unittest.TestCase):
    def setUp(self):
        self.receipt = {"freshness": "current", "observed_source_fingerprint": "abc"}
        self.query = {"context_receipt": self.receipt, "truncated": False, "items": [
            {"anchor": bench.ENTRY, "depth": 0},
            {"anchor": bench.PREFIX + "bench_middle", "depth": 1, "via_edge_types": ["Calls"]},
            {"anchor": bench.PREFIX + "bench_leaf", "depth": 2, "via_edge_types": ["Calls"]},
        ]}

    def understand_value(self, edited=False, middle=False):
        leaf = "bench_twig" if edited else "bench_leaf"
        symbol, line = ("bench_middle", 2) if middle else (leaf, 1)
        source = (bench.AFTER if edited else bench.BEFORE).splitlines(keepends=True)[line - 1]
        context = f"{symbol}\n\n{symbol}() -> i32"
        if middle:
            context += f"\ncalls: {leaf}(2)\n\n---\n\n{leaf}\n\n{leaf}() -> i32"
        return {"context_receipt": self.receipt, "anchor": bench.PREFIX + symbol,
                "definition": dict(anchor=bench.PREFIX + symbol, file="chain.rs", node_type="function",
                                   start_line=str(line), end_line=str(line)),
                "source": dict(state="complete", hash_verified=True, file="chain.rs", text=source,
                               start_line=line, end_line=line, indexed_start_line=line,
                               indexed_end_line=line, last_line_complete=True),
                "relationships": {"backlinks": {"direction": "incoming", "max_depth": 1},
                                  "impact": {"direction": "outgoing", "max_depth": 2}},
                "backlinks": [
                    dict(anchor="mod-graph-read-bench", node_type="Module", depth=1, inferred=False, via_edge_types=["Contains"]),
                    dict(anchor=bench.ENTRY if middle else bench.PREFIX + "bench_middle", node_type="Function",
                         depth=1, inferred=False, via_edge_types=["Calls"])],
                "impact": [dict(anchor=bench.PREFIX + leaf, node_type="Function", depth=1)] if middle else [],
                "context": context}

    def locate_value(self, edited=False, callers=False):
        symbol = "bench_twig" if edited else "bench_leaf"
        item = (dict(anchor=bench.PREFIX + "bench_middle", file="chain.rs", start_line="2") if callers else
                dict(anchor=bench.PREFIX + symbol, file="chain.rs", node_type="function", start_line="1", end_line="1"))
        return {"context_receipt": self.receipt, "truncated": False, "total": 1, "returned": 1,
                "mode": "callers" if callers else "symbol", "match_kind": "call_edges" if callers else "definition",
                "items": [item], "resolution": dict(state="unique", complete=True, anchor=bench.PREFIX + symbol)}

    def test_exact_typed_chain_before_and_after(self):
        self.assertIsNone(bench.validate_query(self.query))
        edited = copy.deepcopy(self.query)
        edited["items"][2]["anchor"] = bench.PREFIX + "bench_twig"
        self.assertIsNone(bench.validate_query(edited, True))
        self.assertIsNotNone(bench.validate_query(self.query, True))
        reverse = copy.deepcopy(self.query)
        for item in reverse["items"]:
            item["depth"] = 2 - item["depth"]
            if item["depth"]:
                item["via_edge_types"] = ["Calls"]
        self.assertIsNone(bench.validate_query(reverse, reverse=True))
        self.assertIsNotNone(bench.validate_query(reverse))

    def test_stale_missing_and_truncated_evidence_rejected(self):
        for receipt in (None, {}, {"freshness": "lagging"}, {"freshness": "current"}):
            value = copy.deepcopy(self.query)
            value["context_receipt"] = receipt
            self.assertIsNotNone(bench.validate_query(value))
        value = copy.deepcopy(self.query)
        value["truncated"] = True
        self.assertIsNotNone(bench.validate_query(value))

    def test_plausible_wrong_paths_edges_depths_and_duplicates_rejected(self):
        changes = [("anchor", "aden://module/wrong/chain.rs#bench_leaf"),
                   ("depth", 1), ("depth", True), ("via_edge_types", ["Contains"]),
                   ("via_edge_types", ["Calls", "Contains"])]
        for field, replacement in changes:
            value = copy.deepcopy(self.query)
            value["items"][2][field] = replacement
            self.assertIsNotNone(bench.validate_query(value))
        for items in (self.query["items"][:2], self.query["items"] + [self.query["items"][0]]):
            value = copy.deepcopy(self.query)
            value["items"] = items
            self.assertIsNotNone(bench.validate_query(value))

    def test_verified_source_must_equal_expected_definition(self):
        value = self.understand_value()
        self.assertIsNone(bench.validate_understand(value))
        for field, replacement in (("text", "pub fn bench_leaf() -> i32 { 9 }\n"),
                                   ("state", "clipped"), ("hash_verified", False)):
            changed = copy.deepcopy(value)
            changed["source"][field] = replacement
            self.assertIsNotNone(bench.validate_understand(changed))

    def test_understand_checks_both_relationship_directions_and_context(self):
        for edited in (False, True):
            for middle in (False, True):
                self.assertIsNone(bench.validate_understand(self.understand_value(edited, middle), edited, middle))
        value = self.understand_value(middle=True)
        for field, replacement in (("backlinks", []), ("impact", []), ("impact", [None]),
                                   ("backlinks", "not an array"), ("context", "bench_middle() -> i32"),
                                   ("definition", {"anchor": bench.PREFIX + "bench_middle"})):
            changed = copy.deepcopy(value)
            changed[field] = replacement
            self.assertIsNotNone(bench.validate_understand(changed, middle=True))
        for array, field, replacement in (("backlinks", "depth", True), ("backlinks", "anchor", []),
                                          ("backlinks", "via_edge_types", [["Calls"]]),
                                          ("impact", "depth", True), ("impact", "anchor", bench.ENTRY)):
            changed = copy.deepcopy(value)
            changed[array][-1][field] = replacement
            self.assertIsNotNone(bench.validate_understand(changed, middle=True))
        for field in ("backlinks", "impact"):
            changed = copy.deepcopy(value)
            changed["relationships"][field]["direction"] = "outgoing" if field == "backlinks" else "incoming"
            self.assertIsNotNone(bench.validate_understand(changed, middle=True))
        changed = self.understand_value(edited=True, middle=True)
        changed["context"] = value["context"]
        self.assertIsNotNone(bench.validate_understand(changed, edited=True, middle=True))

    def test_understand_rejects_wrong_metadata_ranges_and_extra_relationships(self):
        value = self.understand_value()
        for field in ("start_line", "end_line", "indexed_start_line", "indexed_end_line"):
            changed = copy.deepcopy(value)
            changed["source"][field] = True
            self.assertIsNotNone(bench.validate_understand(changed))
        for field, replacement in (("node_type", "module"), ("file", "other.rs"), ("start_line", "2")):
            changed = copy.deepcopy(value)
            changed["definition"][field] = replacement
            self.assertIsNotNone(bench.validate_understand(changed))
        changed = copy.deepcopy(value)
        changed["backlinks"].append(changed["backlinks"][0])
        self.assertIsNotNone(bench.validate_understand(changed))
        changed = copy.deepcopy(value)
        changed["impact"] = [dict(anchor=bench.ENTRY, node_type="Function", depth=1)]
        self.assertIsNotNone(bench.validate_understand(changed))

    def test_locate_exact_definition_metadata_and_recorded_caller(self):
        for edited in (False, True):
            for callers in (False, True):
                value = self.locate_value(edited, callers)
                self.assertIsNone(bench.validate_locate(value, edited, callers))
        value = self.locate_value()
        for field, replacement in (("total", True), ("returned", 0), ("truncated", True),
                                   ("items", [None]), ("mode", "callers"), ("match_kind", "full_text"),
                                   ("resolution", {"complete": False})):
            changed = copy.deepcopy(value)
            changed[field] = replacement
            self.assertIsNotNone(bench.validate_locate(changed))
        for field, replacement in (("anchor", bench.ENTRY), ("file", "wrong.rs"), ("start_line", "1")):
            changed = self.locate_value(callers=True)
            changed["items"][0][field] = replacement
            self.assertIsNotNone(bench.validate_locate(changed, callers=True))
        self.assertIsNotNone(bench.validate_locate(value, edited=True))

    def test_grep_requires_both_exact_matches(self):
        value = {"context_receipt": self.receipt, "truncated": False, "matches": [
            {"anchor": bench.PREFIX + "bench_leaf", "line": 1, "text": bench.BEFORE.splitlines()[0]},
            {"anchor": bench.PREFIX + "bench_middle", "line": 2, "text": bench.BEFORE.splitlines()[1]},
        ]}
        self.assertIsNone(bench.validate_grep(value))
        value["matches"].pop()
        self.assertIsNotNone(bench.validate_grep(value))

    def test_new_definition_anchor_and_deleted_definition(self):
        value = {"context_receipt": self.receipt, "total": 0, "items": []}
        self.assertIsNone(bench.validate_definition(value, False))
        self.assertIsNotNone(bench.validate_definition(value, True))
        value.update(total=1, match_kind="definition", items=[{"anchor": bench.PREFIX + "bench_twig"}])
        self.assertIsNone(bench.validate_definition(value, True))
        self.assertIsNotNone(bench.validate_definition(value, False))
        value["items"][0]["anchor"] = "wrong"
        self.assertIsNotNone(bench.validate_definition(value, True))

    def test_search_requires_current_canonical_definition(self):
        value = {"context_receipt": self.receipt, "truncated": False,
                 "results": [{"anchor": bench.PREFIX + "bench_leaf"}]}
        self.assertIsNone(bench.validate_search(value))
        self.assertIsNotNone(bench.validate_search(value, True))
        value["results"] = [{"anchor": bench.PREFIX + "bench_twig"}]
        self.assertIsNone(bench.validate_search(value, True))
        value["results"].append({"anchor": bench.PREFIX + "bench_leaf"})
        self.assertIsNotNone(bench.validate_search(value, True))

    def test_process_failure_remains_failure_despite_valid_json(self):
        with patch.object(bench, "execute", return_value={"success": False, "error": "timeout", "elapsed_ms": 12, "value": self.query}):
            row = bench.request(Path("aden"), [], Path("."), {}, 1, bench.validate_query)
        self.assertFalse(row["success"])
        self.assertEqual(row["error"], "timeout")

    def test_malformed_nested_evidence_is_rejected_and_retained(self):
        for total, items in ((False, []), (0, [{"anchor": bench.PREFIX + "bench_leaf"}]), (0, None)):
            value = {"context_receipt": self.receipt, "total": total, "items": items}
            self.assertIsNotNone(bench.validate_definition(value, False))
        for item in (None, [], "definition", {"anchor": []}):
            value = {"context_receipt": self.receipt, "total": 1,
                     "match_kind": "definition", "items": [item]}
            self.assertIsNotNone(bench.validate_definition(value, True))
        for field in ("anchor", "line", "text"):
            match = {"anchor": bench.PREFIX + "bench_leaf", "line": 1,
                     "text": bench.BEFORE.splitlines()[0]}
            match[field] = []
            value = {"context_receipt": self.receipt, "truncated": False, "matches": [match]}
            self.assertIsNotNone(bench.validate_grep(value))
        with patch.object(bench, "execute", return_value={"success": True, "elapsed_ms": 12, "value": self.query}):
            row = bench.request(Path("aden"), [], Path("."), {}, 1,
                                lambda value: value["missing"])
        self.assertFalse(row["success"])
        self.assertEqual(row["error"], "validation error: KeyError")
        self.assertEqual(row["evidence"], self.query)

    def test_fixture_reproducible_and_edit_preserves_size(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            self.assertEqual(bench.fixture(root / "one", 2, 3), bench.fixture(root / "two", 2, 3))
            self.assertEqual(len(list((root / "one").glob("*.rs"))), 3)
        self.assertEqual(len(bench.BEFORE.encode()), len(bench.AFTER.encode()))

    def test_cold_arm_order_is_balanced_across_repetitions(self):
        for arms in (["before", "after"], ["a", "b", "c"]):
            orders = [bench.rotated_order(arms, repetition) for repetition in range(2 * len(arms))]
            for order in orders:
                self.assertEqual(set(order), set(arms))
            for position in range(len(arms)):
                for arm in arms:
                    self.assertEqual(sum(order[position] == arm for order in orders), 2)

    def test_failure_counts_and_source_parity(self):
        good = {"arm": "candidate", "workload": "warm_anchor", "repetition": 0,
                "success": True, "elapsed_ms": 10, "evidence": self.query}
        bad = {**good, "success": False, "elapsed_ms": 100}
        summary = bench.summarize([good, bad])[0]
        self.assertEqual((summary["started"], summary["successful"], summary["failures"]), (2, 1, 1))
        self.assertEqual(summary["all_started_median_ms"], 55)
        self.assertTrue(bench.evidence_parity([good]))
        other = copy.deepcopy(good)
        other["evidence"]["context_receipt"]["observed_source_fingerprint"] = "different"
        self.assertFalse(bench.evidence_parity([good, other]))


if __name__ == "__main__":
    unittest.main()
