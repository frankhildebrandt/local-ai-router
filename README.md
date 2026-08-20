# Local AI Router

A private, multi-protocol model gateway for Apple Silicon Macs, Linux, and Windows. Local AI Router combines hosted providers with on-device inference behind OpenAI-, Anthropic- and Gemini-compatible APIs. Local MLX stays Apple-only; Linux and Windows use GGUF via llama.cpp (CPU in this release; CUDA/Vulkan come later).

## What works

- Tauri 2 desktop app for macOS 15+ (menu bar), Linux, and Windows (window + tray), plus a headless `serve` process that hosts the same engine and admin UI in a browser on loopback
- Closing the window hides the app (macOS menu bar; Linux/Windows tray); quit from the app menu, tray, or the platform quit shortcut stops the gateway
- Presets for OpenAI, Anthropic, OpenRouter, Poolside, MiniMax, Z.AI, OpenCode Zen, Gemini, Groq, Cerebras, Mistral, Hugging Face, NVIDIA NIM and SambaNova
- API keys and experimental OpenAI Subscription OAuth tokens stored in the platform keyring (macOS Keychain, Linux Secret Service, Windows Credential Manager) or a `--secrets-file` vault for headless hosts
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
- Native MLX chat, image and speech sidecars plus Metal-enabled llama.cpp on macOS; CPU llama.cpp on Linux and Windows
- Optional speculative decoding for local chat models: pair a smaller draft from the library, or GGUF n-gram without an extra model

The gateway listens on `http://127.0.0.1:11435` by default. Inference APIs require a local token managed under **API keys**. Supply it as `Authorization: Bearer`, `x-api-key`, or `x-goog-api-key`; query-string keys are deliberately rejected because URLs are commonly logged. The admin UI on loopback is local-process privileged (like the desktop app’s IPC): it does not require a login. Sharing the node on a LAN is opt-in under **Settings → Network share** and always uses HTTPS plus a directory login.

## Install

Apple Silicon, macOS 15+:

```bash
brew tap frankhildebrandt/tap
brew install --cask local-ai-router
```

