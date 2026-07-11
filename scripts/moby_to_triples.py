# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Convert Moby Thesaurus II into flat 5-tuple triples.

Input format (mthesaur.txt, raw data file with no Project Gutenberg header):
  Each non-empty line is a comma-separated list. The first field is the root
  word; every subsequent field is a synonym. Example:
    happy,blissful,content,glad,joyful

Output schema: JSON array of 5-tuples [subj, label, obj, pos, source].
  label:  "SynonymOf" (Moby provides only synonym relationships)
  pos:    "unknown" (Moby does not tag part-of-speech)
  source: "moby"

Normalization and filter rules:
  - Lemmas are lowercased and stripped of leading/trailing whitespace.
  - A token is SKIPPED if:
      - fewer than 2 characters after normalization
      - contains any ASCII digit (0-9)
      - contains a space (multi-word phrases are dropped)
  - Single-word tokens with hyphens or apostrophes are kept
    (e.g. "avant-garde", "don't").
  - Self-loops (root == syn after normalization) are skipped.
  - If the root fails the filter, the entire line is skipped.

Usage:
    python3 scripts/moby_to_triples.py \\
        --moby ~/.cache/aden/dict/mthesaur.txt \\
        --out  ~/.cache/aden/dict/moby-triples-flat.json
"""
import argparse
import json
import sys
from pathlib import Path


def _norm(token: str) -> str:
    """Lowercase and strip a token."""
    return token.lower().strip()


def _accept(token: str) -> bool:
    """Return True if the normalized token passes all filter rules."""
    if len(token) < 2:
        return False
    if any(ch.isdigit() for ch in token):
        return False
    if " " in token:
        return False
    return True


def convert(moby_path: Path) -> list[list[str]]:
    """Parse mthesaur.txt and return a list of 5-element lists."""
    triples: list[list[str]] = []
    lines_read = 0

    with moby_path.open(encoding="utf-8", errors="replace") as fh:
        for raw in fh:
            raw = raw.rstrip("\n")
            if not raw.strip():
                continue
            lines_read += 1

            fields = raw.split(",")
            root_raw = _norm(fields[0])
            if not _accept(root_raw):
                continue

            for field in fields[1:]:
                syn = _norm(field)
                if not _accept(syn):
                    continue
                if syn == root_raw:
                    continue
                triples.append([root_raw, "SynonymOf", syn, "unknown", "moby"])

    print(f"Lines read:     {lines_read}")
    print(f"Triples emitted: {len(triples)}")
    return triples


def main() -> None:
    ap = argparse.ArgumentParser(
        description="Convert Moby Thesaurus II to flat 5-tuple triples."
    )
    ap.add_argument(
        "--moby",
        default=str(Path.home() / ".cache/aden/dict/mthesaur.txt"),
        help="Path to mthesaur.txt (default: %(default)s)",
    )
    ap.add_argument(
        "--out",
        default=str(Path.home() / ".cache/aden/dict/moby-triples-flat.json"),
        help="Output flat-triples JSON file (default: %(default)s)",
    )
    args = ap.parse_args()

    moby_path = Path(args.moby)
    if not moby_path.is_file():
        print(f"error: Moby file not found: {moby_path}", file=sys.stderr)
        sys.exit(1)

    print(f"Converting {moby_path} ...")
    triples = convert(moby_path)

    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    with out_path.open("w", encoding="utf-8") as fh:
        json.dump(triples, fh, indent=0)
    print(f"Wrote {len(triples)} triples to {out_path}")

    # 5-line preview.
    print("\nPreview (first 5 triples):")
    for row in triples[:5]:
        print(" ", row)


if __name__ == "__main__":
    main()
