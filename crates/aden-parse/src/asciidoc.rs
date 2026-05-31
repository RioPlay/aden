// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! AsciiDoc extractor for Aden.
//!
//! Extracts headings, code blocks, and links from AsciiDoc files.

use crate::extractor::{build_code_attributes, make_anchor};
use aden_core::{Block, Document, NodeType, Result, SourceSpan};
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

        let (attributes, body) = parse_document_attributes(source);
        let body_lines: Vec<&str> = body.lines().collect();
        let mut headings = Vec::new();
        let mut code_blocks = Vec::new();
        let _in_literal_block = false;
        let mut current_code_lines = Vec::new();
        let mut in_listing_block = false;

        for (line_num, line) in body.lines().enumerate() {
            let line_num = line_num + 1;

            if line == "----" {
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
            } else if line.contains("[[") && line.contains("]]") {
                let start = line.find("[[").unwrap() + 2;
                let end = line.find("]]").unwrap();
                let anchor_name = &line[start..end];
                if !anchor_name.is_empty() {
                    headings.push((0, anchor_name.to_string(), line_num));
                }
            }
        }

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
                    body_lines[body_start..body_end].join("\n").trim().to_string()
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
                let mut attrs = build_code_attributes(
                    source,
                    "heading",
                    Some(path),
                    Some(&span),
                );
                if level > 0 {
                    attrs.insert("heading_level".to_string(), level.to_string());
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
                    metadata: attributes.clone(),
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
                    "AsciiDoc document: {}",
                    file_name
                ))],
                source_span: None,
                metadata: attributes,
                confidence: 0.7,
            });
        }

        for (lang, code) in code_blocks {
            let anchor = make_anchor(
                &crate_name,
                &file_name,
                &format!("code_block_{}", docs.len()),
            );
            let lang_str = lang.as_deref().unwrap_or("");
            let references = extract_code_references(&code, lang_str);
            let mut attrs = build_code_attributes(&code, "code", Some(path), None);
            if !references.is_empty() {
                attrs.insert("symbol_references".to_string(), references.join(","));
            }
            docs.push(Document {
                anchor,
                node_type: NodeType::Script,
                attributes: attrs,
                blocks: vec![Block::Listing { language: lang, code }],
                source_span: None,
                metadata: None,
                confidence: 0.8,
            });
        }

        Ok(docs)
    }
}

fn parse_document_attributes(
    source: &str,
) -> (Option<aden_core::DocumentMetadata>, String) {
    if !source.starts_with(":") {
        return (None, source.to_string());
    }

    let mut metadata = aden_core::DocumentMetadata::default();
    let mut end_line = 0;

    for (i, line) in source.lines().enumerate() {
        if i == 0 && !line.starts_with(':') {
            break;
        }
        if line.starts_with(':') && line.contains(':') {
            let content = line.trim_start_matches(':').trim_end_matches(':');
            if let Some((key, value)) = content.split_once(':') {
                let key = key.trim();
                let value = value.trim();
                match key {
                    "title" => metadata.version = Some(value.to_string()),
                    "author" => metadata.author = Some(value.to_string()),
                    "email" => metadata.email = Some(value.to_string()),
                    "revdate" => metadata.date = Some(value.to_string()),
                    "version" => metadata.version = Some(value.to_string()),
                    "revision" => metadata.revision = Some(value.to_string()),
                    "copyright" => metadata.copyright = Some(value.to_string()),
                    "license" => metadata.license = Some(value.to_string()),
                    _ => {}
                }
            }
            end_line = i;
        } else {
            break;
        }
    }

    let body = if end_line > 0 {
        source.lines().skip(end_line + 1).collect::<Vec<_>>().join("\n")
    } else {
        source.to_string()
    };

    (Some(metadata), body)
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