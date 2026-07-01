// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! AsciiDoc extractor for Aden.
//!
//! Extracts headings, code blocks, and links from AsciiDoc files.

use crate::extractor::{
    build_code_attributes, extract_code_references, infer_project_name, infer_project_root,
    make_anchor, project_relative_file,
};
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
        let crate_name = infer_project_name(path);
        let project_root = infer_project_root(path);
        let file_name = project_relative_file(path, &project_root);

        let (attributes, local_custom_attrs, _, document_title) =
            parse_document_attributes(source);
        let mut custom_attrs = parse_shared_attribute_include_attrs(source, path, &project_root);
        // Page-local attributes win over included shared attributes, matching
        // Asciidoctor's "include common first, override locally" convention.
        custom_attrs.extend(local_custom_attrs);
        let has_sectanchors = custom_attrs.contains_key("sectanchors");
        // Collect composition includes from the raw source before preprocess
        // expands them away; the link phase still emits Requires edges.
        let includes = collect_include_directives(source);
        let index_source = {
            let mut visited = Vec::new();
            crate::asciidoc_preprocess::preprocess_text_for_index(
                source,
                path,
                &custom_attrs,
                &mut visited,
                0,
            )
            .unwrap_or_else(|_| source.to_string())
        };
        let (_, _, body, _) = parse_document_attributes(&index_source);
        let body_lines: Vec<&str> = body.lines().collect();
        let mut headings = Vec::new();
        let mut code_blocks = Vec::new();
        let mut in_literal_block = false;
        let mut current_code_lines = Vec::new();
        let mut current_code_lang: Option<String> = None;
        let mut pending_source_lang: Option<String> = None;
        // 1-based line of the listing block's first body line, captured when the
        // opening `----` fence is seen, so the emitted node spans the real code.
        let mut current_code_start = 0usize;
        let mut in_listing_block = false;
        // Prose cross-references, as (0-based line index, "ref:<target>") pairs.
        // Collected here (where listing/literal fence state is known) and
        // attributed below to the enclosing section node — never from inside a
        // delimited block, where `<<x>>` is a code example, not a reference.
        let mut line_refs: Vec<(usize, String)> = Vec::new();
        // Backtick symbol mentions (Wave-2 `Mentions` channel), same shape and
        // same fence discipline as `line_refs`.
        let mut line_mentions: Vec<(usize, String)> = Vec::new();
        // Supersede-context refs (Wave-3 `Supersedes` channel): refs found on a
        // line with supersede language, as `(idx, "<by|of>:ref:<frag>")` — the
        // direction prefix tells the linker which side the enclosing doc is on.
        let mut line_supersedes: Vec<(usize, String)> = Vec::new();
        // Description-list term lines `(idx, explicit_anchor, name, same-line
        // def)` — Term-node candidates; only those inside glossary-gated
        // sections are promoted below.
        let mut term_lines: Vec<(usize, Option<String>, String, String)> = Vec::new();
        for (line_num, line) in body.lines().enumerate() {
            let line_num = line_num + 1;

            // trim_end so a CRLF checkout (`----\r`) still matches the fence.
            if line.trim_end() == "----" {
                if in_listing_block {
                    code_blocks.push((
                        current_code_lang.take(),
                        current_code_lines.join("\n"),
                        current_code_start,
                    ));
                    current_code_lines.clear();
                } else {
                    current_code_start = line_num + 1;
                    current_code_lang = pending_source_lang.take();
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

            let refs_before = line_refs.len();
            collect_prose_refs(line, line_num - 1, &mut line_refs);
            if line_refs.len() > refs_before
                && let Some(dir) = crate::extractor::supersede_direction(line)
            {
                for (_, r) in &line_refs[refs_before..] {
                    line_supersedes.push((line_num - 1, format!("{dir}:{r}")));
                }
            }
            crate::extractor::collect_backtick_mentions(line, line_num - 1, &mut line_mentions);
            if let Some((explicit, name, def)) = parse_dlist_term(line) {
                term_lines.push((line_num - 1, explicit, name, def));
            }
            if let Some(lang) = parse_source_block_lang(line) {
                pending_source_lang = lang;
                continue;
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
            } else if let Some(anchor_name) = parse_block_anchor_line(line) {
                headings.push((0, anchor_name, line_num));
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
        // in EVERY section; otherwise only glossary-titled sections do. The
        // document title from the header (`= Glossary`) counts — it is not
        // re-emitted in the body by `parse_document_attributes`.
        let is_glossary_doc = document_title
            .as_ref()
            .is_some_and(|t| crate::extractor::is_glossary_title(t))
            || headings
                .iter()
                .find(|(level, ..)| *level == 1)
                .map(|(_, t, _)| crate::extractor::is_glossary_title(t))
                .unwrap_or(false)
            || crate::extractor::is_glossary_title(&file_name);

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
                // Includes are file-level; attach the whole file's targets to the
                // document's representative (first) node so the link phase emits
                // one Requires edge per included file from the document.
                if hi == 0 && !includes.is_empty() {
                    let mut inc = includes.clone();
                    inc.sort();
                    inc.dedup();
                    attrs.insert("doc_includes".to_string(), inc.join(","));
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

                // `[[id]]` / `[#id]` on the line directly above a real heading
                // shares the section's body range — strip channels from the
                // explicit-anchor node so edges are not emitted twice (same
                // discipline as `:sectanchors:` aliases).
                if level == 0
                    && headings
                        .get(hi + 1)
                        .is_some_and(|(next_level, _, next_line)| {
                            *next_level > 0 && *next_line == line_num + 1
                        })
                {
                    let (canon_level, canon_title, _) = &headings[hi + 1];
                    let canonical =
                        make_adoc_anchor(&crate_name, &file_name, canon_title, *canon_level);
                    attrs.insert("alias_of".to_string(), canonical);
                    attrs.remove("doc_refs");
                    attrs.remove("doc_mentions");
                    attrs.remove("doc_supersedes");
                    attrs.remove("doc_terms");
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
                        // doc_refs/doc_mentions/doc_supersedes would emit every
                        // edge twice.
                        alias_attrs.remove("doc_refs");
                        alias_attrs.remove("doc_mentions");
                        alias_attrs.remove("doc_supersedes");
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
            let doc_span = crate::extractor::whole_file_span(source, path);
            let mut attrs = build_code_attributes(source, "document", Some(path), Some(&doc_span));
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
            if !includes.is_empty() {
                let mut inc = includes.clone();
                inc.sort();
                inc.dedup();
                attrs.insert("doc_includes".to_string(), inc.join(","));
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
                    "AsciiDoc document: {}",
                    file_name
                ))],
                source_span: Some(doc_span),
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
        // Whole-file glossaries with no real section headings: either a single
        // document node (`= Glossary` in the header, body has no headings) or a
        // fragment-only dlist (`[[id]]Term::` on every line — Hugo frontmatter
        // glossaries). Fragment-only docs attach each term to its matching
        // `aden://doc/...#id` node; the no-heading case batches on one node.
        if is_glossary_doc && real_headings.is_empty() {
            if headings.is_empty() {
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
            } else {
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
                        slug: slug.clone(),
                        definition,
                    };
                    let term = crate::extractor::build_term_document(&crate_name, path, &entry);
                    let frag_anchor = format!(
                        "aden://doc/{}/{}#{}",
                        crate_name,
                        file_name,
                        explicit.as_deref().unwrap_or(&slug)
                    );
                    if let Some(d) = docs.iter_mut().find(|d| d.anchor == frag_anchor) {
                        d.attributes
                            .insert("doc_terms".to_string(), term.anchor.clone());
                    }
                    term_docs.push(term);
                }
            }
        }

        for (lang, code, code_start) in code_blocks {
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
                    language: lang,
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
    Option<String>,
) {
    let mut metadata = aden_core::DocumentMetadata::default();
    let mut custom_attrs: HashMap<String, String> = HashMap::new();
    let mut document_title: Option<String> = None;
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

        // Document title — `= Title` only (not `==` section headings).
        if line.starts_with("= ") && !line.starts_with("== ") {
            title_seen = true;
            document_title = Some(line[2..].trim().to_string());
            header_end_line = i + 1;
            continue;
        }
        if line == "=" {
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
                        "revdate" | "date" | "updated" => metadata.date = Some(value.to_string()),
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
        document_title,
    )
}

/// Asciidoctor block-attribute anchor: `[#id]` on its own line (optional
/// role/option suffixes after `.`, `%`, or `,`).
fn parse_block_anchor_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let inner = trimmed.strip_prefix("[#")?.strip_suffix(']')?;
    let id = inner.split(['.', '%', ',']).next()?.trim();
    if id.is_empty() || id.contains(' ') || id.contains('{') {
        return None;
    }
    Some(id.to_string())
}

fn parse_shared_attribute_include_attrs(
    source: &str,
    path: &Path,
    project_root: &Path,
) -> HashMap<String, String> {
    let mut attrs = HashMap::new();
    let Ok(root) = project_root.canonicalize() else {
        return attrs;
    };
    let Some(base) = path.parent() else {
        return attrs;
    };
    // Keep this deliberately shallow: shared attribute files are conventionally
    // included in the document header or preamble, before the first section.
    for line in source.lines().take(128) {
        let trimmed = line.trim();
        if trimmed.starts_with("== ") {
            break;
        }
        let Some(target) = parse_include_target(trimmed) else {
            continue;
        };
        if target.contains('{') || target.contains("://") {
            continue;
        }
        let candidate = base.join(target);
        let Ok(canon) = candidate.canonicalize() else {
            continue;
        };
        if !canon.starts_with(&root) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&canon) else {
            continue;
        };
        attrs.extend(parse_attribute_entries(&text));
    }
    attrs
}

