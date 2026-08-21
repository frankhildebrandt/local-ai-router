#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN_DIR="$PROJECT_DIR/sidecars/bin"
LICENSE_DIR="$PROJECT_DIR/sidecars/licenses"
LLAMA_REF="${LLAMA_CPP_REF:-9f0d017efb4a388bd5c60a27a575c90f20868e51}"
BUILD_DIR="$(mktemp -d)"
HOST_TRIPLE="$(rustc -vV | awk '/^host:/{print $2}')"
OS="$(uname -s)"
BUILD_LLAMA_CUDA="${BUILD_LLAMA_CUDA:-auto}"
BUILD_LLAMA_VULKAN="${BUILD_LLAMA_VULKAN:-auto}"
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

should_build_llama_variant() {
  local variant="$1"
  local flag=""
  case "$variant" in
    cpu) return 0 ;;
    cuda) flag="$BUILD_LLAMA_CUDA" ;;
    vulkan) flag="$BUILD_LLAMA_VULKAN" ;;
    *) return 1 ;;
  esac
  case "$flag" in
    1|true|yes|on) return 0 ;;
    0|false|no|off) return 1 ;;
    auto)
      case "$variant" in
        cuda)
          command -v nvcc >/dev/null 2>&1 \
            || [[ -n "${CUDA_PATH:-}" && -d "${CUDA_PATH}/bin" ]] \
            || [[ -d /usr/local/cuda/bin ]]
          ;;
        vulkan)
          pkg-config --exists vulkan 2>/dev/null \
            || [[ -d "${VULKAN_SDK:-}" ]] \
            || [[ -d /usr/include/vulkan ]] \
            || [[ -f /usr/lib/x86_64-linux-gnu/libvulkan.so ]] \
            || [[ -f /usr/lib/libvulkan.so.1 ]]
          ;;
      esac
      ;;
    *) return 1 ;;
  esac
}

find_llama_server() {
  local build_root="$1"
  local candidate
  for candidate in \
    "$build_root/bin/llama-server" \
    "$build_root/bin/llama-server.exe" \
    "$build_root/bin/Release/llama-server.exe" \
    "$build_root/Release/llama-server.exe"; do
    if [[ -f "$candidate" ]]; then
      echo "$candidate"
      return 0
    fi
  done
  return 1
}

