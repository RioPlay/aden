// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Spike test for the real `tract` + bge-small embedding provider.
//!
//! Validates the one risk the embedding-stack research flagged: that `tract`
//! actually loads the exported bge-small ONNX graph and produces sane, useful
//! embeddings. Runs only when (a) the `dense` feature is enabled AND (b) the
//! model is present. Point it at the model dir with `ADEN_BGE_MODEL_DIR`, else it
//! falls back to `~/.cache/aden-models/bge-small-en-v1.5`. If the model isn't
//! there, the test skips (prints a notice) rather than failing.
//!
//! Run: `cargo test -p aden-index --features dense --test dense_spike -- --nocapture`
#![cfg(feature = "dense")]

use aden_index::{EmbeddingProvider, TractEmbedder, cosine_similarity};
use std::path::PathBuf;

fn model_dir() -> Option<PathBuf> {
    let dir = std::env::var("ADEN_BGE_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = dirs::home_dir()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            PathBuf::from(home).join(".cache/aden-models/bge-small-en-v1.5")
        });
    if dir.join("model.onnx").exists() && dir.join("tokenizer.json").exists() {
        Some(dir)
    } else {
        None
    }
}

#[test]
fn tract_loads_bge_and_embeddings_are_sane() {
    let Some(dir) = model_dir() else {
        eprintln!("SKIP: bge-small model not found (set ADEN_BGE_MODEL_DIR); skipping spike");
        return;
    };

    let embedder = TractEmbedder::from_dir(&dir).expect("tract must load the bge-small ONNX graph");

    // 1. Correct dimensionality.
    let v = embedder.embed("hello world");
    assert_eq!(v.len(), 384, "bge-small is 384-dim");
    assert_eq!(embedder.dim(), 384);

    // 2. L2-normalized (unit length).
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!(
        (norm - 1.0).abs() < 1e-3,
        "embedding should be L2-normalized; norm={norm}"
    );

    // 3. Deterministic: same text -> identical vector.
    let v2 = embedder.embed("hello world");
    assert_eq!(v, v2, "embeddings must be deterministic");

    // 4. Semantic sanity: related texts are closer than unrelated ones.
    let q = embedder.embed("how to authenticate a user with a login token");
    let related = embedder.embed("validate the bearer credential and start a session");
    let unrelated =
        embedder.embed("recompute the average document length for length normalization");
    let sim_related = cosine_similarity(&q, &related);
    let sim_unrelated = cosine_similarity(&q, &unrelated);

    println!("cosine(query, related)   = {sim_related:.4}");
    println!("cosine(query, unrelated) = {sim_unrelated:.4}");
    assert!(
        sim_related > sim_unrelated,
        "a semantically related sentence must score higher than an unrelated one \
         (related={sim_related:.4}, unrelated={sim_unrelated:.4})"
    );
    // Related should be meaningfully similar, unrelated meaningfully less so.
    assert!(
        sim_related > 0.5,
        "related similarity unexpectedly low: {sim_related:.4}"
    );
}
