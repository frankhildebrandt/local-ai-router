# Local AI Router

A private, multi-protocol model gateway for Apple Silicon Macs. Local AI Router combines hosted providers with on-device MLX and GGUF inference behind OpenAI-, Anthropic- and Gemini-compatible APIs.

## What works

- Tauri 2 menu-bar app for macOS 14+
- Presets for OpenAI, Anthropic, OpenRouter, Poolside, MiniMax, Z.AI, OpenCode Zen, Gemini, Groq, Cerebras, Mistral, Hugging Face, NVIDIA NIM and SambaNova
- API keys and experimental OpenAI Subscription OAuth tokens stored in macOS Keychain
- Provider model discovery and manually entered model IDs
- Stable model aliases with ordered transient-error fallbacks
- `/v1/chat/completions`, `/v1/responses`, Anthropic `/v1/messages`, and Gemini `/v1beta/models/*` with non-streaming, SSE, vision and tool translation
- Cloud proxying for Images, Audio and Moderations, including multipart uploads
- Named local client keys with per-key usage attribution
- Usage dashboard plus filterable metadata-only request logs, filtered CSV export and automatic 30-day retention
- Managed MLX/GGUF imports and resumable Hugging Face downloads
- Resident local model management with a configurable memory budget and idle unloading
- Native MLX and Metal-enabled llama.cpp sidecars

The gateway listens only on `http://127.0.0.1:11435`. Every request requires a local token managed under Settings. Supply it as `Authorization: Bearer`, `x-api-key`, or `x-goog-api-key`; query-string keys are deliberately rejected because URLs are commonly logged.

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

The same alias can be called through each supported client protocol, provided every target in its route has the required capabilities.

OpenAI:

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

Anthropic:

```python
from anthropic import Anthropic

client = Anthropic(base_url="http://127.0.0.1:11435", api_key="lar_...")
message = client.messages.create(
    model="my-assistant",
    max_tokens=512,
    messages=[{"role": "user", "content": "Hello"}],
)
```

Google Gen AI:

```python
from google import genai
from google.genai import types

client = genai.Client(
    api_key="lar_...",
    http_options=types.HttpOptions(base_url="http://127.0.0.1:11435/v1beta"),
)
response = client.models.generate_content(model="my-assistant", contents="Hello")
```

Only configured aliases appear in `client.models.list()`. The alias must advertise the capability used by the endpoint.

## Provider notes

- Poolside requires the HTTPS deployment domain assigned to your organization; enter its OpenAI-compatible base URL in the preset.
- MiniMax and Z.AI expose separate general, token/coding-plan and regional endpoints. Select the matching preset and adjust the URL only where the provider supports it.
- OpenCode Zen catalog entries are imported only when their upstream wire protocol can be identified. Protocol and capability metadata can be corrected manually before adding a target.
- Free-tier and starter-credit badges are informational snapshots shipped with the app, not availability guarantees. Limits, eligible models and regional access can change at any time.
- Embeddings, image, audio and moderation requests are native-only. Enable those capabilities only when the selected target implements the matching endpoint.
- OpenAI Subscription support is experimental and uses a curated, version-fixed model list. It is not a documented general OpenAI Platform API access method; an [OpenAI API key](https://developers.openai.com/api/reference/overview#authentication) remains the stable choice for applications.

## Data and security

- Provider, Hugging Face and local API tokens are stored only in Keychain. Subscription access/refresh tokens, expiry and account ID are kept together in a versioned Keychain record.
- SQLite stores configuration and request metadata, never request/response bodies or authorization headers.
- Local bearer tokens are stored in Keychain, compared in constant time and can be rotated or revoked immediately.
- The gateway never binds to a LAN address.
- Imported models are copied into the app-managed Application Support directory.
- Cloud prompts and tool data are sent to the provider selected by the route and remain subject to that provider's retention, training and regional-processing terms. Fallback targets may send a request to a different provider only after an eligible pre-stream failure.

## Release setup

The release workflow expects these GitHub Actions secrets:

- `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`
- `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`
- `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`

Before the first public build, replace the updater public-key placeholder in `src-tauri/tauri.conf.json`. Tags matching `v*` build a notarized Apple-Silicon DMG and signed updater artifacts.

## Runtime notes

`scripts/build-sidecars.sh` pins llama.cpp to a reviewed commit and MLX Swift LM to release `3.31.4`. Their licenses must accompany redistributed binaries; see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
