#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
TOKEN="stub-contract-token"
export LOCAL_AI_ROUTER_RUNTIME_TOKEN="$TOKEN"
export LOCAL_AI_ROUTER_STUB_ENGINE=1

find_bin() {
  local name="$1"
  local path
  path="$(find "$PROJECT_DIR/sidecars/$name/.build" -type f -name "$name" -perm -111 2>/dev/null | head -n 1 || true)"
  if [[ -z "$path" ]]; then
    echo "missing $name binary; build the sidecar first" >&2
    exit 1
  fi
  printf '%s' "$path"
}

wait_health() {
  local port="$1"
  for _ in $(seq 1 80); do
    if curl --silent --fail -H "Authorization: Bearer $TOKEN" "http://127.0.0.1:${port}/health" >/dev/null; then
      return 0
    fi
    sleep 0.1
  done
  echo "sidecar on $port did not become ready" >&2
  return 1
}

CHAT_BIN="$(find_bin mlx-server)"
IMAGE_BIN="$(find_bin mlx-image-server)"
SPEECH_BIN="$(find_bin mlx-speech-server)"
PIDS=()
cleanup() { for pid in "${PIDS[@]:-}"; do kill "$pid" 2>/dev/null || true; done; }
trap cleanup EXIT

"$CHAT_BIN" --stub --model /tmp --host 127.0.0.1 --port 12191 --alias stub-chat &
PIDS+=("$!")
"$IMAGE_BIN" --stub --model /tmp --host 127.0.0.1 --port 12192 --alias stub-image --pipeline flux2 &
PIDS+=("$!")
"$SPEECH_BIN" --stub --model /tmp --host 127.0.0.1 --port 12193 --alias stub-speech &
PIDS+=("$!")

wait_health 12191
wait_health 12192
wait_health 12193

curl --silent --fail -H "Authorization: Bearer $TOKEN" -H "content-type: application/json" \
  -d '{"model":"stub-chat","messages":[{"role":"user","content":[{"type":"text","text":"hi"},{"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}}]}]}' \
  http://127.0.0.1:12191/v1/chat/completions | grep -q stub-vision

curl --silent --fail -H "Authorization: Bearer $TOKEN" -H "content-type: application/json" \
  -d '{"model":"stub-image","prompt":"a cat","n":1,"response_format":"b64_json"}' \
  http://127.0.0.1:12192/v1/images/generations | grep -q b64_json

curl --silent --fail -H "Authorization: Bearer $TOKEN" -H "content-type: application/json" \
  -d '{"model":"stub-speech","input":"hello","voice":"af_heart","response_format":"wav"}' \
  http://127.0.0.1:12193/v1/audio/speech | head -c 4 | grep -q RIFF

echo "sidecar stub contracts passed"
