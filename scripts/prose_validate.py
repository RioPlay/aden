#!/usr/bin/env python3
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Prose-side validation of the dict substrate (OFFLINE; nothing written to store).
#
# Independent general-English prose (varied topics), with synonym-mismatch probes: the gold
# passage uses one word, the query uses a real English SYNONYM of it (never the same word). If
# the English dict bridges the synonym, dict-activated expansion lifts R@1 where baseline misses.
# This is the home turf the dict was never tested on (code probes were the wrong domain).
import os, re, math, glob, json, numpy as np, onnxruntime as ort
from collections import defaultdict
from tokenizers import Tokenizer
H=os.path.expanduser; OEWN=H("~/.cache/aden/dict/oewn"); MODEL=H("~/.cache/aden-models/bge-small-en-v1.5")

# --- varied-topic prose corpus: each passage AVOIDS the probe's query word, uses a synonym ---
CORPUS = {
 "weather": "The storm will commence at dawn and the downpour is expected to be heavy across the valley.",
 "cooking": "Slice the onions thinly, then heat the oil in a large pan before adding the spices.",
 "health": "The physician examined the patient and prescribed rest and plenty of fluids for recovery.",
 "travel": "They will depart on a long voyage across the ocean, halting at several small islands.",
 "emotion": "She felt glad and content after the good news, her worries finally fading away.",
 "animal":  "The enormous whale is the largest creature in the sea, dwarfing every fish around it.",
 "money":   "He decided to purchase the old house despite its cost, paying the full amount in cash.",
 "work":    "The project will conclude next month once the team finishes the final report.",
 "speed":   "The rapid train hurtled past the station without slowing, a blur of silver metal.",
 "mind":    "A clever student grasps difficult ideas quickly and recalls them with ease.",
 "error":   "A single mistake in the calculation caused the whole experiment to fail.",
 "car":     "The automobile rolled smoothly down the highway, its engine humming quietly.",
 "build":   "Workers will construct a new bridge over the river to ease the daily traffic.",
 "talk":    "The two leaders will converse privately before addressing the gathered crowd.",
 "fear":    "A sudden dread gripped the climbers as the narrow ledge crumbled beneath them.",
 "light":   "The lamp emits a soft glow that illuminates the entire reading corner at night.",
 "begin2":  "The ceremony starts promptly, so guests should arrive early to find their seats.",
 "happy2":  "The cheerful children laughed and played in the park all afternoon.",
 "small":   "A tiny insect crawled along the leaf, almost invisible to the naked eye.",
 "clean":   "She will tidy the cluttered room and wipe every surface until it shines.",
}
# probe: NL query phrased with a SYNONYM of the gold passage's key word (mismatch)
PROBES = [
 ("when does the rain begin in the morning", "weather"),     # commence/begin
 ("chop the vegetables before frying", "cooking"),           # slice/chop
 ("the doctor treated the sick person", "health"),           # physician/doctor
 ("they will leave on a sea journey", "travel"),             # depart/leave, voyage/journey
 ("he was happy about the announcement", "emotion"),         # glad/happy
 ("the biggest animal in the ocean", "animal"),              # largest/biggest, creature/animal
 ("she chose to buy the property", "money"),                 # purchase/buy
 ("the task will end soon", "work"),                         # conclude/end
 ("a very fast moving train", "speed"),                      # rapid/fast
 ("an intelligent learner understands fast", "mind"),        # clever/intelligent
 ("one error ruined the test", "error"),                     # mistake/error
 ("the vehicle drove down the road", "car"),                 # automobile/vehicle
 ("they will make a new bridge", "build"),                   # construct/make
 ("the leaders will speak in private", "talk"),              # converse/speak
 ("a sudden fear seized the climbers", "fear"),              # dread/fear
 ("the lamp gives off a gentle shine", "light"),             # emits glow / gives shine
]

tok=Tokenizer.from_file(f"{MODEL}/tokenizer.json")
sess=ort.InferenceSession(f"{MODEL}/model.onnx",providers=["CPUExecutionProvider"])
def emb(t):
    e=tok.encode(t or " ");i=e.ids[:128];m=e.attention_mask[:128]
    o=sess.run(None,{"input_ids":[i],"attention_mask":[m],"token_type_ids":[[0]*len(i)]})[0]
    v=o[0,0];return v/(np.linalg.norm(v)+1e-9)
def toks(s): return re.findall(r"[a-z]+",s.lower())

ids=list(CORPUS); docs=[toks(CORPUS[k]) for k in ids]; N=len(docs)
df=defaultdict(int)
for d in docs:
    for w in set(d): df[w]+=1
idf={w:math.log(1+(N-n+0.5)/(n+0.5)) for w,n in df.items()}; avgdl=sum(map(len,docs))/N
vocab=set(df)
def bm25(q):
    sc=np.zeros(N)
    for w in q:
        if w not in idf: continue
        for i,d in enumerate(docs):
            f=d.count(w)
            if f: sc[i]+=idf[w]*(f*2.5)/(f+1.5*(0.25+0.75*len(d)/avgdl))
    return sc
def rank(q,gold):
    o=np.argsort(-bm25(q));
    return next((r+1 for r,i in enumerate(o) if ids[i]==gold),9999)

# full dict lemma->senses(gloss,synonyms) KEPT whole
ls=defaultdict(list)
for f in glob.glob(f"{OEWN}/noun.*.json")+glob.glob(f"{OEWN}/verb.*.json")+glob.glob(f"{OEWN}/adj.*.json"):
    for sid,info in json.load(open(f)).items():
        mem=[m.lower() for m in info.get("members",[]) if isinstance(m,str)]
        d=" ".join(x for x in info.get("definition",[]) if isinstance(x,str))
        if not d: continue
        for m in mem: ls[m].append((d,[s for s in mem if s!=m]))
gc={}
def ge(g):
    if g not in gc: gc[g]=emb(g)
    return gc[g]

base=act=0; fixed=[]
for q,gold in PROBES:
    qt=toks(q); b=rank(qt,gold)
    qv=emb(q); exp=[]; lit=[]
    for w in set(qt):
        ss=ls.get(w)
        if not ss: continue
        best=max(ss,key=lambda ds:float(qv@ge(ds[0])))   # activate THIS query's sense
        ic=[s for s in best[1] if s in vocab]
        if ic: lit.append((w,ic[:3])); exp+=ic
    a=rank(qt+exp,gold)
    base+=b==1; act+=a==1
    if a==1 and b!=1: fixed.append((q,lit))
print(f"\nPROSE general-English synonym-mismatch — R@1 over {len(PROBES)} probes:")
print(f"  BM25 baseline        {base}/{len(PROBES)}")
print(f"  BM25 + DICT-ACTIVATED {act}/{len(PROBES)}")
print(f"\nbridges the dict ACTIVATED (baseline miss -> hit):")
for q,lit in fixed:
    print(f"  '{q}'")
    for w,ic in lit[:3]: print(f"      {w} -> {ic}")
