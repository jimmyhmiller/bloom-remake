#!/usr/bin/env bash
# The test tiers (HD): `fast` for every change (one seed per simulation, the full-tier tests skipped and reported as
# ignored); `full` once per slice, before a merge (every seed, every test, the corpus). The full tier takes long:
# run it on a machine with cores to spare.
#
#   scripts/test-tiers.sh fast [cargo test args...]
#   scripts/test-tiers.sh full
set -euo pipefail
cd "$(dirname "$0")/.."
tier="${1:-fast}"
shift || true
case "$tier" in
  fast)
    cargo test --workspace --no-fail-fast "$@"
    ;;
  full)
    BLOSSOM_FULL=1 cargo test --workspace --no-fail-fast "$@" -- --include-ignored
    for area in core lattices async net; do
      cargo run -q -p xtask -- corpus --check --area "$area"
    done
    ;;
  *)
    echo "usage: $0 fast|full [cargo test args...]" >&2
    exit 2
    ;;
esac
