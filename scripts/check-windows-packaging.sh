#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
CONF="$PROJECT_DIR/src-tauri/tauri.conf.json"

python_bin() {
  if command -v python3 >/dev/null 2>&1; then
    echo python3
  elif command -v python >/dev/null 2>&1; then
    echo python
  else
    echo "python3 or python is required" >&2
    return 1
  fi
}

test -f "$PROJECT_DIR/scripts/package-windows-headless.sh"
grep -q 'Windows Credential' "$PROJECT_DIR/scripts/package-windows-headless.sh"
grep -q 'local-ai-router.exe serve' "$PROJECT_DIR/scripts/package-windows-headless.sh"
test -f "$PROJECT_DIR/scripts/sync-ui.mjs"
test -f "$PROJECT_DIR/src-tauri/icons/icon.ico"
test -f "$CONF"
grep -q 'node scripts/sync-ui.mjs' "$CONF"
"$(python_bin)" - "$CONF" <<'PY'
import json, sys
from pathlib import Path
conf = json.loads(Path(sys.argv[1]).read_text())
assert "nsis" in conf["bundle"]["targets"]
resources = conf["bundle"]["resources"]
assert "ui/" in resources
assert any("sidecars/bin" in str(key) or "sidecars/bin" in str(value) for key, value in resources.items())
print("tauri windows bundle includes nsis, ui, and llama.cpp sidecars")
PY

echo "windows packaging files ok"
