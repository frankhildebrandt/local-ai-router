#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$PROJECT_DIR/sidecars/vendor/flux-2-swift-mlx"
PIN_TAG="v2.4.0"
MARKER="$DEST/.local-ai-router-patched"
ADAMW="$DEST/Sources/Flux2Core/Training/Optimizer/ResumableAdamW.swift"

mkdir -p "$PROJECT_DIR/sidecars/vendor"
if [[ -f "$MARKER" && -f "$ADAMW" ]]; then
  exit 0
fi

if [[ ! -d "$DEST/.git" ]]; then
  rm -rf "$DEST"
  git clone --depth 1 --branch "$PIN_TAG" https://github.com/VincentGourbin/flux-2-swift-mlx.git "$DEST"
fi

python3 - "$ADAMW" <<'PY'
from pathlib import Path
import sys
path = Path(sys.argv[1])
text = path.read_text()
text = text.replace("    public override init(\n", "    public init(\n")
text = text.replace(
    """    public override func applySingle(
        gradient: MLXArray,
        parameter: MLXArray,
        state: TupleState
    ) -> (MLXArray, TupleState) {""",
    """    public override func applySingle(
        gradient: MLXArray,
        parameter: MLXArray,
        state: AdamState
    ) -> (MLXArray, AdamState) {""",
)
path.write_text(text)
PY

printf 'flux-2-swift-mlx %s AdamW compatibility patch for current MLX Swift\n' "$PIN_TAG" > "$MARKER"