fn parse_attribute_entries(source: &str) -> HashMap<String, String> {
    let mut attrs = HashMap::new();
    for line in source.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix(':') else {
            continue;
        };
        let Some(colon_pos) = rest.find(':') else {
            continue;
        };
        let raw_key = rest[..colon_pos].trim();
        let key = raw_key.trim_start_matches('!');
        if key.is_empty() {
            continue;
        }
        attrs.insert(key.to_string(), rest[colon_pos + 1..].trim().to_string());
    }
    attrs
}

fn parse_include_target(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("include::")?;
    let bracket = rest.find('[')?;
    let target = rest[..bracket].trim();
    (!target.is_empty()).then_some(target)
}

/// Fence-aware scan for top-level `include::` directives in raw AsciiDoc.
/// Runs on the un-preprocessed source so composition edges survive include
/// expansion during indexing.
fn collect_include_directives(source: &str) -> Vec<String> {
    let mut includes = Vec::new();
    let mut in_listing_block = false;
    let mut in_literal_block = false;
    for line in source.lines() {
        if line.trim_end() == "----" {
            in_listing_block = !in_listing_block;
            continue;
        }
        if in_listing_block {
            continue;
        }
        if line.trim_end() == "...." {
            in_literal_block = !in_literal_block;
            continue;
        }
        if in_literal_block {
            continue;
        }
        if let Some(target) = parse_include_target(line.trim()) {
            includes.push(target.to_string());
        }
    }
    includes
}

