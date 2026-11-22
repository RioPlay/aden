// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use super::AgentCliError;
use aden_core::source::StoredSourceReader;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::path::Path;

const MANIFEST: &str = "aden-findings.toml";
const MAX_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_SPANS: usize = 128;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    findings: Vec<Finding>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Finding {
    id: String,
    claim: Span,
    evidence: Vec<Span>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Span {
    file: String,
    start_line: usize,
    end_line: usize,
    source_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    relation: Option<Relation>,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Relation {
    Supports,
    Contradicts,
    Tests,
}

impl Relation {
    fn label(self) -> &'static str {
        match self {
            Self::Supports => "supports",
            Self::Contradicts => "contradicts",
            Self::Tests => "tests",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum SpanState {
    Current,
    Changed,
    Unavailable,
    RangeUnavailable,
}

impl SpanState {
    fn label(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Changed => "changed",
            Self::Unavailable => "unavailable",
            Self::RangeUnavailable => "range_unavailable",
        }
    }
}

#[derive(Serialize)]
struct SpanReport<'a> {
    #[serde(flatten)]
    span: &'a Span,
    state: &'static str,
}

#[derive(Serialize)]
struct FindingReport<'a> {
    id: &'a str,
    state: &'static str,
    claim: SpanReport<'a>,
    evidence: Vec<SpanReport<'a>>,
}

#[derive(Serialize)]
struct FindingsGate {
    ok: bool,
    matched: usize,
    needs_review: usize,
    contradictions: usize,
}

impl FindingsGate {
    fn evaluate(findings: &[FindingReport<'_>]) -> Self {
        let matched = findings.len();
        let needs_review = findings
            .iter()
            .filter(|finding| finding.state == "needs_review")
            .count();
        let contradictions = findings
            .iter()
            .filter(|finding| {
                finding
                    .evidence
                    .iter()
                    .any(|entry| matches!(entry.span.relation, Some(Relation::Contradicts)))
            })
            .count();
        Self {
            ok: matched > 0 && needs_review == 0 && contradictions == 0,
            matched,
            needs_review,
            contradictions,
        }
    }
}

struct Snapshot {
    hash: String,
    lines: usize,
}

fn manifest_error(message: impl Into<String>) -> AgentCliError {
    AgentCliError::new(
        "invalid_findings_manifest",
        message,
        "Check aden-findings.toml and use source hashes returned by aden read.",
    )
}

fn portable_file(file: &str) -> bool {
    !file.contains(['\\', ':'])
        && !file.chars().any(char::is_control)
        && file.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

fn validate_manifest(manifest: &Manifest) -> Result<(), AgentCliError> {
    if manifest.version != 1 {
        return Err(manifest_error("Findings manifest version must be 1"));
    }
    let mut ids = HashSet::new();
    let mut span_count = 0;
    for (index, finding) in manifest.findings.iter().enumerate() {
        let id = finding.id.as_str();
        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || !ids.insert(id)
        {
            return Err(manifest_error(format!(
                "Invalid or duplicate ID in finding {}",
                index + 1
            )));
        }
        span_count += 1 + finding.evidence.len();
        if span_count > MAX_SPANS {
            return Err(manifest_error(format!(
                "Findings manifest exceeds {MAX_SPANS} spans"
            )));
        }
        if finding.evidence.is_empty() {
            return Err(manifest_error(format!(
                "Finding {id} requires at least one evidence span"
            )));
        }
        for (span_index, span) in std::iter::once(&finding.claim)
            .chain(&finding.evidence)
            .enumerate()
        {
            if !portable_file(&span.file)
                || span.start_line == 0
                || span.end_line < span.start_line
                || span.source_hash.len() != 16
                || !span
                    .source_hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                || span.relation.is_some() != (span_index != 0)
            {
                return Err(manifest_error(format!(
                    "Invalid span {} in finding {id}",
                    span_index + 1
                )));
            }
        }
    }
    Ok(())
}

fn span_state<'a>(
    reader: &StoredSourceReader,
    cache: &mut HashMap<&'a str, Option<Snapshot>>,
    span: &'a Span,
) -> SpanState {
    let snapshot = cache.entry(&span.file).or_insert_with(|| {
        let source = reader.read(Path::new(&span.file)).ok()?;
        Some(Snapshot {
            hash: aden_core::stable_hash(source.as_bytes()),
            lines: source.split_inclusive('\n').count(),
        })
    });
    match snapshot {
        None => SpanState::Unavailable,
        Some(snapshot) if snapshot.lines < span.end_line => SpanState::RangeUnavailable,
        Some(snapshot) if snapshot.hash != span.source_hash => SpanState::Changed,
        Some(_) => SpanState::Current,
    }
}

fn render_span(span: &SpanReport<'_>) -> String {
    format!(
        "{}:{}-{} [{}]",
        span.span.file, span.span.start_line, span.span.end_line, span.state
    )
}

fn write_human_report(
    output: &mut impl Write,
    findings: &[FindingReport<'_>],
    gate: Option<&FindingsGate>,
) -> io::Result<()> {
    if findings.is_empty() {
        writeln!(output, "No authored findings match.")?;
    }
    for finding in findings {
        writeln!(output, "{} [{}]", finding.id, finding.state)?;
        writeln!(output, "  Claim: {}", render_span(&finding.claim))?;
        for evidence in &finding.evidence {
            let relation = evidence.span.relation.expect("validated evidence relation");
            writeln!(
                output,
                "  {} claim: {}",
                relation.label(),
                render_span(evidence)
            )?;
        }
    }
    if let Some(gate) = gate {
        writeln!(
            output,
            "Findings check {} ({} matched, {} need review, {} with contradictions).",
            if gate.ok { "passed" } else { "failed" },
            gate.matched,
            gate.needs_review,
            gate.contradictions,
        )?;
    }
    Ok(())
}

pub fn cmd_findings(
    path: &Path,
    file: Option<&str>,
    line: Option<usize>,
    check: bool,
    json_output: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if file.is_some_and(|file| !portable_file(file)) || line == Some(0) {
        return Err(AgentCliError::new(
            "invalid_findings_selector",
            "Invalid findings file or line selector",
            "Use a project-relative path with forward slashes and a 1-based line.",
        )
        .into());
    }
    let reader = StoredSourceReader::within_checkout(path)?;
    let source = reader.read(Path::new(MANIFEST)).map_err(|_| {
        AgentCliError::new(
            "findings_manifest_unavailable",
            "Findings manifest is unavailable",
            "Provide a readable aden-findings.toml at the project root.",
        )
    })?;
    if source.len() > MAX_MANIFEST_BYTES {
        return Err(manifest_error("Findings manifest exceeds 64 KiB").into());
    }
    let manifest: Manifest = toml::from_str(&source)
        .map_err(|_| manifest_error("Findings manifest does not match the version 1 schema"))?;
    validate_manifest(&manifest)?;

    let mut cache = HashMap::new();
    let mut report = Vec::new();
    for finding in &manifest.findings {
        let spans = std::iter::once(&finding.claim).chain(&finding.evidence);
        if let Some(file) = file
            && !spans.clone().any(|span| {
                span.file == file
                    && line.is_none_or(|line| (span.start_line..=span.end_line).contains(&line))
            })
        {
            continue;
        }
        let mut current = true;
        let mut inspect = |span| {
            let state = span_state(&reader, &mut cache, span);
            current &= state == SpanState::Current;
            SpanReport {
                span,
                state: state.label(),
            }
        };
        let claim = inspect(&finding.claim);
        let evidence = finding.evidence.iter().map(inspect).collect();
        report.push(FindingReport {
            id: &finding.id,
            state: if current { "current" } else { "needs_review" },
            claim,
            evidence,
        });
    }

    let gate = check.then(|| FindingsGate::evaluate(&report));
    let mut output = io::stdout().lock();
    let written = if json_output {
        let mut value = serde_json::json!({
            "schema_version": 1,
            "hash_algorithm": "aden-core-stable-v1",
            "provenance": "authored",
            "edge_direction": "evidence_to_claim",
            "findings": report,
        });
        if let Some(gate) = &gate {
            value["gate"] = serde_json::to_value(gate)?;
        }
        writeln!(output, "{}", serde_json::to_string_pretty(&value)?)
    } else {
        write_human_report(&mut output, &report, gate.as_ref())
    }
    .and_then(|()| output.flush());
    if let Err(error) = written
        && error.kind() != io::ErrorKind::BrokenPipe
    {
        return Err(error.into());
    }
    drop(output);
    if gate.is_some_and(|gate| !gate.ok) {
        std::process::exit(1);
    }
    Ok(())
}
