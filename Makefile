.PHONY: dev test build build-dmg

dev:
	npm run tauri dev

test:
	npm test
	cargo test --manifest-path src-tauri/Cargo.toml
	npm run test:contract

build:
	npm run tauri build -- --bundles app

build-dmg:
	npm run tauri build -- --bundles dmg
