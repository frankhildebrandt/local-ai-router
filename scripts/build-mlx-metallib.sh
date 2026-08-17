#!/usr/bin/env bash
set -euo pipefail

# Compile MLX's default.metallib from the SwiftPM checkout. `swift build` does
# not compile Metal shaders, and xcodebuild currently fails validating mlx-swift's
# Linux-only CudaBuild plugin.

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PKG="${1:-$PROJECT_DIR/sidecars/mlx-server}"
BIN_DIR="${2:-$PROJECT_DIR/sidecars/bin}"

METAL_DIR="$(find "$PKG/.build/checkouts/mlx-swift" "$PKG/.build/DerivedData/SourcePackages/checkouts/mlx-swift" -path '*/Source/Cmlx/mlx-generated/metal' -type d 2>/dev/null | head -n 1 || true)"
if [[ -z "$METAL_DIR" ]]; then
  echo "mlx-swift Metal sources not found under $PKG; build the sidecar package first" >&2
  exit 1
fi

if ! xcrun -sdk macosx metal -v >/dev/null 2>&1; then
  echo "Metal toolchain missing. Install it with: xcodebuild -downloadComponent MetalToolchain" >&2
  exit 1
fi

WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT
airs=()
while IFS= read -r src; do
  rel="${src#"$METAL_DIR"/}"
  air="$WORKDIR/${rel//\//_}.air"
  echo "metal $rel"
  xcrun -sdk macosx metal -c \
    -fno-fast-math \
    -Wno-c++17-extensions \
    -Wno-c++20-extensions \
    -mmacosx-version-min=15.0 \
    -I "$METAL_DIR" \
    "$src" -o "$air"
  airs+=("$air")
done < <(find "$METAL_DIR" -name '*.metal' | sort)

if [[ ${#airs[@]} -eq 0 ]]; then
  echo "no .metal sources in $METAL_DIR" >&2
  exit 1
fi

mkdir -p "$BIN_DIR"
xcrun -sdk macosx metallib "${airs[@]}" -o "$BIN_DIR/default.metallib"
cp "$BIN_DIR/default.metallib" "$BIN_DIR/mlx.metallib"
