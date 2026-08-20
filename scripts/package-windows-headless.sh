#!/usr/bin/env bash
set -euo pipefail

# Archive the headless binary, admin SPA, and llama.cpp sidecars (CPU/CUDA/Vulkan when built).
PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${1:-$PROJECT_DIR/src-tauri/target/release/local-ai-router.exe}"
if [[ ! -f "$BIN" && -f "${BIN%.exe}" ]]; then
  BIN="${BIN%.exe}"
fi
OUT="${2:-$PROJECT_DIR/src-tauri/target/release/local-ai-router-windows-headless.zip}"

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

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
ROOT="$STAGE/local-ai-router"
mkdir -p "$ROOT/ui" "$ROOT/sidecars/bin"
cp "$BIN" "$ROOT/local-ai-router.exe"
cp -R "$UI/." "$ROOT/ui/"
"$PROJECT_DIR/scripts/copy-llama-sidecars.sh" "$ROOT/sidecars/bin"
cat >"$ROOT/README.txt" <<'EOF'
Local AI Router (headless, Windows)

Extract this archive and start the gateway in the foreground:

  local-ai-router.exe serve

Then open http://127.0.0.1:11435/ in a browser on this machine.

It loads .\ui and .\sidecars\bin next to the executable. sidecars\bin may include
llama-server (CPU), llama-server-cuda (NVIDIA), and llama-server-vulkan (AMD).
The router picks CUDA, then Vulkan, then CPU automatically. Override with
LOCAL_AI_ROUTER_GGUF_BACKEND=cpu|cuda|vulkan. Data lives in
%APPDATA%\app.local-ai-router.desktop. Secrets use Windows Credential
Manager unless you pass --secrets-file:

  local-ai-router.exe serve --data-dir .\data --secrets-file .\data\secrets.json

A Windows service is not required. For 24/7 unattended use, create a
Task Scheduler task that runs `local-ai-router.exe serve` at logon or
startup, or keep a terminal open. Prefer --secrets-file for tasks that
run without an interactive user session.

The desktop NSIS installer is a separate GitHub Release asset. Only one
process can bind port 11435; quit the tray app before serve, or the reverse.

Local MLX is not included. GGUF inference uses the bundled llama-server variants.
NVIDIA GPUs need a recent driver; AMD GPUs need a Vulkan 1.2+ driver. ROCm is
not supported. MLX remains Apple-only.
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
