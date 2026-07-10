#!/usr/bin/env python3
"""Validate Aden's repository-side execution program."""

from __future__ import annotations

import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
PACKETS = ROOT / "docs" / "roadmap" / "packets"
REQUIRED = {
    "packet-id",
    "status",
    "priority",
    "depends-on",
    "unlocks",
    "scope",
    "issue",
    "acceptance-version",
    "evidence-level",
}
STATES = {
    "proposed",
    "ready",
    "claimed",
    "implementing",
    "verifying",
    "review",
    "landed",
    "measured",
    "done",
    "blocked",
    "rework",
    "partial",
    "invalidated",
    "abandoned",
}
SATISFIES_DEPENDENCY = {"done"}
ATTR = re.compile(r"^:([a-z0-9-]+):\s*(.*?)\s*$")


def comma_values(value: str) -> list[str]:
    if value.strip().lower() in {"", "none"}:
        return []
    return [item.strip() for item in value.split(",") if item.strip()]


def load_packets() -> tuple[dict[str, dict[str, str]], list[str]]:
    found: dict[str, dict[str, str]] = {}
    errors: list[str] = []
    if not PACKETS.is_dir():
        return found, [f"missing packet directory: {PACKETS.relative_to(ROOT)}"]

    for path in sorted(PACKETS.glob("*.adoc")):
        attrs: dict[str, str] = {}
        text = path.read_text(encoding="utf-8")
        for line in text.splitlines():
            match = ATTR.match(line)
            if match:
                attrs[match.group(1)] = match.group(2)
            elif attrs and line.strip():
                break

        missing = sorted(REQUIRED - attrs.keys())
        if missing:
            errors.append(f"{path.relative_to(ROOT)}: missing {', '.join(missing)}")
            continue

        packet_id = attrs["packet-id"]
        if packet_id in found:
            errors.append(f"duplicate packet id {packet_id}: {path.relative_to(ROOT)}")
            continue
        if attrs["status"] not in STATES:
            errors.append(f"{packet_id}: invalid status {attrs['status']!r}")
        if not re.fullmatch(r"P[0-3]", attrs["priority"]):
            errors.append(f"{packet_id}: priority must be P0..P3")
        if not re.fullmatch(r"E[0-4]", attrs["evidence-level"]):
            errors.append(f"{packet_id}: evidence-level must be E0..E4")
        if not attrs["acceptance-version"].isdigit():
            errors.append(f"{packet_id}: acceptance-version must be an integer")
        anchor = packet_id.lower().replace("-", "-")
        if f"[[{anchor}]]" not in text:
            errors.append(f"{packet_id}: missing stable anchor [[{anchor}]]")
        for heading in ("Outcome", "Acceptance", "Failure modes and rollback"):
            if f"== {heading}" not in text:
                errors.append(f"{packet_id}: missing section '== {heading}'")
        attrs["_path"] = str(path.relative_to(ROOT))
        found[packet_id] = attrs
    return found, errors


def validate_graph(packets: dict[str, dict[str, str]]) -> list[str]:
    errors: list[str] = []
    for packet_id, attrs in packets.items():
        for dependency in comma_values(attrs["depends-on"]):
            if dependency not in packets:
                errors.append(f"{packet_id}: unknown dependency {dependency}")
        for unlocked in comma_values(attrs["unlocks"]):
            if unlocked not in packets:
                errors.append(f"{packet_id}: unknown unlock target {unlocked}")
            elif packet_id not in comma_values(packets[unlocked]["depends-on"]):
                errors.append(
                    f"{packet_id}: unlock target {unlocked} does not depend on {packet_id}"
                )

    visiting: set[str] = set()
    visited: set[str] = set()

    def visit(packet_id: str, trail: list[str]) -> None:
        if packet_id in visiting:
            cycle = trail[trail.index(packet_id) :] + [packet_id]
            errors.append("dependency cycle: " + " -> ".join(cycle))
            return
        if packet_id in visited:
            return
        visiting.add(packet_id)
        trail.append(packet_id)
        for dependency in comma_values(packets[packet_id]["depends-on"]):
            if dependency in packets:
                visit(dependency, trail)
        trail.pop()
        visiting.remove(packet_id)
        visited.add(packet_id)

    for packet_id in packets:
        visit(packet_id, [])

    for packet_id, attrs in packets.items():
        if attrs["status"] == "ready":
            unfinished = [
                dep
                for dep in comma_values(attrs["depends-on"])
                if dep in packets and packets[dep]["status"] not in SATISFIES_DEPENDENCY
            ]
            if unfinished:
                errors.append(
                    f"{packet_id}: ready with unfinished dependencies {', '.join(unfinished)}"
                )

    return errors


def main() -> int:
    packets, errors = load_packets()
    errors.extend(validate_graph(packets))
    if errors:
        for error in errors:
            print(f"ERROR: {error}")
        print(f"roadmap validation failed: {len(errors)} error(s)")
        return 1
    print(f"roadmap validation passed: {len(packets)} packet(s), dependency graph acyclic")
    return 0


if __name__ == "__main__":
    sys.exit(main())
