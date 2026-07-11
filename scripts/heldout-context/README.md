# Historical context validation

This matrix was originally held out, but its failures have since been inspected
and used during implementation. It is now a frozen regression suite, not blind
evaluation evidence. Integrity is enforced by `scripts/regression-lock.json`.

This suite keeps calibration tasks separate from breadth validation. The manifest
covers unfamiliar Python, Go, Rust, and AsciiDoc repositories using previously
spot-checked labels from `scripts/eval-sets/`.

Run native Aden search and `ask` routing:

```bash
python3 scripts/heldout_context_bench.py \
  --json /tmp/aden-heldout.json
```

Run the conventional recursive-text baseline for one corpus:

```bash
python3 scripts/conventional_nav_bench.py \
  --repo ~/Projects/eval-repos/flask \
  --queries scripts/eval-sets/flask.queries.tsv \
  --json /tmp/flask-conventional.json
```

Use `scripts/candidate_rerank_bench.py` only for routing experiments. Its current
candidate-body policy is a recorded negative control and must not be interpreted
as an accepted product strategy.
