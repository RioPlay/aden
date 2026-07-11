// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::HashSet;
use std::fmt::Write;

/// Find all `<<reference>>` patterns in a line and return the inner reference texts.
/// Ignores anything inside backticks (`) to avoid flagging literal examples.
pub fn find_refs(line: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut in_backticks = false;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'`' {
            in_backticks = !in_backticks;
            i += 1;
            continue;
        }
        if in_backticks {
            i += 1;
            continue;
        }
        if c == b'<'
            && i + 1 < bytes.len()
            && bytes[i + 1] == b'<'
            && let Some(end) = line[i + 2..].find(">>")
        {
            let abs_end = i + 2 + end;
            let inner = &line[i + 2..abs_end];
            let anchor_name = inner.split(',').next().unwrap_or(inner).trim();
            if !anchor_name.is_empty() && !anchor_name.contains(' ') {
                refs.push(anchor_name.to_string());
            }
            i = abs_end + 2;
            continue;
        }
        i += 1;
    }
    refs
}

/// True when `line` toggles a delimited listing (`----`) or literal (`....`)
/// block. `trim_end` so a CRLF checkout (`----\r`) still matches the fence.
fn is_block_fence(line: &str) -> bool {
    let t = line.trim_end();
    t == "----" || t == "...."
}

/// Find all `[[anchor]]` declarations in a single line and return the anchor
/// ids. Anchors are INLINE in real-world AsciiDoc (`[[_term]]Term::` in a
/// description list) and several may share a line, so this scans the whole
/// line instead of requiring the brackets to span it. Backtick-quoted
/// occurrences (`` `[[x]]` ``) are literal examples and are skipped — the same
/// rule [`find_refs`] applies to `<<x>>`. An explicit xreflabel
/// (`[[id,Label]]`) declares only the id before the comma.
fn find_anchor_decls(line: &str, out: &mut HashSet<String>) {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut in_backticks = false;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'`' {
            in_backticks = !in_backticks;
            i += 1;
            continue;
        }
        if in_backticks {
            i += 1;
            continue;
        }
        if c == b'['
            && i + 1 < bytes.len()
            && bytes[i + 1] == b'['
            && let Some(end) = line[i + 2..].find("]]")
        {
            let abs_end = i + 2 + end;
            let inner = &line[i + 2..abs_end];
            let id = inner.split(',').next().unwrap_or(inner).trim();
            if !id.is_empty() && !id.contains(' ') {
                out.insert(id.to_string());
            }
            i = abs_end + 2;
            continue;
        }
        i += 1;
    }
}

/// Collect all `[[anchor]]` declarations from emitted/source text. Tracks
/// delimited listing/literal blocks (`----` / `....`) across lines so a code
/// example *showing* `[[x]]` does not register as a declaration.
pub fn collect_anchors(output: &str) -> HashSet<String> {
    let mut anchors = HashSet::new();
    let mut in_block = false;
    for line in output.lines() {
        if is_block_fence(line) {
            in_block = !in_block;
            continue;
        }
        if in_block {
            continue;
        }
        find_anchor_decls(line, &mut anchors);
    }
    anchors
}

/// Collect all `<<ref>>` targets from text, skipping delimited listing/literal
/// blocks (`----` / `....`) — the multi-line mirror of [`find_refs`], which is
/// line-scoped and cannot know fence state. A `<<x>>` inside such a block is a
/// code example, not a cross-reference to verify.
pub fn collect_refs(text: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let mut in_block = false;
    for line in text.lines() {
        if is_block_fence(line) {
            in_block = !in_block;
            continue;
        }
        if in_block {
            continue;
        }
        refs.extend(find_refs(line));
    }
    refs
}

/// Verify that `output` contains no unresolved `<<refs>>`.
pub fn verify(output: &str) -> Result<(), String> {
    let anchors = collect_anchors(output);
    let mut issues = Vec::new();
    for r in collect_refs(output) {
        if !anchors.contains(&r) {
            issues.push(format!("Unresolved reference: <<{r}>>"));
        }
    }
    if issues.is_empty() {
        Ok(())
    } else {
        let mut msg = String::new();
        for issue in &issues {
            writeln!(msg, "{issue}").unwrap();
        }
        Err(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // DEFECT (rioplay.dev, 58 false ERRORs): real-world AsciiDoc declares
    // anchors INLINE — `[[_context_assembly]]Context assembly::` — but
    // collect_anchors only matched a line that was nothing but `[[x]]`.
    #[test]
    fn collect_anchors_finds_inline_and_multiple_per_line() {
        let text = "\
[[_context_assembly]]Context assembly::
The body of the term.

[[a]] then [[b]] on one line
[[standalone]]
";
        let anchors = collect_anchors(text);
        for a in ["_context_assembly", "a", "b", "standalone"] {
            assert!(anchors.contains(a), "missing anchor {a:?}; got {anchors:?}");
        }
    }

    // `[[x]]` shown literally — inside a ----/.... delimited block or inside
    // backticks — is an EXAMPLE, not a declaration.
    #[test]
    fn collect_anchors_skips_listing_blocks_and_backticks() {
        let text = "\
[[real]]Real anchor::
Some prose with `[[in_ticks]]` quoted literally.

----
[[in_listing]]
----

....
[[in_literal]]
....

[[after_blocks]]
";
        let anchors = collect_anchors(text);
        assert!(anchors.contains("real"), "got {anchors:?}");
        assert!(anchors.contains("after_blocks"), "got {anchors:?}");
        for bogus in ["in_ticks", "in_listing", "in_literal"] {
            assert!(
                !anchors.contains(bogus),
                "{bogus:?} is a literal example, not a declaration; got {anchors:?}"
            );
        }
    }

    // An anchor with an explicit xreflabel — `[[id,Label Text]]` — declares `id`.
    #[test]
    fn collect_anchors_takes_id_before_comma() {
        let anchors = collect_anchors("[[_term,Pretty Label]]Term::\n");
        assert!(anchors.contains("_term"), "got {anchors:?}");
    }

    // Mirror of the fence rule on the REF side: a `<<x>>` shown inside a
    // delimited listing/literal block is an example, not a reference to verify.
    #[test]
    fn collect_refs_skips_listing_blocks() {
        let text = "\
Prose ref <<real_target>> here.

----
literal <<fenced_example>> in code
----

....
literal <<literal_example>>
....
";
        let refs = collect_refs(text);
        assert!(refs.contains(&"real_target".to_string()), "got {refs:?}");
        assert!(
            !refs.iter().any(|r| r.contains("example")),
            "refs inside delimited blocks must be skipped; got {refs:?}"
        );
    }

    #[test]
    fn verify_accepts_inline_anchor_declarations() {
        let text = "\
[[_term]]Term::
Defined here.

See <<_term>> for details.
";
        assert!(verify(text).is_ok(), "inline [[_term]] declares the target");
    }
}
