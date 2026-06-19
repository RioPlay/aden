#!/usr/bin/env python3
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Fetch English Wikipedia article intros as a neutral external PROSE corpus for the
# dictionary-on-prose ablation (crates/aden-cli/tests/prose_lexicon_ab.rs). Eval-only:
# the corpus is used for local measurement, never shipped. Wikipedia text is CC-BY-SA;
# this writes it to a per-user cache dir, not into the repo or any shipped artifact.
#
# Uses prop=extracts with exlimit=max (the extracts API returns ONE extract per request
# unless exlimit is set) and exintro=1 (lead section only: concise, less synonym redundancy,
# so synonym-absence probes stay valid). Writes one plain-text file per article.
#
# Usage: python3 scripts/fetch_prose_corpus.py
import json
import os
import time
import urllib.parse
import urllib.request

OUT = os.path.expanduser("~/.cache/aden/prose-eval/corpus")
os.makedirs(OUT, exist_ok=True)

# Distinct single-concept topics, many with a clear dictionary synonym for probe construction.
TITLES = [
    "Earthquake", "Automobile", "Physician", "Ship", "Happiness", "Money", "Forest",
    "Disease", "Lawyer", "Flood", "Mountain", "Ocean", "Telescope", "Vaccine", "Gravity",
    "Democracy", "Inflation", "Volcano", "Tropical cyclone", "Antibiotic", "Recession",
    "Language", "Soldier", "Prison", "Theft", "Fire", "Dog", "Cat", "House", "River",
    "Bird", "Tree", "King", "War", "Music", "Painting", "Book", "School", "Hospital",
    "Bridge", "Clock", "Mirror", "Knife", "Boat", "Island", "Desert", "Rain", "Snow",
    "Wind", "Lightning", "Earthworm", "Glacier", "Volcanic eruption", "Poison", "Wealth",
    "Anger", "Friendship", "Doctor", "Robbery", "Famine",
]

API = "https://en.wikipedia.org/w/api.php"
written = 0
for i in range(0, len(TITLES), 10):
    batch = TITLES[i:i + 10]
    params = {
        "action": "query", "format": "json", "prop": "extracts",
        "explaintext": "1", "exsectionformat": "plain", "redirects": "1",
        "exlimit": "max", "exintro": "1",
        "titles": "|".join(batch),
    }
    url = API + "?" + urllib.parse.urlencode(params)
    req = urllib.request.Request(url, headers={"User-Agent": "aden-prose-eval/0.1"})
    data = json.load(urllib.request.urlopen(req, timeout=60))
    for page in data.get("query", {}).get("pages", {}).values():
        extract = page.get("extract", "")
        if not extract or len(extract) < 150:
            continue
        slug = page.get("title", "").lower().replace(" ", "_").replace("/", "_")
        with open(os.path.join(OUT, f"{slug}.txt"), "w") as fh:
            fh.write(extract)
        written += 1
    time.sleep(2.0)  # be polite to the API (avoid HTTP 429)

print(f"wrote {written} prose articles to {OUT}")
