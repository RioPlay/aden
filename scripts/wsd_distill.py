#!/usr/bin/env python3
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Embedding-WSD distiller (OFFLINE; writes a forked edge set, never .aden/store).
#
# The dictionary's lemma-level edges flatten every SENSE of a polysemous word, so a
# lemma-string grounding keeps wrong-sense noise (edge->sharpness, vector->virus). This
# distiller fixes the ROOT cause: for each polysemous corpus lemma it picks the synset
# whose gloss best matches the lemma's real CORPUS usage (bge cosine), gated by the
# top-1-vs-top-2 margin (our separation gate), and keeps ONLY that sense's edges.
#
# Output: the clean, sense-disambiguated edge set + separation stats. Pure measurement.
import json, os, glob, sys, re
import numpy as np
import onnxruntime as ort
from tokenizers import Tokenizer

H = os.path.expanduser
OEWN = H("~/.cache/aden/dict/oewn")
CARDS = H("~/.cache/aden/dict/eval/cards.json")
MODEL = H("~/.cache/aden-models/bge-small-en-v1.5")
SLICES = ["noun.cognition","noun.communication","noun.artifact","verb.change","verb.contact","verb.cognition"]
MARGIN_GATE = 0.04  # min top1-top2 cosine gap to call a sense "confident" (else ambiguous)

tok = Tokenizer.from_file(f"{MODEL}/tokenizer.json")
sess = ort.InferenceSession(f"{MODEL}/model.onnx", providers=["CPUExecutionProvider"])

def embed(texts):
    out = []
    for t in texts:
        e = tok.encode(t or " "); ids = e.ids[:256]; mask = e.attention_mask[:256]
        o = sess.run(None, {"input_ids":[ids],"attention_mask":[mask],"token_type_ids":[[0]*len(ids)]})[0]
        v = o[0,0]; out.append(v/(np.linalg.norm(v)+1e-9))
    return out

# --- OEWN slice: synset structure ---
sid_members, lemma_syns = {}, {}      # sid -> members ; lemma -> [(sid, gloss, hypernym_sids)]
for lex in SLICES:
    f = f"{OEWN}/{lex}.json"
    if not os.path.exists(f): continue
    for sid, info in json.load(open(f)).items():
        mem = [m for m in info.get("members",[]) if isinstance(m,str)]
        sid_members[sid] = mem
        defn = " ".join(x for x in info.get("definition",[]) if isinstance(x,str))
        hyp = [h for h in info.get("hypernym",[]) if isinstance(h,str)]
        for m in mem:
            lemma_syns.setdefault(m, []).append((sid, defn, hyp))

# --- corpus contexts: lemma -> concatenated card text where it appears ---
cards = json.load(open(CARDS))
def card_text(c):
    parts = []
    def walk(x):
        if isinstance(x, str): parts.append(x)
        elif isinstance(x, dict):
            for v in x.values(): walk(v)
        elif isinstance(x, list):
            for v in x: walk(v)
    walk(c)
    return " ".join(parts)
corpus_ctx = {}
texts = [card_text(c).lower() for c in cards]
for lemma in lemma_syns:
    pat = re.compile(r"\b"+re.escape(lemma.lower())+r"\b")
    hits = [t for t in texts if pat.search(t)]
    if hits:
        corpus_ctx[lemma] = " ".join(hits[:8])[:4000]

# --- disambiguate polysemous corpus lemmas ---
poly = [l for l in corpus_ctx if len(lemma_syns[l]) >= 2]
print(f"slice synsets: {len(sid_members)} | corpus lemmas in slice: {len(corpus_ctx)} | polysemous: {len(poly)}")

LOW_MATCH = 0.50  # if even the BEST sense scores below this, the context matched no gloss well

clean_edges = []
confident = 0
rej_lowmatch, rej_ambiguous = 0, 0
acc_s, low_s, amb_s = [], [], []
margins_all = []
for lemma in poly:
    cands = lemma_syns[lemma]
    ctx_v = embed([corpus_ctx[lemma]])[0]
    gloss_vs = embed([g for _, g, _ in cands])
    sims = sorted([(float(ctx_v @ gv), i) for i, gv in enumerate(gloss_vs)], reverse=True)
    top = sims[0][0]
    second = sims[1][0] if len(sims) > 1 else 0.0
    margin = top - second
    margins_all.append(margin)
    sid, gloss, hyps = cands[sims[0][1]]
    ctx_snip = " ".join(corpus_ctx[lemma].split())[:85]
    top3 = [(round(s, 3), cands[i][1][:46]) for s, i in sims[:3]]
    # REASON 1: context matched no sense well -> diffuse / off-domain context
    if top < LOW_MATCH:
        rej_lowmatch += 1
        if len(low_s) < 6: low_s.append((lemma, top, ctx_snip, top3))
        continue
    # REASON 2: good match but two senses too close -> genuine polysemy, can't pick
    if margin < MARGIN_GATE:
        rej_ambiguous += 1
        if len(amb_s) < 6: amb_s.append((lemma, top, margin, ctx_snip, top3))
        continue
    confident += 1
    for co in sid_members.get(sid, []):
        if co != lemma: clean_edges.append((lemma, "SynonymOf", co))
    for h in hyps:
        for hl in sid_members.get(h, []): clean_edges.append((lemma, "IsA", hl))
    if len(acc_s) < 8: acc_s.append((lemma, top, margin, gloss[:50]))

out = H("~/.cache/aden/dict/wsd-clean-edges.json")
json.dump(clean_edges, open(out, "w"))
med = sorted(margins_all)[len(margins_all)//2] if margins_all else 0
print(f"\npolysemous {len(poly)} | confident {confident} | REJECTED: low-match {rej_lowmatch}, ambiguous {rej_ambiguous} | clean edges {len(clean_edges)}")
print(f"median margin over all polysemous lemmas: {med:.3f}  (cheap-test 'vector' margin was 0.34)")
print(f"\n>>> WHY REJECTED #1 LOW-MATCH (top cosine < {LOW_MATCH}: corpus context too diffuse to match ANY gloss):")
for l, top, ctx, t3 in low_s:
    print(f"  {l:13} bestcos={top:.3f}  ctx=\"{ctx}\"")
    for s, g in t3: print(f"        {s:.3f}  {g}")
print(f"\n>>> WHY REJECTED #2 AMBIGUOUS (good match, margin < {MARGIN_GATE}: two senses tie, genuine polysemy):")
for l, top, m, ctx, t3 in amb_s:
    print(f"  {l:13} bestcos={top:.3f} margin={m:.3f}  ctx=\"{ctx}\"")
    for s, g in t3: print(f"        {s:.3f}  {g}")
print(f"\n>>> ACCEPTED (confident) sample:")
for l, top, m, g in acc_s:
    print(f"  {l:13} cos={top:.3f} margin={m:.3f}  -> {g}")
