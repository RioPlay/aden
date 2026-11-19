// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Opt-in source discovery for C APIs declared by project-specific macros.
//!
//! This is not preprocessing. No command, expansion, parameter type, ABI name,
//! or body call graph is inferred. Only a configured identifier argument and
//! the original invocation with a balanced body are recorded. Configuration:
//!
//! ```toml
//! [macro_declarations]
//! enabled = true
//! [[macro_declarations.rules]]
//! name = "DECLARE_API"
//! name_argument = 0
//! ```
//!
//! Missing, invalid, oversized, or unsupported mappings are inert.
//! C line splices are normalized only for lexing; recorded spans retain original
//! bytes. Spliced macro or API name identifiers are conservatively opaque.

use crate::extractor::{
    CodeSource, infer_project_name, infer_project_root, make_anchor, project_relative_file,
};
use aden_core::{Block, Document, NodeType, SourceSpan};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_CONFIG_BYTES: usize = 64 * 1024;
const MAX_RULES: usize = 64;
const MAX_NAME: usize = 128;
const MAX_ARGUMENTS: usize = 64;
const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;
const MAX_TOKENS: usize = 262_144;
const MAX_NESTING: usize = 64;
const MAX_INVOCATION_BYTES: usize = 64 * 1024;
const MAX_DECLARATIONS: usize = 1024;

/// Locate project-local configuration without crossing a nearer Git boundary.
/// A real `.aden` directory also defines an explicit boundary for tarballs.
/// No global or user-level configuration is consulted. This function does not
/// certify that the file exists or is safe to load.
pub fn project_config_path(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    let absolute = std::fs::canonicalize(parent).ok()?;
    for dir in absolute.ancestors().take(64) {
        if dir.join(".aden").exists() || dir.join(".git").exists() {
            return Some(dir.join(".aden/config.toml"));
        }
    }
    None
}

/// Fingerprint the selected project's mapping input, including missing and
/// rejected states. Callers must propagate read errors rather than reuse old
/// mapped documents. Oversized inputs are inert; only their bounded prefix and
/// length contribute. No configuration contents are returned.
pub fn config_fingerprint(path: &Path) -> aden_core::Result<String> {
    let Some(config) = project_config_path(path) else {
        return Ok(aden_core::hash_source("macro-config:no-project"));
    };
    let mut state = config.to_string_lossy().into_owned();
    for candidate in [config.parent().unwrap(), config.as_path()] {
        match std::fs::symlink_metadata(candidate) {
            Ok(meta) if meta.file_type().is_symlink() => {
                state.push_str(":rejected-link");
                return Ok(aden_core::hash_source(&state));
            }
            Ok(meta) if candidate == config.as_path() && !meta.is_file() => {
                // Check before opening: a FIFO can block inside File::open.
                state.push_str(":rejected-nonfile");
                return Ok(aden_core::hash_source(&state));
            }
            Ok(meta) if candidate != config.as_path() && !meta.is_dir() => {
                state.push_str(":rejected-nondirectory");
                return Ok(aden_core::hash_source(&state));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                state.push_str(":absent");
                return Ok(aden_core::hash_source(&state));
            }
            Err(error) => return Err(aden_core::Error::Io(error.to_string())),
        }
    }
    let file = std::fs::File::open(&config).map_err(|e| aden_core::Error::Io(e.to_string()))?;
    let meta = file
        .metadata()
        .map_err(|e| aden_core::Error::Io(e.to_string()))?;
    if !meta.is_file() {
        state.push_str(":rejected-nonfile");
        return Ok(aden_core::hash_source(&state));
    }
    let mut bytes = Vec::new();
    file.take((MAX_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| aden_core::Error::Io(e.to_string()))?;
    state.push_str(&format!(":{}:", meta.len()));
    // Hash arbitrary bytes losslessly without requiring UTF-8 configuration.
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(state, "{byte:02x}");
    }
    Ok(aden_core::hash_source(&state))
}

