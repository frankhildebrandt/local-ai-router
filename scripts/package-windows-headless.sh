#!/usr/bin/env bash
set -euo pipefail

# Archive the headless binary, admin SPA, and CPU llama.cpp sidecar for Windows.
PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${1:-$PROJECT_DIR/src-tauri/target/release/local-ai-router.exe}"
if [[ ! -f "$BIN" && -f "${BIN%.exe}" ]]; then
  BIN="${BIN%.exe}"
fi
OUT="${2:-$PROJECT_DIR/src-tauri/target/release/local-ai-router-windows-headless.zip}"
HOST_TRIPLE="$(rustc -vV | awk '/^host:/{print $2}')"

if [[ ! -f "$BIN" ]]; then
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
  "$PROJECT_DIR/sidecars/bin/llama-server-$HOST_TRIPLE.exe" \
  "$PROJECT_DIR/sidecars/bin/llama-server-$HOST_TRIPLE" \
  "$PROJECT_DIR/src-tauri/target/release/sidecars/bin/llama-server-$HOST_TRIPLE.exe" \
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
cp "$BIN" "$ROOT/local-ai-router.exe"
cp -R "$UI/." "$ROOT/ui/"
cp "$LLAMA" "$ROOT/sidecars/bin/"
shopt -s nullglob
for dll in "$(dirname "$LLAMA")"/*.dll "$PROJECT_DIR/sidecars/bin"/*.dll; do
  cp "$dll" "$ROOT/sidecars/bin/"
done
shopt -u nullglob
cat >"$ROOT/README.txt" <<'EOF'
Local AI Router (headless, Windows)

Extract this archive and start the gateway in the foreground:

  local-ai-router.exe serve

Then open http://127.0.0.1:11435/ in a browser on this machine.

It loads .\ui and .\sidecars\bin next to the executable. Data lives in
%APPDATA%\app.local-ai-router.desktop. Secrets use Windows Credential
Manager unless you pass --secrets-file:

  local-ai-router.exe serve --data-dir .\data --secrets-file .\data\secrets.json

A Windows service is not required. For 24/7 unattended use, create a
Task Scheduler task that runs `local-ai-router.exe serve` at logon or
startup, or keep a terminal open. Prefer --secrets-file for tasks that
run without an interactive user session.

The desktop NSIS installer is a separate GitHub Release asset. Only one
process can bind port 11435; quit the tray app before serve, or the reverse.

Local MLX is not included. GGUF CPU inference uses the bundled llama-server.
CUDA and Vulkan are later releases.
EOF

mkdir -p "$(dirname "$OUT")"
python_bin="python3"
command -v python3 >/dev/null 2>&1 || python_bin="python"
"$python_bin" - "$STAGE" "$OUT" <<'PY'
import pathlib, shutil, sys
stage, out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
if out.exists():
    out.unlink()
base = out.with_suffix("")
archive = shutil.make_archive(str(base), "zip", root_dir=stage, base_dir="local-ai-router")
if pathlib.Path(archive) != out:
    pathlib.Path(archive).replace(out)
print("wrote", out)
PY
echo "wrote $OUT"
