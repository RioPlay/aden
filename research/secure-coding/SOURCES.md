# Sources & Licensing — Secure Coding Knowledge Base

This folder is a **third-party reference corpus**, NOT part of aden's source
code. It is kept separate from the AGPL-3.0 codebase precisely so its source
licenses are not mixed with aden's. aden *parses and queries* these documents;
it does not compile or link them.

## Provenance

The documents here are paraphrases/summaries assembled from the authoritative
public sources below, restructured with `[[anchors]]` and `<<refs>>` so aden can
build a relationship graph. Where text closely follows a source, that source's
license governs it (see per-document headers).

| Document | Primary source | Source license |
|---|---|---|
| `scp-checklist.adoc` | OWASP Secure Coding Practices — Quick Reference Guide | CC BY-SA 4.0 |
| `owasp-top10-2025.adoc` | OWASP Top 10:2025 (A05 Injection) | CC BY 3.0 (attribution only) |
| `cwe-top25.adoc` | 2024 CWE Top 25 Most Dangerous Software Weaknesses (MITRE/CISA) | MITRE CWE Terms of Use |
| `aden-audit-map.adoc` | Original aden work (maps the above onto aden's own audit) | aden-authored (AGPL-3.0) |
| `index.adoc` | Original (navigation hub) | aden-authored (AGPL-3.0) |

Note the two OWASP works carry **different** licenses: the Secure Coding
Practices guide is CC BY-SA 4.0 (ShareAlike); the Top 10 is CC BY 3.0
(attribution only, no ShareAlike) — each verified against its own project
page / license footer (OWASP SCP page states CC BY-SA 4.0; OWASP Top 10 footer
states CC BY 3.0).

## Citations

- **OWASP Secure Coding Practices — Quick Reference Guide.** The OWASP
  Foundation. https://owasp.org/www-project-secure-coding-practices-quick-reference-guide/
  Licensed under Creative Commons Attribution-ShareAlike 4.0
  (https://creativecommons.org/licenses/by-sa/4.0/).
- **OWASP Top 10:2025.** The OWASP Foundation.
  https://owasp.org/Top10/2025/A05_2025-Injection/  "This work is licensed under
  a Creative Commons Attribution 3.0 Unported License." (CC BY 3.0,
  https://creativecommons.org/licenses/by/3.0/).
- **2024 CWE Top 25 Most Dangerous Software Weaknesses.** The MITRE Corporation /
  CISA. https://cwe.mitre.org/top25/archive/2024/2024_cwe_top25.html  Used under
  the CWE Terms of Use (https://cwe.mitre.org/about/termsofuse.html): "The MITRE
  Corporation hereby grants you a non-exclusive, royalty-free license to use CWE
  for research, development, and commercial purposes."

## Required notices (reproduced verbatim per the source terms)

> Copyright © 2006–2026, The MITRE Corporation.
>
> CWE, CWSS, CWRAF, and the CWE logo are trademarks of The MITRE Corporation.

## Trademark & non-endorsement

CWE™ is a trademark of The MITRE Corporation; "OWASP" and the OWASP logo are
trademarks of the OWASP Foundation. These marks are used here **nominatively** —
to identify the standards this corpus summarizes. Neither The MITRE Corporation
nor the OWASP Foundation endorses, sponsors, or is affiliated with aden.
References in aden's own documentation/CLI to an "OWASP-aligned" or
"OWASP-Top-10-informed" audit describe aden's own functionality and do not imply
affiliation.

## License of this corpus

Because `scp-checklist.adoc` closely follows CC BY-SA 4.0 material, that
document (and any redistribution of this corpus that includes it) is licensed
under **CC BY-SA 4.0**, NOT aden's AGPL-3.0. `owasp-top10-2025.adoc` follows
CC BY 3.0 material (attribution only). The `cwe-top25.adoc` entries reproduce
CWE identifiers/names under MITRE's terms with the notices above. `index.adoc`
and `aden-audit-map.adoc` are aden-authored.

Keep this segregation intact: do not copy CC BY-SA / CC BY / CWE text into
AGPL-licensed source files, and do not `include_str!`/`include_bytes!` these
documents into any aden binary.