fn identifier(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_NAME
        && text
            .bytes()
            .enumerate()
            .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit()))
}

fn parse_rules(text: &str) -> Option<HashMap<String, usize>> {
    if text.len() > MAX_CONFIG_BYTES {
        return None;
    }
    // Value::from_str parses one TOML value, not a configuration document.
    let value = toml::from_str::<toml::Value>(text).ok()?;
    let table = value.get("macro_declarations")?.as_table()?;
    if table.keys().any(|k| k != "enabled" && k != "rules") || !table.get("enabled")?.as_bool()? {
        return None;
    }
    let rules = table.get("rules")?.as_array()?;
    if rules.len() > MAX_RULES {
        return None;
    }
    let mut result = HashMap::new();
    for rule in rules {
        let rule = rule.as_table()?;
        if rule.len() != 2 {
            return None;
        }
        let name = rule.get("name")?.as_str()?;
        let index = usize::try_from(rule.get("name_argument")?.as_integer()?).ok()?;
        if !identifier(name) || index >= MAX_ARGUMENTS || result.contains_key(name) {
            return None;
        }
        result.insert(name.to_owned(), index);
    }
    Some(result)
}

fn load_rules(path: &Path) -> Option<HashMap<String, usize>> {
    let config = project_config_path(path)?;
    // Reject links rather than allowing a repository mapping to load another
    // project's configuration. File reads are capped even if metadata races.
    for candidate in [config.parent()?, config.as_path()] {
        let meta = std::fs::symlink_metadata(candidate).ok()?;
        if meta.file_type().is_symlink() {
            return None;
        }
        if (candidate == config.as_path() && !meta.is_file())
            || (candidate != config.as_path() && !meta.is_dir())
        {
            return None;
        }
    }
    let meta = std::fs::metadata(&config).ok()?;
    if !meta.is_file() || meta.len() > MAX_CONFIG_BYTES as u64 {
        return None;
    }
    let mut bytes = Vec::new();
    let file = std::fs::File::open(config).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_CONFIG_BYTES as u64 {
        return None;
    }
    file.take((MAX_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    parse_rules(std::str::from_utf8(&bytes).ok()?)
}

#[derive(Clone, Copy)]
struct Token<'a> {
    text: &'a str,
    start: usize,
    end: usize,
}

// C translation phase 2 precedes comment/directive recognition. Preserve one
// original offset per retained byte so lexing cannot change emitted spans.
fn lexical_view(source: &str) -> Option<(String, Vec<usize>)> {
    if source.len() > MAX_SOURCE_BYTES {
        return None;
    }
    let bytes = source.as_bytes();
    let mut view = Vec::with_capacity(bytes.len());
    let mut offsets = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"\\\r\n") {
            i += 3;
        } else if bytes[i..].starts_with(b"\\\n") {
            i += 2;
        } else {
            view.push(bytes[i]);
            offsets.push(i);
            i += 1;
        }
    }
    Some((String::from_utf8(view).ok()?, offsets))
}

