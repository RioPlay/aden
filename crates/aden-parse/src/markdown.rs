// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Markdown extractor for Aden.
//!
//! Extracts headings, code blocks, and links from markdown files.

use crate::extractor::{
    build_code_attributes, extract_code_references, infer_project_name, infer_project_root,
    make_anchor, project_relative_file,
};
use aden_core::{Block, Document, NodeType, Result, SourceSpan};
use std::path::Path;

pub struct MarkdownExtractor;

impl Default for MarkdownExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl MarkdownExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl crate::extractor::LanguageExtractor for MarkdownExtractor {
    fn language_id(&self) -> &'static str {
        "markdown"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &["md", "markdown", "mdown", "mkd", "mkdn"]
    }

    fn extract_documents(&self, source: &str, path: &Path) -> Result<Vec<Document>> {
        let mut docs = Vec::new();
        let crate_name = infer_project_name(path);
        let project_root = infer_project_root(path);
        let file_name = project_relative_file(path, &project_root);

        let (frontmatter, body) = parse_frontmatter(source);
        let body_lines: Vec<&str> = body.lines().collect();
        let mut headings = Vec::new();
        let mut code_blocks = Vec::new();
        let mut links = Vec::new();
        // Fragment-bearing link targets, as (0-based line index, "ref:<frag>")
        // pairs — the markdown side of the prose cross-reference channel (the
        // AsciiDoc parser emits the same `ref:` form for `<<target>>`).
        let mut line_refs: Vec<(usize, String)> = Vec::new();
        // Backtick symbol mentions (Wave-2 `Mentions` channel), same shape and
        // same fence discipline as `line_refs`.
        let mut line_mentions: Vec<(usize, String)> = Vec::new();
        // Supersede-context refs (Wave-3 `Supersedes` channel): refs found on a
        // line with supersede language, as `(idx, "<by|of>:ref:<frag>")` — the
        // direction prefix tells the linker which side the enclosing doc is on.
        let mut line_supersedes: Vec<(usize, String)> = Vec::new();
        // Glossary bullet entries `(idx, name, def)` — Term-node candidates;
        // only those inside glossary-gated sections are promoted below.
        let mut term_lines: Vec<(usize, String, String)> = Vec::new();
        let mut in_code_block = false;
        let mut current_code_lang = String::new();
        let mut current_code_lines = Vec::new();
        // 1-based line of the fenced block's first body line, captured at the
        // opening ``` fence so the emitted node spans the real code.
        let mut current_code_start = 0usize;

        for (line_num, line) in body.lines().enumerate() {
            let line_num = line_num + 1;

            if line.starts_with("```") {
                if in_code_block {
                    code_blocks.push((
                        current_code_lang.clone(),
                        current_code_lines.join("\n"),
                        current_code_start,
                    ));
                    current_code_lines.clear();
                    current_code_lang.clear();
                    in_code_block = false;
                } else {
                    in_code_block = true;
                    current_code_lang = line.trim_start_matches("```").to_string();
                    current_code_start = line_num + 1;
                }
                continue;
            }

            if !in_code_block && let Some(link) = extract_markdown_link(line) {
                links.push(link);
            }

            if !in_code_block {
                let refs_before = line_refs.len();
                collect_fragment_refs(line, line_num - 1, &mut line_refs);
                if line_refs.len() > refs_before
                    && let Some(dir) = crate::extractor::supersede_direction(line)
                {
                    for (_, r) in &line_refs[refs_before..] {
                        line_supersedes.push((line_num - 1, format!("{dir}:{r}")));
                    }
                }
                crate::extractor::collect_backtick_mentions(line, line_num - 1, &mut line_mentions);
                if let Some((name, def)) = parse_glossary_bullet(line) {
                    term_lines.push((line_num - 1, name, def));
                }
            }

            if in_code_block {
                current_code_lines.push(line.to_string());
                continue;
            }

            if line.starts_with('#') {
                let level = line.chars().take_while(|c| *c == '#').count().min(6);
                let title = line[level..].trim().to_string();
                if !title.is_empty() {
                    headings.push((level, title, line_num));
                }
            }
        }

        // Term docs are appended AFTER the code-block loop so the historical
        // `code_block_{docs.len()}` numbering never shifts under existing stores.
        let mut term_docs: Vec<Document> = Vec::new();
        // Glossary gate, document level (same rule as the AsciiDoc parser).
        let is_glossary_doc = headings
            .iter()
            .find(|(level, ..)| *level == 1)
            .map(|(_, t, _)| crate::extractor::is_glossary_title(t))
            .unwrap_or_else(|| crate::extractor::is_glossary_title(&file_name));

        if !headings.is_empty() {
            for hi in 0..headings.len() {
                let (level, ref title, line_num) = headings[hi];
                let anchor = make_doc_anchor(&crate_name, &file_name, title, level);

                // Capture the section's prose so the node carries real content,
                // not just its title. The body runs from just after this heading
                // to the next heading.
                let body_start = line_num; // 0-based index of the line after the heading
                let body_end = headings[hi + 1..]
                    .iter()
                    .find(|h| h.2 > line_num)
                    .map(|h| h.2 - 1)
                    .unwrap_or(body_lines.len());
                let mut body_text = if body_start < body_end && body_end <= body_lines.len() {
                    body_lines[body_start..body_end]
                        .join("\n")
                        .trim()
                        .to_string()
                } else {
                    String::new()
                };

                // Surface markdown links as AsciiDoc-style cross-references so the
                // gen-time linker turns them into RelatesTo edges. The targets are
                // normalized down to bare anchor names.
                if !links.is_empty() {
                    let targets: Vec<String> = links
                        .iter()
                        .map(|l| {
                            let url = l.split_once("->").map(|(_, u)| u).unwrap_or(l.as_str());
                            normalize_link_target(url)
                        })
                        .filter(|t| !t.is_empty())
                        .map(|t| format!("<<{}>>", t))
                        .collect();
                    if !targets.is_empty() {
                        let refs_line = format!("See: {}", targets.join(", "));
                        if body_text.is_empty() {
                            body_text = refs_line;
                        } else {
                            body_text.push('\n');
                            body_text.push_str(&refs_line);
                        }
                    }
                }

                let span = SourceSpan {
                    file: path.to_string_lossy().to_string(),
                    start_line: line_num,
                    end_line: body_end.max(line_num),
                    start_byte: 0,
                    end_byte: 0,
                };
                let attrs = build_code_attributes(source, "heading", Some(path), Some(&span));
                let mut attrs = attrs;
                attrs.insert("heading_level".to_string(), level.to_string());
                if !links.is_empty() {
                    attrs.insert("links".to_string(), links.join(";"));
                }

                // Attribute fragment refs to this section (heading line + body).
                // The FIRST node also adopts any pre-heading prose refs so they
                // still become graph edges.
                let ref_start = if hi == 0 { 0 } else { line_num - 1 };
                let mut section_refs: Vec<String> = line_refs
                    .iter()
                    .filter(|(idx, _)| *idx >= ref_start && *idx < body_end)
                    .map(|(_, r)| r.clone())
                    .collect();
                section_refs.sort();
                section_refs.dedup();
                if !section_refs.is_empty() {
                    attrs.insert("doc_refs".to_string(), section_refs.join(","));
                }
                // Same attribution rule for prose mentions (Wave-2 Mentions).
                let mut section_mentions: Vec<String> = line_mentions
                    .iter()
                    .filter(|(idx, _)| *idx >= ref_start && *idx < body_end)
                    .map(|(_, m)| m.clone())
                    .collect();
                section_mentions.sort();
                section_mentions.dedup();
                if !section_mentions.is_empty() {
                    attrs.insert("doc_mentions".to_string(), section_mentions.join(","));
                }
                // Same attribution rule for supersede refs (Wave-3 Supersedes).
                let mut section_supersedes: Vec<String> = line_supersedes
                    .iter()
                    .filter(|(idx, _)| *idx >= ref_start && *idx < body_end)
                    .map(|(_, s)| s.clone())
                    .collect();
                section_supersedes.sort();
                section_supersedes.dedup();
                if !section_supersedes.is_empty() {
                    attrs.insert("doc_supersedes".to_string(), section_supersedes.join(","));
                }

                // Glossary gate: promote this section's `- **term**: def`
                // bullets to Term nodes (same rule as the AsciiDoc parser).
                if is_glossary_doc || crate::extractor::is_glossary_title(title) {
                    let mut term_anchors: Vec<String> = Vec::new();
                    for (_, name, def) in term_lines
                        .iter()
                        .filter(|(idx, ..)| *idx >= ref_start && *idx < body_end)
                    {
                        let slug = crate::extractor::term_slug(name);
                        if slug.is_empty() {
                            continue;
                        }
                        let entry = crate::extractor::GlossaryEntry {
                            name: name.clone(),
                            slug,
                            definition: def.clone(),
                        };
                        let term = crate::extractor::build_term_document(&crate_name, path, &entry);
                        term_anchors.push(term.anchor.clone());
                        term_docs.push(term);
                    }
                    term_anchors.sort();
                    term_anchors.dedup();
                    if !term_anchors.is_empty() {
                        attrs.insert("doc_terms".to_string(), term_anchors.join(","));
                    }
                }

                let mut blocks = vec![Block::Paragraph(title.clone())];
                if !body_text.is_empty() {
                    blocks.push(Block::Paragraph(body_text));
                }

                docs.push(Document {
                    anchor,
                    node_type: NodeType::Module,
                    attributes: attrs,
                    blocks,
                    source_span: Some(span),
                    metadata: frontmatter.clone(),
                    confidence: 0.9,
                });
            }
        } else {
            let anchor = make_anchor(&crate_name, &file_name, "document");
            let doc_span = crate::extractor::whole_file_span(source, path);
            let mut attrs = build_code_attributes(source, "document", Some(path), Some(&doc_span));
            // No headings: the whole file is one node — it owns every ref.
            let mut all_refs: Vec<String> = line_refs.iter().map(|(_, r)| r.clone()).collect();
            all_refs.sort();
            all_refs.dedup();
            if !all_refs.is_empty() {
                attrs.insert("doc_refs".to_string(), all_refs.join(","));
            }
            let mut all_mentions: Vec<String> =
                line_mentions.iter().map(|(_, m)| m.clone()).collect();
            all_mentions.sort();
            all_mentions.dedup();
            if !all_mentions.is_empty() {
                attrs.insert("doc_mentions".to_string(), all_mentions.join(","));
            }
            let mut all_supersedes: Vec<String> =
                line_supersedes.iter().map(|(_, s)| s.clone()).collect();
            all_supersedes.sort();
            all_supersedes.dedup();
            if !all_supersedes.is_empty() {
                attrs.insert("doc_supersedes".to_string(), all_supersedes.join(","));
            }
            docs.push(Document {
                anchor,
                node_type: NodeType::Module,
                attributes: attrs,
                blocks: vec![Block::Paragraph(format!(
                    "Markdown document: {}",
                    file_name
                ))],
                source_span: Some(doc_span),
                metadata: frontmatter,
                confidence: 0.7,
            });
        }

        for (lang, code, code_start) in code_blocks {
            let anchor = make_anchor(
                &crate_name,
                &file_name,
                &format!("code_block_{}", docs.len()),
            );
            let mut references = extract_code_references(&code, &lang);
            // Language-neutral call-shaped tokens (Wave-2 Demonstrates): the
            // per-language declaration scan above misses what a listing CALLS,
            // which is exactly what it demonstrates.
            references.extend(
                crate::extractor::listing_call_tokens(&code)
                    .into_iter()
                    .map(|t| format!("call:{t}")),
            );
            references.dedup();
            let code_span = SourceSpan {
                file: path.to_string_lossy().to_string(),
                start_line: code_start.max(1),
                end_line: (code_start + code.lines().count())
                    .saturating_sub(1)
                    .max(code_start),
                start_byte: 0,
                end_byte: 0,
            };
            let mut attrs = build_code_attributes(&code, "code", Some(path), Some(&code_span));
            if !references.is_empty() {
                attrs.insert("symbol_references".to_string(), references.join(","));
            }
            docs.push(Document {
                anchor,
                node_type: NodeType::Script,
                attributes: attrs,
                blocks: vec![Block::Listing {
                    language: if lang.is_empty() { None } else { Some(lang) },
                    code,
                }],
                source_span: Some(code_span),
                metadata: None,
                confidence: 0.8,
            });
        }

        docs.extend(term_docs);

        Ok(docs)
    }
}

