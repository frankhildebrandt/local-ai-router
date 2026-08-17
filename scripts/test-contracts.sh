#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cargo run --quiet --manifest-path "$PROJECT_DIR/src-tauri/Cargo.toml" --example contract_server >"${TMPDIR:-/tmp}/local-ai-router-contract.log" 2>&1 &
SERVER_PID=$!
trap 'kill "$SERVER_PID" 2>/dev/null || true' EXIT

for _ in $(seq 1 120); do
  if curl --silent --fail http://127.0.0.1:11436/health >/dev/null; then break; fi
  sleep 0.25
done
curl --silent --fail http://127.0.0.1:11436/health >/dev/null
node "$PROJECT_DIR/tests/openai-contract.mjs"

PYTHON_BIN="${PYTHON_BIN:-python3}"
if "$PYTHON_BIN" -c 'import openai' >/dev/null 2>&1; then
  "$PYTHON_BIN" "$PROJECT_DIR/tests/openai_contract.py"
elif [[ "${REQUIRE_PYTHON_CONTRACT:-0}" == "1" ]]; then
  echo "Python OpenAI SDK is required but unavailable" >&2
  exit 1
fi