// A bounded lexical scan keeps comments, strings, and preprocessor directives
// opaque. Incomplete quotes/comments invalidate the scan, never creating names.
fn tokens(source: &str) -> Option<Vec<Token<'_>>> {
    if source.len() > MAX_SOURCE_BYTES {
        return None;
    }
    let bytes = source.as_bytes();
    let mut result = Vec::new();
    let mut i = 0;
    let mut line_start = true;
    while i < bytes.len() {
        let start = i;
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            if b == b'\n' {
                line_start = true;
            }
            i += 1;
            continue;
        }
        if line_start && b == b'#' {
            loop {
                let end = source[i..].find('\n').map(|n| i + n).unwrap_or(bytes.len());
                let continued = source[i..end].trim_end_matches('\r').ends_with('\\');
                i = (end + 1).min(bytes.len());
                if !continued || i == bytes.len() {
                    break;
                }
            }
            continue;
        }
        if bytes[i..].starts_with(b"//") {
            i = source[i..].find('\n').map(|n| i + n).unwrap_or(bytes.len());
            continue;
        }
        if bytes[i..].starts_with(b"/*") {
            let end = source[i + 2..].find("*/")? + i + 4;
            if source[i..end].contains('\n') {
                line_start = true;
            }
            i = end;
            continue;
        }
        line_start = false;
        if b == b'\'' || b == b'"' {
            i += 1;
            loop {
                let next = *bytes.get(i)?;
                i += 1;
                if next == b'\\' {
                    i += 1;
                } else if next == b {
                    break;
                } else if next == b'\n' {
                    return None;
                }
            }
        } else if b == b'_' || b.is_ascii_alphabetic() {
            i += 1;
            while i < bytes.len() && (bytes[i] == b'_' || bytes[i].is_ascii_alphanumeric()) {
                i += 1;
            }
        } else {
            i += source[i..].chars().next()?.len_utf8();
        }
        result.push(Token {
            text: &source[start..i],
            start,
            end: i,
        });
        if result.len() > MAX_TOKENS {
            return None;
        }
    }
    Some(result)
}

fn closing(text: &str) -> Option<&'static str> {
    match text {
        "(" => Some(")"),
        "[" => Some("]"),
        "{" => Some("}"),
        _ => None,
    }
}

// Returns matched delimiters once, avoiding repeated rescans on adversarial
// unmatched invocations. All work is linear in the bounded token count.
fn pairs(tokens: &[Token<'_>]) -> Option<Vec<Option<usize>>> {
    let mut pairs = vec![None; tokens.len()];
    let mut stack: Vec<(usize, &str)> = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        if let Some(close) = closing(token.text) {
            if stack.len() == MAX_NESTING {
                return None;
            }
            stack.push((i, close));
        } else if matches!(token.text, ")" | "]" | "}") {
            let (open, expected) = stack.pop()?;
            if token.text != expected {
                return None;
            }
            pairs[open] = Some(i);
        }
    }
    stack.is_empty().then_some(pairs)
}

pub(crate) fn discover(source: &str, path: &Path) -> Vec<Document> {
    let Some(rules) = load_rules(path) else {
        return Vec::new();
    };
    discover_with_rules(source, path, &rules)
}

fn discover_with_rules(source: &str, path: &Path, rules: &HashMap<String, usize>) -> Vec<Document> {
    if rules.is_empty() {
        return Vec::new();
    }
    let Some((view, offsets)) = lexical_view(source) else {
        return Vec::new();
    };
    let Some(mut tokens) = tokens(&view) else {
        return Vec::new();
    };
    let Some(pairs) = pairs(&tokens) else {
        return Vec::new();
    };
    for token in &mut tokens {
        token.start = offsets[token.start];
        token.end = offsets[token.end - 1] + 1;
    }
    let context = EmitContext {
        code: CodeSource::new(source, path),
        module: infer_project_name(path),
        file: project_relative_file(path, &infer_project_root(path)),
        newlines: source
            .bytes()
            .enumerate()
            .filter_map(|(i, b)| (b == b'\n').then_some(i))
            .collect(),
    };
    let mut docs = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i];
        if let Some(&argument) = rules.get(token.text)
            && &source[token.start..token.end] == token.text
            && tokens.get(i + 1).map(|t| t.text) == Some("(")
            && let Some(close) = pairs[i + 1]
            && tokens.get(close + 1).map(|t| t.text) == Some("{")
            && let Some(end) = pairs[close + 1]
            && tokens[end].end - token.start <= MAX_INVOCATION_BYTES
        {
            let mut args = Vec::new();
            let mut start = i + 2;
            let mut j = start;
            while j < close {
                if tokens[j].text == "," {
                    args.push((start, j));
                    start = j + 1;
                } else if let Some(nested_end) = pairs[j] {
                    j = nested_end;
                }
                j += 1;
            }
            args.push((start, close));
            if args.len() <= MAX_ARGUMENTS
                && args.iter().all(|(start, end)| start < end)
                && let Some(&(name_start, name_end)) = args.get(argument)
                && name_end == name_start + 1
                && identifier(tokens[name_start].text)
                && &source[tokens[name_start].start..tokens[name_start].end]
                    == tokens[name_start].text
            {
                docs.push(emit(
                    &context,
                    token,
                    tokens[name_start],
                    tokens[close + 1],
                    tokens[end],
                ));
                if docs.len() == MAX_DECLARATIONS {
                    break;
                }
            }
            i = end + 1;
            continue;
        }
        // Only translation-unit invocations qualify, never invocations inside
        // ordinary function bodies or other parenthesized declarations.
        i = pairs[i].map(|end| end + 1).unwrap_or(i + 1);
    }
    docs
}

