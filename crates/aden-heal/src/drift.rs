// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

/// Prose file extensions that are never expected to resolve to a code symbol.
///
/// Add a new entry here when Aden begins tracking another documentation format.
const PROSE_EXTENSIONS: &[&str] = &[".adoc", ".md", ".rst", ".txt"];

/// Whether an anchor is *expected* to have no live source symbol.
///
/// Reference docs (doc headings, ADRs, plans, use-cases, agent docs, README)
/// normally have no backing code symbol, so they must not be reported as
/// actionable `OrphanAnchor` drift. Single source of truth shared by the heal
/// scanner and the CLI's `classify_orphans`/`is_expected_metadata` so they can
/// never disagree on what counts as a real orphan.
pub fn is_expected_metadata(anchor: &str) -> bool {
    if anchor.starts_with("aden://doc/")
        || anchor.starts_with("adr-")
        || anchor.starts_with("plan-")
        || anchor.starts_with("use-case-")
        || anchor.starts_with("agent-")
        || anchor.starts_with("mod-") // synthesized module hub nodes (mod-*, mod-project)
        || anchor == "readme"
    {
        return true;
    }

    // Split `aden://module/{crate}/{file}#{symbol}` into the file path and the
    // symbol fragment so we can recognize doc-backed anchors that use the
    // `module/` prefix (e.g. `aden://module/aden/ai-integration.adoc#code_block_3`).
    let (file_part, frag) = match anchor.rsplit_once('#') {
        Some((f, frag)) => (f, frag),
        None => (anchor, ""),
    };

    // Generated doc snippets carry no backing source symbol.
    if frag.starts_with("code_block_") {
        return true;
    }

    // The "source" is itself a prose document (Markdown/AsciiDoc/reST), so it
    // is never expected to resolve to a code symbol.
    PROSE_EXTENSIONS.iter().any(|ext| file_part.ends_with(ext))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DriftEvent {
    StaleHash {
        target_path: String,
        expected_hash: String,
        actual_hash: String,
    },
    SignatureMismatch {
        anchor: String,
        contract_path: String,
        expected_sig: Vec<String>,
        actual_sig: Vec<String>,
    },
    MissingContract {
        source_path: String,
        anchor: String,
        symbol_name: String,
    },
    OrphanAnchor {
        anchor: String,
        contract_path: String,
    },
    BrokenReference {
        contract_path: String,
        ref_anchor: String,
        line: usize,
    },
    DeadLink {
        contract_path: String,
        include_path: String,
    },
    MarkdownDrift {
        md_path: String,
        expected_content: String,
        actual_content: String,
    },
    StaleMarkdown {
        md_path: String,
        source_files_changed: Vec<String>,
    },
    MissingMarkdownTemplate {
        md_path: String,
        template_source: String,
    },
    /// A documentation file declares a function in a fenced code block whose
    /// parameter arity no longer matches the real symbol in the codebase — the
    /// docs describe a stale API. Detected by parsing the doc's code fences with
    /// the real language parser, so only genuine *declarations* (not call-site
    /// usage examples) are considered.
    DocSignatureDivergence {
        doc_path: String,
        line: usize,
        symbol_name: String,
        documented_params: usize,
        actual_params: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DriftSeverity {
    Critical,
    High,
    Medium,
    Low,
}

impl DriftSeverity {
    pub fn weight(self) -> f64 {
        match self {
            DriftSeverity::Critical => 0.3,
            DriftSeverity::High => 0.2,
            DriftSeverity::Medium => 0.1,
            DriftSeverity::Low => 0.05,
        }
    }
}

impl DriftEvent {
    pub fn severity(&self) -> DriftSeverity {
        match self {
            DriftEvent::StaleHash { .. } => DriftSeverity::High,
            DriftEvent::SignatureMismatch { .. } => DriftSeverity::High,
            DriftEvent::MissingContract { .. } => DriftSeverity::Medium,
            DriftEvent::OrphanAnchor { .. } => DriftSeverity::Medium,
            DriftEvent::BrokenReference { .. } => DriftSeverity::Critical,
            DriftEvent::DeadLink { .. } => DriftSeverity::Low,
            DriftEvent::MarkdownDrift { .. } => DriftSeverity::High,
            DriftEvent::StaleMarkdown { .. } => DriftSeverity::Medium,
            DriftEvent::MissingMarkdownTemplate { .. } => DriftSeverity::Medium,
            DriftEvent::DocSignatureDivergence { .. } => DriftSeverity::High,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_expected_metadata;

    #[test]
    fn expected_metadata_covers_docs_adrs_and_doc_backed_anchors() {
        // Reference-doc prefixes.
        assert!(is_expected_metadata("aden://doc/aden/commands.adoc#h3heal"));
        assert!(is_expected_metadata("adr-001"));
        assert!(is_expected_metadata("plan-rollout"));
        assert!(is_expected_metadata("readme"));
        assert!(is_expected_metadata("mod-aden-cli"));
        assert!(is_expected_metadata("mod-project"));

        // Doc-backed anchors that use the module/ prefix (regression: these leaked
        // through and flooded OrphanAnchor on every run).
        assert!(is_expected_metadata(
            "aden://module/aden/ai-integration.adoc#code_block_38"
        ));
        assert!(is_expected_metadata("aden://module/aden/notes.md#intro"));
        assert!(is_expected_metadata("aden://module/aden/df.txt#document"));

        // Generated snippet fragments, regardless of file.
        assert!(is_expected_metadata(
            "aden://module/aden-cli/x.rs#code_block_7"
        ));
    }

    #[test]
    fn live_code_symbols_are_not_expected_metadata() {
        // These must remain actionable — they are real code symbols.
        assert!(!is_expected_metadata(
            "aden://module/aden-cli/mcp.rs#default_scope"
        ));
        assert!(!is_expected_metadata(
            "aden://module/aden-parse/rust.rs#RustExtractor::new"
        ));
    }

    #[test]
    fn prose_extensions_const_drives_is_expected_metadata() {
        // Every extension in PROSE_EXTENSIONS must be recognized.
        for ext in super::PROSE_EXTENSIONS {
            let anchor = format!("aden://module/aden/some-doc{ext}#section");
            assert!(
                is_expected_metadata(&anchor),
                "{ext} should be expected metadata but was not"
            );
        }

        // .rst specifically (regression guard: was previously the fourth explicit branch).
        assert!(is_expected_metadata(
            "aden://module/aden/guide.rst#introduction"
        ));

        // A plain Rust source file must NOT be considered prose.
        assert!(!is_expected_metadata(
            "aden://module/aden-heal/src/drift.rs#is_expected_metadata"
        ));
    }
}
