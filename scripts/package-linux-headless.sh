#!/usr/bin/env bash
set -euo pipefail

# Archive the headless binary, admin SPA, CPU llama.cpp sidecar, and systemd unit.
PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${1:-$PROJECT_DIR/src-tauri/target/release/local-ai-router}"
OUT="${2:-$PROJECT_DIR/src-tauri/target/release/local-ai-router-linux-headless.tar.gz}"
HOST_TRIPLE="$(rustc -vV | awk '/^host:/{print $2}')"

if [[ ! -x "$BIN" ]]; then
  echo "missing headless binary: $BIN" >&2
  exit 1
fi

UI=""
for candidate in "$PROJECT_DIR/src-tauri/ui" "$PROJECT_DIR/dist"; do
  if [[ -f "$candidate/index.html" ]]; then
    UI="$candidate"
    break
  fi
done
if [[ -z "$UI" ]]; then
  echo "missing admin SPA (src-tauri/ui or dist)" >&2
  exit 1
fi

LLAMA=""
for candidate in \
  "$PROJECT_DIR/sidecars/bin/llama-server-$HOST_TRIPLE" \
  "$PROJECT_DIR/src-tauri/target/release/sidecars/bin/llama-server-$HOST_TRIPLE"; do
  if [[ -f "$candidate" ]]; then
    LLAMA="$candidate"
    break
  fi
done
if [[ -z "$LLAMA" ]]; then
  echo "missing llama-server-$HOST_TRIPLE (run ./scripts/build-sidecars.sh)" >&2
  exit 1
fi

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
ROOT="$STAGE/local-ai-router"
mkdir -p "$ROOT/ui" "$ROOT/sidecars/bin"
cp "$BIN" "$ROOT/local-ai-router"
cp -R "$UI/." "$ROOT/ui/"
cp "$LLAMA" "$ROOT/sidecars/bin/"
cp "$PROJECT_DIR/packaging/linux/local-ai-router.service" "$ROOT/local-ai-router.service"
cat >"$ROOT/README.txt" <<'EOF'
Local AI Router (headless)

This archive is meant to be extracted to /opt/local-ai-router:

  sudo mkdir -p /opt/local-ai-router
  sudo cp -a . /opt/local-ai-router
  sudo ln -sf /opt/local-ai-router/local-ai-router /usr/bin/local-ai-router

Then install the systemd unit from this directory (or from a .deb, which
already places the binary and resources under /usr). The unit binds
127.0.0.1 only; open http://127.0.0.1:11435/ on that host.

  sudo install -m 644 local-ai-router.service /etc/systemd/system/local-ai-router.service
  sudo useradd --system --home /var/lib/local-ai-router --shell /usr/sbin/nologin local-ai-router
  sudo mkdir -p /var/lib/local-ai-router
  sudo chown local-ai-router:local-ai-router /var/lib/local-ai-router
  sudo systemctl daemon-reload
  sudo systemctl enable --now local-ai-router

If you run the binary in place instead of installing to /usr:

  ./local-ai-router serve --data-dir ./data --secrets-file ./data/secrets.json

It loads ./ui and ./sidecars/bin next to the executable. Desktop Secret
Service is used when --secrets-file is omitted; systemd should always
pass --secrets-file (the unit does).
EOF

mkdir -p "$(dirname "$OUT")"
tar -C "$STAGE" -czf "$OUT" local-ai-router
echo "wrote $OUT"
