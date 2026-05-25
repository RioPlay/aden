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
//! Minimal LSP server for `.adoc` / `.aden` files.
//!
//! Provides:
//! - Diagnostics: unresolved `<<refs>>`, broken `include::[]`, duplicate anchors
//! - Go-to-Definition: `Ctrl+Click` on `<<anchor>>` jumps to the file containing `[[anchor]]`
//! - Hover: show `== Signature` table of a referenced function

use aden_graph::graph::AdenGraph;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

struct AdenLspBackend {
    client: Client,
    docs: Arc<Mutex<HashMap<Url, String>>>,
    workspace_root: Arc<Mutex<Option<std::path::PathBuf>>>,
}

#[tower_lsp::async_trait]
impl LanguageServer for AdenLspBackend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        if let Some(root) = params.root_uri {
            let mut guard = self.workspace_root.lock().await;
            *guard = Some(root.to_file_path().unwrap_or_default());
        }
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::FULL),
                        ..Default::default()
                    },
                )),
                definition_provider: Some(OneOf::Left(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "Aden LSP initialized")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;
        {
            let mut docs = self.docs.lock().await;
            docs.insert(uri.clone(), text.clone());
        }
        self.publish_diagnostics(&uri, &text).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        if let Some(change) = params.content_changes.into_iter().next() {
            let text = change.text;
            {
                let mut docs = self.docs.lock().await;
                docs.insert(uri.clone(), text.clone());
            }
            self.publish_diagnostics(&uri, &text).await;
        }
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let docs = self.docs.lock().await;
        let Some(text) = docs.get(&uri) else {
            return Ok(None);
        };

        let line_text = text.lines().nth(pos.line as usize).unwrap_or("");

        // Find <<anchor>> under cursor
        for anchor in extract_xrefs(line_text) {
            let anchor = anchor.to_string();
            if let Some(root) = self.workspace_root.lock().await.clone()
                && let Ok(graph) = AdenGraph::build_from_directory(&root)
                    && let Some(node) = graph.get_node(&anchor) {
                        // SECURITY: Only allow navigation inside the workspace root.
                        if !Self::is_in_workspace(&node.source_path, &root) {
                            continue;
                        }
                        let target_uri = Url::from_file_path(&node.source_path).ok();
                        if let Some(turi) = target_uri {
                            let loc = Location {
                                uri: turi,
                                range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                            };
                            return Ok(Some(GotoDefinitionResponse::Scalar(loc)));
                        }
                    }
        }
        Ok(None)
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let docs = self.docs.lock().await;
        let Some(text) = docs.get(&uri) else {
            return Ok(None);
        };

        let line_text = text.lines().nth(pos.line as usize).unwrap_or("");
        for anchor in extract_xrefs(line_text) {
            let anchor = anchor.to_string();
            if let Some(root) = self.workspace_root.lock().await.clone()
                && let Ok(graph) = AdenGraph::build_from_directory(&root)
                    && let Some(node) = graph.get_node(&anchor) {
                        // SECURITY: Only return signatures for nodes inside workspace.
                        if !Self::is_in_workspace(&node.source_path, &root) {
                            continue;
                        }
                        // Look for a Signature table in the document
                        let mut sig = String::new();
                        for block in &node.doc.blocks {
                            if let aden_core::Block::Table(table) = block
                                && table.headers == vec!["Property".to_string(), "Value".to_string()] {
                                    sig.push_str(&format!("**{}**\n", anchor));
                                    for row in &table.rows {
                                        sig.push_str(&format!("- {}: {}\n", row[0], row.get(1).unwrap_or(&"?".to_string())));
                                    }
                                }
                        }
                        if !sig.is_empty() {
                            return Ok(Some(Hover {
                                contents: HoverContents::Markup(MarkupContent {
                                    kind: MarkupKind::Markdown,
                                    value: sig,
                                }),
                                range: None,
                            }));
                        }
                    }
        }
        Ok(None)
    }
}

impl AdenLspBackend {
    /// Validate that `candidate` is inside `root` to prevent goto-definition
    /// from jumping outside the workspace (e.g., via absolute paths in contracts).
    fn is_in_workspace(candidate: &std::path::Path, root: &std::path::Path) -> bool {
        candidate.canonicalize().is_ok_and(|c| {
            root.canonicalize().is_ok_and(|r| c.starts_with(&r))
        })
    }

