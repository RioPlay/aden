# research/

Third-party reference material that aden **parses and queries** — never code
that aden compiles or links. Kept here, segregated from the AGPL-3.0 source
tree, so each item's own license stays clean and unmixed.

## Legal posture

- Nothing in this folder is `include_str!`/`include_bytes!`-embedded into any
  aden binary. (Verified: the only `include_str!` of a `research.adoc` in the
  codebase is aden's own generic `.agent/templates/research.adoc`, which
  contains no third-party content.)
- Each corpus carries its own source citation and license. See
  `secure-coding/SOURCES.md`.
- Where third-party standards are *named* in aden's own AGPL docs/CLI (e.g.
  "OWASP-style security audit"), that is a referential mention of the standard,
  not a reproduction of its text — which is fine.

## Contents

- `secure-coding/` — a cross-linked knowledge base of secure-coding guidance
  (OWASP Secure Coding Practices, OWASP Top 10:2025, MITRE CWE Top 25), built as
  a dogfood of aden's "turn any docs into a queryable graph" capability. License
  details and full citations in `secure-coding/SOURCES.md`. The design for
  promoting this into a built-in standard is in `../docs/adr-002-secure-coding-standard.adoc`.
