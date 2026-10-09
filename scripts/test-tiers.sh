#!/usr/bin/env bash
# The test tiers (HD): `fast` for every change (one seed per simulation, the full-tier tests skipped and reported as
# ignored); `full` once per slice, before a merge (every seed, every test, the corpus, the simulated clusters again
# with the engine checked against the oracle). The full tier takes long:
# run it on a machine with cores to spare. `web` is the browser host's end-to-end tests (docs/design/BROWSER.md): the
# wasm build, then Playwright in headless Chromium (needs node, wasm-bindgen from scripts/install-dev-tools.sh, and
# Playwright's Chromium: `npx playwright install chromium` in tests/web); the full tier runs it too.
#
# `services` is the tests that need Postgres and S3 (docs/design/STATELESS.md §10): it starts them
# (scripts/test-services.sh) and runs the state store adapters' suites; the full tier runs it too.
#
#   scripts/test-tiers.sh fast [cargo test args...]
#   scripts/test-tiers.sh full
#   scripts/test-tiers.sh web
#   scripts/test-tiers.sh services
set -euo pipefail
cd "$(dirname "$0")/.."
# The checkout's own tools come first: the pinned solvers (scripts/install-solvers.sh) and any client tools a machine
# links in (a kcat whose librdkafka speaks the broker's protocol versions, see kafka_gate.rs).
export PATH="$PWD/.tools/bin:$PATH"
tier="${1:-fast}"
shift || true

web() {
  scripts/build-web.sh
  # The client-member specs (tests/web/clients.spec.mjs) run real nodes: `blossom run --web`.
  cargo build -q -p blossom-cli
  export BLOSSOM_BIN="${CARGO_TARGET_DIR:-$PWD/target}/debug/blossom"
  # The Durable Object specs (tests/web/object.spec.mjs) run polls and rooms on workerd, locally
  # (docs/design/DURABLE-OBJECTS.md, docs/design/KEYED.md).
  scripts/build-do.sh polls
  scripts/build-do.sh rooms Room
  (cd do && npm ci --no-audit --no-fund)
  export BLOSSOM_DO=1
  (cd tests/web && npm ci --no-audit --no-fund && npx playwright test)
}

services() {
  scripts/test-services.sh start
  eval "$(scripts/test-services.sh env)"
  cargo test --no-fail-fast -p blossom-statestore-postgres -p blossom-statestore-s3 -- --include-ignored
}

case "$tier" in
  fast)
    cargo test --workspace --no-fail-fast "$@"
    ;;
  full)
    # Every part runs whatever an earlier one found; the exit status says whether all passed.
    status=0
    BLOSSOM_FULL=1 cargo test --workspace --no-fail-fast "$@" -- --include-ignored || status=1
    # The simulated clusters again with every node's engine checked against the oracle at every tick (its tiered
    # tables included, docs/design/DATABASE.md §7).
    BLOSSOM_EVALUATOR=checked cargo test -p blossom-integration-tests --no-fail-fast "$@" \
      --test kafka_sim --test raft_kv --test raft_groups --test sim_streams --test kafka_retention || status=1
    # The cluster tests that need a release build's speed (a debug build skips them).
    BLOSSOM_FULL=1 cargo test --release -p blossom-cli --test it --no-fail-fast "$@" -- --ignored \
      a_restarted_follower_catches_up_on_batches_near_the_size_limit || status=1
    for area in core lattices async net; do
      cargo run -q -p xtask -- corpus --check --area "$area" || status=1
    done
    services || status=1
    web || status=1
    exit "$status"
    ;;
  web)
    web
    ;;
  services)
    services
    ;;
  *)
    echo "usage: $0 fast|full|web|services [cargo test args...]" >&2
    exit 2
    ;;
esac
