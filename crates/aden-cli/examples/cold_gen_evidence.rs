// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Untimed semantic evidence for cold-generation benchmarks. No source is emitted.
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;

fn canonical(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut pairs: Vec<_> = map.into_iter().collect();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            serde_json::Value::Object(pairs.into_iter().map(|(k, v)| (k, canonical(v))).collect())
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(canonical).collect())
        }
        other => other,
    }
}

struct HashWriter(Sha256);
impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn digest(value: &impl serde::Serialize) -> Result<String, serde_json::Error> {
    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer(&mut writer, value)?;
    Ok(writer
        .0
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

type Documents = Vec<(String, serde_json::Value)>;
type Edges = Vec<(String, String, String)>;

fn normalize_generated_time(doc: &mut aden_core::Document) {
    // aden-parse::extractor stamps generated documents on each run, including
    // Markdown/AsciiDoc headings and glossary terms. Keep the
    // attribute's presence and all source hashes; only the wall clock varies.
    if !doc.attributes.contains_key("source_hash") || !doc.attributes.contains_key("node-type") {
        return;
    }
    if let Some(value) = doc.attributes.get_mut("last-verified") {
        let bytes = value.as_bytes();
        if bytes.len() == 20
            && bytes.iter().enumerate().all(|(i, b)| match i {
                4 | 7 => *b == b'-',
                10 => *b == b'T',
                13 | 16 => *b == b':',
                19 => *b == b'Z',
                _ => b.is_ascii_digit(),
            })
        {
            *value = "<generation-time>".to_string();
        }
    }
}

fn graph_rows(
    snapshot: &Path,
    normalize_time: bool,
) -> Result<(Documents, Edges), Box<dyn std::error::Error>> {
    let (docs, edges) = aden_graph::snapshot::read_snapshot_file(snapshot)?;
    let mut documents: Vec<_> = docs
        .into_iter()
        .map(|(anchor, mut doc)| {
            if normalize_time {
                normalize_generated_time(&mut doc);
            }
            Ok((anchor, canonical(serde_json::to_value(doc)?)))
        })
        .collect::<Result<_, serde_json::Error>>()?;
    documents.sort_by(|a, b| a.0.cmp(&b.0));
    let mut edges: Vec<_> = edges
        .into_iter()
        .map(|(a, b, kind)| (a, b, format!("{kind:?}")))
        .collect();
    edges.sort();
    Ok((documents, edges))
}

fn compare_fields(
    left: &serde_json::Value,
    right: &serde_json::Value,
    field: &str,
    differences: &mut std::collections::BTreeMap<String, usize>,
) {
    if left == right {
        return;
    }
    match (left, right) {
        (serde_json::Value::Object(a), serde_json::Value::Object(b)) => {
            let keys: std::collections::BTreeSet<_> = a.keys().chain(b.keys()).collect();
            for key in keys {
                let path = format!("{field}/{key}");
                match (a.get(key), b.get(key)) {
                    (Some(a), Some(b)) => compare_fields(a, b, &path, differences),
                    _ => *differences.entry(path).or_default() += 1,
                }
            }
        }
        (serde_json::Value::Array(a), serde_json::Value::Array(b)) if a.len() == b.len() => {
            let path = format!("{field}/[]");
            for (a, b) in a.iter().zip(b) {
                compare_fields(a, b, &path, differences);
            }
        }
        _ => *differences.entry(field.to_string()).or_default() += 1,
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let argument = args.next().ok_or("expected graph.snapshot path")?;
    if argument == "--compare" {
        let left = args.next().ok_or("expected left snapshot")?;
        let right = args.next().ok_or("expected right snapshot")?;
        let (left_docs, left_edges) = graph_rows(Path::new(&left), false)?;
        let (right_docs, right_edges) = graph_rows(Path::new(&right), false)?;
        let left: std::collections::BTreeMap<_, _> = left_docs.into_iter().collect();
        let right: std::collections::BTreeMap<_, _> = right_docs.into_iter().collect();
        let mut differences = std::collections::BTreeMap::new();
        for (anchor, doc) in &left {
            if let Some(other) = right.get(anchor) {
                compare_fields(doc, other, "", &mut differences);
            }
        }
        println!(
            "{}",
            serde_json::json!({
                "edges_equal": left_edges == right_edges,
                "anchor_sets_equal": left.keys().eq(right.keys()),
                "different_document_fields": differences,
            })
        );
        return Ok(());
    }
    let snapshot = Path::new(&argument);
    let (documents, edges) = graph_rows(snapshot, true)?;
    let cache: serde_json::Value = serde_json::from_slice(&std::fs::read(
        snapshot
            .parent()
            .ok_or("snapshot has no parent")?
            .join("gen-cache.json"),
    )?)?;
    let cache = canonical(cache);
    println!(
        "{}",
        serde_json::json!({
            "schema_version": 1,
            "documents": documents.len(),
            "edges": edges.len(),
        "documents_sha256": digest(&documents)?,
        "edges_sha256": digest(&edges)?,
            "graph_sha256": digest(&(&documents, &edges))?,
            "cache_sha256": digest(&cache)?,
        })
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_objects_ignore_key_order_but_keep_array_order_and_values() {
        let a: serde_json::Value =
            serde_json::from_str(r#"{"b":{"y":2,"x":1},"a":[1,2]}"#).unwrap();
        let b: serde_json::Value =
            serde_json::from_str(r#"{"a":[1,2],"b":{"x":1,"y":2}}"#).unwrap();
        assert_eq!(
            digest(&canonical(a.clone())).unwrap(),
            digest(&canonical(b)).unwrap()
        );
        let changed = serde_json::json!({"a":[2,1],"b":{"x":1,"y":2}});
        assert_ne!(
            digest(&canonical(a)).unwrap(),
            digest(&canonical(changed)).unwrap()
        );
    }

    #[test]
    fn only_generated_wall_clock_value_is_normalized() {
        let mut doc = aden_core::Document::default();
        doc.attributes
            .insert("last-verified".into(), "2026-11-04T22:22:22Z".into());
        let original = doc.clone();
        normalize_generated_time(&mut doc);
        assert_eq!(doc, original);
        doc.attributes
            .insert("source_hash".into(), "actual-source-hash".into());
        doc.attributes.insert("node-type".into(), "heading".into());
        normalize_generated_time(&mut doc);
        assert_eq!(doc.attributes["last-verified"], "<generation-time>");
        assert_eq!(doc.attributes["source_hash"], "actual-source-hash");
        doc.attributes
            .insert("last-verified".into(), "malformed".into());
        normalize_generated_time(&mut doc);
        assert_eq!(doc.attributes["last-verified"], "malformed");
    }
}
