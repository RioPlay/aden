// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! AsciiDoc extractor for Aden.
//!
//! Extracts headings, code blocks, and links from AsciiDoc files.

use crate::extractor::{build_code_attributes, extract_code_references, infer_project_name, make_anchor};
use aden_core::{Block, Document, NodeType, Result, SourceSpan};
use std::collections::HashMap;
use std::path::Path;

pub struct AsciiDocExtractor;

impl Default for AsciiDocExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl AsciiDocExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl crate::extractor::LanguageExtractor for AsciiDocExtractor {
    fn language_id(&self) -> &'static str {
        "asciidoc"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &["adoc", "asciidoc", "asc"]
    }

    fn extract_documents(&self, source: &str, path: &Path) -> Result<Vec<Document>> {
        let mut docs = Vec::new();
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let crate_name = infer_project_name(path);

        let (attributes, custom_attrs, body) = parse_document_attributes(source);
        let has_sectanchors = custom_attrs.contains_key("sectanchors");
        let body_lines: Vec<&str> = body.lines().collect();
        let mut headings = Vec::new();
        let mut code_blocks = Vec::new();
        let mut in_literal_block = false;
        let mut current_code_lines = Vec::new();
        let mut in_listing_block = false;
        // Prose cross-references, as (0-based line index, "ref:<target>") pairs.
        // Collected here (where listing/literal fence state is known) and
        // attributed below to the enclosing section node — never from inside a
        // delimited block, where `<<x>>` is a code example, not a reference.
        let mut line_refs: Vec<(usize, String)> = Vec::new();
        // Backtick symbol mentions (Wave-2 `Mentions` channel), same shape and
        // same fence discipline as `line_refs`.
        let mut line_mentions: Vec<(usize, String)> = Vec::new();
        // Description-list term lines `(idx, explicit_anchor, name, same-line
        // def)` — Term-node candidates; only those inside glossary-gated
        // sections are promoted below.
        let mut term_lines: Vec<(usize, Option<String>, String, String)> = Vec::new();

        for (line_num, line) in body.lines().enumerate() {
            let line_num = line_num + 1;

            // trim_end so a CRLF checkout (`----\r`) still matches the fence.
            if line.trim_end() == "----" {
                if in_listing_block {
                    code_blocks.push((None, current_code_lines.join("\n")));
                    current_code_lines.clear();
                }
                in_listing_block = !in_listing_block;
                continue;
            }

            if in_listing_block {
                current_code_lines.push(line.to_string());
                continue;
            }

            // Literal blocks (`....`) carry no code worth extracting, but their
            // content is still literal — never a cross-reference.
            if line.trim_end() == "...." {
                in_literal_block = !in_literal_block;
                continue;
            }
            if in_literal_block {
                continue;
            }

            collect_prose_refs(line, line_num - 1, &mut line_refs);
            crate::extractor::collect_backtick_mentions(line, line_num - 1, &mut line_mentions);
            if let Some((explicit, name, def)) = parse_dlist_term(line) {
                term_lines.push((line_num - 1, explicit, name, def));
            }

            if let Some(rest) = line.strip_prefix("= ") {
                let title = rest.trim().to_string();
                if !title.is_empty() {
                    headings.push((1, title, line_num));
                }
            } else if let Some(rest) = line.strip_prefix("== ") {
                let title = rest.trim().to_string();
                if !title.is_empty() {
                    headings.push((2, title, line_num));
                }
            } else if let Some(rest) = line.strip_prefix("=== ") {
                let title = rest.trim().to_string();
                if !title.is_empty() {
                    headings.push((3, title, line_num));
                }
            } else if let Some(rest) = line.strip_prefix("==== ") {
                let title = rest.trim().to_string();
                if !title.is_empty() {
                    headings.push((4, title, line_num));
                }
            } else if let Some(rest) = line.strip_prefix("===== ") {
                let title = rest.trim().to_string();
                if !title.is_empty() {
                    headings.push((5, title, line_num));
                }
            } else if let Some(rest) = line.strip_prefix("====== ") {
                let title = rest.trim().to_string();
                if !title.is_empty() {
                    headings.push((6, title, line_num));
                }
            } else if let (Some(open), Some(close)) = (line.find("[["), line.find("]]"))
                && open + 2 <= close
            {
                let anchor_name = &line[open + 2..close];
                if !anchor_name.is_empty() {
                    headings.push((0, anchor_name.to_string(), line_num));
                }
            }
        }

        // Term docs are appended AFTER the code-block loop so the historical
        // `code_block_{docs.len()}` numbering never shifts under existing stores.
        let mut term_docs: Vec<Document> = Vec::new();
        // Glossary gate, document level: a glossary-titled doc promotes terms
        // in EVERY section; otherwise only glossary-titled sections do.
        let is_glossary_doc = headings
            .iter()
            .find(|(level, ..)| *level == 1)
            .map(|(_, t, _)| crate::extractor::is_glossary_title(t))
            .unwrap_or_else(|| crate::extractor::is_glossary_title(&file_name));

        if !headings.is_empty() {
            for hi in 0..headings.len() {
                let (level, ref title, line_num) = headings[hi];
                let anchor = if level > 0 {
                    make_adoc_anchor(&crate_name, &file_name, title, level)
                } else {
                    format!("aden://doc/{}/{}#{}", crate_name, file_name, title)
                };

                // Capture the section's prose so the node carries real content,
                // not just its title. The body runs from just after this heading
                // to the next heading that starts a new block — adjacent heading
                // lines (e.g. an `[[anchor]]` directly above its `= Title`) are
                // treated as one header for the same section.
                let body_start = line_num; // 0-based index of the line after the heading
                let body_end = headings[hi + 1..]
                    .iter()
                    .find(|h| h.2 > line_num + 1)
                    .map(|h| h.2 - 1)
                    .unwrap_or(body_lines.len());
                let body_text = if body_start < body_end && body_end <= body_lines.len() {
                    body_lines[body_start..body_end]
                        .join("\n")
                        .trim()
                        .to_string()
                } else {
                    String::new()
                };

                let span = SourceSpan {
                    file: path.to_string_lossy().to_string(),
                    start_line: line_num,
                    end_line: body_end.max(line_num),
                    start_byte: 0,
                    end_byte: 0,
                };
                let mut attrs = build_code_attributes(source, "heading", Some(path), Some(&span));
                // Merge document-level custom attributes (tags, status, updated, etc.)
                for (k, v) in &custom_attrs {
                    attrs.insert(k.clone(), v.clone());
                }
                if level > 0 {
                    attrs.insert("heading_level".to_string(), level.to_string());
                }

                // Attribute prose cross-references to this section: every ref on
                // the heading line itself or in its body range. The FIRST node
                // additionally adopts preamble refs (prose before any heading —
                // common when frontmatter holds the title), so they still become
                // graph edges instead of being dropped.
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

                let mut blocks = vec![Block::Paragraph(title.clone())];
                if !body_text.is_empty() {
                    blocks.push(Block::Paragraph(body_text));
                }

                docs.push(Document {
                    anchor: anchor.clone(),
                    node_type: NodeType::Module,
                    attributes: attrs.clone(),
                    blocks: blocks.clone(),
                    source_span: Some(span.clone()),
                    metadata: attributes.clone(),
                    confidence: 0.9,
                });

                // When :sectanchors: is set, also emit an Asciidoctor-compatible
                // alias anchor (_slug format) so xref:file#_heading links resolve.
                if has_sectanchors && level > 0 {
                    let alias = make_sectanchors_anchor(&crate_name, &file_name, title);
                    if alias != anchor {
                        let mut alias_attrs = attrs.clone();
                        alias_attrs.insert("alias_of".to_string(), anchor.clone());
                        // The alias points at the SAME section; duplicating its
                        // doc_refs/doc_mentions would emit every edge twice.
                        alias_attrs.remove("doc_refs");
                        alias_attrs.remove("doc_mentions");
                        docs.push(Document {
                            anchor: alias,
                            node_type: NodeType::Module,
                            attributes: alias_attrs,
                            blocks: blocks.clone(),
                            source_span: Some(span),
                            metadata: attributes.clone(),
                            confidence: 0.8,
                        });
                    }
                }
            }
        } else {
            let anchor = make_anchor(&crate_name, &file_name, "document");
            let mut attrs = build_code_attributes(source, "document", Some(path), None);
            for (k, v) in &custom_attrs {
                attrs.insert(k.clone(), v.clone());
            }
            // No headings: the whole file is one node — it owns every prose ref.
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
            docs.push(Document {
                anchor,
                node_type: NodeType::Module,
                attributes: attrs,
                blocks: vec![Block::Paragraph(format!(
                    "AsciiDoc document: {}",
                    file_name
                ))],
                source_span: None,
                metadata: attributes,
                confidence: 0.7,
            });
        }

        // Glossary post-pass over REAL (level>0) headings: inline `[[x]]`
        // declarations register as level-0 pseudo-headings and would otherwise
        // split a glossary section's range, orphaning every entry below the
        // first explicitly-anchored term. Term anchors are recorded on the
        // already-pushed section doc (`doc_terms`) for the linker's
        // section —DefinesTerm→ term edges.
        let real_headings: Vec<(usize, &String, usize)> = headings
            .iter()
            .filter(|(l, ..)| *l > 0)
            .map(|(l, t, n)| (*l, t, *n))
            .collect();
        for (i, (level, title, line_num)) in real_headings.iter().enumerate() {
            if !(is_glossary_doc || crate::extractor::is_glossary_title(title)) {
                continue;
            }
            let start = *line_num; // 0-based index of the first body line
            let end = real_headings[i + 1..]
                .first()
                .map(|(.., n)| n.saturating_sub(1))
                .unwrap_or(body_lines.len());
            let mut term_anchors: Vec<String> = Vec::new();
            for (idx, explicit, name, def) in term_lines
                .iter()
                .filter(|(idx, ..)| *idx >= start && *idx < end)
            {
                let definition = if def.is_empty() {
                    following_definition_lines(&body_lines, *idx)
                } else {
                    def.clone()
                };
                let slug = explicit
                    .clone()
                    .unwrap_or_else(|| crate::extractor::term_slug(name));
                if slug.is_empty() {
                    continue;
                }
                let entry = crate::extractor::GlossaryEntry {
                    name: name.clone(),
                    slug,
                    definition,
                };
                let term = crate::extractor::build_term_document(&crate_name, path, &entry);
                term_anchors.push(term.anchor.clone());
                term_docs.push(term);
            }
            term_anchors.sort();
            term_anchors.dedup();
            if !term_anchors.is_empty() {
                let section_anchor = make_adoc_anchor(&crate_name, &file_name, title, *level);
                if let Some(d) = docs.iter_mut().find(|d| d.anchor == section_anchor) {
                    d.attributes
                        .insert("doc_terms".to_string(), term_anchors.join(","));
                }
            }
        }
        // Whole-file glossaries with no headings at all (rare): the single
        // document node owns every term.
        if headings.is_empty() && crate::extractor::is_glossary_title(&file_name) {
            let mut term_anchors: Vec<String> = Vec::new();
            for (idx, explicit, name, def) in &term_lines {
                let definition = if def.is_empty() {
                    following_definition_lines(&body_lines, *idx)
                } else {
                    def.clone()
                };
                let slug = explicit
                    .clone()
                    .unwrap_or_else(|| crate::extractor::term_slug(name));
                if slug.is_empty() {
                    continue;
                }
                let entry = crate::extractor::GlossaryEntry {
                    name: name.clone(),
                    slug,
                    definition,
                };
                let term = crate::extractor::build_term_document(&crate_name, path, &entry);
                term_anchors.push(term.anchor.clone());
                term_docs.push(term);
            }
            term_anchors.sort();
            term_anchors.dedup();
            if !term_anchors.is_empty()
                && let Some(d) = docs.last_mut()
            {
                d.attributes
                    .insert("doc_terms".to_string(), term_anchors.join(","));
            }
        }

        for (lang, code) in code_blocks {
            let anchor = make_anchor(
                &crate_name,
                &file_name,
                &format!("code_block_{}", docs.len()),
            );
            let lang_str = lang.as_deref().unwrap_or("");
            let mut references = extract_code_references(&code, lang_str);
            // Language-neutral call-shaped tokens (Wave-2 Demonstrates): the
            // per-language declaration scan above misses what a listing CALLS,
            // which is exactly what it demonstrates.
            references.extend(
                crate::extractor::listing_call_tokens(&code)
                    .into_iter()
                    .map(|t| format!("call:{t}")),
            );
            references.dedup();
            let mut attrs = build_code_attributes(&code, "code", Some(path), None);
            if !references.is_empty() {
                attrs.insert("symbol_references".to_string(), references.join(","));
            }
            docs.push(Document {
                anchor,
                node_type: NodeType::Script,
                attributes: attrs,
                blocks: vec![Block::Listing {
                    language: lang,
                    code,
                }],
                source_span: None,
                metadata: None,
                confidence: 0.8,
            });
        }

        docs.extend(term_docs);

        Ok(docs)
    }
}

