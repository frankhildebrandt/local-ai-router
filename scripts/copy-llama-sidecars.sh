#!/usr/bin/env bash
# Copy every bundled llama-server variant (CPU, CUDA, Vulkan) into a destination directory.
set -euo pipefail

DEST="$1"
HOST_TRIPLE="$(rustc -vV | awk '/^host:/{print $2}')"
PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
shopt -s nullglob

copied=0
for candidate in \
  "$PROJECT_DIR/sidecars/bin"/llama-server*-"$HOST_TRIPLE" \
  "$PROJECT_DIR/sidecars/bin"/llama-server*-"$HOST_TRIPLE".exe \
  "$PROJECT_DIR/src-tauri/target/release/sidecars/bin"/llama-server*-"$HOST_TRIPLE" \
  "$PROJECT_DIR/src-tauri/target/release/sidecars/bin"/llama-server*-"$HOST_TRIPLE".exe; do
  if [[ -f "$candidate" ]]; then
    cp "$candidate" "$DEST/"
    copied=$((copied + 1))
  fi
done

for dll in "$PROJECT_DIR/sidecars/bin"/*.dll; do
  cp "$dll" "$DEST/"
done

shopt -u nullglob

if [[ "$copied" -eq 0 ]]; then
  echo "missing llama-server sidecars for $HOST_TRIPLE (run ./scripts/build-sidecars.sh)" >&2
  exit 1
fi

echo "copied $copied llama-server variant(s) to $DEST"