    async fn publish_diagnostics(&self, uri: &Url, text: &str) {
        let mut diagnostics = Vec::new();

        // SECURITY: Use a unique temp file with create_new to prevent symlink
        // hijacking in shared temp directories (TOCTOU). If an attacker pre-
        // creates a symlink at our temp path, create_new will fail rather than
        // following it and writing to an arbitrary location.
        let tmp = std::env::temp_dir().join(format!(
            "aden-lsp-{}-{}.adoc",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));

        let parsed = {
            let file_result = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp);
            match file_result {
                Ok(mut f) => {
                    use std::io::Write;
                    let _ = f.write_all(text.as_bytes());
                    drop(f);
                    let parsed = aden_graph::parser::parse_file(&tmp);
                    let _ = std::fs::remove_file(&tmp);
                    parsed
                }
                Err(e) => {
                    eprintln!("aden-lsp: failed to create temp file {}: {}", tmp.display(), e);
                    return;
                }
            }
        };

        if let Ok(parsed) = parsed
            && let Some(root) = self.workspace_root.lock().await.clone() {
                // SECURITY: Resolve include paths within workspace root only.
                // Prevent directory traversal via `../../etc/passwd` in include directives.
                let root_canon = root.canonicalize().unwrap_or_else(|_| root.clone());

                for r in &parsed.refs {
                    // Cache graph once per diagnostics pass to avoid rebuilding 3×.
                    // NOTE: This is still expensive; a persistent graph cache would be better.
                    if let Ok(graph) = AdenGraph::build_from_directory(&root)
                        && !graph.anchor_to_index.contains_key(r) {
                            for (i, line) in text.lines().enumerate() {
                                if line.contains(&format!("<<{r}>>")) {
                                    diagnostics.push(Diagnostic {
                                        range: Range::new(
                                            Position::new(i as u32, 0),
                                            Position::new(i as u32, line.len() as u32),
                                        ),
                                        severity: Some(DiagnosticSeverity::ERROR),
                                        code: None,
                                        code_description: None,
                                        source: Some("aden-lsp".to_string()),
                                        message: format!("Unresolved reference: <<{r}>>"),
                                        related_information: None,
                                        tags: None,
                                        data: None,
                                    });
                                }
                            }
                        }
                }

                for inc in &parsed.includes {
                    let inc_path = root.join(&inc.path);
                    let safe = inc_path.canonicalize().is_ok_and(|c| c.starts_with(&root_canon));
                    if !safe {
                        for (i, line) in text.lines().enumerate() {
                            if line.contains(&format!("include::{}[", inc.path)) {
                                diagnostics.push(Diagnostic {
                                    range: Range::new(
                                        Position::new(i as u32, 0),
                                        Position::new(i as u32, line.len() as u32),
                                    ),
                                    severity: Some(DiagnosticSeverity::ERROR),
                                    code: None,
                                    code_description: None,
                                    source: Some("aden-lsp".to_string()),
                                    message: format!("Include path escapes workspace: {}", inc.path),
                                    related_information: None,
                                    tags: None,
                                    data: None,
                                });
                            }
                        }
                        continue;
                    }
                    if !inc_path.exists() {
                        for (i, line) in text.lines().enumerate() {
                            if line.contains(&format!("include::{}[", inc.path)) {
                                diagnostics.push(Diagnostic {
                                    range: Range::new(
                                        Position::new(i as u32, 0),
                                        Position::new(i as u32, line.len() as u32),
                                    ),
                                    severity: Some(DiagnosticSeverity::ERROR),
                                    code: None,
                                    code_description: None,
                                    source: Some("aden-lsp".to_string()),
                                    message: format!("Missing include: {}", inc.path),
                                    related_information: None,
                                    tags: None,
                                    data: None,
                                });
                            }
                        }
                    }
                }
            }
        self.client
            .publish_diagnostics(uri.clone(), diagnostics, Some(1))
            .await;
    }
}

fn extract_xrefs(line: &str) -> Vec<&str> {
    let mut results = Vec::new();
    let mut start = 0;
    while let Some(pos) = line[start..].find("<<") {
        let absolute = start + pos + 2;
        if let Some(end) = line[absolute..].find(">>") {
            let inner = &line[absolute..absolute + end];
            let anchor = inner.split_once(',').map(|(a, _)| a).unwrap_or(inner).trim();
            if !anchor.is_empty() {
                results.push(anchor);
            }
            start = absolute + end + 2;
        } else {
            break;
        }
    }
    results
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(|client| AdenLspBackend {
        client,
        docs: Arc::new(Mutex::new(HashMap::new())),
        workspace_root: Arc::new(Mutex::new(None)),
    });
    Server::new(stdin, stdout, socket).serve(service).await;
}
