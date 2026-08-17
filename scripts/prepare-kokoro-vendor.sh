#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$PROJECT_DIR/sidecars/vendor/kokoro-swift"
PIN="20bf04c506e913ff129d7d2229398180ba24c690"

mkdir -p "$PROJECT_DIR/sidecars/vendor"
if [[ -d "$DEST/.git" ]]; then
  current="$(git -C "$DEST" rev-parse HEAD)"
  if [[ "$current" == "$PIN"* ]]; then
    exit 0
  fi
fi

rm -rf "$DEST"
git clone https://github.com/mweinbach/kokoro-swift.git "$DEST"
git -C "$DEST" checkout --detach "$PIN"