struct EmitContext<'a> {
    code: CodeSource<'a>,
    module: String,
    file: String,
    newlines: Vec<usize>,
}

fn emit(
    context: &EmitContext<'_>,
    invocation: Token<'_>,
    name: Token<'_>,
    body: Token<'_>,
    end: Token<'_>,
) -> Document {
    let span = SourceSpan {
        file: context.code.path.to_string_lossy().into_owned(),
        start_line: context.newlines.partition_point(|&i| i < invocation.start) + 1,
        end_line: context.newlines.partition_point(|&i| i < end.end) + 1,
        start_byte: invocation.start,
        end_byte: end.end,
    };
    let mut attributes = context.code.attributes("function", &span);
    for (key, value) in [
        ("analysis_language", "c"),
        ("analysis_scope", "symbols_only"),
        ("analysis_origin", "macro_mapping"),
        ("macro_name", invocation.text),
        ("logical_api_name", name.text),
        (
            "analysis_limitation",
            "Logical API name from a configured source macro; no compiler expansion, ABI identity, parameter types, or body-call analysis.",
        ),
    ] {
        attributes.insert(key.into(), value.into());
    }
    attributes.insert("name_start_byte".into(), name.start.to_string());
    attributes.insert("name_end_byte".into(), name.end.to_string());
    attributes.insert("body_start_byte".into(), body.start.to_string());
    attributes.insert("body_end_byte".into(), end.end.to_string());
    // A separate anchor namespace prevents collisions with a real C function
    // carrying the same name. Function is the existing searchable node kind.
    let anchor = make_anchor(
        &context.module,
        &context.file,
        &format!("macro-api::{}", name.text),
    );
    Document {
        anchor,
        node_type: NodeType::Function,
        attributes,
        // Graph readers interpret edge macros inside listings independently
        // of their language. Keep untrusted C source behind its verified span,
        // never in graph-bearing document blocks.
        blocks: vec![Block::Paragraph(format!(
            "Logical C API `{}` declared by source macro `{}`. Compiler expansion and body calls are not analyzed.",
            name.text, invocation.text
        ))],
        source_span: Some(span),
        metadata: None,
        confidence: 0.7,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "[macro_declarations]\nenabled = true\n[[macro_declarations.rules]]\nname = 'DECLARE_API'\nname_argument = 1\n";

    fn fixture(config: Option<&str>) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let file = dir.path().join("sample.c");
        std::fs::write(&file, "").unwrap();
        if let Some(config) = config {
            std::fs::create_dir(dir.path().join(".aden")).unwrap();
            std::fs::write(dir.path().join(".aden/config.toml"), config).unwrap();
        }
        (dir, file)
    }

    #[test]
    fn generic_mapping_preserves_original_bytes_and_logical_identity() {
        let (_dir, path) = fixture(Some(CONFIG));
        let source = "// unicode: café\nDECLARE_API((tag, nested), read_item)\n{\n return backend_read();\n}\n";
        let docs = discover(source, &path);
        assert_eq!(docs.len(), 1);
        let doc = &docs[0];
        assert_eq!(doc.node_type, NodeType::Function);
        assert!(doc.anchor.ends_with("#macro-api::read_item"));
        assert_eq!(doc.attributes["analysis_origin"], "macro_mapping");
        assert_eq!(doc.attributes["analysis_scope"], "symbols_only");
        assert_eq!(
            doc.attributes["source_hash"],
            aden_core::hash_source(source)
        );
        let span = doc.source_span.as_ref().unwrap();
        assert_eq!((span.start_line, span.end_line), (2, 5));
        assert_eq!(
            &source[span.start_byte..span.end_byte],
            "DECLARE_API((tag, nested), read_item)\n{\n return backend_read();\n}"
        );
        let start: usize = doc.attributes["name_start_byte"].parse().unwrap();
        let end: usize = doc.attributes["name_end_byte"].parse().unwrap();
        assert_eq!(&source[start..end], "read_item");
        let body_start: usize = doc.attributes["body_start_byte"].parse().unwrap();
        let body_end: usize = doc.attributes["body_end_byte"].parse().unwrap();
        assert_eq!(
            &source[body_start..body_end],
            "{\n return backend_read();\n}"
        );
        assert!(!doc.attributes.contains_key("call_sites"));
        assert!(
            !doc.blocks
                .iter()
                .any(|b| matches!(b, Block::Listing { code, .. } if code.contains("edge::calls[")))
        );
    }

    #[test]
    fn source_graph_macros_cannot_become_document_edge_evidence() {
        let (_dir, path) = fixture(Some(CONFIG));
        let source = "DECLARE_API(tag, read_item) {\n/*\nedge::calls[real_function]\nedge::uses[real_type]\n*/\nreturn backend_read();\n}";
        let docs = discover(source, &path);
        assert_eq!(docs.len(), 1);
        let doc = &docs[0];
        let span = doc.source_span.as_ref().unwrap();
        assert_eq!(&source[span.start_byte..span.end_byte], source);
        assert_eq!(
            doc.attributes["source_hash"],
            aden_core::hash_source(source)
        );
        assert_eq!(doc.attributes["analysis_origin"], "macro_mapping");
        assert_eq!(doc.attributes["analysis_scope"], "symbols_only");
        assert!(!doc.attributes.contains_key("call_sites"));
        assert!(
            doc.attributes
                .values()
                .all(|value| !value.contains("edge::"))
        );
        assert_eq!(doc.blocks.len(), 1);
        assert!(matches!(&doc.blocks[0], Block::Paragraph(text) if !text.contains("edge::")));
        assert!(
            !doc.blocks
                .iter()
                .any(|block| matches!(block, Block::Listing { .. }))
        );
    }

    #[test]
    fn c_line_splices_cannot_expose_commented_api_declarations() {
        let (_dir, path) = fixture(Some(CONFIG));
        for newline in ["\n", "\r\n"] {
            let continued_comment =
                format!("// continued comment \\{newline}DECLARE_API(tag, fake) {{}}{newline}");
            let split_comment =
                format!("/\\{newline}/ comment \\{newline}DECLARE_API(tag, fake) {{}}{newline}");
            for source in [continued_comment, split_comment] {
                assert!(discover(&source, &path).is_empty());
            }
        }
        assert_eq!(
            discover("// ordinary comment\nDECLARE_API(tag, real) {}", &path).len(),
            1
        );
    }

    #[test]
    fn unrelated_directive_splices_preserve_api_discovery_and_original_spans() {
        let (_dir, path) = fixture(Some(CONFIG));
        for newline in ["\n", "\r\n"] {
            let prefix = format!("#define UNRELATED(x) \\{newline} ((x) + 1){newline}");
            let invocation = format!(
                "DECLARE_API(tag, real) {{{newline}return backend(\\{newline}1);{newline}}}"
            );
            let source = format!("{prefix}{invocation}{newline}");
            let docs = discover(&source, &path);
            assert_eq!(docs.len(), 1);
            let doc = &docs[0];
            let span = doc.source_span.as_ref().unwrap();
            assert_eq!(span.start_byte, prefix.len());
            assert_eq!(&source[span.start_byte..span.end_byte], invocation);
            assert_eq!((span.start_line, span.end_line), (3, 6));
            assert_eq!(
                doc.attributes["source_hash"],
                aden_core::hash_source(&source)
            );
            let start: usize = doc.attributes["name_start_byte"].parse().unwrap();
            let end: usize = doc.attributes["name_end_byte"].parse().unwrap();
            assert_eq!(&source[start..end], "real");
        }
        for source in [
            "DECLARE_\\\nAPI(tag, fake) {}",
            "DECLARE_API(tag, fa\\\nke) {}",
        ] {
            assert!(discover(source, &path).is_empty());
        }
    }

    #[cfg(unix)]
    #[test]
    fn nonregular_configuration_is_rejected_before_opening() {
        let (owner, path) = fixture(None);
        std::fs::create_dir(owner.path().join(".aden")).unwrap();
        let config = owner.path().join(".aden/config.toml");
        // A socket gives a bounded nonregular-file regression: opening it as a
        // File would return an error. No FIFO writer, process, or wait is needed.
        let _listener = std::os::unix::net::UnixListener::bind(&config).unwrap();
        assert!(config_fingerprint(&path).is_ok());
        assert!(load_rules(&path).is_none());
    }

    #[test]
    fn linux_like_rule_is_explicit_and_survives_router_annotation() {
        let config = CONFIG
            .replace("DECLARE_API", "SYSCALL_DEFINE3")
            .replace("name_argument = 1", "name_argument = 0");
        let (_dir, path) = fixture(Some(&config));
        let source = "SYSCALL_DEFINE3(read, unsigned int, fd, char *, buf, size_t, count) { return ksys_read(fd, buf, count); }";
        let docs = crate::LanguageRouter::new()
            .parse_file(&path, source)
            .unwrap();
        let doc = docs
            .iter()
            .find(|d| {
                d.attributes.get("analysis_origin").map(String::as_str) == Some("macro_mapping")
            })
            .unwrap();
        assert_eq!(doc.attributes["logical_api_name"], "read");
        assert_eq!(doc.attributes["analysis_scope"], "symbols_only");
        assert!(doc.attributes["analysis_limitation"].contains("no compiler expansion"));
        assert!(!doc.anchor.contains("__x64"));
    }

    #[test]
    fn disabled_unknown_and_malformed_inputs_are_opaque() {
        let (_dir, path) = fixture(None);
        assert!(discover("DECLARE_API(tag, read) {}", &path).is_empty());
        let rules = parse_rules(CONFIG).unwrap();
        for source in [
            "UNKNOWN(tag, read) {}",
            "DECLARE_API(tag, read);",
            "DECLARE_API(tag, read + other) {}",
            "DECLARE_API(tag, read,) {}",
            "DECLARE_API(tag, read) {",
            "DECLARE_API(tag, read) { return (x]; }",
            "DECLARE_API(tag, read) { \"unterminated }",
            "/* DECLARE_API(tag, read) {} */",
            "const char *s = \"DECLARE_API(tag, read) {}\";",
            "void normal(void) { DECLARE_API(tag, read) {} }",
            "#define HIDDEN DECLARE_API(tag, read) {}\n",
            "#define HIDDEN \\\nDECLARE_API(tag, read) {}\n",
        ] {
            assert!(
                discover_with_rules(source, &path, &rules).is_empty(),
                "{source}"
            );
        }
    }

    #[test]
    fn untrusted_mapping_data_cannot_add_commands_or_expansion() {
        for config in [
            CONFIG.replace("enabled = true", "enabled = false"),
            CONFIG.replace("name_argument = 1", "name_argument = -1"),
            CONFIG.replace("name_argument = 1", "name_argument = 64"),
            CONFIG.replace("DECLARE_API", "$(run_command)"),
            format!("{CONFIG}command = 'run-me'\n"),
            format!("{CONFIG}expansion = 'int fabricated()'\n"),
            format!("{CONFIG}[[macro_declarations.rules]]\nname='DECLARE_API'\nname_argument=0\n"),
            "[macro_declarations]\nenabled=true\nrules='not a list'".to_owned(),
            "[macro_declarations".to_owned(),
            "x".repeat(MAX_CONFIG_BYTES + 1),
        ] {
            assert!(parse_rules(&config).is_none());
        }
    }

    #[test]
    fn mapping_stays_in_its_project_and_fingerprint_tracks_changes() {
        let (owner, path) = fixture(Some(CONFIG));
        let (_other, other_path) = fixture(None);
        let source = "DECLARE_API(tag, read) {}";
        assert_eq!(discover(source, &path).len(), 1);
        assert!(discover(source, &other_path).is_empty());
        let enabled = config_fingerprint(&path).unwrap();
        let config = owner.path().join(".aden/config.toml");
        std::fs::write(&config, CONFIG.replace("enabled = true", "enabled = false")).unwrap();
        let disabled = config_fingerprint(&path).unwrap();
        assert_ne!(enabled, disabled);
        assert!(discover(source, &path).is_empty());
        std::fs::write(&config, "malformed [").unwrap();
        let malformed = config_fingerprint(&path).unwrap();
        assert_ne!(disabled, malformed);
        std::fs::remove_file(config).unwrap();
        assert_ne!(malformed, config_fingerprint(&path).unwrap());
    }

    #[test]
    fn nearer_git_boundary_blocks_parent_mapping_and_nested_mapping_can_opt_in() {
        let (owner, _) = fixture(Some(CONFIG));
        let nested = owner.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        std::fs::create_dir(nested.join(".git")).unwrap();
        let path = nested.join("nested.c");
        let source = "DECLARE_API(tag, read) {}";
        assert!(discover(source, &path).is_empty());
        let absent = config_fingerprint(&path).unwrap();
        std::fs::create_dir(nested.join(".aden")).unwrap();
        std::fs::write(nested.join(".aden/config.toml"), CONFIG).unwrap();
        assert_eq!(discover(source, &path).len(), 1);
        assert_ne!(absent, config_fingerprint(&path).unwrap());
    }

    #[test]
    fn scan_and_mapping_limits_fail_closed() {
        let (_dir, path) = fixture(Some(CONFIG));
        let rules = parse_rules(CONFIG).unwrap();
        assert!(discover_with_rules(&" ".repeat(MAX_SOURCE_BYTES + 1), &path, &rules).is_empty());
        let source = format!(
            "DECLARE_API(tag, read) {{ {} }}",
            "(".repeat(MAX_NESTING + 1)
        );
        assert!(discover_with_rules(&source, &path, &rules).is_empty());
        assert!(parse_rules(&CONFIG.replace("DECLARE_API", &"A".repeat(MAX_NAME + 1))).is_none());
        let rules = (0..=MAX_RULES)
            .map(|i| format!("[[macro_declarations.rules]]\nname='API{i}'\nname_argument=0\n"))
            .collect::<String>();
        assert!(parse_rules(&format!("[macro_declarations]\nenabled=true\n{rules}")).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_configuration_is_inert() {
        let (owner, path) = fixture(None);
        let (other, _) = fixture(Some(CONFIG));
        std::fs::create_dir(owner.path().join(".aden")).unwrap();
        std::os::unix::fs::symlink(
            other.path().join(".aden/config.toml"),
            owner.path().join(".aden/config.toml"),
        )
        .unwrap();
        assert!(discover("DECLARE_API(tag, read) {}", &path).is_empty());
        assert!(config_fingerprint(&path).is_ok());
    }
}
