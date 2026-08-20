# Local AI Router

A private, multi-protocol model gateway for Apple Silicon Macs. Local AI Router combines hosted providers with on-device MLX and GGUF inference behind OpenAI-, Anthropic- and Gemini-compatible APIs.

## What works

- Tauri 2 menu-bar app for macOS 15+, plus a headless `serve` process that hosts the same engine and admin UI in a browser on loopback
- Closing the window hides to the menu bar; quit from the app menu, tray, or Cmd+Q stops the gateway
- Presets for OpenAI, Anthropic, OpenRouter, Poolside, MiniMax, Z.AI, OpenCode Zen, Gemini, Groq, Cerebras, Mistral, Hugging Face, NVIDIA NIM and SambaNova
- API keys and experimental OpenAI Subscription OAuth tokens stored in macOS Keychain
- Provider model discovery and manually entered model IDs, with features and list prices from the provider API or known-model defaults
- Configured local and cloud models are published automatically as public model IDs
- Custom aliases remain optional named stacks. Primaries form the routing pool; fallbacks are sequential failover after the pool is exhausted. Each alias can use Performance (listed primary order) or Adaptive ranking; Adaptive stays off by default
- Built-in `adaptive-routing` ranks every enabled model by task quality, price and inferred task
- `/v1/chat/completions`, `/v1/responses`, Anthropic `/v1/messages`, and Gemini `/v1beta/models/*` with non-streaming, SSE, vision, audio/video input and tool translation
- Local `/v1/images/generations` and `/v1/audio/speech` for installed MLX image and TTS models
- Cloud proxying for Images, Audio and Moderations, including multipart uploads
- Named local client keys with per-key usage attribution
- Built-in open-source chat playground, preconfigured for enabled chat models
- Usage dashboard plus filterable metadata-only request logs, filtered CSV export and automatic 30-day retention
- Curated MLX catalog with RAM-aware install, live Hugging Face MLX search, CivitAI image downloads, managed imports and resumable downloads
- Stealth, Balanced, Performance and Custom resource profiles with automatic loading, idle unloading, prompt concurrency and per-model overrides
- Native MLX chat, image and speech sidecars plus Metal-enabled llama.cpp
- Optional speculative decoding for local chat models: pair a smaller draft from the library, or GGUF n-gram without an extra model

The gateway listens only on `http://127.0.0.1:11435`. Inference APIs require a local token managed under **API keys**. Supply it as `Authorization: Bearer`, `x-api-key`, or `x-goog-api-key`; query-string keys are deliberately rejected because URLs are commonly logged. The admin UI on that same loopback port is local-process privileged (like the desktop app’s IPC): it does not use a bearer token.

## Install

Apple Silicon, macOS 15+:

```bash
brew tap frankhildebrandt/tap
brew install --cask local-ai-router
```

