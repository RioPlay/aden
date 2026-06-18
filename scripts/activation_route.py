#!/usr/bin/env python3
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Contextual-activation router (OFFLINE measurement; nothing pruned, nothing written to store).
#
# The full English relational graph is KEPT whole. We do NOT distill senses globally. Instead,
# per QUERY (the entrypoint), we ACTIVATE the sense of each query lemma whose gloss best matches
# THIS query's embedding, and expand with that lit sense's synonyms. Every other sense stays in
# the graph, dormant, for a different entrypoint. This is routing-as-activation, not pruning.
#
# Arms (R@1 over 40 vocab-mismatch probes, BM25 retrieval where the synonym bridge matters):
#   BM25                  baseline
#   BM25 + ALL-SENSE      expand with synonyms from EVERY sense (the +1/12 uniform-expansion frame)
#   BM25 + ACTIVATED      expand only with the sense the QUERY lights up (per-entrypoint routing)
import json, os, re, math, numpy as np, onnxruntime as ort
from collections import defaultdict
from tokenizers import Tokenizer

H = os.path.expanduser
OEWN = H("~/.cache/aden/dict/oewn"); MODEL = H("~/.cache/aden-models/bge-small-en-v1.5")
probes = json.load(open(H("~/.cache/aden/dict/eval/probes.json")))
cards = json.load(open(H("~/.cache/aden/dict/eval/cards.json")))

tok = Tokenizer.from_file(f"{MODEL}/tokenizer.json")
sess = ort.InferenceSession(f"{MODEL}/model.onnx", providers=["CPUExecutionProvider"])
def emb(t):
    e=tok.encode(t or " "); ids=e.ids[:128]; m=e.attention_mask[:128]
    o=sess.run(None,{"input_ids":[ids],"attention_mask":[m],"token_type_ids":[[0]*len(ids)]})[0]
    v=o[0,0]; return v/(np.linalg.norm(v)+1e-9)

def toks(s): return re.findall(r"[a-z][a-z]+", s.lower())

# --- corpus + BM25 ---
anchors=[c[0] for c in cards]; docs=[toks(c[1]) for c in cards]
N=len(docs); df=defaultdict(int)
for d in docs:
    for w in set(d): df[w]+=1
idf={w: math.log(1+(N-n+0.5)/(n+0.5)) for w,n in df.items()}
avgdl=sum(len(d) for d in docs)/N
tf=[defaultdict(int) for _ in docs]
for i,d in enumerate(docs):
    for w in d: tf[i][w]+=1
corpus_vocab=set(df)
def bm25(qterms, weights=None):
    k1,b=1.5,0.75; sc=np.zeros(N)
    for w in qterms:
        if w not in idf: continue
        wt=(weights or {}).get(w,1.0)
        for i in range(N):
            f=tf[i].get(w,0)
            if f: sc[i]+=wt*idf[w]*(f*(k1+1))/(f+k1*(1-b+b*len(docs[i])/avgdl))
    return sc

# --- full OEWN lemma -> senses (gloss + co-member synonyms); KEEP EVERYTHING ---
import glob
lemma_senses=defaultdict(list)
for f in glob.glob(f"{OEWN}/noun.*.json")+glob.glob(f"{OEWN}/verb.*.json")+glob.glob(f"{OEWN}/adj.*.json"):
    for sid,info in json.load(open(f)).items():
        mem=[m for m in info.get("members",[]) if isinstance(m,str)]
        d=" ".join(x for x in info.get("definition",[]) if isinstance(x,str))
        if not d: continue
        for m in mem:
            syns=[s.lower() for s in mem if s!=m]
            lemma_senses[m.lower()].append((d, syns))

gcache={}
def gemb(g):
    if g not in gcache: gcache[g]=emb(g)
    return gcache[g]

def rank_of(scores, golds):
    order=np.argsort(-scores)
    for r,i in enumerate(order):
        if any(g in anchors[i] for g in golds): return r+1
    return 9999

base_hits=allsense_hits=act_hits=0
samples=[]
for q,golds,_ in probes:
    qt=toks(q); gold=set(golds)
    base=rank_of(bm25(qt), gold)
    qv=emb(q)
    all_exp=[]; act_exp=[]; lit=[]
    for w in set(qt):
        senses=lemma_senses.get(w)
        if not senses or len(senses)<1: continue
        # ALL-SENSE: every sense's in-corpus synonyms (uniform, no routing)
        for d,syns in senses:
            all_exp+=[s for s in syns if s in corpus_vocab]
        # ACTIVATED: only the sense THIS query lights up (max gloss-cosine to query)
        if len(senses)>=1:
            best=max(senses,key=lambda ds: float(qv@gemb(ds[0])))
            ic=[s for s in best[1] if s in corpus_vocab]
            if ic: lit.append((w,best[0][:40],ic[:4]))
            act_exp+=ic
    alls=rank_of(bm25(qt+all_exp), gold)
    act=rank_of(bm25(qt+act_exp), gold)
    base_hits+=base==1; allsense_hits+=alls==1; act_hits+=act==1
    if len(samples)<10 and (act==1 and base!=1):
        samples.append((q[:46],base,alls,act,lit[:2]))

n=len(probes)
print(f"\nR@1 over {n} vocab-mismatch probes (full graph KEPT, routing = activation):")
print(f"  BM25 baseline          {base_hits}/{n}")
print(f"  BM25 + ALL-SENSE       {allsense_hits}/{n}   (uniform expansion, every sense fires)")
print(f"  BM25 + ACTIVATED       {act_hits}/{n}   (per-query lit sense only)")
print(f"\nprobes ACTIVATION fixed (base miss -> activated hit):")
for q,b,a,ac,lit in samples:
    print(f"  '{q}'  base#{b} all#{a} act#{ac}")
    for w,g,ic in lit: print(f"        lit: {w} -> [{g}] -> {ic}")
