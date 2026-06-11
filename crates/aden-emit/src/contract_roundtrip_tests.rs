// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Round-trip suite: `parse_contract` must be the exact inverse of
//! `emit_contract_document` for every valid `ContractDocument`.
//!
//! "Valid" excludes inputs the line-based format cannot represent
//! unambiguously: newlines inside tags/attributes, whitespace inside tags,
//! attribute values embedding a ` :word: ` marker, and leading blank prose
//! lines in a doc with no header and no blocks.

use aden_core::contract::{
    ContractDocument, ContractRegion, ParseMode, RegionBlock, parse_contract,
};
use std::collections::HashMap;

use crate::{emit_contract_document, emit_contract_document_rendered};

fn block(region: ContractRegion, tag: Option<&str>, content: &str) -> RegionBlock {
    RegionBlock {
        region,
        tag: tag.map(String::from),
        attributes: HashMap::new(),
        content: content.to_string(),
        start_line: 0,
        end_line: 0,
    }
}

/// Line spans are positional metadata computed by the parser from the emitted
/// text; they are not part of the document's semantic identity, so equality
/// is asserted with spans zeroed on both sides.
fn normalized(mut doc: ContractDocument) -> ContractDocument {
    for b in &mut doc.blocks {
        b.start_line = 0;
        b.end_line = 0;
    }
    doc
}

fn assert_round_trip(doc: &ContractDocument, mode: ParseMode) {
    let emitted = emit_contract_document(doc);
    let parsed = parse_contract(&emitted, mode).expect("emitted output must parse");
    assert_eq!(
        normalized(parsed),
        normalized(doc.clone()),
        "round-trip mismatch ({mode:?});\nemitted:\n{emitted}"
    );
}

#[test]
fn empty_doc_round_trips() {
    let doc = ContractDocument::default();
    assert_round_trip(&doc, ParseMode::Strict);
    assert_round_trip(&doc, ParseMode::Permissive);
}

#[test]
fn every_region_variant_round_trips() {
    let regions = [
        ContractRegion::Generated,
        ContractRegion::Human,
        ContractRegion::Agent,
        ContractRegion::Security,
        ContractRegion::Contract,
        ContractRegion::Constitution,
        ContractRegion::Override,
        ContractRegion::Proposed,
    ];
    for region in regions {
        let doc = ContractDocument {
            header_attrs: HashMap::new(),
            blocks: vec![
                block(region, Some("tagged"), "tagged content"),
                block(region, None, "untagged content"),
            ],
            prose: Vec::new(),
        };
        assert_round_trip(&doc, ParseMode::Strict);
        assert_round_trip(&doc, ParseMode::Permissive);
    }
}

#[test]
fn tag_containing_hash_round_trips() {
    // Anchors like `src/lib.rs#foo` are used as tags by `from_document`;
    // only the FIRST `#` separates region from tag.
    let doc = ContractDocument {
        header_attrs: HashMap::new(),
        blocks: vec![block(
            ContractRegion::Generated,
            Some("src/lib.rs#foo"),
            "fn foo() {}",
        )],
        prose: Vec::new(),
    };
    assert_round_trip(&doc, ParseMode::Strict);
}

#[test]
fn block_attributes_round_trip() {
    let mut attrs = HashMap::new();
    attrs.insert("status".to_string(), "conflict".to_string());
    attrs.insert(
        "reason".to_string(),
        "value with spaces and a trailing colon: yes".to_string(),
    );
    attrs.insert("empty".to_string(), String::new());
    let doc = ContractDocument {
        header_attrs: HashMap::new(),
        blocks: vec![RegionBlock {
            region: ContractRegion::Proposed,
            tag: Some("foo".to_string()),
            attributes: attrs,
            content: "// CONFLICT: resolve manually".to_string(),
            start_line: 0,
            end_line: 0,
        }],
        prose: Vec::new(),
    };
    assert_round_trip(&doc, ParseMode::Strict);
}

#[test]
fn header_attrs_round_trip() {
    let mut header = HashMap::new();
    header.insert("source_hash".to_string(), "abc123".to_string());
    header.insert("docs_url".to_string(), "https://example.com/x".to_string());
    let doc = ContractDocument {
        header_attrs: header,
        blocks: vec![block(ContractRegion::Generated, Some("foo"), "fn foo() {}")],
        prose: Vec::new(),
    };
    assert_round_trip(&doc, ParseMode::Strict);
}

