#!/usr/bin/env bash
# The per-crate checks every WP runs before finishing (PLAN §2.6).
#
#   scripts/wp-check.sh <crate>...
#
# For each crate: rustfmt, clippy with every target and feature (-D warnings) and its tests; then the workspace
# layer and code-registry checks. Every step runs; the exit status is non-zero if any failed.
set -uo pipefail

if [ "$#" -lt 1 ]; then
  sed -n '2,7p' "$0" >&2
  exit 2
fi
cd "$(dirname "${BASH_SOURCE[0]}")/.." || exit 1

failed=()
step() {
  echo "==> $*"
  "$@" || failed+=("$*")
}
for crate in "$@"; do
  step cargo fmt -p "$crate" --check
  step cargo clippy -p "$crate" --all-targets --all-features -- -D warnings
  step cargo test -p "$crate" --all-features
done
step cargo run -q -p xtask -- check-layers
step cargo run -q -p xtask -- check-codes

if [ "${#failed[@]}" -gt 0 ]; then
  echo "wp-check: FAILED:" >&2
  printf '  %s\n' "${failed[@]}" >&2
  exit 1
fi
echo "wp-check: ok ($*)"