copy_llama_runtime_libs() {
  local llama_server="$1"
  local llama_dir
  llama_dir="$(dirname "$llama_server")"
  shopt -s nullglob
  for dll in "$llama_dir"/*.dll; do
    cp "$dll" "$BIN_DIR/"
  done
  shopt -u nullglob
}

build_llama_variant() {
  local variant="$1"
  local stem="llama-server"
  local cmake_flags=(-DCMAKE_BUILD_TYPE=Release -DLLAMA_BUILD_SERVER=ON -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=OFF -DLLAMA_BUILD_UI=OFF)
  if [[ "$HOST_TRIPLE" == *windows* ]]; then
    cmake_flags+=(-G Ninja)
  fi
  case "$variant" in
    cpu)
      cmake_flags+=(-DGGML_METAL=OFF -DGGML_CUDA=OFF -DGGML_VULKAN=OFF -DBUILD_SHARED_LIBS=OFF)
      ;;
    cuda)
      stem="llama-server-cuda"
      cmake_flags+=(-DGGML_METAL=OFF -DGGML_CUDA=ON -DGGML_VULKAN=OFF -DBUILD_SHARED_LIBS=OFF)
      if [[ -n "${CUDA_PATH:-}" ]]; then
        local nvcc="$CUDA_PATH/bin/nvcc"
        [[ "$HOST_TRIPLE" == *windows* ]] && nvcc="${nvcc}.exe"
        cmake_flags+=(
          -DCMAKE_CUDA_COMPILER="$nvcc"
          -DCUDAToolkit_ROOT="$CUDA_PATH"
        )
        if [[ "$HOST_TRIPLE" == *windows* ]]; then
          cmake_flags+=(-DCMAKE_CUDA_ARCHITECTURES=86)
        fi
      fi
      ;;
    vulkan)
      stem="llama-server-vulkan"
      cmake_flags+=(-DGGML_METAL=OFF -DGGML_CUDA=OFF -DGGML_VULKAN=ON -DBUILD_SHARED_LIBS=OFF)
      if [[ -n "${VULKAN_SDK:-}" ]]; then
        cmake_flags+=(-DCMAKE_PREFIX_PATH="$VULKAN_SDK")
      fi
      if [[ -n "${SPIRV-Headers_DIR:-}" ]]; then
        cmake_flags+=(-DSPIRV-Headers_DIR="$SPIRV-Headers_DIR")
      fi
      ;;
    *)
      echo "unknown llama variant: $variant" >&2
      return 1
      ;;
  esac

  local src="$BUILD_DIR/llama.cpp-$variant"
  local build="$src/build"
  git -C "$BUILD_DIR" init "llama.cpp-$variant"
  git -C "$src" remote add origin https://github.com/ggml-org/llama.cpp.git
  git -C "$src" fetch --depth 1 origin "$LLAMA_REF"
  git -C "$src" checkout --detach FETCH_HEAD
  cmake -S "$src" -B "$build" "${cmake_flags[@]}"
  cmake --build "$build" --config Release --target llama-server -j "$(job_count)"

  local llama_server
  llama_server="$(find_llama_server "$build")"
  local dest="$BIN_DIR/$stem-$HOST_TRIPLE"
  case "$llama_server" in
    *.exe) dest="$dest.exe" ;;
  esac
  cp "$llama_server" "$dest"
  copy_llama_runtime_libs "$llama_server"
  chmod +x "$dest"
  echo "built $dest"
}

if [[ "$OS" == Darwin ]]; then
  git -C "$BUILD_DIR" init llama.cpp
  git -C "$BUILD_DIR/llama.cpp" remote add origin https://github.com/ggml-org/llama.cpp.git
  git -C "$BUILD_DIR/llama.cpp" fetch --depth 1 origin "$LLAMA_REF"
  git -C "$BUILD_DIR/llama.cpp" checkout --detach FETCH_HEAD
  cp "$BUILD_DIR/llama.cpp/LICENSE" "$LICENSE_DIR/llama.cpp-LICENSE"
  cmake -S "$BUILD_DIR/llama.cpp" -B "$BUILD_DIR/llama.cpp/build" \
    -DCMAKE_BUILD_TYPE=Release \
    -DLLAMA_BUILD_SERVER=ON \
    -DLLAMA_BUILD_TESTS=OFF \
    -DLLAMA_BUILD_EXAMPLES=OFF \
    -DLLAMA_BUILD_UI=OFF \
    -DGGML_METAL=ON
  cmake --build "$BUILD_DIR/llama.cpp/build" --config Release --target llama-server -j "$(job_count)"
  llama_server="$(find_llama_server "$BUILD_DIR/llama.cpp/build")"
  llama_dest="$BIN_DIR/llama-server-$HOST_TRIPLE"
  cp "$llama_server" "$llama_dest"
  chmod +x "$llama_dest"
else
  build_llama_variant cpu
  cp "$BUILD_DIR/llama.cpp-cpu/LICENSE" "$LICENSE_DIR/llama.cpp-LICENSE"
  if should_build_llama_variant cuda; then
    if [[ "$BUILD_LLAMA_CUDA" =~ ^(1|true|yes|on)$ ]]; then
      build_llama_variant cuda
    else
      build_llama_variant cuda || echo "warning: CUDA llama-server build failed; CPU fallback remains" >&2
    fi
  else
    echo "Skipping CUDA llama-server (set BUILD_LLAMA_CUDA=1 and install the CUDA toolkit to enable)."
  fi
  if should_build_llama_variant vulkan; then
    if [[ "$BUILD_LLAMA_VULKAN" =~ ^(1|true|yes|on)$ ]]; then
      build_llama_variant vulkan
    else
      build_llama_variant vulkan || echo "warning: Vulkan llama-server build failed; CPU fallback remains" >&2
    fi
  else
    echo "Skipping Vulkan llama-server (set BUILD_LLAMA_VULKAN=1 and install Vulkan dev packages to enable)."
  fi
fi

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
