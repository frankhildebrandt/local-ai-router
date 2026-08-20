#!/bin/sh
set -eu
cd "$(dirname "$0")/.."

ROOT="$(pwd)"
DATA="$(mktemp -d "${TMPDIR:-/tmp}/lar-headless.XXXXXX")"
UI="$DATA/ui"
if command -v python3 >/dev/null 2>&1; then
  PYTHON_BIN=python3
elif command -v python >/dev/null 2>&1; then
  PYTHON_BIN=python
else
  echo "python3 or python is required" >&2
  exit 1
fi
PORT="$("$PYTHON_BIN" -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')"
cleanup() {
  if [ -n "${PID:-}" ]; then
    kill "$PID" 2>/dev/null || true
    wait "$PID" 2>/dev/null || true
  fi
  rm -rf "$DATA"
}
trap cleanup EXIT INT TERM

mkdir -p "$UI"
if [ -f "$ROOT/dist/index.html" ]; then
  cp -R "$ROOT/dist/." "$UI/"
else
  printf '%s\n' '<!doctype html><title>Local AI Router</title><h1>Admin</h1>' > "$UI/index.html"
fi

cargo build --manifest-path src-tauri/Cargo.toml --quiet
BIN="$ROOT/src-tauri/target/debug/local-ai-router"
if [ -f "$BIN.exe" ]; then
  BIN="$BIN.exe"
fi
"$BIN" serve \
  --port "$PORT" \
  --data-dir "$DATA/data" \
  --ui-dir "$UI" \
  --secrets-file "$DATA/secrets.json" \
  >"$DATA/serve.log" 2>&1 &
PID=$!

for _ in $(seq 1 50); do
  if curl -fsS "http://127.0.0.1:$PORT/health" >/dev/null 2>&1; then
    break
  fi
  if ! kill -0 "$PID" 2>/dev/null; then
    echo "headless serve exited before becoming ready" >&2
    cat "$DATA/serve.log" >&2
    exit 1
  fi
  sleep 0.2
done

spa="$(curl -fsS "http://127.0.0.1:$PORT/")"
case "$spa" in
  *"Local AI Router"*) ;;
  *)
    echo "admin SPA was not served" >&2
    exit 1
    ;;
esac

created="$(curl -fsS -H "Content-Type: application/json" \
  -d '{"name":"Smoke"}' \
  "http://127.0.0.1:$PORT/admin/create_local_api_key")"
token="$("$PYTHON_BIN" -c 'import json,sys; print(json.loads(sys.argv[1])["token"])' "$created")"
case "$token" in
  lar_*) ;;
  *)
    echo "did not receive a local API token" >&2
    exit 1
    ;;
esac

models="$(curl -fsS -H "Authorization: Bearer $token" "http://127.0.0.1:$PORT/v1/models")"
case "$models" in
  *'"object":"list"'*|*"\"object\": \"list\""*|*"data"*) ;;
  *)
    echo "authenticated /v1/models failed: $models" >&2
    exit 1
    ;;
esac

echo "headless smoke ok on http://127.0.0.1:$PORT"
