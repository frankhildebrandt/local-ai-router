Vendored sidecar sources that cannot be consumed as unmodified SwiftPM git dependencies.

`kokoro-swift` is cloned by `scripts/prepare-kokoro-vendor.sh` at commit
`20bf04c506e913ff129d7d2229398180ba24c690`. SwiftPM rejects that revision as a
git dependency because Misaki is a local path package.

`flux-2-swift-mlx` is cloned by `scripts/prepare-flux-vendor.sh` at tag `v2.4.0`.
A one-line AdamW API patch is applied so Flux2Core compiles against current
MLX Swift; the sidecar does not use Flux2 training.