/// Parse one description-list term line: `Name:: def` or
/// `[[anchor]]Name::` (definition on the following lines). Returns
/// `(explicit_anchor, name, same_line_def)`. The `::` must sit at a word
/// boundary (followed by a space or end-of-line) so Rust paths like
/// `foo::bar` never match; indented lines are dlist *continuations*, not
/// terms.
fn parse_dlist_term(line: &str) -> Option<(Option<String>, String, String)> {
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    let (explicit, rest) = if let Some(after) = line.strip_prefix("[[") {
        let close = after.find("]]")?;
        (Some(after[..close].to_string()), &after[close + 2..])
    } else {
        (None, line)
    };
    // The FIRST `::` followed by space/EOL ends the name.
    let mut search = 0;
    let sep = loop {
        let off = rest[search..].find("::")?;
        let pos = search + off;
        match rest.as_bytes().get(pos + 2) {
            None | Some(b' ') => break pos,
            _ => search = pos + 2,
        }
    };
    let name = rest[..sep].trim();
    if name.is_empty() || name.len() > 64 || name.contains('`') {
        return None;
    }
    let def = rest[sep + 2..].trim().to_string();
    Some((explicit, name.to_string(), def))
}

/// Definition text for a `Name::` entry whose definition starts on the next
/// line: subsequent lines up to a blank line, a lone `+` continuation marker,
/// the next term, or a heading — capped so a malformed glossary cannot swallow
/// the document.
fn following_definition_lines(body_lines: &[&str], term_idx: usize) -> String {
    let mut out: Vec<&str> = Vec::new();
    for line in body_lines.iter().skip(term_idx + 1).take(8) {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed == "+"
            || line.starts_with('=')
            || parse_dlist_term(line).is_some()
        {
            break;
        }
        out.push(trimmed);
    }
    out.join("\n")
}

