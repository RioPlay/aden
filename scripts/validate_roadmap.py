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
REPOSITORY_STATES = {"proposed", "ready", "done", "invalidated", "abandoned"}
SATISFIES_DEPENDENCY = {"done"}
ATTR = re.compile(r"^:([a-z0-9-]+):\s*(.*?)\s*$")
ISSUE_URL = re.compile(r"^https://github\.com/[^/]+/[^/]+/issues/(\d+)$")
SECTIONS = (
    "Current evidence",
    "Outcome",
    "Scope",
    "Non-goals",
    "Acceptance",
    "Failure modes and rollback",
    "Completion evidence",
)


def comma_values(value: str) -> list[str]:
    if value.strip().lower() in {"", "none"}:
        return []
    return [item.strip() for item in value.split(",") if item.strip()]


def display_path(path: Path) -> str:
    try:
        return str(path.relative_to(ROOT))
    except ValueError:
        return str(path)


def load_packets() -> tuple[dict[str, dict[str, str]], list[str]]:
    found: dict[str, dict[str, str]] = {}
    errors: list[str] = []
    if not PACKETS.is_dir():
        return found, [f"missing packet directory: {PACKETS.relative_to(ROOT)}"]

    for path in sorted(PACKETS.glob("*.adoc")):
        attrs: dict[str, str] = {}
        text = path.read_text(encoding="utf-8")
        seen_attrs: set[str] = set()
        for line in text.splitlines():
            match = ATTR.match(line)
            if match:
                if match.group(1) in seen_attrs:
                    errors.append(
                        f"{display_path(path)}: duplicate attribute {match.group(1)}"
                    )
                seen_attrs.add(match.group(1))
                attrs[match.group(1)] = match.group(2)
            elif attrs and line.strip():
                break

        missing = sorted(REQUIRED - attrs.keys())
        if missing:
            errors.append(f"{display_path(path)}: missing {', '.join(missing)}")
            continue

        packet_id = attrs["packet-id"]
        if packet_id in found:
            errors.append(f"duplicate packet id {packet_id}: {path.relative_to(ROOT)}")
            continue
        if attrs["status"] not in REPOSITORY_STATES:
            errors.append(
                f"{packet_id}: invalid repository status {attrs['status']!r}"
            )
        if not re.fullmatch(r"P[0-3]", attrs["priority"]):
            errors.append(f"{packet_id}: priority must be P0..P3")
        if not re.fullmatch(r"E[0-4]", attrs["evidence-level"]):
            errors.append(f"{packet_id}: evidence-level must be E0..E4")
        if not attrs["acceptance-version"].isdigit():
            errors.append(f"{packet_id}: acceptance-version must be an integer")
        if not ISSUE_URL.fullmatch(attrs["issue"]):
            errors.append(f"{packet_id}: issue must be a GitHub issue URL")
        anchor = packet_id.lower().replace("-", "-")
        if f"[[{anchor}]]" not in text:
            errors.append(f"{packet_id}: missing stable anchor [[{anchor}]]")
        for heading in SECTIONS:
            if f"== {heading}" not in text:
                errors.append(f"{packet_id}: missing section '== {heading}'")
        attrs["_path"] = display_path(path)
        found[packet_id] = attrs
    return found, errors


def validate_graph(packets: dict[str, dict[str, str]]) -> list[str]:
    errors: list[str] = []
    issues: dict[str, str] = {}
    for packet_id, attrs in packets.items():
        issue = attrs["issue"]
        if issue in issues:
            errors.append(f"{packet_id}: shares issue with {issues[issue]}")
        else:
            issues[issue] = packet_id
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
        if attrs["status"] in {"ready", "done"}:
            unfinished = [
                dep
                for dep in comma_values(attrs["depends-on"])
                if dep in packets and packets[dep]["status"] not in SATISFIES_DEPENDENCY
            ]
            if unfinished:
                errors.append(
                    f"{packet_id}: {attrs['status']} with unfinished dependencies {', '.join(unfinished)}"
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
