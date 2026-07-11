// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// `aden model fetch` — opt-in, checksum-verified download of the local embedding
// model used by the `dense` (hybrid retrieval) feature.
//
// This is the ONLY network code in the aden binary, and it is compiled in only
// when built `--features model-fetch`. The default build is network-free by
// design (see scripts/fetch-bge-model.sh). Offline / air-gapped users skip this
// command and place `model.onnx` + `tokenizer.json` into the model dir by hand.

use std::error::Error;
use std::io::{Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};

const BASE: &str = "https://huggingface.co/BAAI/bge-small-en-v1.5/resolve/main";

/// Hard ceiling on a single downloaded file. `model.onnx` is ~127 MB; 512 MB
/// leaves generous headroom while bounding disk/IO if a misbehaving or hostile
/// server streams without end. The sha256 pin is the integrity gate; this is the
/// resource gate (ureq's default reader is unbounded — see `download_to_file`).
const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;

// (local filename, url path, pinned sha256). The sha256 values are PUBLIC file
// checksums (integrity, not credentials) and mirror scripts/fetch-bge-model.sh.
const FILES: &[(&str, &str, &str)] = &[
    (
        "model.onnx",
        "onnx/model.onnx",
        "828e1496d7fabb79cfa4dcd84fa38625c0d3d21da474a00f08db0f559940cf35", // aden:allow-secret
    ),
    (
        "tokenizer.json",
        "tokenizer.json",
        "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66", // aden:allow-secret
    ),
];

const LICENSE_NOTE: &str = "\
This directory contains the BAAI/bge-small-en-v1.5 model, licensed under the MIT
License. Copyright (c) BAAI. See https://huggingface.co/BAAI/bge-small-en-v1.5.
It is fetched by the user and is NOT part of aden's AGPL-licensed source.
";

/// Download (if needed) and verify the bge embedding model into `bge_model_dir()`.
/// `force` re-downloads even when a verified copy is already present.
pub fn cmd_model_fetch(force: bool) -> Result<(), Box<dyn Error>> {
    let dest = crate::util::bge_model_dir();
    std::fs::create_dir_all(&dest)?;
    println!(
        "aden: fetching bge-small-en-v1.5 (MIT) into {}",
        dest.display()
    );

    for (name, path, want) in FILES {
        let out = dest.join(name);
        if !force && out.exists() && file_sha256(&out)? == *want {
            println!("  ✓ {name} already present and verified");
            continue;
        }
        println!("  ↓ downloading {name} ...");
        let url = format!("{BASE}/{path}");
        // Stream to a `<name>.partial` sibling while hashing incrementally, then
        // verify the digest BEFORE the atomic rename, so the bytes never sit
        // wholesale in RAM and an interrupted/oversized/corrupt download never
        // leaves a bad model.onnx that loads as garbage. (`<name>.partial`, not
        // with_extension("partial") which would strip `.onnx` -> `model.partial`.)
        let tmp = dest.join(format!("{name}.partial"));
        let got = match download_to_file(&url, &tmp) {
            Ok(h) => h,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        };
        if got != *want {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!(
                "checksum mismatch for {name}\n  expected {want}\n  got      {got}"
            )
            .into());
        }
        std::fs::rename(&tmp, &out)?;
        println!("  ✓ {name} verified");
    }

    std::fs::write(dest.join("LICENSE-MODEL.txt"), LICENSE_NOTE)?;
    println!(
        "aden: model ready. Build with the dense feature to use hybrid search:\n      \
         cargo build -p aden-cli --features dense"
    );
    Ok(())
}

/// Stream `url` into `tmp`, returning the hex sha256 of what was written. Bytes
/// are read through a bounded reader (`MAX_FILE_BYTES`) in fixed chunks that are
/// hashed and written incrementally, so memory stays flat regardless of response
/// size. `https_only` refuses any redirect that would downgrade to http (defense
/// in depth alongside the sha256 pin the caller checks). ureq returns `Err` for
/// non-2xx by default, so a returned `Ok` means a real 2xx body.
fn download_to_file(url: &str, tmp: &Path) -> Result<String, Box<dyn Error>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .build()
        .into();
    let resp = agent
        .get(url)
        .header("User-Agent", "aden-model-fetch")
        .call()
        .map_err(|e| format!("failed to fetch {url}: {e}"))?;
    let mut reader = resp
        .into_body()
        .into_with_config()
        .limit(MAX_FILE_BYTES)
        .reader();
    let mut file = std::fs::File::create(tmp)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])?;
    }
    file.flush()?;
    Ok(hex(&hasher.finalize()))
}

fn file_sha256(path: &Path) -> Result<String, Box<dyn Error>> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_is_lowercase_zero_padded() {
        assert_eq!(hex(&[0x00, 0x0f, 0xff, 0xa5]), "000fffa5");
        assert_eq!(hex(&[]), "");
    }

    #[test]
    fn file_sha256_matches_known_digest() {
        // sha256("abc") is a fixed, well-known public test vector (integrity, not a
        // credential) — same category as the FILES checksums above.
        let want = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"; // aden:allow-secret
        let p =
            std::env::temp_dir().join(format!("aden-model-fetch-test-{}.bin", std::process::id()));
        std::fs::write(&p, b"abc").unwrap();
        let got = file_sha256(&p);
        let _ = std::fs::remove_file(&p);
        assert_eq!(got.unwrap(), want);
    }
}
