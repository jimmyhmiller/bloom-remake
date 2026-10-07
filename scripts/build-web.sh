#!/usr/bin/env bash
# Builds the browser host (docs/design/BROWSER.md) into web/pkg/: the wasm module (compiler and engine), its
# wasm-bindgen glue, and the example programs the page loads. Serve web/ statically (scripts/serve-web.mjs).
set -euo pipefail
cd "$(dirname "$0")/.."
# wasm-bindgen's CLI must match the crate's version (scripts/install-dev-tools.sh pins it into .tools/).
export PATH="$PWD/.tools/bin:$PATH"
want="$(sed -n 's/^wasm-bindgen = "=\(.*\)"$/\1/p' crates/blossom-web/Cargo.toml)"
have="$(wasm-bindgen --version 2>/dev/null | awk '{print $2}')" || true
if [ -z "$want" ] || [ "$have" != "$want" ]; then
  echo "build-web: wasm-bindgen ${want:-?} is needed (found: ${have:-nothing}); run scripts/install-dev-tools.sh" >&2
  exit 1
fi
cargo build -p blossom-web --target wasm32-unknown-unknown --release
rm -rf web/pkg
wasm-bindgen --target web --no-typescript --out-dir web/pkg "${CARGO_TARGET_DIR:-target}/wasm32-unknown-unknown/release/blossom_web.wasm"
mkdir -p web/pkg/apps
cp examples/web/*.bls web/pkg/apps/
echo "built web/pkg ($(du -h web/pkg/blossom_web_bg.wasm | cut -f1) wasm)"