/// Parse one markdown glossary bullet: `- **Term**: definition` (also `*`
/// bullets and an em/en dash separator). The bold marker is the term-ness
/// signal — plain bullets stay prose.
fn parse_glossary_bullet(line: &str) -> Option<(String, String)> {
    let rest = line
        .trim_start()
        .strip_prefix("- ")
        .or_else(|| line.trim_start().strip_prefix("* "))?;
    let rest = rest.trim_start().strip_prefix("**")?;
    let close = rest.find("**")?;
    let name = rest[..close].trim();
    if name.is_empty() || name.len() > 64 || name.contains('`') {
        return None;
    }
    let def = rest[close + 2..]
        .trim_start()
        .trim_start_matches([':', '—', '–', '-'])
        .trim();
    if def.is_empty() {
        return None;
    }
    Some((name.to_string(), def.to_string()))
}

fn parse_frontmatter(source: &str) -> (Option<aden_core::DocumentMetadata>, &str) {
    if !source.starts_with("---") {
        return (None, source);
    }

    let after_first = source[3..].trim_start();
    if !after_first.starts_with('\n') {
        return (None, source);
    }

    let end = after_first.find("\n---").unwrap_or(after_first.len());
    let fm_text = &after_first[..end];

    let mut metadata = aden_core::DocumentMetadata::default();
    for line in fm_text.lines() {
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim();
            let value = v.trim();
            match key {
                "title" => metadata.version = Some(value.to_string()),
                "author" => metadata.author = Some(value.to_string()),
                "email" => metadata.email = Some(value.to_string()),
                "date" => metadata.date = Some(value.to_string()),
                "version" => metadata.version = Some(value.to_string()),
                "revision" => metadata.revision = Some(value.to_string()),
                "copyright" => metadata.copyright = Some(value.to_string()),
                "license" => metadata.license = Some(value.to_string()),
                _ => {}
            }
        }
    }

    let body_start = 3 + end + 5;
    let body = if body_start < source.len() {
        &source[body_start..]
    } else {
        ""
    };

    (Some(metadata), body)
}

