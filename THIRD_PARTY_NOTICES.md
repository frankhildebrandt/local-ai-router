# Third-party runtime notices

Local AI Router redistributes third-party components, including bundled frontend code and separately built inference executables:

- **llama.cpp**, copyright its contributors, MIT License: <https://github.com/ggml-org/llama.cpp/blob/master/LICENSE>
- **MLX Swift / MLX Swift LM**, copyright Apple Inc. and contributors, MIT License: <https://github.com/ml-explore/mlx-swift-lm/blob/main/LICENSE>
- **FLUX.2 Swift MLX**, copyright Vincent Gourbin, MIT License, pinned to `v2.4.0`: <https://github.com/VincentGourbin/flux-2-swift-mlx>. The image sidecar applies a local AdamW API compatibility patch so Flux2Core compiles against current MLX Swift; Flux2 training is unused.
- **mlx-swift-examples Stable Diffusion**, copyright Apple Inc. and contributors, MIT License: <https://github.com/ml-explore/mlx-swift-examples>
- **kokoro-swift**, copyright Maxime Weinbach and contributors, Apache License 2.0, pinned to `20bf04c506e913ff129d7d2229398180ba24c690`: <https://github.com/mweinbach/kokoro-swift>
- **assistant-ui**, copyright AgentbaseAI Inc., MIT License: <https://github.com/assistant-ui/assistant-ui/blob/main/LICENSE>

Release packaging must include the complete corresponding license texts. Model weights are not distributed with the application; users are responsible for the licenses and access terms of models they import or download, including Gemma, Llama, Mistral, Qwen, gpt-oss, Phi, DeepSeek, FLUX.2, SDXL Turbo and Kokoro.
