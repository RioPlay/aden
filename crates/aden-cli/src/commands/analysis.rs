// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use aden_core::Document;
use serde_json::{Value, json};

pub(crate) const SUMMARY: &str = "Graph relationships are partial. Missing edges do not prove absence; macros, indirect calls and runtime dispatch may be omitted.";

/// Graph result completeness describes traversal of stored edges, not whether
/// extraction found every relationship in the program. Keep these independent.
pub(crate) fn scope(document: Option<&Document>) -> Value {
    let mut value = json!({
        "schema_version": 1,
        "completeness": "partial",
        "absence_is_proof": false,
        "limitations": ["unexpanded_macros", "indirect_calls", "runtime_dispatch"],
    });
    if let Some(doc) = document {
        value["scope"] = json!(
            doc.attributes
                .get("analysis_scope")
                .map(String::as_str)
                .unwrap_or("unknown")
        );
        for key in [
            "analysis_origin",
            "macro_name",
            "logical_api_name",
            "analysis_limitation",
        ] {
            if let Some(attribute) = doc.attributes.get(key) {
                value[key] = json!(attribute);
            }
        }
        if let Some(language) = doc.attributes.get("analysis_language") {
            value["language"] = json!(language);
        }
        if value["scope"] == "symbols_only" {
            value["limitations"]
                .as_array_mut()
                .unwrap()
                .push(json!("call_relationships_not_analyzed"));
        }
        if value["scope"] == "unknown" {
            value["limitations"]
                .as_array_mut()
                .unwrap()
                .push(json!("extractor_capabilities_not_recorded"));
        }
        // This is a per-definition observation, never a project-wide count.
        let count: usize = doc
            .blocks
            .iter()
            .filter_map(|block| {
                if let aden_core::Block::Table(table) = block {
                    (table
                        .headers
                        .first()
                        .is_some_and(|h| h == "Indirect callee"))
                    .then_some(table.rows.len())
                } else {
                    None
                }
            })
            .sum();
        value["observed_indirect_calls"] = json!(count);
    }
    value
}

pub(crate) fn add_origin(value: &mut Value, doc: &Document) {
    if doc.attributes.contains_key("analysis_origin") {
        value["analysis"] = scope(Some(doc));
    }
}

pub(crate) fn augment(path: &std::path::Path, mut value: Value) -> Value {
    if let Some(object) = value.as_object_mut() {
        object.entry("analysis").or_insert_with(|| scope(None));
    }
    super::augment_read_json(path, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_graph_and_empty_results_never_claim_semantic_completeness() {
        let value = scope(None);
        assert_eq!(value["completeness"], "partial");
        assert_eq!(value["absence_is_proof"], false);
        assert!(value.get("language").is_none());
    }
    #[test]
    fn per_definition_capabilities_and_indirect_observations_are_explicit() {
        let mut doc = aden_parse::parse_file(std::path::Path::new("sample.rs"), "fn sample() {}")
            .unwrap()
            .remove(0);
        assert_eq!(scope(Some(&doc))["scope"], "static_relationships");
        doc.attributes
            .insert("analysis_scope".into(), "symbols_only".into());
        let symbol_only = scope(Some(&doc));
        assert!(
            symbol_only["limitations"]
                .as_array()
                .unwrap()
                .contains(&json!("call_relationships_not_analyzed"))
        );
        doc.attributes.remove("analysis_scope");
        assert_eq!(scope(Some(&doc))["scope"], "unknown");
        doc.blocks.push(aden_core::Block::Table(aden_core::Table {
            headers: vec!["Indirect callee".into(), "Line".into()],
            rows: vec![vec!["callback".into(), "3".into()]],
        }));
        assert_eq!(scope(Some(&doc))["observed_indirect_calls"], 1);
    }
}
