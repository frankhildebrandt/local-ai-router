#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN_DIR="$PROJECT_DIR/sidecars/bin"
LICENSE_DIR="$PROJECT_DIR/sidecars/licenses"
LLAMA_REF="${LLAMA_CPP_REF:-9f0d017efb4a388bd5c60a27a575c90f20868e51}"
BUILD_DIR="$(mktemp -d)"
trap 'rm -rf "$BUILD_DIR"' EXIT

mkdir -p "$BIN_DIR" "$LICENSE_DIR"
git -C "$BUILD_DIR" init llama.cpp
git -C "$BUILD_DIR/llama.cpp" remote add origin https://github.com/ggml-org/llama.cpp.git
git -C "$BUILD_DIR/llama.cpp" fetch --depth 1 origin "$LLAMA_REF"
git -C "$BUILD_DIR/llama.cpp" checkout --detach FETCH_HEAD
cmake -S "$BUILD_DIR/llama.cpp" -B "$BUILD_DIR/llama.cpp/build" -DCMAKE_BUILD_TYPE=Release -DGGML_METAL=ON -DLLAMA_BUILD_SERVER=ON -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=OFF
cmake --build "$BUILD_DIR/llama.cpp/build" --config Release --target llama-server -j "$(sysctl -n hw.logicalcpu)"
cp "$BUILD_DIR/llama.cpp/build/bin/llama-server" "$BIN_DIR/llama-server-aarch64-apple-darwin"
cp "$BUILD_DIR/llama.cpp/LICENSE" "$LICENSE_DIR/llama.cpp-LICENSE"

swift build --package-path "$PROJECT_DIR/sidecars/mlx-server" -c release --arch arm64
cp "$PROJECT_DIR/sidecars/mlx-server/.build/arm64-apple-macosx/release/mlx-server" "$BIN_DIR/mlx-server-aarch64-apple-darwin"
cp "$PROJECT_DIR/sidecars/mlx-server/.build/checkouts/mlx-swift-lm/LICENSE" "$LICENSE_DIR/mlx-swift-lm-LICENSE"
chmod +x "$BIN_DIR"/*-aarch64-apple-darwin
