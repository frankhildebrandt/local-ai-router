# Local AI Router

A private, OpenAI-compatible model gateway for Apple Silicon Macs. Local AI Router combines OpenAI and OpenRouter models with on-device MLX and GGUF inference behind one authenticated endpoint.

## What works

- Tauri 2 menu-bar app for macOS 14+
- OpenAI and OpenRouter credentials stored in macOS Keychain
- Provider model discovery and manually entered model IDs
- Stable model aliases with ordered transient-error fallbacks
- `/v1/models`, Chat Completions, Responses, legacy Completions and Embeddings routing
- Cloud proxying for Images, Audio and Moderations, including multipart uploads
- Metadata-only request logs, CSV export and automatic 30-day retention
- Managed MLX/GGUF imports and resumable Hugging Face downloads
- Resident local model management with a configurable memory budget and idle unloading
- Native MLX and Metal-enabled llama.cpp sidecars

The gateway listens only on `http://127.0.0.1:11435/v1`. Every `/v1` request requires the local bearer token shown under Settings.

## Development

Requirements: Apple Silicon, macOS 14+, Node.js 22+, Rust 1.77+, Swift 6.2+, CMake, and Xcode command-line tools.

```bash
npm install
./scripts/build-sidecars.sh
npm run tauri dev
```

Building both inference engines takes time. The desktop shell and cloud router can be developed without them; local model startup will report a clear “runtime sidecar missing” error until the script has run.

Run all fast checks:

```bash
npm test
npm run build
cargo test --manifest-path src-tauri/Cargo.toml
```

### SDK usage

```python
from openai import OpenAI

client = OpenAI(
    base_url="http://127.0.0.1:11435/v1",
    api_key="lar_...",  # copy from Settings
)

response = client.chat.completions.create(
    model="my-assistant",
    messages=[{"role": "user", "content": "Hello"}],
)
```

Only configured aliases appear in `client.models.list()`. The alias must advertise the capability used by the endpoint.

## Data and security

- Provider, Hugging Face and local API tokens are stored only in Keychain.
- SQLite stores configuration and request metadata, never request/response bodies or authorization headers.
- The local bearer token is compared in constant time and can be rotated immediately.
- The gateway never binds to a LAN address.
- Imported models are copied into the app-managed Application Support directory.

## Release setup

The release workflow expects these GitHub Actions secrets:

- `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`
- `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`
- `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`

Before the first public build, replace the updater public-key placeholder in `src-tauri/tauri.conf.json`. Tags matching `v*` build a notarized Apple-Silicon DMG and signed updater artifacts.

## Runtime notes

`scripts/build-sidecars.sh` pins llama.cpp to a reviewed commit and MLX Swift LM to release `3.31.4`. Their licenses must accompany redistributed binaries; see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
