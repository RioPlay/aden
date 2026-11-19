#!/usr/bin/env python3
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Check Cargo's JSON output for a repeated default build of the grammar pack."""
import json
import sys
from pathlib import Path


def check(messages):
    libraries = {}
    outputs = {}
    for message in messages:
        if message.get("reason") == "build-script-executed":
            outputs.setdefault(message["package_id"], set()).add(Path(message["out_dir"]))
        elif (message.get("reason") == "compiler-artifact"
              and message["target"]["name"] == "tree_sitter_language_pack"
              and "lib" in message["target"]["kind"]):
            package = message["package_id"]
            libraries[package] = libraries.get(package, True) and message["fresh"]
    if not libraries:
        raise ValueError("Cargo did not report the grammar library")
    for package, fresh in libraries.items():
        if not fresh:
            raise ValueError("Cargo rebuilt the grammar library on an unchanged build")
        directories = outputs.get(package)
        if not directories or any(not directory.is_dir() for directory in directories):
            raise ValueError("Missing grammar build output directory")
        if any((directory / name).exists() for directory in directories
               for name in ("_parsers", "_aden_bundle_inputs")):
            raise ValueError("The grammar build retained extracted parser sources")


if __name__ == "__main__":
    try:
        check(json.loads(line) for line in sys.stdin if line.strip())
    except ValueError as error:
        sys.exit(str(error))
