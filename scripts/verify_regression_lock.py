#!/usr/bin/env python3
"""DEPRECATED: use `cargo test -p aden-cli --test regression_lock`.

Fail if a tuned regression dataset changes without an explicit lock update.
Kept for offline/manual use; CI uses the Rust port.
"""

from __future__ import annotations

import hashlib
import json
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
LOCK = ROOT / "scripts/regression-lock.json"


def main() -> None:
    lock = json.loads(LOCK.read_text(encoding="utf-8"))
    failures = []
    for relative, expected in lock["files"].items():
        path = ROOT / relative
        if path.is_file():
            # Match git-canonical LF hashes even when the working tree has CRLF.
            data = path.read_bytes().replace(b"\r\n", b"\n").replace(b"\r", b"\n")
            actual = hashlib.sha256(data).hexdigest()
        else:
            actual = "missing"
        if actual != expected:
            failures.append(f"{relative}: expected {expected}, got {actual}")
    if failures:
        raise SystemExit("Regression lock mismatch:\n" + "\n".join(failures))
    print(f"Regression lock verified: {len(lock['files'])} frozen files")


if __name__ == "__main__":
    main()
