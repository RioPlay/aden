#!/usr/bin/env bash
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Runs the GPU retrieval-research sidecar (scripts/gpu_eval.py) with the onnxruntime-gpu
# CUDA libraries on the loader path, so it uses the GPU instead of falling back to CPU.
#
# ONE-TIME SETUP (Python 3.14 system has no GPU wheels; uv fetches a 3.12 toolchain):
#   uv venv --python 3.12 ~/.cache/aden/gpu-venv
#   uv pip install --python ~/.cache/aden/gpu-venv/bin/python \
#       onnxruntime-gpu numpy tokenizers \
#       nvidia-cublas-cu12 nvidia-cudnn-cu12 nvidia-cuda-runtime-cu12 \
#       nvidia-cuda-nvrtc-cu12 nvidia-cufft-cu12 nvidia-curand-cu12
#
# THEN refresh the dumped corpus whenever the store changes:
#   cargo test -p aden-cli --test dump_corpus -- --include-ignored --nocapture
#
# Run:  scripts/gpu_eval.sh
set -euo pipefail
VENV="${ADEN_GPU_VENV:-$HOME/.cache/aden/gpu-venv}"
NVIDIA_LIBS="$(echo "$VENV"/lib/python*/site-packages/nvidia/*/lib | tr ' ' ':')"
export LD_LIBRARY_PATH="${NVIDIA_LIBS}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$VENV/bin/python" "$(dirname "$0")/gpu_eval.py" "$@"
