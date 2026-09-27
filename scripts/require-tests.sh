#!/usr/bin/env bash
# Fails unless every given substring names at least one test of the crate, then runs those tests (PLAN §2.6).
#
#   scripts/require-tests.sh <crate> <substring>...
#
# Tests are listed with `cargo test -p <crate> --all-features -- --list` (unit, integration, harness = false and doc
# tests). Name tests so the substrings of your WP's plan entry match them. The matching tests then run, one
# `cargo test` per substring; the exit status is non-zero if any substring matched nothing or any run failed.
set -uo pipefail

if [ "$#" -lt 2 ]; then
  sed -n '2,9p' "$0" >&2
  exit 2
fi
crate="$1"
shift
cd "$(dirname "${BASH_SOURCE[0]}")/.." || exit 1

if ! listing="$(cargo test -q -p "$crate" --all-features -- --list 2>/dev/null)"; then
  echo "require-tests: cannot list the tests of $crate:" >&2
  cargo test -q -p "$crate" --all-features -- --list >/dev/null
  exit 1
fi
names="$(printf '%s\n' "$listing" | grep -E ': (test|bench)$' | sed -E 's/: (test|bench)$//')"

missing=0
for substring in "$@"; do
  if ! printf '%s\n' "$names" | grep -qF -- "$substring"; then
    echo "require-tests: no test of $crate has a name containing '$substring'" >&2
    missing=1
  fi
done
if [ "$missing" -ne 0 ]; then
  exit 1
fi

echo "require-tests: every required substring names a test of $crate; running them"
# One run per substring: the `harness = false` binaries (libtest-mimic) accept a single filter.
status=0
for substring in "$@"; do
  echo "==> cargo test -p $crate --all-features -- $substring"
  cargo test -p "$crate" --all-features -- "$substring" || status=1
done
exit "$status"