fn parse_source_block_lang(line: &str) -> Option<Option<String>> {
    let trimmed = line.trim();
    let inner = trimmed.strip_prefix('[')?.strip_suffix(']')?;
    let mut parts = inner.split(',').map(str::trim);
    let first = parts.next()?;
    if first != "source" && !first.starts_with("source%") {
        return None;
    }
    Some(
        parts
            .next()
            .filter(|s| !s.is_empty())
            .map(str::to_string),
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
/// - `xref:file.adoc#frag[label]` / `xref:file#frag` macros → `frag`;
/// - file-level `xref:file.adoc[label]` macros → `file:<target>` so the linker
///   can attach the edge to the target file's representative doc node.
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
        let target = target.trim();
        if target.is_empty() || target.contains('{') || target.contains(' ') || target.len() >= 120
        {
            return;
        }
        match target.rsplit_once('#') {
            Some((_, frag)) => {
                let frag = frag.trim();
                if !frag.is_empty() {
                    out.push((line_idx, format!("ref:{frag}")));
                }
            }
            None if is_adoc_file_target(target) => {
                out.push((line_idx, format!("file:{target}")));
            }
            None => {
                out.push((line_idx, format!("ref:{target}")));
            }
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
        // xref:path#frag[label], xref:path.adoc[label], or bare xref:path#frag
        // macro. Match on the
        // BYTE slice: `i` walks bytes, so `&line[i..]` would panic mid-way
        // through a multibyte char (em-dash, typographic quote).
        if bytes[i..].starts_with(b"xref:") {
            let after = &line[i + 5..];
            let target_end = after
                .find(|ch: char| ch == '[' || ch.is_whitespace())
                .unwrap_or(after.len());
            let target = &after[..target_end];
            push(target, out);
            i += 5 + target_end;
            continue;
        }
        i += 1;
    }
}

fn is_adoc_file_target(target: &str) -> bool {
    let path = target.split_once('#').map(|(p, _)| p).unwrap_or(target);
    matches!(
        std::path::Path::new(path).extension().and_then(|e| e.to_str()),
        Some("adoc" | "asciidoc" | "asc")
    )
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
        let (meta, custom, body, title) = parse_document_attributes(src);

        // Standard metadata
        assert!(meta.is_some());
        let m = meta.unwrap();
        assert_eq!(m.date.as_deref(), Some("2026-06-07"));
        assert_eq!(title.as_deref(), Some("AsciiDoc Mastery"));

        // Custom attributes captured
        assert_eq!(
            custom.get("tags").map(|s| s.as_str()),
            Some("asciidoc, aden, knowledge-graph")
        );
        assert_eq!(custom.get("status").map(|s| s.as_str()), Some("draft"));
        assert_eq!(custom.get("docinfo").map(|s| s.as_str()), Some("shared"));
        assert!(
            custom.contains_key("sectanchors"),
            "sectanchors must be captured"
        );
        assert_eq!(custom.get("toc").map(|s| s.as_str()), Some("left"));

        // Body does not include header lines
        assert!(
            !body.contains(":tags:"),
            "body must not contain attribute lines"
        );
        assert!(
            body.contains("== First Section"),
            "body must contain section headings"
        );
    }

    #[test]
    fn parse_document_attributes_empty_when_no_header() {
        let src = "Just plain content\nwith no header.\n";
        let (meta, custom, body, title) = parse_document_attributes(src);
        assert!(meta.is_none());
        assert!(custom.is_empty());
        assert!(title.is_none());
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
        assert_eq!(
            first.attributes.get("tags").map(|s| s.as_str()),
            Some("aden, graph")
        );
        assert_eq!(
            first.attributes.get("status").map(|s| s.as_str()),
            Some("draft")
        );
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
        assert!(
            has_primary,
            "primary h2 anchor must be present; got: {:?}",
            anchors
        );
        assert!(
            has_alias,
            "sectanchors alias _core_concepts must be present; got: {:?}",
            anchors
        );
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
        assert!(
            a.contains(&"ref:_term_b".to_string()),
            "Section A refs: {a:?}"
        );
        assert!(
            a.contains(&"ref:_term_c".to_string()),
            "Section A refs: {a:?}"
        );
        assert!(
            a.contains(&"ref:_frag".to_string()),
            "xref fragment; got {a:?}"
        );
        assert!(
            !a.iter()
                .any(|r| r.contains("_not_a_ref") || r.contains("_in_ticks")),
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
        let refs = first
            .attributes
            .get("doc_refs")
            .cloned()
            .unwrap_or_default();
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
            primary.attributes.contains_key("doc_refs"),
            "primary must carry the refs"
        );
        let alias = docs
            .iter()
            .find(|d| d.anchor.ends_with("_core_concepts"))
            .expect("alias node");
        assert!(
            !alias.attributes.contains_key("doc_refs"),
            "alias_of node must not duplicate refs (double edges)"
        );
    }

    /// `include::target[]` directives must surface as a `doc_includes` attribute
    /// on the document's representative node, so the link phase can emit a
    /// `Requires` edge. The canonical gen path previously dropped includes.
    #[test]
    fn include_directive_emits_doc_includes_attribute() {
        let ext = AsciiDocExtractor::new();
        let src = "\
= Master

include::chapter-one.adoc[]

== Overview

Body.
";
        let docs = ext
            .extract_documents(src, Path::new("master.adoc"))
            .expect("extraction must succeed");
        let with_inc = docs
            .iter()
            .find(|d| d.attributes.contains_key("doc_includes"))
            .expect("a node must carry doc_includes");
        assert_eq!(
            with_inc.attributes.get("doc_includes").map(String::as_str),
            Some("chapter-one.adoc"),
        );
    }

    /// `include::` inside a delimited listing block is a code example, not a real
    /// directive — it must NOT produce a doc_includes entry.
    #[test]
    fn include_inside_listing_block_is_ignored() {
        let ext = AsciiDocExtractor::new();
        let src = "\
= Master

----
include::not-real.adoc[]
----

Body.
";
        let docs = ext
            .extract_documents(src, Path::new("master.adoc"))
            .expect("extraction must succeed");
        assert!(
            docs.iter()
                .all(|d| !d.attributes.contains_key("doc_includes")),
            "include inside a listing block must be ignored"
        );
    }

    #[test]
    fn source_listing_language_is_preserved() {
        let ext = AsciiDocExtractor::new();
        let src = "\
= Example

[source,rust]
----
fn main() {
    println!(\"hello\");
}
----
";
        let docs = ext
            .extract_documents(src, Path::new("example.adoc"))
            .expect("extraction must succeed");
        let listing = docs
            .iter()
            .find_map(|d| match d.blocks.first() {
                Some(Block::Listing { language, .. }) => language.as_deref(),
                _ => None,
            })
            .expect("listing language must be captured");
        assert_eq!(listing, "rust");
    }

    #[test]
    fn shared_attribute_include_enables_sectanchors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let shared = dir.path().join("common.adoc");
        std::fs::write(&shared, ":sectanchors:\n:tags: shared\n").expect("write shared attrs");
        let page = dir.path().join("guide.adoc");
        std::fs::write(
            &page,
            "\
= Guide
include::common.adoc[]

== Core Concepts

Body.
",
        )
        .expect("write page");

        let ext = AsciiDocExtractor::new();
        let source = std::fs::read_to_string(&page).expect("read page");
        let docs = ext
            .extract_documents(&source, &page)
            .expect("extraction must succeed");
        assert!(
            docs.iter().any(|d| d.anchor.ends_with("_core_concepts")),
            "included :sectanchors: should generate heading alias; got {:?}",
            docs.iter().map(|d| &d.anchor).collect::<Vec<_>>()
        );
        assert!(
            docs.iter()
                .any(|d| d.attributes.get("tags").map(String::as_str) == Some("shared")),
            "included custom attributes should propagate"
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
    fn file_level_xref_emits_file_ref() {
        let mut out = Vec::new();
        collect_prose_refs("See xref:guide.adoc[the guide].", 0, &mut out);
        assert_eq!(out, vec![(0usize, "file:guide.adoc".to_string())]);
    }

    #[test]
    fn explicit_anchor_above_heading_carries_no_doc_refs() {
        let ext = AsciiDocExtractor::new();
        let src = "\
= Doc

[[adr-2]]
== Second decision

This superseded <<adr-1>>.

[[adr-1]]
== First decision

Original approach.
";
        let docs = ext
            .extract_documents(src, Path::new("adr.adoc"))
            .expect("extraction must succeed");

        let with_refs: Vec<_> = docs
            .iter()
            .filter(|d| d.attributes.contains_key("doc_refs"))
            .collect();
        assert_eq!(
            with_refs.len(),
            1,
            "only the canonical section node may own doc_refs; got {:?}",
            docs.iter()
                .filter(|d| d.attributes.contains_key("doc_refs"))
                .map(|d| &d.anchor)
                .collect::<Vec<_>>()
        );
        assert!(
            with_refs[0].anchor.contains("h2second-decision"),
            "canonical section must own the ref"
        );

        let alias = docs
            .iter()
            .find(|d| d.anchor.ends_with("#adr-2"))
            .expect("explicit anchor node");
        assert_eq!(
            alias.attributes.get("alias_of").map(String::as_str),
            Some(with_refs[0].anchor.as_str()),
            "explicit anchor must alias the following heading"
        );
        assert!(
            !alias.attributes.contains_key("doc_refs"),
            "alias must not duplicate refs"
        );
    }

    #[test]
    fn block_anchor_hash_syntax_declares_fragment() {
        let ext = AsciiDocExtractor::new();
        let src = "\
= Doc

[#adr-5]
== Fifth decision

Content five.

[[adr-6]]
== Sixth decision

Refers to <<adr-5>>.
";
        let docs = ext
            .extract_documents(src, Path::new("hash.adoc"))
            .expect("extraction must succeed");
        assert!(
            docs.iter().any(|d| d.anchor.ends_with("#adr-5")),
            "[#adr-5] must declare a fragment node; got {:?}",
            docs.iter().map(|d| &d.anchor).collect::<Vec<_>>()
        );
        let sixth = docs
            .iter()
            .find(|d| d.anchor.contains("h2sixth-decision"))
            .expect("sixth section");
        let refs = sixth
            .attributes
            .get("doc_refs")
            .cloned()
            .unwrap_or_default();
        assert!(
            refs.contains("ref:adr-5"),
            "xref to [#adr-5] must resolve; got {refs:?}"
        );
    }

    #[test]
    fn glossary_document_title_gates_term_extraction() {
        let ext = AsciiDocExtractor::new();
        let src = "\
= Glossary

Linker:: The component that resolves refs.
Resolver:: The sibling component.
";
        let docs = ext
            .extract_documents(src, Path::new("definitions.adoc"))
            .expect("extraction must succeed");
        let term_anchors: Vec<_> = docs
            .iter()
            .filter(|d| d.anchor.starts_with("aden://term/"))
            .map(|d| d.anchor.as_str())
            .collect();
        assert!(
            term_anchors.iter().any(|a| a.contains("linker")),
            "= Glossary title must gate term extraction; got {term_anchors:?}"
        );
        assert!(
            term_anchors.iter().any(|a| a.contains("resolver")),
            "= Glossary title must gate term extraction; got {term_anchors:?}"
        );
    }

    #[test]
    fn fragment_only_glossary_promotes_term_nodes() {
        let ext = AsciiDocExtractor::new();
        let src = "\
---
title: Glossary
---

[[_linker]]Linker::
The component that resolves refs.

[[_resolver]]Resolver::
The sibling component.
";
        let docs = ext
            .extract_documents(src, Path::new("glossary.adoc"))
            .expect("extraction must succeed");
        let term_anchors: Vec<_> = docs
            .iter()
            .filter(|d| d.anchor.starts_with("aden://term/"))
            .map(|d| d.anchor.as_str())
            .collect();
        assert_eq!(
            term_anchors.len(),
            2,
            "fragment-only glossary must emit Term nodes; got {term_anchors:?}"
        );
        let linker_frag = docs
            .iter()
            .find(|d| d.anchor.ends_with("#_linker"))
            .expect("linker fragment node");
        assert!(
            linker_frag
                .attributes
                .get("doc_terms")
                .is_some_and(|t| t.starts_with("aden://term/") && t.contains("linker")),
            "fragment node must DefinesTerm its term; attrs={:?}",
            linker_frag.attributes
        );
    }

    #[test]
    fn inactive_ifdef_branch_is_excluded_from_indexing() {
        let ext = AsciiDocExtractor::new();
        let src = "\
= Doc
:draft:

ifdef::draft[]
== Draft Only

See <<_draft_ref>>.
endif::[]

ifndef::draft[]
== Published Only

See <<_published_ref>>.
endif::[]
";
        let docs = ext
            .extract_documents(src, Path::new("draft.adoc"))
            .expect("extraction must succeed");
        let anchors: Vec<&str> = docs.iter().map(|d| d.anchor.as_str()).collect();
        assert!(
            anchors.iter().any(|a| a.contains("h2draft-only")),
            "active ifdef branch must be indexed; got {anchors:?}"
        );
        assert!(
            !anchors.iter().any(|a| a.contains("published")),
            "inactive ifndef branch must be excluded; got {anchors:?}"
        );
        let draft = docs
            .iter()
            .find(|d| d.anchor.contains("h2draft-only"))
            .expect("draft section");
        let refs = draft
            .attributes
            .get("doc_refs")
            .cloned()
            .unwrap_or_default();
        assert!(
            refs.contains("ref:_draft_ref"),
            "refs from active branch only; got {refs:?}"
        );
        assert!(
            !refs.contains("ref:_published_ref"),
            "inactive branch refs must not appear; got {refs:?}"
        );
    }

    #[test]
    fn included_chapter_content_shapes_indexed_headings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let chapter = dir.path().join("chapter.adoc");
        std::fs::write(&chapter, "== Included Chapter\n\nBody.\n").expect("write chapter");
        let master = dir.path().join("master.adoc");
        std::fs::write(
            &master,
            "= Master\n\ninclude::chapter.adoc[]\n",
        )
        .expect("write master");

        let ext = AsciiDocExtractor::new();
        let source = std::fs::read_to_string(&master).expect("read master");
        let docs = ext
            .extract_documents(&source, &master)
            .expect("extraction must succeed");
        assert!(
            docs.iter()
                .any(|d| d.anchor.contains("h2included-chapter")),
            "preprocessed include must promote chapter heading into master; got {:?}",
            docs.iter().map(|d| &d.anchor).collect::<Vec<_>>()
        );
    }

    #[test]
    fn include_leveloffset_adjusts_promoted_heading_level() {
        let dir = tempfile::tempdir().expect("tempdir");
        let part = dir.path().join("part.adoc");
        std::fs::write(&part, "= Part Title\n\nPart body.\n").expect("write part");
        let master = dir.path().join("book.adoc");
        std::fs::write(
            &master,
            "= Book\n\ninclude::part.adoc[leveloffset=+1]\n",
        )
        .expect("write book");

        let ext = AsciiDocExtractor::new();
        let source = std::fs::read_to_string(&master).expect("read book");
        let docs = ext
            .extract_documents(&source, &master)
            .expect("extraction must succeed");
        assert!(
            docs.iter().any(|d| d.anchor.contains("h2part-title")),
            "leveloffset=+1 should promote = Part to ==; got {:?}",
            docs.iter().map(|d| &d.anchor).collect::<Vec<_>>()
        );
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