GitHub Releases also ship a DMG. After a published release, the [homebrew-tap](https://github.com/frankhildebrandt/homebrew-tap) cask is bumped automatically (daily, or immediately when `HOMEBREW_TAP_TOKEN` can dispatch the tap workflow).

## Development

Requirements: Apple Silicon, macOS 15+, Node.js 22+, Rust 1.77+, Swift 6.2+, CMake, and Xcode command-line tools.

```bash
npm install
./scripts/build-sidecars.sh
npm run tauri dev
```

Headless (no window or tray). Uses the same Application Support directory, loopback port and Keychain as the desktop app unless you override them:

```bash
npm run build
cargo run --manifest-path src-tauri/Cargo.toml -- serve --ui-dir dist
```

Then open `http://127.0.0.1:11435/` in a browser. The packaged macOS binary accepts the same command:

```bash
"/Applications/Local AI Router.app/Contents/MacOS/local-ai-router" serve
```

| | Desktop / tray | Headless `serve` |
| --- | --- | --- |
| Data directory | `~/Library/Application Support/app.local-ai-router.desktop` | Same, or `--data-dir` |
| Bind address | `127.0.0.1` (never LAN) | Same; `--port` overrides the saved setting (default `11435`) |
| Secrets | macOS Keychain service `app.local-ai-router.desktop` | Same Keychain, or `--secrets-file` for an isolated 0600 JSON vault (CI / extra data dirs) |
| Admin UI | Native window + tray | Browser at `http://127.0.0.1:<port>/` |
| Inference APIs | `/v1/*` with a local API key | Same |

Only one process can bind the port. Quit the menu-bar app before `serve`, or the reverse.

Building both inference engines takes time. `swift build` compiles the MLX servers but skips Metal shaders, so `scripts/build-sidecars.sh` also compiles `mlx.metallib` next to the binaries. That step needs Xcode’s Metal toolchain (`xcodebuild -downloadComponent MetalToolchain`). The desktop shell and cloud router can be developed without sidecars; local model startup will report a missing binary or missing Metal kernels until the script has run.

Run all fast checks:

```bash
npm test
npm run build
cargo test --manifest-path src-tauri/Cargo.toml
./scripts/test-headless.sh
```

### SDK usage

The same public model ID can be called through each supported client protocol, provided the selected model has the required capabilities. Configured local and cloud models appear automatically; custom aliases are optional named stacks.

OpenAI:

```python
from openai import OpenAI

client = OpenAI(
    base_url="http://127.0.0.1:11435/v1",
    api_key="lar_...",  # copy from API keys
)

response = client.chat.completions.create(
    model="adaptive-routing",
    messages=[{"role": "user", "content": "Hello"}],
)
```

Anthropic:

```python
from anthropic import Anthropic

client = Anthropic(base_url="http://127.0.0.1:11435", api_key="lar_...")
message = client.messages.create(
    model="adaptive-routing",
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
response = client.models.generate_content(model="adaptive-routing", contents="Hello")
```

`client.models.list()` includes every enabled local and cloud model, custom routes, and `adaptive-routing` when at least one model is enabled. The selected model must advertise the capability used by the endpoint.

### Adaptive routing

`adaptive-routing` is always on and ranks every enabled model by task quality, predicted cost and the inferred task. Custom routes default to **Performance** routing: the **primary pool** in listed order, plus skipping individual slow, rate-limited or failing primaries. Switching a custom route to **Adaptive** ranks only those primaries. **Fallbacks** are sequential failover after the primary pool is exhausted by errors, circuit/slow skips, or a missing feature such as vision. They are not ranked and do not use the tight first-byte timeout. Adaptive policies filter primaries by capabilities, context, privacy and configured cost limits, then deterministically rank them. Draft policies on custom routes do not affect traffic; Shadow policies (Expert) record the model they would select while the Performance primary order continues serving. Existing aliases whose extra hops should stay in the Adaptive pool need those hops set as Primaries in the editor.

Before the first response chunk, the gateway retries the same target once after a short pause (400 ms, or a `Retry-After` of at most 2 seconds) on transient failures: 429, 5xx, connect errors and timeouts. 400/401/403/404 are not retried on the same target. If that replay still fails, it falls back to the next candidate on connect errors, timeouts, and **all 4xx/5xx** responses. Remaining primaries are tried before reserve fallbacks. A pinned public model (a specific NVIDIA id, for example) only retries that target; it does not silently switch models. Use `adaptive-routing` or a custom route with extra hops for rerouting. Router-generated client errors (invalid local API key, query-string key, malformed JSON, unknown task header) stay final, and there is no mid-stream failover. 400/401/403 trigger fallback but do not open a circuit. Three consecutive 404s on a target open a two-minute circuit. A 429 without a reset deadline cools that target for 30 seconds; `Retry-After` and provider reset headers (`x-ratelimit-reset-requests`, `x-ratelimit-reset`, `anthropic-ratelimit-requests-reset`) are honored when present. A primary whose recent TTFT (or latency) is at least 8 seconds and three times the peer median is skipped for 45 seconds; remaining primaries also get a tighter first-byte timeout. Fallbacks keep the full 120-second timeout and only open a circuit after repeated transient failures.

Custom-route hops can name another public model (`adaptive-routing` or a custom alias). Cycles are skipped. Request metadata stays on Request logs; ranking, retries and fallback decisions are on the Routing page. Overview shows in-flight requests with the current model, retry/reroute phase and last upstream error via live desktop events and lets you stop them.

Clients can bypass automatic task rules with a local-only header:

```bash
curl http://127.0.0.1:11435/v1/chat/completions \
  -H "Authorization: Bearer lar_..." \
  -H "Content-Type: application/json" \
  -H "X-Local-AI-Task: coding" \
  -d '{"model":"adaptive-routing","messages":[{"role":"user","content":"Refactor this function"}]}'
```

The task header is validated and never forwarded upstream. Responses expose `X-Local-AI-Task`, `X-Local-AI-Target`, `X-Local-AI-Routing-Mode` and `X-Local-AI-Routing-Reason`. Routing profiles, policies and custom tasks can be previewed and imported/exported as `local-ai-router/routing-policy/v1` JSON; credentials, prompts, responses and measurement history are excluded.

### Background inference and persistent sessions

New installations default to Stealth mode: local sidecars run at low process priority and are runnable for 100 ms of each 400 ms window while inference is active. This is a 25% scheduler duty cycle, not a guaranteed 25% Metal utilization value; macOS does not expose per-process GPU or Neural Engine compute quotas to these runtimes. GGUF additionally exposes GPU-layer and CPU-thread limits. MLX uses Metal and receives process memory/cache limits; the current engines do not target the Apple Neural Engine.

Stopped local models load automatically on their first request. Active prompts are limited per model, while additional authenticated requests wait in a FIFO queue until the client disconnects or the app shuts down. Resource changes restart loaded sidecars only after active and queued work has drained.

GGUF clients can opt into persistent llama.cpp KV snapshots by sending `X-Local-AI-Session` with a stable value of at most 128 characters. MLX chat reuses a token prefix KV in RAM without that header: each request still sends the full history, and the sidecar prefills only the suffix after a longest-common-prefix match. After idle unload, MLX restores from disk using SHA-256 hashes of token-block prefixes (256-token windows plus the exact saved length), so a session header is not required for a hit. `X-Local-AI-Session` remains optional isolation for two chats that share a system prompt, and it is still required for named GGUF snapshots. Persistent KV requires one parallel prompt. Snapshots are isolated by API key and model (and session when present), stored with private filesystem permissions under Application Support, and evicted least-recently-used above 10 GiB. They are not encrypted independently of the Mac filesystem.

Local chat models accept OpenAI `input_audio` and the Local AI Router extension `input_video`, plus Gemini `inlineData` for image, audio and video. Anthropic remains text and image only. Remote media must be a data URL or public HTTPS URL; private, loopback and link-local destinations are blocked. Generated images and speech are returned to the client and are neither persisted nor written to request logs.

`/v1/images/generations` for local targets supports `model`, `prompt`, `n: 1`, engine-compatible sizes and `response_format: "b64_json"`. The image sidecar runs FLUX.2, SDXL, and non-XL Stable Diffusion (SD 1.x / 2.x Diffusers layouts). Image search can download from Hugging Face or CivitAI. `/v1/audio/speech` supports `model`, `input`, `voice`, `speed`, plus `wav` and `pcm`. New local speech targets must advertise `speech`; stored cloud targets may still use `audio`.

## Provider notes

- Poolside requires the HTTPS deployment domain assigned to your organization; enter its OpenAI-compatible base URL in the preset.
- MiniMax and Z.AI expose separate general, token/coding-plan and regional endpoints. Select the matching preset and adjust the URL only where the provider supports it.
- OpenCode Zen catalog entries are imported only when their upstream wire protocol can be identified. Protocol and capability metadata can be corrected manually before adding a target.
- Free-tier and starter-credit badges are informational snapshots shipped with the app, not availability guarantees. Limits, eligible models and regional access can change at any time.
- Embeddings, image, audio and moderation requests are native-only. Enable those capabilities only when the selected target implements the matching endpoint.
- OpenAI Subscription support is experimental and uses a curated, version-fixed model list. It is not a documented general OpenAI Platform API access method; an [OpenAI API key](https://developers.openai.com/api/reference/overview#authentication) remains the stable choice for applications.

## Data and security

- Provider, Hugging Face, CivitAI and local API tokens default to macOS Keychain, together in a single `credentials` item. Logical keys stay separate inside that versioned record. Subscription access/refresh tokens, expiry and account ID are kept in the same vault under the provider key. Headless `--secrets-file` is an optional 0600 JSON vault for isolated data directories and CI; it is not used by the desktop app.
- SQLite stores configuration and request metadata, never request/response bodies or authorization headers.
- Local bearer tokens are stored in that Keychain vault, compared in constant time and can be rotated or revoked immediately.
- The gateway never binds to a LAN address. Headless mode uses the same loopback bind; pass `--port` to choose the port without writing it back to settings.
- Imported models are copied into the app-managed Application Support directory. `--data-dir` relocates SQLite, the model library and KV snapshots for that process only.
- Optional local KV snapshots (GGUF slots and MLX prefix hashes) can encode sensitive conversation state. They are unencrypted app-private files and can be removed from Settings at any time.
- Cloud prompts and tool data are sent to the provider selected by the route and remain subject to that provider's retention, training and regional-processing terms. Fallback targets may send a request to a different provider only after an eligible pre-stream failure.

## Release setup

Tags matching `v*` build an Apple-Silicon DMG. Without a Developer ID Application certificate the app is ad-hoc signed (unsigned for Gatekeeper). Users must right-click Open the first time.

Apple notarization is enabled only when `APPLE_SIGNING_IDENTITY` starts with `Developer ID Application`. Development certificates are ignored so the workflow does not attempt notarization with invalid credentials. When you have a real Developer ID, set:

- `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`
- `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`

Updater archives are signed with `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. The matching public key lives in `src-tauri/tauri.conf.json`.

## Runtime notes

`scripts/build-sidecars.sh` pins llama.cpp to a reviewed commit, MLX Swift LM to release `3.31.4`, FLUX.2 Swift MLX to release `v2.4.0`, and kokoro-swift to commit `20bf04c506e913ff129d7d2229398180ba24c690`. Metal shaders are compiled into `sidecars/bin/mlx.metallib` because SwiftPM command-line builds skip them. FLUX.2 and Kokoro are prepared as local Swift packages (`scripts/prepare-flux-vendor.sh`, `scripts/prepare-kokoro-vendor.sh`) because unmodified git dependencies cannot be consumed as-is. Their licenses must accompany redistributed binaries; see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). Model weights are not shipped with the app.
