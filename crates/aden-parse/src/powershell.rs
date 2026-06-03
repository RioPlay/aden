// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// All rights reserved.
//
// Aden: A Dense Referential Context Compiler
// Original author and maintainer: RioPlay
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
use aden_core::{Block, Document, Error, NodeType, Parameter, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct PsAstOutput {
    #[serde(rename = "File")]
    file: String,
    #[serde(rename = "Functions")]
    functions: Vec<PsFunction>,
    #[serde(rename = "Types")]
    types: Vec<PsType>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct PsFunction {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Type")]
    ty: String,
    #[serde(default, rename = "Parameters")]
    parameters: Vec<String>,
    #[serde(rename = "Text")]
    text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct PsType {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Type")]
    ty: String,
    #[serde(default, rename = "Members")]
    members: Vec<String>,
    #[serde(rename = "Text")]
    text: String,
}

/// Compute a `SourceSpan` for a PowerShell symbol by locating its verbatim
/// `Extent.Text` within the source. The PS adapter does not emit line/offset
/// data, so we recover the span here — without it every symbol had no span and
/// span-based navigation (jump-to-definition) was impossible.
fn span_for_text(text: &str, source: &str, path: &Path) -> aden_core::SourceSpan {
    let file = path.to_string_lossy().to_string();
    if let Some(start_byte) = source.find(text) {
        let end_byte = start_byte + text.len();
        let start_line = source[..start_byte].bytes().filter(|b| *b == b'\n').count() + 1;
        let end_line = start_line + text.bytes().filter(|b| *b == b'\n').count();
        aden_core::SourceSpan {
            file,
            start_line,
            end_line,
            start_byte,
            end_byte,
        }
    } else {
        // Text not found verbatim (e.g. CRLF normalization): fall back to a
        // whole-file span rather than dropping the span entirely.
        aden_core::SourceSpan {
            file,
            start_line: 1,
            end_line: source.bytes().filter(|b| *b == b'\n').count() + 1,
            start_byte: 0,
            end_byte: source.len(),
        }
    }
}

pub fn extract_documents(path: &Path, source: &str) -> Result<Vec<Document>> {
    // SECURITY: Resolve script only from the binary's directory to avoid
    // cwd-borne attacks. Never fall back to a relative path.
    let script_path = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.join("Export-AST.ps1")))
        .filter(|p| p.is_file())
        .ok_or_else(|| {
            Error::Io(
                "Export-AST.ps1 not found next to aden binary. Install aden properly.".to_string(),
            )
        })?;

    let shell = discover_powershell().ok_or_else(|| {
        Error::Io("PowerShell not available (tried pwsh and powershell)".to_string())
    })?;

    let output = Command::new(shell)
        .arg("-File")
        .arg(&script_path)
        .arg("-Path")
        .arg(path)
        .output()
        .map_err(|e| Error::Io(e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Io(format!("PowerShell adapter failed: {stderr}")));
    }

    let json = String::from_utf8_lossy(&output.stdout);
    const MAX_JSON_SIZE: usize = 50_000_000; // 50 MiB safety limit
    if json.len() > MAX_JSON_SIZE {
        return Err(Error::Io(format!(
            "PowerShell JSON output exceeds {} bytes (got {})",
            MAX_JSON_SIZE,
            json.len()
        )));
    }
    let parsed: PsAstOutput =
        serde_json::from_str(&json).map_err(|e| Error::Parse(e.to_string()))?;

    let mut docs = Vec::new();
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    let crate_name = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    for func in parsed.functions {
        let anchor = crate::make_anchor(&crate_name, &file_name, &func.name);
        let span = span_for_text(&func.text, source, path);
        let mut attrs = HashMap::new();
        attrs.insert(
            "source_hash".to_string(),
            aden_core::stable_hash(func.text.as_bytes()),
        );
        attrs.insert("last-verified".to_string(), aden_core::rfc3339_now());
        attrs.insert("node-type".to_string(), "function".to_string());
        attrs.insert(
            "source_file".to_string(),
            path.to_string_lossy().to_string(),
        );
        let mut blocks = Vec::new();
        let params: Vec<Parameter> = func
            .parameters
            .iter()
            .map(|p| Parameter {
                name: p.clone(),
                ty: "Object".to_string(),
                default_value: None,
            })
            .collect();
        let rows: Vec<Vec<String>> = params
            .iter()
            .map(|p| vec![format!("param {}", p.name), p.ty.clone()])
            .collect();
        if !rows.is_empty() {
            blocks.push(Block::Table(aden_core::Table {
                headers: vec!["Property".to_string(), "Value".to_string()],
                rows,
            }));
        }
        docs.push(Document {
            anchor,
            node_type: NodeType::Function,
            attributes: attrs,
            blocks,
            source_span: Some(span),
            metadata: None,
            confidence: 0.9,
        });
    }

    for ty in parsed.types {
        let anchor = crate::make_anchor(&crate_name, &file_name, &ty.name);
        let span = span_for_text(&ty.text, source, path);
        let mut attrs = HashMap::new();
        attrs.insert(
            "source_hash".to_string(),
            aden_core::stable_hash(ty.text.as_bytes()),
        );
        attrs.insert("last-verified".to_string(), aden_core::rfc3339_now());
        attrs.insert("node-type".to_string(), "type".to_string());
        attrs.insert(
            "source_file".to_string(),
            path.to_string_lossy().to_string(),
        );
        let mut blocks = Vec::new();
        let mut rows = vec![vec!["Kind".to_string(), ty.ty.clone()]];
        for m in &ty.members {
            rows.push(vec![format!("member {}", m), m.clone()]);
        }
        blocks.push(Block::Table(aden_core::Table {
            headers: vec!["Property".to_string(), "Value".to_string()],
            rows,
        }));
        docs.push(Document {
            anchor,
            node_type: NodeType::Type,
            attributes: attrs,
            blocks,
            source_span: Some(span),
            metadata: None,
            confidence: 0.9,
        });
    }

    Ok(docs)
}

/// Discover a safe PowerShell executable path, avoiding cwd-borne attacks.
fn discover_powershell() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let candidates = [
            PathBuf::from(r"C:\Program Files\PowerShell\7\pwsh.exe"),
            PathBuf::from(r"C:\Program Files\PowerShell\6\pwsh.exe"),
            PathBuf::from(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"),
        ];
        for c in &candidates {
            if c.is_file() {
                return Some(c.clone());
            }
        }
    }
    #[cfg(not(windows))]
    {
        // On Unix, search PATH explicitly to avoid cwd resolution.
        for dir in std::env::split_paths(&std::env::var_os("PATH")?) {
            for name in ["pwsh", "powershell"] {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}
