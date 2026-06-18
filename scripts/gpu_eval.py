#!/usr/bin/env python3
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# GPU sidecar for the doc-side retrieval research. Runs the SAME bge-small-en-v1.5 ONNX model
# aden uses, but on CUDA via onnxruntime-gpu, so a full lever pass is seconds instead of minutes.
# It reproduces the Rust harness recipe exactly (CLS pooling + L2 normalize, no query prefix;
# depth-2 BFS over the dumped graph; 120-char neighbour gists) so the numbers are comparable to
# the committed CPU results (DENSE ~0.281, CTX-D2 ~0.312, ORACLE ~0.443).
#
# Inputs: ~/.cache/aden/dict/eval/{cards,edges,probes}.json (from `dump_corpus` harness).
# Run:    ~/.cache/aden/gpu-venv/bin/python scripts/gpu_eval.py

import json
from pathlib import Path
import numpy as np
import onnxruntime as ort
from tokenizers import Tokenizer

EVAL = Path.home() / ".cache/aden/dict/eval"
MODEL = Path.home() / ".cache/aden-models/bge-small-en-v1.5"

cards = json.loads((EVAL / "cards.json").read_text())
edges = json.loads((EVAL / "edges.json").read_text())
probes = json.loads((EVAL / "probes.json").read_text())

anchors = [c[0] for c in cards]
texts = [c[1] for c in cards]
idx = {a: i for i, a in enumerate(anchors)}
n = len(cards)

out_nbrs = [[] for _ in range(n)]
in_nbrs = [[] for _ in range(n)]
for s, t, _et in edges:
    if s in idx and t in idx and idx[s] != idx[t]:
        out_nbrs[idx[s]].append(idx[t])
        in_nbrs[idx[t]].append(idx[s])

tok = Tokenizer.from_file(str(MODEL / "tokenizer.json"))
tok.enable_truncation(max_length=512)
tok.enable_padding()
sess = ort.InferenceSession(
    str(MODEL / "model.onnx"),
    providers=["CUDAExecutionProvider", "CPUExecutionProvider"],
)
print("provider:", sess.get_providers()[0])
in_names = {i.name for i in sess.get_inputs()}


def embed(items, batch=128):
    vecs = []
    for b in range(0, len(items), batch):
        encs = tok.encode_batch(items[b : b + batch])
        ids = np.array([e.ids for e in encs], dtype=np.int64)
        mask = np.array([e.attention_mask for e in encs], dtype=np.int64)
        feed = {}
        if "input_ids" in in_names:
            feed["input_ids"] = ids
        if "attention_mask" in in_names:
            feed["attention_mask"] = mask
        if "token_type_ids" in in_names:
            feed["token_type_ids"] = np.zeros_like(ids)
        out = sess.run(None, feed)[0]
        cls = out[:, 0, :] if out.ndim == 3 else out
        norm = np.linalg.norm(cls, axis=1, keepdims=True)
        norm[norm == 0] = 1e-9
        vecs.append((cls / norm).astype(np.float32))
    return np.vstack(vecs)


def head(t, hd):
    return " ".join(t.split())[:hd]


def nbr_indices(i, depth, per):
    seen = {i}
    frontier = [i]
    result = []
    for _ in range(depth):
        nxt = []
        for x in frontier:
            c = 0
            for j in out_nbrs[x] + in_nbrs[x]:
                if c >= per:
                    break
                if j not in seen:
                    seen.add(j)
                    result.append(j)
                    nxt.append(j)
                    c += 1
        if not nxt:
            break
        frontier = nxt
    return result


def build(depth, per, hd):
    res = []
    for i in range(n):
        s = texts[i]
        for j in nbr_indices(i, depth, per):
            s += " " + head(texts[j], hd)
        res.append(s)
    return res


def eval_bank(bank, qmat):
    r1 = r5 = 0
    mrr = 0.0
    for k, (_q, accept, _e) in enumerate(probes):
        order = np.argsort(-(bank @ qmat[k]))
        rank = None
        for rr, i in enumerate(order, 1):
            if any(a in anchors[i] for a in accept):
                rank = rr
                break
        if rank:
            r1 += rank == 1
            r5 += rank <= 5
            mrr += 1.0 / rank
    m = len(probes)
    return f"R@1 {r1:>2}/{m}  R@5 {r5:>2}/{m}  MRR {mrr / m:.3f}"


qv = embed([p[0] for p in probes])
ov = embed([f"{p[0]} {p[2]}" for p in probes])

# Sweep the embedding sequence length. aden's tract path is fixed at 128 tokens; 512 is the
# model's real limit. maxlen=128 should reproduce the Rust harness (DENSE ~0.281, CTX-D2 ~0.312)
# and validate this sidecar; 512 measures what aden leaves on the floor by truncating.
ctx_text = build(2, 12, 120)
for maxlen in (128, 512):
    tok.enable_truncation(max_length=maxlen)
    plain = embed(texts)
    ctx_d2 = embed(ctx_text)
    print(f"\n=== maxlen={maxlen} ({len(probes)} probes, {n} cards) ===")
    print(f"  DENSE     {eval_bank(plain, qv)}")
    print(f"  CTX-D2    {eval_bank(ctx_d2, qv)}")
    print(f"  ORACLE    {eval_bank(plain, ov)}")
