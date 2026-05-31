// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Markdown extractor for Aden.
//!
//! Extracts headings, code blocks, and links from markdown files.

use crate::extractor::{build_code_attributes, make_anchor};
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
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let crate_name = infer_project_name(path);

        let (frontmatter, body) = parse_frontmatter(source);
        let body_lines: Vec<&str> = body.lines().collect();
        let mut headings = Vec::new();
        let mut code_blocks = Vec::new();
        let mut links = Vec::new();
        let mut in_code_block = false;
        let mut current_code_lang = String::new();
        let mut current_code_lines = Vec::new();

        for (line_num, line) in body.lines().enumerate() {
            let line_num = line_num + 1;

            if line.starts_with("```") {
                if in_code_block {
                    code_blocks.push((
                        current_code_lang.clone(),
                        current_code_lines.join("\n"),
                    ));
                    current_code_lines.clear();
                    current_code_lang.clear();
                    in_code_block = false;
                } else {
                    in_code_block = true;
                    current_code_lang = line.trim_start_matches("```").to_string();
                }
                continue;
            }

            if !in_code_block
                && let Some(link) = extract_markdown_link(line) {
                    links.push(link);
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
                    body_lines[body_start..body_end].join("\n").trim().to_string()
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
                let attrs = build_code_attributes(
                    source,
                    "heading",
                    Some(path),
                    Some(&span),
                );
                let mut attrs = attrs;
                attrs.insert("heading_level".to_string(), level.to_string());
                if !links.is_empty() {
                    attrs.insert("links".to_string(), links.join(";"));
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
            let attrs = build_code_attributes(source, "document", Some(path), None);
            docs.push(Document {
                anchor,
                node_type: NodeType::Module,
                attributes: attrs,
                blocks: vec![Block::Paragraph(format!(
                    "Markdown document: {}",
                    file_name
                ))],
                source_span: None,
                metadata: frontmatter,
                confidence: 0.7,
            });
        }

        for (lang, code) in code_blocks {
            let anchor = make_anchor(
                &crate_name,
                &file_name,
                &format!("code_block_{}", docs.len()),
            );
            let references = extract_code_references(&code, &lang);
            let mut attrs = build_code_attributes(&code, "code", Some(path), None);
            if !references.is_empty() {
                attrs.insert("symbol_references".to_string(), references.join(","));
            }
            docs.push(Document {
                anchor,
                node_type: NodeType::Script,
                attributes: attrs,
                blocks: vec![Block::Listing {
                    language: if lang.is_empty() {
                        None
                    } else {
                        Some(lang)
                    },
                    code,
                }],
                source_span: None,
                metadata: None,
                confidence: 0.8,
            });
        }

        Ok(docs)
    }
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

fn extract_code_references(code: &str, lang: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let lang_lower = lang.to_lowercase();
    match lang_lower.as_str() {
        "rust" => {
            for line in code.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("fn ") {
                    if let Some(name) = trimmed.strip_prefix("fn ") {
                        let name = name.split('(').next().unwrap_or(name).split('{').next().unwrap_or(name);
                        refs.push(format!("fn:{}", name.trim()));
                    }
                } else if trimmed.starts_with("struct ") {
                    if let Some(name) = trimmed.strip_prefix("struct ") {
                        let name = name.split_whitespace().next().unwrap_or(name);
                        refs.push(format!("struct:{}", name));
                    }
                } else if trimmed.starts_with("enum ") {
                    if let Some(name) = trimmed.strip_prefix("enum ") {
                        let name = name.split_whitespace().next().unwrap_or(name);
                        refs.push(format!("enum:{}", name));
                    }
                } else if trimmed.starts_with("impl ") || trimmed.starts_with("trait ") {
                    if let Some(name) = trimmed.split_whitespace().nth(1) {
                        refs.push(format!("type:{}", name));
                    }
                } else if trimmed.starts_with("use ") {
                    if let Some(name) = trimmed.strip_prefix("use ") {
                        let name = name.split_whitespace().next().unwrap_or(name);
                        let name = name.trim_end_matches(';');
                        refs.push(format!("use:{}", name));
                    }
                } else if trimmed.contains("::") {
                    let parts: Vec<&str> = trimmed.split("::").collect();
                    if parts.len() >= 2 {
                        refs.push(format!("mod:{}", parts[0]));
                    }
                }
            }
        }
        "python" => {
            for line in code.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("def ") {
                    if let Some(name) = trimmed.strip_prefix("def ") {
                        let name = name.split('(').next().unwrap_or(name);
                        refs.push(format!("fn:{}", name.trim()));
                    }
                } else if trimmed.starts_with("class ") {
                    if let Some(name) = trimmed.strip_prefix("class ") {
                        let name = name.split('(').next().unwrap_or(name);
                        refs.push(format!("class:{}", name.trim()));
                    }
                } else if trimmed.starts_with("import ") || trimmed.starts_with("from ") {
                    let name = trimmed.split_whitespace().nth(1).unwrap_or(trimmed);
                    refs.push(format!("use:{}", name));
                }
            }
        }
        "javascript" | "typescript" | "js" | "ts" => {
            for line in code.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("function ") {
                    if let Some(name) = trimmed.strip_prefix("function ") {
                        let name = name.split('(').next().unwrap_or(name);
                        refs.push(format!("fn:{}", name.trim()));
                    }
                } else if trimmed.starts_with("const ") && trimmed.contains("=>") {
                    if let Some(name) = trimmed.strip_prefix("const ") {
                        let name = name.split('=').next().unwrap_or(name);
                        refs.push(format!("fn:{}", name.trim()));
                    }
                } else if trimmed.starts_with("class ") {
                    if let Some(name) = trimmed.strip_prefix("class ") {
                        let name = name.split('{').next().unwrap_or(name);
                        refs.push(format!("class:{}", name.trim()));
                    }
                } else if (trimmed.starts_with("interface ") || trimmed.starts_with("type "))
                    && let Some(name) = trimmed.split_whitespace().nth(1) {
                        let name = name.split('{').next().unwrap_or(name);
                        refs.push(format!("type:{}", name));
                    }
            }
        }
        "go" => {
            for line in code.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("func ") {
                    if let Some(name) = trimmed.strip_prefix("func ") {
                        let name = name.split('(').next().unwrap_or(name);
                        refs.push(format!("fn:{}", name.trim()));
                    }
                } else if trimmed.starts_with("type ") {
                    if let Some(name) = trimmed.strip_prefix("type ") {
                        let name = name.split_whitespace().next().unwrap_or(name);
                        refs.push(format!("type:{}", name));
                    }
                } else if trimmed.starts_with("import ")
                    && let Some(name) = trimmed.strip_prefix("import ") {
                        let name = name.trim_matches('"');
                        refs.push(format!("use:{}", name));
                    }
            }
        }
        _ => {}
    }
    refs
}

fn infer_project_name(path: &Path) -> String {
    path.ancestors()
        .find(|p| {
            p.join("Cargo.toml").exists()
                || p.join("package.json").exists()
                || p.join("pyproject.toml").exists()
                || p.join("setup.py").exists()
                || p.join("go.mod").exists()
                || p.join("tsconfig.json").exists()
        })
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
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
        && let Some(bracket_end) = line.find("](") {
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