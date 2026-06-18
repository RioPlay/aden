#!/usr/bin/env python3
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# OEWN -> neutral lexical-semantic triples (OFFLINE extraction; writes NOTHING to .aden/store).
#
# Open English WordNet 2025 (CC-BY 4.0) ships explicit typed relations, so we map its
# pointers DIRECTLY onto aden's EdgeType vocabulary instead of parsing gloss text:
#   synset members   -> SynonymOf   (all lemmas in a synset are synonyms; any POS)
#   hypernym          -> IsA         (this synset IS-A its hypernym; noun/verb)
#   mero_part/_member/_substance -> PartOf  (the meronym is PART-OF this synset; noun)
#   antonym (sense)   -> AntonymOf   (when present in the entries' sense relations)
# This is the HIGH-confidence path the dry-run extractor only approximated by gloss
# heuristics. It honors aden's "extraction per-format, resolution neutral" law: OEWN
# parsing lives here (offline), the neutral triples JSON is what aden's producer consumes.
#
# Scoping: a real importer scopes to the corpus vocabulary so the graph stays small and
# relevant. Pass --seed-file <vocab.txt> (one lemma per line, e.g. aden's index tokens),
# or rely on the built-in CODE_SEED to demonstrate the NL->code synonym bridge. --all
# converts everything (large).
#
# Usage:
#   python3 scripts/oewn_to_triples.py --oewn ~/.cache/aden/dict/oewn \
#       [--seed-file vocab.txt | --all] [--out ~/.cache/aden/dict/oewn-triples.json] [--preview 40]

import argparse
import glob
import json
import os

# POS keys OEWN uses (n=noun, v=verb, a/s=adjective, r=adverb).
POS_NAME = {"n": "noun", "v": "verb", "a": "adjective", "s": "adjective", "r": "adverb"}
MERONYM_KEYS = ("mero_part", "mero_member", "mero_substance")

# Demonstration seed: natural-language words that show up in queries and bridge to code
# vocabulary (the +oracle gap the A/B measured). Some have no CS sense in WordNet — that
# gap is itself a finding worth seeing, so they stay in.
CODE_SEED = [
    "save", "store", "keep", "fetch", "retrieve", "merge", "combine", "fuse", "blend",
    "cluster", "group", "split", "parse", "resolve", "search", "query", "rank", "score",
    "encode", "embed", "vector", "distance", "secret", "credential", "neighbor",
    "orphan", "reference", "graph", "node", "edge", "token", "document", "index",
]


def load_synsets(oewn_dir):
    """Load every synset file into {synset_id: synset_obj}."""
    synsets = {}
    for path in glob.glob(os.path.join(oewn_dir, "*.json")):
        base = os.path.basename(path)
        if base.startswith("entries-") or base == "frames.json":
            continue  # entries handled separately; frames are syntactic, not lexical
        with open(path) as fh:
            synsets.update(json.load(fh))
    return synsets


def first_member(synsets, sid):
    """A representative lemma for a synset (its first member), lowercased; None if empty."""
    m = synsets.get(sid, {}).get("members") or []
    return m[0].lower() if m else None


def extract(synsets, seed=None):
    """Yield (subject, EdgeType, object, pos) triples from OEWN's explicit pointers,
    scoped to synsets whose members intersect `seed` (None = all)."""
    seed_lc = {s.lower() for s in seed} if seed is not None else None
    for sid, syn in synsets.items():
        members = [m.lower() for m in (syn.get("members") or [])]
        if not members:
            continue
        if seed_lc is not None and not (set(members) & seed_lc):
            continue
        pos = POS_NAME.get(syn.get("partOfSpeech", ""), "unknown")
        head = members[0]

        # SynonymOf — every co-member is a synonym of the head (any POS).
        for other in members[1:]:
            if other != head:
                yield (head, "SynonymOf", other, pos)

        # IsA — hypernym pointers (noun hypernymy / verb troponymy).
        if pos in ("noun", "verb"):
            for h in syn.get("hypernym", []):
                obj = first_member(synsets, h)
                if obj and obj != head:
                    yield (head, "IsA", obj, pos)

        # PartOf — meronyms ARE parts of this synset, so meronym PART-OF head (noun-gated).
        if pos == "noun":
            for key in MERONYM_KEYS:
                for part in syn.get(key, []):
                    obj = first_member(synsets, part)
                    if obj and obj != head:
                        yield (obj, "PartOf", head, pos)


def to_wordset(triples, synsets, seed=None):
    """Re-shape triples into the enriched wordset schema aden's producer reads: per lemma,
    a list of meanings carrying explicit synonyms/hypernyms/meronyms (not just a gloss)."""
    out = {}
    # Index a gloss per seed lemma for context (first synset's definition).
    for subj, edge, obj, pos in triples:
        ent = out.setdefault(subj, {"word": subj, "meanings": []})
        # Collapse onto a single (pos) meaning bucket for compactness.
        meaning = next((m for m in ent["meanings"] if m["speech_part"] == pos), None)
        if meaning is None:
            meaning = {"speech_part": pos, "def": "", "synonyms": [],
                       "hypernyms": [], "meronyms": []}
            ent["meanings"].append(meaning)
        bucket = {"SynonymOf": "synonyms", "IsA": "hypernyms", "PartOf": "meronyms"}[edge]
        if obj not in meaning[bucket]:
            meaning[bucket].append(obj)
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--oewn", default=os.path.expanduser("~/.cache/aden/dict/oewn"))
    ap.add_argument("--seed-file", help="one lemma per line (e.g. aden index vocab)")
    ap.add_argument("--all", action="store_true", help="convert the whole dictionary")
    ap.add_argument("--out", default=os.path.expanduser("~/.cache/aden/dict/oewn-triples.json"))
    ap.add_argument("--preview", type=int, default=40)
    args = ap.parse_args()

    if args.all:
        seed = None
    elif args.seed_file:
        with open(args.seed_file) as fh:
            seed = [ln.strip() for ln in fh if ln.strip()]
    else:
        seed = CODE_SEED

    synsets = load_synsets(args.oewn)
    triples = list(extract(synsets, seed))

    counts = {}
    for _, edge, _, _ in triples:
        counts[edge] = counts.get(edge, 0) + 1

    print(f"OEWN synsets loaded : {len(synsets)}")
    print(f"Scope               : {'ALL' if seed is None else f'{len(seed)} seed lemmas'}")
    print(f"Triples extracted   : {len(triples)}  {counts}")
    print(f"\n-- preview (first {args.preview}, explicit OEWN pointers -> aden EdgeType) --")
    for subj, edge, obj, pos in triples[: args.preview]:
        print(f"    {subj:<14} --{edge:<9}--> {obj:<16} [{pos}]")

    wordset = to_wordset(triples, synsets, seed)
    os.makedirs(os.path.dirname(args.out), exist_ok=True)
    with open(args.out, "w") as fh:
        json.dump(wordset, fh, indent=1, sort_keys=True)
    # Flat [subject, edge, object, pos] list — the unambiguous form aden's Rust
    # grounding/dry-run harness consumes (the wordset shape is for the producer).
    flat_out = args.out.replace(".json", "-flat.json")
    if flat_out == args.out:
        flat_out = args.out + ".flat.json"
    with open(flat_out, "w") as fh:
        json.dump([list(t) for t in triples], fh, indent=0)
    print(f"\nWrote enriched wordset JSON ({len(wordset)} lemmas) -> {args.out}")
    print(f"Wrote flat triples ({len(triples)}) -> {flat_out}")
    print("DRY RUN: nothing written to .aden/store.")


if __name__ == "__main__":
    main()