GitHub Releases also ship a DMG. After a published release, the [homebrew-tap](https://github.com/frankhildebrandt/homebrew-tap) cask is bumped automatically (daily, or immediately when `HOMEBREW_TAP_TOKEN` can dispatch the tap workflow).

### Linux

GitHub Releases ship an **AppImage**, a **.deb**, and a **headless tarball** (binary, admin SPA, CPU llama.cpp sidecar, and systemd unit). Local MLX is not included; GGUF CPU inference is.

Debian/Ubuntu desktop:

```bash
sudo apt install ./local-ai-router_*.deb
local-ai-router
```

Portable desktop:

```bash
chmod +x Local-AI-Router_*.AppImage
./Local-AI-Router_*.AppImage
```

Headless 24/7 with systemd (loopback admin UI). Prefer the `.deb`, which installs `/usr/bin/local-ai-router` and resources under `/usr/lib/local-ai-router` so `serve` finds the admin SPA and GGUF sidecar without extra flags.

```bash
sudo apt install ./local-ai-router_*.deb
sudo useradd --system --home /var/lib/local-ai-router --shell /usr/sbin/nologin local-ai-router
sudo mkdir -p /var/lib/local-ai-router
sudo chown local-ai-router:local-ai-router /var/lib/local-ai-router
sudo install -m 644 /usr/lib/local-ai-router/local-ai-router.service /etc/systemd/system/local-ai-router.service
sudo systemctl daemon-reload
sudo systemctl enable --now local-ai-router
```

The headless tarball is the same layout extracted to `/opt/local-ai-router` (see its `README.txt`): binary, `ui/`, `sidecars/bin/`, and `local-ai-router.service`.

Then open `http://127.0.0.1:11435/` in a browser on that machine. Headless Linux should always pass `--secrets-file` (the unit does) because servers often have no Secret Service session. Desktop Linux uses GNOME Keyring or KWallet via Secret Service.

### Windows

GitHub Releases ship an **NSIS installer** and a **headless zip** (binary, admin SPA, and CPU llama.cpp sidecar). Local MLX is not included; GGUF CPU inference is. Windows 10/11 x64.

Desktop: run the installer from GitHub Releases, then start **Local AI Router** from the Start menu. SmartScreen may warn on unsigned builds; choose More info → Run anyway. WebView2 is installed by the NSIS bootstrapper if it is missing.

Headless (foreground `serve` on loopback). Extract the zip and run:

```powershell
.\local-ai-router.exe serve
```

Then open `http://127.0.0.1:11435/` in a browser on that machine. The exe loads `.\ui` and `.\sidecars\bin` next to itself. Data defaults to `%APPDATA%\app.local-ai-router.desktop`. Desktop and interactive `serve` use Windows Credential Manager; use `--secrets-file` for CI, extra data dirs, or Task Scheduler tasks that run without an interactive user:

```powershell
.\local-ai-router.exe serve --data-dir .\data --secrets-file .\data\secrets.json
```

A Windows service is not shipped. For 24/7 unattended use, keep a terminal open or create a Task Scheduler task that runs `local-ai-router.exe serve` at logon. Only one process can bind port 11435; quit the tray app before `serve`, or the reverse.

The packaged desktop app accepts the same command:

```powershell
& "$env:ProgramFiles\Local AI Router\Local AI Router.exe" serve
```

## Development

macOS: Apple Silicon, macOS 15+, Node.js 22+, Rust 1.77+, Swift 6.2+, CMake, and Xcode command-line tools.

Linux: x86_64, Node.js 22+, Rust 1.77+, CMake, and the GTK/WebKit packages in `scripts/install-linux-build-deps.sh`. MLX sidecars are skipped; `./scripts/build-sidecars.sh` builds CPU llama.cpp only.

Windows: x64, Windows 10/11, Node.js 22+, Rust (MSVC), CMake, Visual Studio Build Tools with the C++ workload, and WebView2. Git Bash is enough to run the repo scripts. MLX sidecars are skipped; `./scripts/build-sidecars.sh` builds CPU llama.cpp only.

```bash
npm install
./scripts/build-sidecars.sh
npm run tauri dev
```

Headless (no window or tray). Uses the same data directory, loopback port and platform keyring as the desktop app unless you override them:

```bash
npm run build
cargo run --manifest-path src-tauri/Cargo.toml -- serve --ui-dir dist
```

Then open `http://127.0.0.1:11435/` in a browser. Packaged binaries accept the same command:

```bash
# macOS
"/Applications/Local AI Router.app/Contents/MacOS/local-ai-router" serve

# Linux (.deb)
local-ai-router serve

# Linux (AppImage)
./Local-AI-Router_*.AppImage serve

# Windows (NSIS install)
& "$env:ProgramFiles\Local AI Router\Local AI Router.exe" serve

# Windows (headless zip)
.\local-ai-router.exe serve
```

| | Desktop | Headless `serve` |
| --- | --- | --- |
| Data directory (macOS) | `~/Library/Application Support/app.local-ai-router.desktop` | Same, or `--data-dir` |
| Data directory (Linux) | `$XDG_DATA_HOME/app.local-ai-router.desktop` (default `~/.local/share/...`) | Same, or `--data-dir` (systemd unit uses `/var/lib/local-ai-router`) |
| Data directory (Windows) | `%APPDATA%\app.local-ai-router.desktop` | Same, or `--data-dir` |
| Bind address | `127.0.0.1` HTTP (default). Opt-in LAN/address bind is HTTPS | Same; `--port` overrides the saved setting (default `11435`) |
| Secrets (macOS) | Keychain service `app.local-ai-router.desktop` | Same Keychain, or `--secrets-file` for an isolated 0600 JSON vault (CI / extra data dirs) |
| Secrets (Linux) | Secret Service (GNOME Keyring/KWallet) | Same, or `--secrets-file` (required for systemd; no desktop session) |
| Secrets (Windows) | Windows Credential Manager | Same, or `--secrets-file` (CI, extra data dirs, or tasks without an interactive user) |
| Admin UI | Native window + tray | Browser at `http://127.0.0.1:<port>/` |
| Inference APIs | `/v1/*` with a local API key | Same |
| Local inference | macOS: MLX + Metal GGUF; Linux/Windows: GGUF CPU | Same sidecars as desktop on that OS |

Only one process can bind the port. Quit the desktop app before `serve`, or the reverse.

On macOS, building both inference engines takes time. `swift build` compiles the MLX servers but skips Metal shaders, so `scripts/build-sidecars.sh` also compiles `mlx.metallib` next to the binaries. That step needs Xcode’s Metal toolchain (`xcodebuild -downloadComponent MetalToolchain`). On Linux and Windows the same script builds CPU llama.cpp and skips MLX. The desktop shell and cloud router can be developed without sidecars; local model startup will report a missing binary until the script has run.

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

GGUF clients can opt into persistent llama.cpp KV snapshots by sending `X-Local-AI-Session` with a stable value of at most 128 characters. MLX chat reuses a token prefix KV in RAM without that header: each request still sends the full history, and the sidecar prefills only the suffix after a longest-common-prefix match. After idle unload, MLX restores from disk using SHA-256 hashes of token-block prefixes (256-token windows plus the exact saved length), so a session header is not required for a hit. `X-Local-AI-Session` remains optional isolation for two chats that share a system prompt, and it is still required for named GGUF snapshots. Persistent KV requires one parallel prompt. Snapshots are isolated by API key and model (and session when present), stored with private filesystem permissions under the app data directory, and evicted least-recently-used above 10 GiB. They are not encrypted independently of the host filesystem.

Local chat models accept OpenAI `input_audio` and the Local AI Router extension `input_video`, plus Gemini `inlineData` for image, audio and video. Anthropic remains text and image only. Remote media must be a data URL or public HTTPS URL; private, loopback and link-local destinations are blocked. Generated images and speech are returned to the client and are neither persisted nor written to request logs.

`/v1/images/generations` for local targets supports `model`, `prompt`, `n: 1`, engine-compatible sizes and `response_format: "b64_json"`. The image sidecar runs FLUX.2, SDXL, and non-XL Stable Diffusion (SD 1.x / 2.x Diffusers layouts). Image search can download from Hugging Face or CivitAI. `/v1/audio/speech` supports `model`, `input`, `voice`, `speed`, plus `wav` and `pcm`. New local speech targets must advertise `speech`; stored cloud targets may still use `audio`.

### Identity and LAN share

Each node has a single-tenant user directory. First run creates a local `operator` account (password in the Users page until you change it). Groups grant public model IDs, `may_publish`, and `may_admin`; a user-level allowlist or flag replaces the inherited group grant.

Loopback remains HTTP with an unlocked admin UI. **Settings → Network share** can bind all interfaces or a specific address. That bind requires HTTPS. If you do not supply a certificate and key, the node writes a self-signed pair under the data directory (`tls/server.crt` and `tls/server.key`, mode 0600 on Unix) and shows the SHA-256 fingerprint in Settings and on the login screen. Pin that fingerprint in browsers and HTTP clients (`curl --cacert` or `--pinnedpubkey` / TOFU). Browsers still warn if the hostname is not on the certificate (LAN share often uses a private IP); pin the fingerprint and continue, or supply a certificate whose SAN matches the share address. A specific-address bind adds that IP to the generated certificate. Tailscale is the intended overlay if you need reachability off the LAN: join the tailnet, enable LAN share, pin the fingerprint. Register the gateway callback `https://<host>:11435/auth/oidc/callback` (or the loopback HTTP equivalent) on the OAuth app. OpenID sign-in is for browsers against the hosted gateway; the desktop webview stays a local unlocked console. Unknown GitHub/Google accounts cannot sign in until invited on the Users page. Local operator passwords still work with OpenID disabled or when the node is offline. Local API keys authenticate inference only; they are never uploaded to a parent.

Restart the desktop app or `serve` process after changing bind mode. Clients against a shared node use `https://<host>:11435/v1` and the same local API key.

## Provider notes

- Poolside requires the HTTPS deployment domain assigned to your organization; enter its OpenAI-compatible base URL in the preset.
- MiniMax and Z.AI expose separate general, token/coding-plan and regional endpoints. Select the matching preset and adjust the URL only where the provider supports it.
- OpenCode Zen catalog entries are imported only when their upstream wire protocol can be identified. Protocol and capability metadata can be corrected manually before adding a target.
- Free-tier and starter-credit badges are informational snapshots shipped with the app, not availability guarantees. Limits, eligible models and regional access can change at any time.
- Embeddings, image, audio and moderation requests are native-only. Enable those capabilities only when the selected target implements the matching endpoint.
- OpenAI Subscription support is experimental and uses a curated, version-fixed model list. It is not a documented general OpenAI Platform API access method; an [OpenAI API key](https://developers.openai.com/api/reference/overview#authentication) remains the stable choice for applications.

## Data and security

- Provider, Hugging Face, CivitAI and local API tokens default to the platform keyring (macOS Keychain, Linux Secret Service, or Windows Credential Manager), together in a single `credentials` item. Logical keys stay separate inside that versioned record. Subscription access/refresh tokens, expiry and account ID are kept in the same vault under the provider key. Headless `--secrets-file` is a JSON vault for isolated data directories, CI, Linux systemd hosts without a Secret Service session, and Windows tasks without an interactive user; the desktop app uses the platform keyring. On Unix the file is created mode 0600.
- SQLite stores configuration and request metadata, never request/response bodies or authorization headers.
- Local bearer tokens are stored in that vault, compared in constant time and can be rotated or revoked immediately.
- The gateway defaults to `127.0.0.1` over HTTP. **Settings → Network share** can bind a LAN or specific address; that always requires HTTPS (an auto-generated self-signed certificate whose SHA-256 fingerprint is shown in Settings, or a certificate/key you provide). Off-loopback admin UI requires a directory login. Inference APIs still need a local API key on every bind.
- Tailscale (or another overlay VPN) is the preferred way to reach a shared node across networks: join the tailnet, then bind LAN/HTTPS on the node and pin the certificate fingerprint. This is not a public multi-tenant service.
- Imported models are copied into the app-managed data directory. `--data-dir` relocates SQLite, the model library and KV snapshots for that process only.
- Optional local KV snapshots (GGUF slots and MLX prefix hashes) can encode sensitive conversation state. They are unencrypted app-private files and can be removed from Settings at any time.
- Cloud prompts and tool data are sent to the provider selected by the route and remain subject to that provider's retention, training and regional-processing terms. Fallback targets may send a request to a different provider only after an eligible pre-stream failure.

## Release setup

Tags matching `v*` build macOS (Apple-Silicon DMG), Linux (AppImage, `.deb`, and a headless tarball), and Windows (NSIS installer and a headless zip) artifacts onto the same GitHub Release draft.

Without a Developer ID Application certificate the macOS app is ad-hoc signed (unsigned for Gatekeeper). Users must right-click Open the first time. Windows NSIS builds are unsigned unless you add Authenticode later; SmartScreen may warn.

Apple notarization is enabled only when `APPLE_SIGNING_IDENTITY` starts with `Developer ID Application`. Development certificates are ignored so the workflow does not attempt notarization with invalid credentials. When you have a real Developer ID, set:

- `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`
- `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`

Updater archives are signed with `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. The matching public key lives in `src-tauri/tauri.conf.json`.

## Runtime notes

`scripts/build-sidecars.sh` pins llama.cpp to a reviewed commit, MLX Swift LM to release `3.31.4`, FLUX.2 Swift MLX to release `v2.4.0`, and kokoro-swift to commit `20bf04c506e913ff129d7d2229398180ba24c690`. On macOS, Metal shaders are compiled into `sidecars/bin/mlx.metallib` because SwiftPM command-line builds skip them. On Linux and Windows the script builds CPU llama.cpp and skips MLX. FLUX.2 and Kokoro are prepared as local Swift packages (`scripts/prepare-flux-vendor.sh`, `scripts/prepare-kokoro-vendor.sh`) because unmodified git dependencies cannot be consumed as-is. Their licenses must accompany redistributed binaries; see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). Model weights are not shipped with the app.
