// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Local, offline, deterministic dense embeddings via `tract` (pure-Rust ONNX).
//!
//! Implements [`EmbeddingProvider`] with the BAAI/bge-small-en-v1.5 model
//! (MIT-licensed, 384-dim). Everything runs on CPU with no network access and no
//! native runtime — `tract` is self-contained Rust, chosen for determinism and
//! footprint (see the embedding-stack decision in the devlog). The model file is
//! loaded from a directory and is FETCHED ON DEMAND (a one-time `setup` step via
//! `scripts/fetch-bge-model.sh`, into `~/.cache/aden-models`), not bundled in the
//! binary; nothing is downloaded at query time, so retrieval stays fully offline.
//!
//! Pooling note: bge-small uses **CLS-token pooling** (confirmed from the model's
//! `1_Pooling/config.json`: `pooling_mode_cls_token: true`), i.e. the first
//! token of `last_hidden_state`, followed by L2 normalization — NOT mean pooling.
//!
//! This module is behind the `dense` Cargo feature so the default build stays
//! lean and free of the ML dependency stack.

use crate::EmbeddingProvider;
use kitoken::Kitoken;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tract_onnx::prelude::*;

/// bge-small-en-v1.5 embedding dimension.
const EMBED_DIM: usize = 384;
/// bge/BERT special-token ids. The model uses CLS pooling, so every input must be
/// wrapped `[CLS] .. [SEP]`. kitoken returns the core WordPiece ids; we prepend and
/// append these two ourselves. Verified byte-identical to the reference HuggingFace
/// tokenizer by `tests/tokenizer_parity.rs`.
const CLS_TOKEN: u32 = 101;
const SEP_TOKEN: u32 = 102;
/// Truncation cap. The model is built with a SYMBOLIC sequence dimension (see
/// `from_dir`), so each text runs at its OWN token count with NO padding — a
/// short symbol doc does a short forward, not a fixed-128 one. This cap bounds
/// long docs (BERT supports up to 512; 128 keeps the per-forward cost low and
/// covers a symbol signature + doc comment, which is what we embed).
const MAX_SEQ: usize = 128;

type Runnable = RunnableModel<TypedFact, Box<dyn TypedOp>>;

/// A `tract`-backed ONNX embedding provider.
pub struct TractEmbedder {
    model: Arc<Runnable>,
    tokenizer: Kitoken,
    /// Graph input names in declared order (e.g. input_ids, attention_mask,
    /// token_type_ids) so per-call tensors are fed in the order tract expects.
    input_order: Vec<String>,
}

impl TractEmbedder {
    /// Load the model (`model.onnx`) and tokenizer (`tokenizer.json`) from
    /// `model_dir`. Deterministic and fully offline — no network access.
    pub fn from_dir(model_dir: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let model_path = model_dir.join("model.onnx");
        let tokenizer_path = model_dir.join("tokenizer.json");

        let tokenizer_bytes = std::fs::read(&tokenizer_path)
            .map_err(|e| format!("reading tokenizer {}: {e}", tokenizer_path.display()))?;
        let tokenizer = Kitoken::from_tokenizers_slice(&tokenizer_bytes)
            .map_err(|e| format!("loading tokenizer {}: {e}", tokenizer_path.display()))?;

        // Load the inference model, capture its input order, then pin every input
        // to shape [1, S] where S is a SYMBOLIC sequence length. One optimized
        // graph then runs any length, so each text embeds at its actual token
        // count with no padding waste (the fixed-128 shape paid for 128 tokens
        // even on a 10-token doc — the dominant first-build cost).
        let mut model = tract_onnx::onnx().model_for_path(&model_path)?;
        let input_order: Vec<String> = model
            .input_outlets()?
            .iter()
            .map(|o| model.node(o.node).name.clone())
            .collect();
        let seq = model.symbols.sym("S");
        for i in 0..input_order.len() {
            model = model.with_input_fact(
                i,
                InferenceFact::dt_shape(
                    i64::datum_type(),
                    tvec!(TDim::from(1), TDim::from(seq.clone())),
                ),
            )?;
        }
        // `into_runnable()` already yields an `Arc<SimplePlan>` in tract 0.23.
        let model = model.into_optimized()?.into_runnable()?;

        Ok(Self {
            model,
            tokenizer,
            input_order,
        })
    }

    /// The model input ids for `text`: kitoken's core WordPiece ids wrapped
    /// `[CLS] .. [SEP]` and truncated to `MAX_SEQ` (no padding). `encode(_, true)`
    /// parses any literal special tokens in the text, matching the reference's
    /// add_special_tokens behavior. Exposed (hidden) so `tests/tokenizer_parity.rs`
    /// can pin these ids byte-identical to the reference HuggingFace tokenizer.
    #[doc(hidden)]
    pub fn token_ids(&self, text: &str) -> Result<Vec<u32>, Box<dyn std::error::Error>> {
        let mut ids: Vec<u32> = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| format!("tokenizing: {e}"))?;
        ids.insert(0, CLS_TOKEN);
        ids.push(SEP_TOKEN);
        ids.truncate(MAX_SEQ);
        Ok(ids)
    }

    /// Encode `text` into the three i64 input tensors (input_ids,
    /// attention_mask, token_type_ids) at the text's actual token length
    /// (truncated to MAX_SEQ). No padding — the symbolic graph runs this length.
    fn encode(&self, text: &str) -> Result<HashMap<String, Tensor>, Box<dyn std::error::Error>> {
        let ids = self.token_ids(text)?;
        let n = ids.len();

        let input_ids: Vec<i64> = ids.iter().map(|&i| i as i64).collect();
        // No padding: every position is a real token, so the mask is all ones.
        let attention_mask: Vec<i64> = vec![1; n];
        let token_type_ids = vec![0i64; n];

        let mk = |v: Vec<i64>| -> Result<Tensor, Box<dyn std::error::Error>> {
            Ok(Tensor::from_shape(&[1, n], &v)?)
        };
        let mut map = HashMap::new();
        map.insert("input_ids".to_string(), mk(input_ids)?);
        map.insert("attention_mask".to_string(), mk(attention_mask)?);
        map.insert("token_type_ids".to_string(), mk(token_type_ids)?);
        Ok(map)
    }

    /// Embed `text`, returning a CLS-pooled, L2-normalized vector. Falls back to
    /// a zero vector on any inference error (callers treat zero vectors as
    /// "no signal" via cosine = 0).
    fn embed_inner(&self, text: &str) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
        let tensors = self.encode(text)?;
        let inputs: TVec<TValue> = self
            .input_order
            .iter()
            .map(|name| {
                tensors
                    .get(name)
                    .cloned()
                    .ok_or_else(|| format!("model expects unknown input '{name}'"))
                    .map(TValue::from)
            })
            .collect::<Result<_, _>>()?;

        let result = self.model.run(inputs)?;
        // Output 0 is last_hidden_state: [1, MAX_SEQ, EMBED_DIM]. CLS pooling
        // takes position 0 along the sequence axis.
        let view = result[0].to_plain_array_view::<f32>()?;
        let mut v: Vec<f32> = (0..EMBED_DIM).map(|k| view[[0, 0, k]]).collect();

        // L2 normalize so cosine similarity reduces to a dot product and
        // magnitudes don't bias ranking.
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        Ok(v)
    }
}

impl EmbeddingProvider for TractEmbedder {
    fn embed(&self, text: &str) -> Vec<f32> {
        self.embed_inner(text)
            .unwrap_or_else(|_| vec![0.0; EMBED_DIM])
    }

    fn dim(&self) -> usize {
        EMBED_DIM
    }
}