fn make_doc_anchor(crate_name: &str, file_name: &str, title: &str, level: usize) -> String {
    let slug: String = title
        .chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' => c,
            'A'..='Z' => c.to_ascii_lowercase(),
            ' ' | '-' | '_' => '-',
            _ => '-',
        })
        .collect();
    format!("aden://doc/{}/{}/h{}{}", crate_name, file_name, level, slug)
}

/// Extract fragment-bearing markdown link targets from one line into `out` as
/// `(line_idx, "ref:<frag>")` pairs — markdown's explicit cross-reference
/// idiom, mirroring the AsciiDoc parser's `<<target>>` extraction.
///
/// Recognized: `[text](#frag)` (intra-doc heading link) and
/// `[text](path/file.md#frag)` (cross-file) — both reduce to `frag`, which the
/// gen-time linker exact-matches against doc anchor fragments/heading slugs.
/// Deliberately excluded: bare file links (`[text](file.md)`) carry no anchor
/// fragment to match, so they stay out of the ref channel (they keep their
/// existing `<<stem>>` body-text rendering for display only). Inline code
/// spans (backticks) are treated as literal examples.
fn collect_fragment_refs(line: &str, line_idx: usize, out: &mut Vec<(usize, String)>) {
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
        // `](url)` — the url part of an inline link.
        if c == b']'
            && i + 1 < bytes.len()
            && bytes[i + 1] == b'('
            && let Some(close) = line[i + 2..].find(')')
        {
            let url = &line[i + 2..i + 2 + close];
            if let Some((_, frag)) = url.rsplit_once('#') {
                let frag = frag.trim();
                if !frag.is_empty() && !frag.contains(' ') && frag.len() < 80 {
                    out.push((line_idx, format!("ref:{frag}")));
                }
            }
            i += 2 + close + 1;
            continue;
        }
        i += 1;
    }
}

