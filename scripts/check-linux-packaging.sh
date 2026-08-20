#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
UNIT="$PROJECT_DIR/packaging/linux/local-ai-router.service"

test -f "$UNIT"
grep -q '^ExecStart=/usr/bin/local-ai-router serve' "$UNIT"
grep -q -- '--secrets-file /var/lib/local-ai-router/secrets.json' "$UNIT"
grep -q -- '--data-dir /var/lib/local-ai-router' "$UNIT"
grep -q '^Type=simple' "$UNIT"
grep -q '^WantedBy=multi-user.target' "$UNIT"
test -f "$PROJECT_DIR/src-tauri/tauri.conf.json"
python3 - "$PROJECT_DIR/src-tauri/tauri.conf.json" <<'PY'
import json, sys
from pathlib import Path
conf = json.loads(Path(sys.argv[1]).read_text())
resources = conf["bundle"]["resources"]
assert "ui/" in resources
assert any(
    "local-ai-router.service" in str(value) or key.endswith("local-ai-router.service")
    for key, value in resources.items()
)
print("tauri linux resources include systemd unit")
PY

echo "linux packaging files ok"