#[test]
fn source_file_header_does_not_pollute_round_trip() {
    // `source_file` triggers the derived "See also" module link in the
    // rendered output; the canonical emit must stay free of it so the
    // parser does not read it back as prose.
    let mut header = HashMap::new();
    header.insert(
        "source_file".to_string(),
        "crates/aden-core/src/contract.rs".to_string(),
    );
    let doc = ContractDocument {
        header_attrs: header,
        blocks: vec![block(ContractRegion::Generated, Some("foo"), "fn foo() {}")],
        prose: Vec::new(),
    };
    assert_round_trip(&doc, ParseMode::Permissive);
}

#[test]
fn prose_round_trips_in_permissive_mode() {
    let doc = ContractDocument {
        header_attrs: HashMap::new(),
        blocks: vec![block(ContractRegion::Human, Some("notes"), "design notes")],
        prose: vec![
            "Freeform line one.".to_string(),
            String::new(),
            "Freeform line two after a blank.".to_string(),
        ],
    };
    assert_round_trip(&doc, ParseMode::Permissive);
}

#[test]
fn multi_block_mixed_doc_round_trips() {
    let mut header = HashMap::new();
    header.insert("anchor".to_string(), "mod-foo".to_string());
    let doc = ContractDocument {
        header_attrs: header,
        blocks: vec![
            block(ContractRegion::Generated, Some("foo"), "fn foo() {}"),
            block(ContractRegion::Human, Some("foo"), "Why foo exists."),
            block(ContractRegion::Agent, Some("perf"), "Agent perf analysis."),
            block(ContractRegion::Security, None, "[forbid] unsafe"),
        ],
        prose: vec!["Trailing prose.".to_string()],
    };
    assert_round_trip(&doc, ParseMode::Permissive);
}

#[test]
fn content_containing_delimiter_line_round_trips() {
    // A literal `----` line inside content must not terminate the block:
    // the emitter has to pick a delimiter that does not collide.
    let doc = ContractDocument {
        header_attrs: HashMap::new(),
        blocks: vec![block(
            ContractRegion::Generated,
            Some("foo"),
            "before\n----\nafter\n-----\nlast",
        )],
        prose: Vec::new(),
    };
    assert_round_trip(&doc, ParseMode::Strict);
}

#[test]
fn content_edge_shapes_round_trip() {
    for content in [
        "",                   // empty block
        "single",             // no newline
        "trailing newline\n", // trailing newline preserved
        "interior\n\nblank",  // interior blank line
        "====",               // delimiter-pattern line of another char class
    ] {
        let doc = ContractDocument {
            header_attrs: HashMap::new(),
            blocks: vec![block(ContractRegion::Generated, Some("x"), content)],
            prose: Vec::new(),
        };
        assert_round_trip(&doc, ParseMode::Strict);
    }
}

#[test]
fn consecutive_blocks_do_not_accrete_prose() {
    // The blank separator the emitter writes between blocks must not be
    // read back as prose — otherwise every emit/parse cycle grows the doc.
    let doc = ContractDocument {
        header_attrs: HashMap::new(),
        blocks: vec![
            block(ContractRegion::Generated, Some("a"), "fn a() {}"),
            block(ContractRegion::Generated, Some("b"), "fn b() {}"),
        ],
        prose: Vec::new(),
    };
    let emitted = emit_contract_document(&doc);
    let parsed = parse_contract(&emitted, ParseMode::Permissive).unwrap();
    assert!(
        parsed.prose.is_empty(),
        "separator blanks leaked into prose: {:?}",
        parsed.prose
    );
    assert_round_trip(&doc, ParseMode::Permissive);
}

#[test]
fn rendered_emit_appends_module_link() {
    let mut header = HashMap::new();
    header.insert(
        "source_file".to_string(),
        "crates/aden-core/src/contract.rs".to_string(),
    );
    let doc = ContractDocument {
        header_attrs: header,
        blocks: vec![block(ContractRegion::Generated, Some("foo"), "fn foo() {}")],
        prose: Vec::new(),
    };
    let rendered = emit_contract_document_rendered(&doc);
    assert!(rendered.contains("See also: <<module-aden-core>>"));
    assert!(
        !emit_contract_document(&doc).contains("See also:"),
        "canonical emit must stay free of derived content"
    );
}

#[test]
fn double_round_trip_is_stable() {
    // parse ∘ emit must be idempotent: a second cycle changes nothing.
    let doc = ContractDocument {
        header_attrs: HashMap::new(),
        blocks: vec![
            block(ContractRegion::Generated, Some("foo"), "fn foo() {}"),
            block(ContractRegion::Human, None, "notes"),
        ],
        prose: vec!["prose".to_string()],
    };
    let once = parse_contract(&emit_contract_document(&doc), ParseMode::Permissive).unwrap();
    let twice = parse_contract(&emit_contract_document(&once), ParseMode::Permissive).unwrap();
    assert_eq!(normalized(once), normalized(twice));
}
