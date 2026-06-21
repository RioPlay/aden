// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Golden parity test for the dense tokenizer.
//!
//! aden tokenizes for the bge-small embedder with `kitoken` (pure-Rust WordPiece)
//! instead of HuggingFace `tokenizers`, to drop the `paste` dependency
//! (RUSTSEC-2024-0436) and the C/C++ build deps. Correctness hinges on producing
//! token ids BYTE-IDENTICAL to the reference tokenizer, or embeddings silently
//! degrade. The `EXPECTED` ids below were generated from HuggingFace `tokenizers`
//! 0.23 over this corpus with the real bge-small `tokenizer.json` (add_special_tokens
//! = true). This test drives the actual production path (`TractEmbedder::token_ids`,
//! i.e. kitoken + the manual `[CLS] .. [SEP]` wrap) and asserts it reproduces them.
//!
//! Runs only when the `dense` feature is on AND the model is present (point at it with
//! `ADEN_BGE_MODEL_DIR`, else `~/.cache/aden-models/bge-small-en-v1.5`); skips otherwise,
//! mirroring `dense_spike.rs`. To regenerate EXPECTED after a model/vocab change, encode
//! this corpus through HuggingFace `tokenizers` with the new `tokenizer.json`.
//!
//! Run: `cargo test -p aden-index --features dense --test tokenizer_parity -- --nocapture`
#![cfg(feature = "dense")]

use aden_index::TractEmbedder;
use std::path::PathBuf;

/// (input text, reference token ids including the [CLS] .. [SEP] wrap).
const EXPECTED: &[(&str, &[u32])] = &[
    ("hello world", &[101, 7592, 2088, 102]),
    (
        "fn build_from_directory(dir: &Path) -> Result<AdenGraph, Error>",
        &[
            101, 1042, 2078, 3857, 1035, 2013, 1035, 14176, 1006, 16101, 1024, 1004, 4130, 1007,
            1011, 1028, 2765, 1026, 16298, 14413, 1010, 7561, 1028, 102,
        ],
    ),
    (
        "get_all_edges_per_type",
        &[
            101, 2131, 1035, 2035, 1035, 7926, 1035, 2566, 1035, 2828, 102,
        ],
    ),
    (
        "snake_case_name camelCaseName SCREAMING_SNAKE",
        &[
            101, 7488, 1035, 2553, 1035, 2171, 19130, 18382, 18442, 7491, 1035, 7488, 102,
        ],
    ),
    (
        "crates/aden-index/src/dense.rs",
        &[
            101, 27619, 1013, 16298, 1011, 5950, 1013, 5034, 2278, 1013, 9742, 1012, 12667, 102,
        ],
    ),
    (
        "a.b.c::d->e(f)[g]{h}!?;,",
        &[
            101, 1037, 1012, 1038, 1012, 1039, 1024, 1024, 1040, 1011, 1028, 1041, 1006, 1042,
            1007, 1031, 1043, 1033, 1063, 1044, 1065, 999, 1029, 1025, 1010, 102,
        ],
    ),
    (
        "café résumé naïve Zürich Müller",
        &[101, 7668, 13746, 15743, 10204, 12304, 102],
    ),
    (
        "東京 北京 こんにちは 你好世界",
        &[
            101, 1879, 1755, 1781, 1755, 1655, 30217, 30194, 30188, 30198, 100, 100, 1745, 100, 102,
        ],
    ),
    ("rocket 🚀 launch 👍🏽", &[101, 7596, 100, 4888, 100, 102]),
    (
        "supercalifragilisticexpialidociousandthensomeextralongtokenbeyondonehundredcharssxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        &[101, 100, 102],
    ),
    (
        "[CLS] already wrapped? [SEP]",
        &[101, 101, 2525, 5058, 1029, 102, 102],
    ),
    (
        "MixedРусскийΕλληνικάand한국어text",
        &[
            101, 3816, 16856, 29748, 29747, 29747, 23925, 15414, 29723, 29727, 29727, 24824, 16177,
            18199, 29726, 14608, 5685, 30005, 30006, 30021, 29991, 30014, 30020, 29999, 30008,
            18209, 102,
        ],
    ),
    ("", &[101, 102]),
    ("a", &[101, 1037, 102]),
];

fn model_dir() -> Option<PathBuf> {
    let dir = std::env::var("ADEN_BGE_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            PathBuf::from(home).join(".cache/aden-models/bge-small-en-v1.5")
        });
    (dir.join("model.onnx").exists() && dir.join("tokenizer.json").exists()).then_some(dir)
}

#[test]
fn kitoken_matches_reference_token_ids() {
    let Some(dir) = model_dir() else {
        eprintln!("SKIP: bge-small model not found (set ADEN_BGE_MODEL_DIR); skipping parity test");
        return;
    };
    let embedder = TractEmbedder::from_dir(&dir).expect("load bge-small");

    let mut mismatches = 0;
    for (text, expected) in EXPECTED {
        let got = embedder.token_ids(text).expect("tokenize");
        if got != *expected {
            mismatches += 1;
            eprintln!("MISMATCH for {text:?}\n  expected: {expected:?}\n  got:      {got:?}");
        }
    }
    assert_eq!(
        mismatches,
        0,
        "{mismatches}/{} inputs diverged from the reference tokenizer; kitoken output \
         is no longer byte-identical to HuggingFace tokenizers for bge-small",
        EXPECTED.len()
    );
}
