#!/usr/bin/env bash
# fetch-bge-model.sh — one-time setup for aden's optional dense (hybrid) search.
#
# Downloads the local embedding model used by `aden`'s `dense` feature into the
# model cache. This is a *setup* step (like installing the binary), not a runtime
# dependency: once the model is present, `aden search`/`ask` run fully offline.
#
# aden itself has NO network code by design — this script is the only thing that
# touches the network, and only when you choose to run it.
#
# Model: BAAI/bge-small-en-v1.5 (MIT-licensed, 384-dim). tract (aden's pure-Rust
# ONNX runtime) needs the fp32 graph; quantized/fp16 exports don't load, so this
# is ~127 MB. Downloads are checksum-verified.
#
# Usage:
#   scripts/fetch-bge-model.sh            # -> ~/.cache/aden-models/bge-small-en-v1.5
#   ADEN_BGE_MODEL_DIR=/path scripts/fetch-bge-model.sh
#
# Offline / air-gapped: skip this script and place `model.onnx` + `tokenizer.json`
# from BAAI/bge-small-en-v1.5 into the target dir by hand; aden picks them up.
set -euo pipefail

DEST="${ADEN_BGE_MODEL_DIR:-$HOME/.cache/aden-models/bge-small-en-v1.5}"
BASE="https://huggingface.co/BAAI/bge-small-en-v1.5/resolve/main"

# (filename, url-path, sha256) — pinned for integrity + reproducibility.
# The sha256 values are PUBLIC file checksums, not credentials. The trailing
# `aden:allow-secret` marks them so the secret scanner doesn't flag the long-hex.
FILES=(
  "model.onnx|onnx/model.onnx|828e1496d7fabb79cfa4dcd84fa38625c0d3d21da474a00f08db0f559940cf35" # aden:allow-secret
  "tokenizer.json|tokenizer.json|d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66" # aden:allow-secret
)

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'
  elif command -v shasum  >/dev/null 2>&1; then shasum -a 256 "$1" | awk '{print $1}'
  else echo "ERROR: need sha256sum or shasum for verification" >&2; exit 1; fi
}

command -v curl >/dev/null 2>&1 || { echo "ERROR: curl is required" >&2; exit 1; }
mkdir -p "$DEST"
echo "aden: fetching bge-small-en-v1.5 (MIT) into $DEST"

for entry in "${FILES[@]}"; do
  IFS='|' read -r name path want <<<"$entry"
  out="$DEST/$name"
  if [[ -f "$out" && "$(sha256_of "$out")" == "$want" ]]; then
    echo "  ✓ $name already present and verified"
    continue
  fi
  echo "  ↓ downloading $name ..."
  tmp="$out.partial"
  curl -fSL --retry 3 "$BASE/$path" -o "$tmp"
  got="$(sha256_of "$tmp")"
  if [[ "$got" != "$want" ]]; then
    rm -f "$tmp"
    echo "ERROR: checksum mismatch for $name" >&2
    echo "  expected $want" >&2
    echo "  got      $got" >&2
    exit 1
  fi
  mv -f "$tmp" "$out"
  echo "  ✓ $name verified"
done

# Record the model's own license alongside it (MIT requires attribution).
cat > "$DEST/LICENSE-MODEL.txt" <<'EOF'
This directory contains the BAAI/bge-small-en-v1.5 model, licensed under the MIT
License. Copyright (c) BAAI. See https://huggingface.co/BAAI/bge-small-en-v1.5.
It is fetched by the user and is NOT part of aden's AGPL-licensed source.
EOF

echo "aden: model ready. Build with the dense feature to use hybrid search:"
echo "      cargo build -p aden-cli --features dense"
