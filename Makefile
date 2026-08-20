.PHONY: dev test build build-dmg build-linux build-windows

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

build-linux:
	npm run tauri build -- --bundles appimage,deb

build-windows:
	npm run tauri build -- --bundles nsis
