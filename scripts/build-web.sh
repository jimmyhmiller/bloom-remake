#!/usr/bin/env bash
# Builds the browser host (docs/design/BROWSER.md) into web/pkg/: the wasm module (compiler and engine), its
# wasm-bindgen glue, and the example programs the page loads. Serve web/ statically (scripts/serve-web.mjs).
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build -p blossom-web --target wasm32-unknown-unknown --release
rm -rf web/pkg
wasm-bindgen --target web --no-typescript --out-dir web/pkg target/wasm32-unknown-unknown/release/blossom_web.wasm
mkdir -p web/pkg/apps
cp examples/web/*.bls web/pkg/apps/
echo "built web/pkg ($(du -h web/pkg/blossom_web_bg.wasm | cut -f1) wasm)"