/// Normalize a markdown link target down to a bare anchor name suitable for an
/// AsciiDoc-style `<<target>>` cross-reference.
///
/// Handles forms like `./foo.md#bar` -> `bar`, `#bar` -> `bar`,
/// `[[wikilink]]` -> `wikilink`, and `path/to/foo.md` -> `foo`.
fn normalize_link_target(url: &str) -> String {
    let mut t = url.trim();

    // Wikilinks: [[target]]
    t = t.trim_start_matches("[[").trim_end_matches("]]");

    // Drop any explicit aden:// scheme noise by keeping the trailing portion.
    // Prefer a `#fragment` when present, otherwise fall back to the file stem.
    if let Some((before, fragment)) = t.rsplit_once('#') {
        let fragment = fragment.trim();
        if !fragment.is_empty() {
            return fragment.to_string();
        }
        t = before;
    }

    // Strip a leading `./` and any path components, keeping the last segment.
    let last = t
        .trim_start_matches("./")
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(t);

    // Strip a trailing `.md` (or related) extension to keep a bare stem.
    last.strip_suffix(".md")
        .or_else(|| last.strip_suffix(".markdown"))
        .unwrap_or(last)
        .trim()
        .to_string()
}

fn extract_markdown_link(line: &str) -> Option<String> {
    let line = line.trim();
    if line.starts_with('[')
        && let Some(bracket_end) = line.find("](")
    {
        let inner = &line[1..bracket_end];
        if let Some(paren_start) = line.find("](") {
            let url_start = paren_start + 2;
            if let Some(paren_end) = line[url_start..].find(')') {
                let url = &line[url_start..url_start + paren_end];
                return Some(format!("{}->{}", inner, url));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extractor::LanguageExtractor;
    use std::path::Path;

    /// Fragment-bearing markdown links (`[t](#frag)`, `[t](file.md#frag)`)
    /// become `ref:`-prefixed entries in the enclosing section's `doc_refs`
    /// attribute. Bare file links carry no anchor fragment and are excluded
    /// from the ref channel; links inside fenced code blocks are ignored.
    #[test]
    fn fragment_links_attributed_to_enclosing_section() {
        let ext = MarkdownExtractor::new();
        let src = "\
# Title

Intro with a [local link](#setup-guide) and an [external one](other.md#install-steps),
plus a [bare file link](other.md).

```text
a fenced [example](#not-a-ref) link
```

## Setup Guide

Body with no links.
";
        let docs = ext
            .extract_documents(src, Path::new("readme.md"))
            .expect("extraction must succeed");

        let title = docs
            .iter()
            .find(|d| d.anchor.contains("h1title"))
            .expect("title node");
        let refs = title
            .attributes
            .get("doc_refs")
            .cloned()
            .unwrap_or_default();
        let refs: Vec<&str> = refs.split(',').collect();
        assert!(refs.contains(&"ref:setup-guide"), "got {refs:?}");
        assert!(refs.contains(&"ref:install-steps"), "got {refs:?}");
        assert!(
            !refs
                .iter()
                .any(|r| r.contains("not-a-ref") || r.contains("other")),
            "fenced/bare-file links must not enter the ref channel; got {refs:?}"
        );

        let section = docs
            .iter()
            .find(|d| d.anchor.contains("h2setup-guide"))
            .expect("section node");
        assert!(
            !section.attributes.contains_key("doc_refs"),
            "section has no links of its own"
        );
    }
}
