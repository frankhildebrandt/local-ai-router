#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN_DIR="$PROJECT_DIR/sidecars/bin"
LICENSE_DIR="$PROJECT_DIR/sidecars/licenses"
LLAMA_REF="${LLAMA_CPP_REF:-9f0d017efb4a388bd5c60a27a575c90f20868e51}"
BUILD_DIR="$(mktemp -d)"
HOST_TRIPLE="$(rustc -vV | awk '/^host:/{print $2}')"
OS="$(uname -s)"
trap 'rm -rf "$BUILD_DIR"' EXIT

job_count() {
  if command -v nproc >/dev/null 2>&1; then
    nproc
  elif command -v sysctl >/dev/null 2>&1; then
    sysctl -n hw.logicalcpu
  elif [[ -n "${NUMBER_OF_PROCESSORS:-}" ]]; then
    echo "$NUMBER_OF_PROCESSORS"
  else
    echo 4
  fi
}

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

llama_cmake_flags=(-DCMAKE_BUILD_TYPE=Release -DLLAMA_BUILD_SERVER=ON -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=OFF)
if [[ "$OS" == Darwin ]]; then
  llama_cmake_flags+=(-DGGML_METAL=ON)
else
  # CPU GGUF only on Linux/Windows for this slice; CUDA/Vulkan ship in later issues.
  llama_cmake_flags+=(-DGGML_METAL=OFF -DGGML_CUDA=OFF -DGGML_VULKAN=OFF -DBUILD_SHARED_LIBS=OFF)
fi
cmake -S "$BUILD_DIR/llama.cpp" -B "$BUILD_DIR/llama.cpp/build" "${llama_cmake_flags[@]}"
cmake --build "$BUILD_DIR/llama.cpp/build" --config Release --target llama-server -j "$(job_count)"

llama_server=""
for candidate in \
  "$BUILD_DIR/llama.cpp/build/bin/llama-server" \
  "$BUILD_DIR/llama.cpp/build/bin/llama-server.exe" \
  "$BUILD_DIR/llama.cpp/build/bin/Release/llama-server.exe" \
  "$BUILD_DIR/llama.cpp/build/Release/llama-server.exe"; do
  if [[ -f "$candidate" ]]; then
    llama_server="$candidate"
    break
  fi
done
if [[ -z "$llama_server" ]]; then
  echo "missing llama-server after cmake build" >&2
  exit 1
fi
llama_dest="$BIN_DIR/llama-server-$HOST_TRIPLE"
case "$llama_server" in
  *.exe) llama_dest="$llama_dest.exe" ;;
esac
cp "$llama_server" "$llama_dest"
llama_dir="$(dirname "$llama_server")"
shopt -s nullglob
for dll in "$llama_dir"/*.dll; do
  cp "$dll" "$BIN_DIR/"
done
shopt -u nullglob
cp "$BUILD_DIR/llama.cpp/LICENSE" "$LICENSE_DIR/llama.cpp-LICENSE"
chmod +x "$llama_dest"

if [[ "$OS" != Darwin ]]; then
  echo "Skipping MLX sidecars on $OS (Apple Silicon only)."
  chmod u+w "$LICENSE_DIR"/* 2>/dev/null || true
  exit 0
fi

swift build --package-path "$PROJECT_DIR/sidecars/mlx-server" -c release --arch arm64
cp "$PROJECT_DIR/sidecars/mlx-server/.build/arm64-apple-macosx/release/mlx-server" "$BIN_DIR/mlx-server-$HOST_TRIPLE"
copy_first "$LICENSE_DIR/mlx-swift-lm-LICENSE" \
  "$PROJECT_DIR/sidecars/mlx-server/.build/checkouts/mlx-swift-lm/LICENSE"

"$PROJECT_DIR/scripts/prepare-flux-vendor.sh"
swift build --package-path "$PROJECT_DIR/sidecars/mlx-image-server" -c release --arch arm64
cp "$PROJECT_DIR/sidecars/mlx-image-server/.build/arm64-apple-macosx/release/mlx-image-server" "$BIN_DIR/mlx-image-server-$HOST_TRIPLE"
copy_first "$LICENSE_DIR/flux-2-swift-mlx-LICENSE" \
  "$PROJECT_DIR/sidecars/vendor/flux-2-swift-mlx/LICENSE"
copy_first "$LICENSE_DIR/mlx-swift-examples-LICENSE" \
  "$PROJECT_DIR/sidecars/mlx-image-server/.build/checkouts/mlx-swift-examples/LICENSE"

"$PROJECT_DIR/scripts/prepare-kokoro-vendor.sh"
swift build --package-path "$PROJECT_DIR/sidecars/mlx-speech-server" -c release --arch arm64
cp "$PROJECT_DIR/sidecars/mlx-speech-server/.build/arm64-apple-macosx/release/mlx-speech-server" "$BIN_DIR/mlx-speech-server-$HOST_TRIPLE"
copy_first "$LICENSE_DIR/kokoro-swift-LICENSE" \
  "$PROJECT_DIR/sidecars/vendor/kokoro-swift/LICENSE"

"$PROJECT_DIR/scripts/build-mlx-metallib.sh" "$PROJECT_DIR/sidecars/mlx-server" "$BIN_DIR"
chmod +x "$BIN_DIR"/*-"$HOST_TRIPLE"
chmod u+w "$LICENSE_DIR"/* 2>/dev/null || true
