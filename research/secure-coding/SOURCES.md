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
| `scp-checklist.adoc` | OWASP Secure Coding Practices — Quick Reference Guide | CC BY-SA 3.0 |
| `owasp-top10-2025.adoc` | OWASP Top 10:2025 (A05 Injection) | CC BY-SA 4.0 / 3.0 |
| `cwe-top25.adoc` | 2024 CWE Top 25 Most Dangerous Software Weaknesses (MITRE/CISA) | MITRE CWE Terms of Use |
| `aden-audit-map.adoc` | Original aden work (maps the above onto aden's own audit) | AGPL-3.0-compatible / aden-authored |
| `index.adoc` | Original (navigation hub) | aden-authored |

## Citations

- **OWASP Secure Coding Practices — Quick Reference Guide.** The OWASP
  Foundation. https://owasp.org/www-project-secure-coding-practices-quick-reference-guide/
  Licensed under Creative Commons Attribution-ShareAlike 3.0
  (https://creativecommons.org/licenses/by-sa/3.0/).
- **OWASP Top 10:2025.** The OWASP Foundation.
  https://owasp.org/Top10/2025/A05_2025-Injection/  Licensed under Creative
  Commons Attribution-ShareAlike.
- **2024 CWE Top 25 Most Dangerous Software Weaknesses.** The MITRE Corporation /
  CISA. https://cwe.mitre.org/top25/archive/2024/2024_cwe_top25.html
  CWE™ is © The MITRE Corporation. Used under the CWE Terms of Use
  (https://cwe.mitre.org/about/termsofuse.html): "The MITRE Corporation hereby
  grants you a non-exclusive, royalty-free license to use CWE for research,
  development, and commercial purposes." CWE is a trademark of The MITRE
  Corporation.

## License of this corpus

Because `scp-checklist.adoc` and `owasp-top10-2025.adoc` are derivative works of
CC BY-SA material, those documents (and any redistribution of this corpus that
includes them) are licensed under **CC BY-SA 3.0**, NOT aden's AGPL-3.0. The
`cwe-top25.adoc` entries reproduce CWE identifiers/names under MITRE's terms with
the attribution above. `index.adoc` and `aden-audit-map.adoc` are aden-authored.

Keep this segregation intact: do not copy CC BY-SA prose into AGPL-licensed
source files.
