# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Merge N flat 5-tuple (or legacy 4-tuple) triple files into the merged schema.

INPUT (per file): JSON array of rows, each row being either:
  - 5-element list: [subj, label, obj, pos, source]     (current format)
  - 4-element list: [subj, label, obj, pos]              (legacy format, no source field)
    For legacy rows, the source is assigned by --source-fallback if provided,
    otherwise "unknown".

OUTPUT: JSON array of objects with the merged schema:
  {"subj": str, "label": str, "obj": str, "pos": str, "sources": [str, ...], "agreement": int}
  - "sources" is a sorted, unique list of source ids that produced this edge.
  - "pos" is the first non-"unknown" value seen across all inputs for this key,
    falling back to "unknown" if all inputs tagged it as unknown.
  - "agreement" is len(sources).
  - Rows are keyed by (subj, label, obj). Subj/obj are lowercased and stripped.
  - Self-loops (subj == obj after normalization) are dropped.

Usage:
    python3 scripts/merge_triples.py \\
        --input oewn-triples-flat.json \\
        --input moby-triples-flat.json \\
        --out   merged-triples.json \\
        --min-agreement 1 \\
        --stats

    To supply a source fallback for a legacy 4-tuple file:
        --source-fallback oewn   (applies to ALL inputs that lack a source field)
    For per-file control, use the oewn file's own 5-tuple rows instead.
"""
import argparse
import json
import sys
from collections import defaultdict
from pathlib import Path


def _norm(s: str) -> str:
    """Lowercase and strip a lemma."""
    return s.lower().strip()


def load_flat(path: Path, fallback_source: str) -> list[tuple[str, str, str, str, str]]:
    """Load a flat triples file and return normalized 5-tuples.

    Rows of length 4 (legacy) get their source from fallback_source.
    Rows of length 5 use their own source field.
    Any other row length is logged and skipped.
    """
    with path.open(encoding="utf-8") as fh:
        data = json.load(fh)

    if not isinstance(data, list):
        print(f"  warning: {path.name}: expected JSON array, skipping", file=sys.stderr)
        return []

    out: list[tuple[str, str, str, str, str]] = []
    skipped = 0
    for row in data:
        if not isinstance(row, list):
            skipped += 1
            continue
        if len(row) == 5:
            subj, label, obj, pos, source = row
        elif len(row) == 4:
            subj, label, obj, pos = row
            source = fallback_source
        else:
            skipped += 1
            continue
        subj = _norm(str(subj))
        obj = _norm(str(obj))
        label = str(label)
        pos = str(pos)
        source = str(source)
        # Drop self-loops after normalization.
        if subj == obj:
            skipped += 1
            continue
        out.append((subj, label, obj, pos, source))

    if skipped:
        print(f"  {path.name}: skipped {skipped} malformed/self-loop rows", file=sys.stderr)
    return out


def merge(
    all_rows: list[tuple[str, str, str, str, str]],
    min_agreement: int,
) -> list[dict]:
    """Accumulate rows by (subj, label, obj) key and produce merged objects."""
    # key -> {"pos": str, "sources": set[str]}
    acc: dict[tuple[str, str, str], dict] = {}

    for subj, label, obj, pos, source in all_rows:
        key = (subj, label, obj)
        if key not in acc:
            acc[key] = {"pos": "unknown", "sources": set()}
        entry = acc[key]
        # Take the first non-"unknown" pos seen.
        if entry["pos"] == "unknown" and pos != "unknown":
            entry["pos"] = pos
        entry["sources"].add(source)

    result = []
    for (subj, label, obj), entry in acc.items():
        sources = sorted(entry["sources"])
        agreement = len(sources)
        if agreement < min_agreement:
            continue
        result.append({
            "subj": subj,
            "label": label,
            "obj": obj,
            "pos": entry["pos"],
            "sources": sources,
            "agreement": agreement,
        })

    # Stable sort: by agreement descending, then by (subj, label, obj).
    result.sort(key=lambda r: (-r["agreement"], r["subj"], r["label"], r["obj"]))
    return result


def print_stats(result: list[dict], per_source: dict[str, int]) -> None:
    """Print per-source counts, per-edge-type counts, and agreement histogram."""
    print("\n-- Statistics --")

    print("\nPer-source edge counts (before merge):")
    for src in sorted(per_source):
        print(f"  {src}: {per_source[src]}")

    edge_counts: dict[str, int] = defaultdict(int)
    agreement_hist: dict[int, int] = defaultdict(int)
    for row in result:
        edge_counts[row["label"]] += 1
        agreement_hist[row["agreement"]] += 1

    print("\nPer-edge-type counts (merged output):")
    for label in sorted(edge_counts):
        print(f"  {label}: {edge_counts[label]}")

    print("\nAgreement distribution (merged output):")
    for agree in sorted(agreement_hist):
        print(f"  agreement={agree}: {agreement_hist[agree]} edges")

    print(f"\nTotal merged edges: {len(result)}")


def main() -> None:
    ap = argparse.ArgumentParser(
        description="Merge flat triple files into the merged object schema."
    )
    ap.add_argument(
        "--input",
        action="append",
        dest="inputs",
        metavar="FILE",
        required=True,
        help="Flat triples JSON file (repeatable); may be 4-tuple or 5-tuple rows",
    )
    ap.add_argument(
        "--out",
        default=str(Path.home() / ".cache/aden/dict/merged-triples.json"),
        help="Output merged triples JSON file (default: %(default)s)",
    )
    ap.add_argument(
        "--min-agreement",
        type=int,
        default=1,
        metavar="N",
        help="Minimum number of sources that must agree on an edge (default: %(default)s)",
    )
    ap.add_argument(
        "--source-fallback",
        default="unknown",
        metavar="SRC",
        help=(
            "Source id to assign to legacy 4-tuple rows that have no source field "
            "(default: %(default)r). Applies to ALL input files."
        ),
    )
    ap.add_argument(
        "--stats",
        action="store_true",
        help="Print per-source counts, per-edge-type counts, and agreement histogram",
    )
    args = ap.parse_args()

    all_rows: list[tuple[str, str, str, str, str]] = []
    per_source: dict[str, int] = defaultdict(int)

    for input_path_str in args.inputs:
        input_path = Path(input_path_str)
        if not input_path.is_file():
            print(f"error: input file not found: {input_path}", file=sys.stderr)
            sys.exit(1)
        print(f"Loading {input_path} ...")
        rows = load_flat(input_path, args.source_fallback)
        print(f"  loaded {len(rows)} rows")
        for row in rows:
            per_source[row[4]] += 1
        all_rows.extend(rows)

    print(f"\nTotal rows across all inputs: {len(all_rows)}")
    result = merge(all_rows, args.min_agreement)
    print(f"Merged edges (min-agreement={args.min_agreement}): {len(result)}")

    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    with out_path.open("w", encoding="utf-8") as fh:
        json.dump(result, fh, indent=2)
    print(f"Wrote {len(result)} merged edges to {out_path}")

    if args.stats:
        print_stats(result, dict(per_source))


if __name__ == "__main__":
    main()
