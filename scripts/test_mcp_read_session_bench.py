# SPDX-License-Identifier: AGPL-3.0-or-later
import copy
import unittest
from unittest.mock import patch
from mcp_read_session_bench import validate, validate_deleted, read_environment


class EvidenceValidation(unittest.TestCase):
    def test_symbol_comparison_keeps_both_workers_resident_and_overrides_inherited_switches(self):
        with patch.dict("os.environ", {"ADEN_READ_SESSION": "0", "ADEN_SYMBOL_LOOKUP_CACHE": "invalid"}):
            off = read_environment("binary", "data", False, "symbols")
            on = read_environment("binary", "data", True, "symbols")
            self.assertEqual(off["ADEN_READ_SESSION"], "1")
            self.assertEqual(on["ADEN_READ_SESSION"], "1")
            self.assertEqual(off["ADEN_SYMBOL_LOOKUP_CACHE"], "0")
            self.assertEqual(on["ADEN_SYMBOL_LOOKUP_CACHE"], "1")
            self.assertEqual(read_environment("binary", "data", False)["ADEN_READ_SESSION"], "0")
            self.assertNotIn("ADEN_SYMBOL_LOOKUP_CACHE", read_environment("binary", "data", True))
        with self.assertRaises(ValueError):
            read_environment("binary", "data", True, "invalid")

    def test_deleted_definition_requires_explicit_current_zero_results(self):
        response = {"context_receipt": {"freshness": "current", "graph_revision": "r", "observed_source_fingerprint": "f"},
            "items": [], "returned": 0, "total": 0, "match_kind": "full_text_fallback", "resolution": {"state": "not_found"}, "truncated": False}
        validate_deleted(response)
        for broken in [{}, {**response, "items": [{}]}, {**response, "total": False}, {**response, "returned": 1},
                       {**response, "context_receipt": {"freshness": "lagging"}}]:
            with self.assertRaises(ValueError): validate_deleted(broken)

    def test_accepts_exact_current_chain_and_rejects_representative_failures(self):
        response = {"context_receipt": {"freshness": "current", "graph_revision": "r", "observed_source_fingerprint": "f"},
            "items": [{"anchor": "aden://module/read-session-fixture/lib.rs#" + symbol, "depth": depth, "via_edge_types": [] if depth == 0 else ["Calls"]} for depth, symbol in enumerate(["entry", "middle", "before"])], "truncated": False}
        self.assertEqual(len(validate("query", response, "before")["items"]), 3)
        for mutation in [lambda x: x["items"].pop(), lambda x: x.update(truncated=True),
                         lambda x: x["context_receipt"].update(freshness="lagging"),
                         lambda x: x["items"][2].update(anchor="aden://module/other/lib.rs#before"),
                         lambda x: x["items"][1].update(depth=99),
                         lambda x: x["items"][1].update(via_edge_types=["Contains"])]:
            broken = copy.deepcopy(response)
            mutation(broken)
            with self.assertRaises(ValueError):
                validate("query", broken, "before")

    def test_verified_source_and_exact_definition_are_required(self):
        receipt = {"freshness": "current", "graph_revision": "r", "observed_source_fingerprint": "f"}
        source = {"context_receipt": receipt, "anchor": "aden://module/read-session-fixture/lib.rs#before", "source": {"state": "complete", "hash_verified": True, "file": "lib.rs", "start_line": 3, "end_line": 3, "text": "pub fn before() {}"}}
        source.update(backlinks=[{"anchor": "aden://module/read-session-fixture/lib.rs#middle", "depth": 1, "via_edge_types": ["Calls"]}], impact=[])
        validate("understand", source, "before")
        for mutation in [lambda x: x["source"].update(hash_verified=False),
                         lambda x: x.update(anchor="aden://module/other/lib.rs#before"),
                         lambda x: x["source"].update(text="// fn before() is not defined here"),
                         lambda x: x.update(backlinks=[])]:
            broken = copy.deepcopy(source)
            mutation(broken)
            with self.assertRaises(ValueError): validate("understand", broken, "before")
        definition = {"context_receipt": receipt, "match_kind": "definition", "items": [{"anchor": "aden://module/read-session-fixture/lib.rs#before", "file": "lib.rs", "start_line": "3", "end_line": "3"}]}
        validate("locate", definition, "before")
        definition["match_kind"] = "suggestion"
        with self.assertRaises(ValueError): validate("locate", definition, "before")


if __name__ == "__main__": unittest.main()
