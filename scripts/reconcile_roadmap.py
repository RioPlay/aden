#!/usr/bin/env python3
"""Check that GitHub delivery state agrees with repository admission state."""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
PACKETS = ROOT / "docs" / "roadmap" / "packets"
ISSUE_URL = re.compile(r"/issues/(\d+)$")
ATTR = re.compile(r"^:([a-z0-9-]+):\s*(.*?)\s*$")


def attrs(path: Path) -> dict[str, str]:
    result: dict[str, str] = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        match = ATTR.match(line)
        if match:
            result[match.group(1)] = match.group(2)
        elif result and line.strip():
            break
    return result


def issue_data(number: str) -> dict[str, object]:
    command = ["gh", "issue", "view", number, "--json", "state,labels,assignees"]
    return json.loads(subprocess.check_output(command, cwd=ROOT, text=True))


def validate_live(packet: dict[str, str], live: dict[str, object]) -> list[str]:
    errors: list[str] = []
    labels = {item["name"] for item in live["labels"]}
    assignees = live["assignees"]
    admission = packet.get("status")
    packet_id = packet["packet-id"]
    if admission in {"proposed", "ready"} and live["state"] != "OPEN":
        errors.append(f"{packet_id}: packet issue is not open")
    if admission == "proposed" and "packet-proposed" not in labels:
        errors.append(f"{packet_id}: proposed packet lacks packet-proposed label")
    if admission == "ready":
        delivery = {"packet-ready", "packet-claimed", "packet-review", "packet-blocked"}
        if not labels.intersection(delivery):
            errors.append(f"{packet_id}: ready packet lacks live delivery label")
        if "packet-claimed" in labels and len(assignees) != 1:
            errors.append(f"{packet_id}: claimed packet needs exactly one assignee")
    if admission == "done":
        if live["state"] != "CLOSED" or "packet-done" not in labels:
            errors.append(f"{packet_id}: done packet needs closed packet-done issue")
    if admission in {"invalidated", "abandoned"}:
        if live["state"] != "CLOSED" or "packet-cancelled" not in labels:
            errors.append(f"{packet_id}: cancelled packet needs closed packet-cancelled issue")
    return errors


def main() -> int:
    errors: list[str] = []
    for path in sorted(PACKETS.glob("*.adoc")):
        packet = attrs(path)
        match = ISSUE_URL.search(packet.get("issue", ""))
        if not match:
            errors.append(f"{path.name}: missing issue number")
            continue
        try:
            live = issue_data(match.group(1))
        except (OSError, subprocess.CalledProcessError, json.JSONDecodeError) as error:
            errors.append(f"{packet.get('packet-id', path.name)}: cannot read issue: {error}")
            continue
        errors.extend(validate_live(packet, live))
    if errors:
        for error in errors:
            print(f"ERROR: {error}")
        return 1
    print("roadmap reconciliation passed: repository admission and GitHub delivery agree")
    return 0


if __name__ == "__main__":
    sys.exit(main())
