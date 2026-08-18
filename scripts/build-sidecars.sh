#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN_DIR="$PROJECT_DIR/sidecars/bin"
LICENSE_DIR="$PROJECT_DIR/sidecars/licenses"
LLAMA_REF="${LLAMA_CPP_REF:-9f0d017efb4a388bd5c60a27a575c90f20868e51}"
BUILD_DIR="$(mktemp -d)"
trap 'rm -rf "$BUILD_DIR"' EXIT

mkdir -p "$BIN_DIR" "$LICENSE_DIR"

copy_first() {
  local dest="$1"
  shift
  local candidate
  for candidate in "$@"; do
    if [[ -f "$candidate" ]]; then
      chmod u+w "$dest" 2>/dev/null || true
      cp "$candidate" "$dest"
      chmod u+w "$dest"
      return 0
    fi
  done
  echo "missing $(basename "$dest")" >&2
  return 1
}

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
copy_first "$LICENSE_DIR/mlx-swift-lm-LICENSE" \
  "$PROJECT_DIR/sidecars/mlx-server/.build/checkouts/mlx-swift-lm/LICENSE"

"$PROJECT_DIR/scripts/prepare-flux-vendor.sh"
swift build --package-path "$PROJECT_DIR/sidecars/mlx-image-server" -c release --arch arm64
cp "$PROJECT_DIR/sidecars/mlx-image-server/.build/arm64-apple-macosx/release/mlx-image-server" "$BIN_DIR/mlx-image-server-aarch64-apple-darwin"
copy_first "$LICENSE_DIR/flux-2-swift-mlx-LICENSE" \
  "$PROJECT_DIR/sidecars/vendor/flux-2-swift-mlx/LICENSE"
copy_first "$LICENSE_DIR/mlx-swift-examples-LICENSE" \
  "$PROJECT_DIR/sidecars/mlx-image-server/.build/checkouts/mlx-swift-examples/LICENSE"

"$PROJECT_DIR/scripts/prepare-kokoro-vendor.sh"
swift build --package-path "$PROJECT_DIR/sidecars/mlx-speech-server" -c release --arch arm64
cp "$PROJECT_DIR/sidecars/mlx-speech-server/.build/arm64-apple-macosx/release/mlx-speech-server" "$BIN_DIR/mlx-speech-server-aarch64-apple-darwin"
copy_first "$LICENSE_DIR/kokoro-swift-LICENSE" \
  "$PROJECT_DIR/sidecars/vendor/kokoro-swift/LICENSE"

"$PROJECT_DIR/scripts/build-mlx-metallib.sh" "$PROJECT_DIR/sidecars/mlx-server" "$BIN_DIR"
chmod +x "$BIN_DIR"/*-aarch64-apple-darwin
chmod u+w "$LICENSE_DIR"/* 2>/dev/null || true
