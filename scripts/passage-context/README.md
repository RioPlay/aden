# Passage-level context benchmark

> **Regression-only:** this suite was repeatedly inspected and tuned during
> development. It is frozen by `scripts/regression-lock.json` and must not be
> described as held-out, blind, or independent evidence.

`tasks.json` contains sixteen source-grounded prose questions across seven
repository structures, including thick book chapters, topic documents, short
chronological logs, Markdown READMEs from Go and Rust projects, Flask RST, and a
generated plain-text Go API reference.
`../passage_context_bench.py` runs the real bounded
`aden ask --strict` product path with navigation disabled and automatic, scoring
fact recall, complete-task sufficiency, serialized context tokens, and latency.

Required-fact patterns are authored from source content before running retrieval.
They test whether the final context contains answer-bearing passages, not merely
the expected filename. Keep this suite small and reviewed; regex presence is a
deterministic lower-bound proxy for answerability, not semantic answer grading.

```bash
python3 scripts/passage_context_bench.py --budget 512 --json /tmp/passage.json
```

Measure the strict quality/cost frontier with bounded per-query execution:

```bash
timeout 300s python3 scripts/passage_budget_frontier.py --aden-bin aden --timeout 12
```