/// Parse the AsciiDoc document header and return extracted metadata, custom
/// attributes, and the remaining body text.
///
/// AsciiDoc header format:
/// ```text
/// = Document Title
/// Optional Author Name <email@example.com>
/// :attribute-key: attribute value
/// :boolean-attribute:
/// :!unset-attribute:
///
/// Body starts after the first blank line.
/// ```
///
/// All standard metadata fields are captured into `DocumentMetadata`. Every
/// other attribute (`:tags:`, `:status:`, `:updated:`, `:sectanchors:`, etc.)
/// is stored in the returned `HashMap` so callers can insert them into
/// `Document.attributes` for search and filtering.
fn parse_document_attributes(
    source: &str,
) -> (
    Option<aden_core::DocumentMetadata>,
    HashMap<String, String>,
    String,
) {
    let mut metadata = aden_core::DocumentMetadata::default();
    let mut custom_attrs: HashMap<String, String> = HashMap::new();
    let mut title_seen = false;
    let mut any_attr_seen = false;
    let mut header_end_line = 0usize;

    let all_lines: Vec<&str> = source.lines().collect();

    for (i, &line) in all_lines.iter().enumerate() {
        if line.is_empty() {
            // Blank line terminates the header block.
            header_end_line = i + 1;
            break;
        }

        // Document title line(s) — `= Title`, `== Embedded`, etc.
        if line.starts_with("= ") || line == "=" {
            title_seen = true;
            header_end_line = i + 1;
            continue;
        }

        if let Some(rest) = line.strip_prefix(':') {
            // Attribute entry: `:key: value` (or `:key:` for boolean, `:!key:` to unset).
            if let Some(colon_pos) = rest.find(':') {
                let raw_key = rest[..colon_pos].trim();
                // Strip the `!` unset prefix; we just capture the key/value as-is.
                let key = raw_key.trim_start_matches('!');
                let value = rest[colon_pos + 1..].trim();
                if !key.is_empty() {
                    any_attr_seen = true;
                    match key {
                        "author" => metadata.author = Some(value.to_string()),
                        "email" => metadata.email = Some(value.to_string()),
                        "revdate" | "date" | "updated" => {
                            metadata.date = Some(value.to_string())
                        }
                        "version" | "revnumber" => metadata.version = Some(value.to_string()),
                        "revision" => metadata.revision = Some(value.to_string()),
                        "copyright" => metadata.copyright = Some(value.to_string()),
                        "license" => metadata.license = Some(value.to_string()),
                        _ => {
                            custom_attrs.insert(key.to_string(), value.to_string());
                        }
                    }
                }
            }
            header_end_line = i + 1;
            continue;
        }

        // The implicit author/revision line is only valid immediately after a
        // document title and before any attributes. Guard on `title_seen` so
        // plain-content files (no `= Title`) are never misidentified as headers.
        if title_seen && !any_attr_seen && metadata.author.is_none() {
            if let (Some(lt), Some(gt)) = (line.find('<'), line.find('>')) {
                if lt < gt {
                    let name = line[..lt].trim();
                    let email = line[lt + 1..gt].trim();
                    if !name.is_empty() {
                        metadata.author = Some(name.to_string());
                    }
                    if !email.is_empty() {
                        metadata.email = Some(email.to_string());
                    }
                }
            } else {
                metadata.author = Some(line.trim().to_string());
            }
            header_end_line = i + 1;
            continue;
        }

        // Non-blank, non-title, non-attribute line after attributes were seen
        // (or with no title) — the header is over.
        break;
    }

    let body = all_lines[header_end_line..].join("\n");

    let has_metadata = metadata.author.is_some()
        || metadata.email.is_some()
        || metadata.date.is_some()
        || metadata.version.is_some()
        || metadata.revision.is_some()
        || metadata.copyright.is_some()
        || metadata.license.is_some()
        || !custom_attrs.is_empty();

    (
        if has_metadata { Some(metadata) } else { None },
        custom_attrs,
        body,
    )
}

