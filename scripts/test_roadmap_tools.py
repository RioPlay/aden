#!/usr/bin/env python3
"""Regression tests for the roadmap structural validator."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("validate_roadmap", ROOT / "scripts/validate_roadmap.py")
assert SPEC and SPEC.loader
validator = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(validator)
RECONCILE_SPEC = importlib.util.spec_from_file_location(
    "reconcile_roadmap", ROOT / "scripts/reconcile_roadmap.py"
)
assert RECONCILE_SPEC and RECONCILE_SPEC.loader
reconciler = importlib.util.module_from_spec(RECONCILE_SPEC)
RECONCILE_SPEC.loader.exec_module(reconciler)


def packet(packet_id: str, status: str = "proposed", depends_on: str = "none") -> str:
    return f""":packet-id: {packet_id}
:status: {status}
:priority: P0
:depends-on: {depends_on}
:unlocks: none
:scope: crates/example
:issue: https://github.com/RioPlay/aden/issues/999
:acceptance-version: 1
:evidence-level: E3

[[{packet_id.lower()}]]
= {packet_id}: fixture

== Current evidence

Fixture.

== Outcome

Fixture.

== Scope

Fixture.

== Non-goals

Fixture.

== Acceptance

Fixture.

== Failure modes and rollback

Fixture.

== Completion evidence

Fixture.
"""


class RoadmapValidatorTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.original_packets = validator.PACKETS
        validator.PACKETS = Path(self.tmp.name)

    def tearDown(self) -> None:
        validator.PACKETS = self.original_packets
        self.tmp.cleanup()

    def test_rejects_duplicate_metadata(self) -> None:
        (validator.PACKETS / "packet.adoc").write_text(
            packet("AP-999").replace(":priority: P0", ":priority: P0\n:priority: P1"),
            encoding="utf-8",
        )
        _, errors = validator.load_packets()
        self.assertTrue(any("duplicate attribute priority" in error for error in errors))

    def test_rejects_missing_contract_section(self) -> None:
        (validator.PACKETS / "packet.adoc").write_text(
            packet("AP-999").replace("== Completion evidence\n\nFixture.\n", ""),
            encoding="utf-8",
        )
        _, errors = validator.load_packets()
        self.assertTrue(any("Completion evidence" in error for error in errors))

    def test_partial_dependency_does_not_satisfy_ready_packet(self) -> None:
        packets = {
            "AP-001": {"status": "partial", "depends-on": "none", "unlocks": "AP-002", "issue": "https://github.com/RioPlay/aden/issues/1"},
            "AP-002": {"status": "ready", "depends-on": "AP-001", "unlocks": "none", "issue": "https://github.com/RioPlay/aden/issues/2"},
        }
        errors = validator.validate_graph(packets)
        self.assertTrue(any("ready with unfinished dependencies AP-001" in error for error in errors))

    def test_done_packet_cannot_skip_unfinished_dependency(self) -> None:
        packets = {
            "AP-001": {"status": "proposed", "depends-on": "none", "unlocks": "AP-002", "issue": "https://github.com/RioPlay/aden/issues/1"},
            "AP-002": {"status": "done", "depends-on": "AP-001", "unlocks": "none", "issue": "https://github.com/RioPlay/aden/issues/2"},
        }
        errors = validator.validate_graph(packets)
        self.assertTrue(any("done with unfinished dependencies AP-001" in error for error in errors))

    def test_done_packet_requires_closed_done_issue(self) -> None:
        packet = {"packet-id": "AP-001", "status": "done"}
        open_issue = {"state": "OPEN", "labels": [{"name": "packet-done"}], "assignees": []}
        self.assertTrue(reconciler.validate_live(packet, open_issue))
        closed_issue = {"state": "CLOSED", "labels": [{"name": "packet-done"}], "assignees": []}
        self.assertEqual(reconciler.validate_live(packet, closed_issue), [])


if __name__ == "__main__":
    unittest.main()
