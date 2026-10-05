#!/usr/bin/env bash
# The test tiers (HD): `fast` for every change (one seed per simulation, the full-tier tests skipped and reported as
# ignored); `full` once per slice, before a merge (every seed, every test, the corpus). The full tier takes long:
# run it on a machine with cores to spare. `web` is the browser host's end-to-end tests (docs/design/BROWSER.md): the
# wasm build, then Playwright in headless Chromium (needs node, wasm-bindgen from scripts/install-dev-tools.sh, and
# Playwright's Chromium: `npx playwright install chromium` in tests/web); the full tier runs it too.
#
#   scripts/test-tiers.sh fast [cargo test args...]
#   scripts/test-tiers.sh full
#   scripts/test-tiers.sh web
set -euo pipefail
cd "$(dirname "$0")/.."
# The checkout's own tools come first: the pinned solvers (scripts/install-solvers.sh) and any client tools a machine
# links in (a kcat whose librdkafka speaks the broker's protocol versions, see kafka_gate.rs).
export PATH="$PWD/.tools/bin:$PATH"
tier="${1:-fast}"
shift || true

web() {
  scripts/build-web.sh
  (cd tests/web && npm ci --no-audit --no-fund && npx playwright test)
}

case "$tier" in
  fast)
    cargo test --workspace --no-fail-fast "$@"
    ;;
  full)
    # Every part runs whatever an earlier one found; the exit status says whether all passed.
    status=0
    BLOSSOM_FULL=1 cargo test --workspace --no-fail-fast "$@" -- --include-ignored || status=1
    for area in core lattices async net; do
      cargo run -q -p xtask -- corpus --check --area "$area" || status=1
    done
    web || status=1
    exit "$status"
    ;;
  web)
    web
    ;;
  *)
    echo "usage: $0 fast|full|web [cargo test args...]" >&2
    exit 2
    ;;
esac