fn make_adoc_anchor(crate_name: &str, file_name: &str, title: &str, level: usize) -> String {
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

/// Build an Asciidoctor `:sectanchors:`-compatible anchor for a heading.
///
/// Asciidoctor generates `_slug` format: lowercase, non-alphanumeric → `_`,
/// collapse repeated `_`, prefix with `_`. This lets xrefs like
/// `xref:file.adoc#_core_concepts` resolve to the correct graph node.
fn make_sectanchors_anchor(crate_name: &str, file_name: &str, title: &str) -> String {
    let raw: String = title
        .chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' => c,
            'A'..='Z' => c.to_ascii_lowercase(),
            _ => '_',
        })
        .collect();
    let slug: String = raw
        .split('_')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    format!("aden://doc/{}/{}/_{}", crate_name, file_name, slug)
}


/// Extract prose cross-reference targets from one line into `out` as
/// `(line_idx, "ref:<target>")` pairs.
///
/// Recognized forms (all reduced to the bare anchor fragment):
/// - `<<target>>` and `<<target,label>>` shorthand xrefs;
/// - `<<file.adoc#frag>>` file-qualified shorthand → `frag`;
/// - `xref:file.adoc#frag[label]` / `xref:file#frag` macros → `frag`
///   (file-level xrefs with no `#fragment` name no anchor and are skipped).
///
/// Backtick-quoted spans are literal examples and never produce a ref — the
/// caller additionally guarantees we are not inside a `----`/`....` block.
///
/// Namespace: the `ref:` prefix marks prose cross-reference targets in the
/// linker's ref channel. It is disjoint from the `fn:`/`struct:`/`enum:`/
/// `type:`/`class:`/`use:`/`mod:` prefixes that [`extract_code_references`]
/// emits into `symbol_references`, so the gen-time linker can resolve `ref:`
/// records exclusively against DOC anchor fragments (format-neutral), never
/// letting a prose ref fuzzy-match a same-named code symbol.
fn collect_prose_refs(line: &str, line_idx: usize, out: &mut Vec<(usize, String)>) {
    let push = |target: &str, out: &mut Vec<(usize, String)>| {
        // Reduce a file-qualified target to its trailing fragment; anchors are
        // globally unique, so the fragment alone identifies the declaration.
        let frag = match target.rsplit_once('#') {
            Some((_, f)) => f.trim(),
            None => target.trim(),
        };
        if !frag.is_empty() && !frag.contains('{') && !frag.contains(' ') && frag.len() < 80 {
            out.push((line_idx, format!("ref:{frag}")));
        }
    };

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
        // <<target>> / <<target,label>> shorthand.
        if c == b'<'
            && i + 1 < bytes.len()
            && bytes[i + 1] == b'<'
            && let Some(end) = line[i + 2..].find(">>")
        {
            let abs_end = i + 2 + end;
            let inner = &line[i + 2..abs_end];
            push(inner.split(',').next().unwrap_or(inner), out);
            i = abs_end + 2;
            continue;
        }
        // xref:path#frag[label] (or bare xref:path#frag) macro. Match on the
        // BYTE slice: `i` walks bytes, so `&line[i..]` would panic mid-way
        // through a multibyte char (em-dash, typographic quote).
        if bytes[i..].starts_with(b"xref:") {
            let after = &line[i + 5..];
            let target_end = after
                .find(|ch: char| ch == '[' || ch.is_whitespace())
                .unwrap_or(after.len());
            let target = &after[..target_end];
            if target.contains('#') {
                push(target, out);
            }
            i += 5 + target_end;
            continue;
        }
        i += 1;
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::extractor::LanguageExtractor;
    use std::path::Path;

    // Regression: a line where `]]` precedes `[[` (e.g. "]]bar[[") used to panic
    // with a reversed slice index `start > end`. It must now parse without panic.
    #[test]
    fn malformed_anchor_order_does_not_panic() {
        let ext = AsciiDocExtractor::new();
        let src = "= Title\n\n]]bar[[\n\nSome content.\n";
        let docs = ext
            .extract_documents(src, Path::new("doc.adoc"))
            .expect("malformed anchor line must not error");
        assert!(!docs.is_empty());
    }

    #[test]
    fn parse_document_attributes_captures_custom_attrs() {
        let src = "\
= AsciiDoc Mastery
:docinfo: shared
:tags: asciidoc, aden, knowledge-graph
:status: draft
:updated: 2026-06-07
:sectanchors:
:toc: left

== First Section

Content here.
";
        let (meta, custom, body) = parse_document_attributes(src);

        // Standard metadata
        assert!(meta.is_some());
        let m = meta.unwrap();
        assert_eq!(m.date.as_deref(), Some("2026-06-07"));

        // Custom attributes captured
        assert_eq!(custom.get("tags").map(|s| s.as_str()), Some("asciidoc, aden, knowledge-graph"));
        assert_eq!(custom.get("status").map(|s| s.as_str()), Some("draft"));
        assert_eq!(custom.get("docinfo").map(|s| s.as_str()), Some("shared"));
        assert!(custom.contains_key("sectanchors"), "sectanchors must be captured");
        assert_eq!(custom.get("toc").map(|s| s.as_str()), Some("left"));

        // Body does not include header lines
        assert!(!body.contains(":tags:"), "body must not contain attribute lines");
        assert!(body.contains("== First Section"), "body must contain section headings");
    }

    #[test]
    fn parse_document_attributes_empty_when_no_header() {
        let src = "Just plain content\nwith no header.\n";
        let (meta, custom, body) = parse_document_attributes(src);
        assert!(meta.is_none());
        assert!(custom.is_empty());
        // Body is the full source when no header is detected
        assert!(body.contains("Just plain content"));
    }

    #[test]
    fn extract_documents_propagates_tags_into_attributes() {
        let ext = AsciiDocExtractor::new();
        let src = "\
= My Doc
:tags: aden, graph
:status: draft

== Section One

Body text.
";
        let docs = ext
            .extract_documents(src, Path::new("test.adoc"))
            .expect("extraction must succeed");
        assert!(!docs.is_empty());
        let first = &docs[0];
        assert_eq!(first.attributes.get("tags").map(|s| s.as_str()), Some("aden, graph"));
        assert_eq!(first.attributes.get("status").map(|s| s.as_str()), Some("draft"));
    }

    #[test]
    fn sectanchors_alias_generated_alongside_primary() {
        let ext = AsciiDocExtractor::new();
        let src = "\
= My Doc
:sectanchors:

== Core Concepts

Explanation here.
";
        let docs = ext
            .extract_documents(src, Path::new("guide.adoc"))
            .expect("extraction must succeed");

        // Should have both h2core-concepts and _core_concepts anchors
        let anchors: Vec<&str> = docs.iter().map(|d| d.anchor.as_str()).collect();
        let has_primary = anchors.iter().any(|a| a.contains("h2core-concepts"));
        let has_alias = anchors.iter().any(|a| a.ends_with("_core_concepts"));
        assert!(has_primary, "primary h2 anchor must be present; got: {:?}", anchors);
        assert!(has_alias, "sectanchors alias _core_concepts must be present; got: {:?}", anchors);
    }

    /// Prose `<<target>>` refs are extracted into the enclosing section node's
    /// `doc_refs` attribute (ref:-prefixed, comma-joined), with code listings
    /// and backtick-quoted examples excluded.
    #[test]
    fn prose_refs_attributed_to_enclosing_section() {
        let ext = AsciiDocExtractor::new();
        let src = "\
= My Doc

== Section A

Prose that references <<_term_b>> and a labeled <<_term_c,custom label>>.

----
code showing <<_not_a_ref>> literally
----

More prose with `<<_in_ticks>>` quoted, plus xref:other.adoc#_frag[a label].

[[_term_b]]Term B::
Definition of b, which mentions <<_term_c>>.

== Section C

[[_term_c]]
Body of c with no refs.
";
        let docs = ext
            .extract_documents(src, Path::new("doc.adoc"))
            .expect("extraction must succeed");

        let refs_of = |needle: &str| -> Vec<String> {
            let d = docs
                .iter()
                .find(|d| d.anchor.contains(needle))
                .unwrap_or_else(|| {
                    panic!(
                        "no doc node matching {needle:?}; got {:?}",
                        docs.iter().map(|d| &d.anchor).collect::<Vec<_>>()
                    )
                });
            d.attributes
                .get("doc_refs")
                .map(|v| v.split(',').map(str::to_string).collect())
                .unwrap_or_default()
        };

        let a = refs_of("h2section-a");
        assert!(a.contains(&"ref:_term_b".to_string()), "Section A refs: {a:?}");
        assert!(a.contains(&"ref:_term_c".to_string()), "Section A refs: {a:?}");
        assert!(a.contains(&"ref:_frag".to_string()), "xref fragment; got {a:?}");
        assert!(
            !a.iter().any(|r| r.contains("_not_a_ref") || r.contains("_in_ticks")),
            "listing/backtick examples must not become refs; got {a:?}"
        );

        // The description-list term node owns the refs in ITS body.
        let b = refs_of("#_term_b");
        assert_eq!(b, vec!["ref:_term_c".to_string()], "term B refs: {b:?}");

        // A section with no refs carries no doc_refs attribute at all.
        let c = refs_of("h2section-c");
        assert!(c.is_empty(), "Section C has no refs; got {c:?}");
    }

    /// Refs in the preamble — prose BEFORE the first heading (common with
    /// external frontmatter, e.g. Hugo) — attach to the file's first doc node
    /// so they still become graph edges instead of being dropped.
    #[test]
    fn preamble_refs_attach_to_first_node() {
        let ext = AsciiDocExtractor::new();
        let src = "\
A lead paragraph referencing <<_term>> before any heading.

== First Section

Body.
";
        let docs = ext
            .extract_documents(src, Path::new("post.adoc"))
            .expect("extraction must succeed");
        let first = &docs[0];
        let refs = first.attributes.get("doc_refs").cloned().unwrap_or_default();
        assert!(
            refs.split(',').any(|r| r == "ref:_term"),
            "preamble ref must attach to the first node ({}); got {refs:?}",
            first.anchor
        );
    }

    /// The sectanchors alias node must NOT duplicate the primary node's refs —
    /// otherwise every ref would emit double RelatesTo edges.
    #[test]
    fn sectanchors_alias_carries_no_doc_refs() {
        let ext = AsciiDocExtractor::new();
        let src = "\
= My Doc
:sectanchors:

== Core Concepts

References <<_other>> here.
";
        let docs = ext
            .extract_documents(src, Path::new("guide.adoc"))
            .expect("extraction must succeed");
        let primary = docs
            .iter()
            .find(|d| d.anchor.contains("h2core-concepts"))
            .expect("primary node");
        assert!(
            primary.attributes.get("doc_refs").is_some(),
            "primary must carry the refs"
        );
        let alias = docs
            .iter()
            .find(|d| d.anchor.ends_with("_core_concepts"))
            .expect("alias node");
        assert!(
            alias.attributes.get("doc_refs").is_none(),
            "alias_of node must not duplicate refs (double edges)"
        );
    }

    /// Regression: multibyte characters (em-dashes, typographic quotes) before
    /// an `xref:` must not panic the byte-indexed scanner on a non-boundary
    /// slice (`line[i..]` with i inside a UTF-8 sequence).
    #[test]
    fn prose_refs_survive_multibyte_text() {
        let mut out = Vec::new();
        collect_prose_refs(
            "Confident — and a little smug — see xref:guide.adoc#_frag[the guide] and “quotes”.",
            0,
            &mut out,
        );
        assert_eq!(out, vec![(0usize, "ref:_frag".to_string())]);
    }

    #[test]
    fn make_sectanchors_anchor_matches_asciidoctor_format() {
        let a = make_sectanchors_anchor("proj", "file.adoc", "Core Concepts");
        assert_eq!(a, "aden://doc/proj/file.adoc/_core_concepts");

        let b = make_sectanchors_anchor("proj", "file.adoc", "Why AsciiDoc?");
        assert_eq!(b, "aden://doc/proj/file.adoc/_why_asciidoc");

        let c = make_sectanchors_anchor("proj", "file.adoc", "BM25 & Vector Search");
        assert_eq!(c, "aden://doc/proj/file.adoc/_bm25_vector_search");
    }
}
