#!/usr/bin/env bash
# tier: fast
# The whole test suite with every feature enabled (ARCHITECTURE §11.10, test job). Uses cargo-nextest when it is
# installed (per-test timeouts from .config/nextest.toml; profile `ci` in the gate tier) and runs doctests
# separately, since nextest does not run them. Owned by M1.1.
set -euo pipefail
if cargo nextest --version >/dev/null 2>&1; then
  profile=default
  [ "${CI_TIER:-fast}" = gate ] && profile=ci
  cargo nextest run --workspace --all-features --profile "$profile"
  cargo test --workspace --all-features --doc
else
  echo "40-test: cargo-nextest is not installed; running cargo test (scripts/install-dev-tools.sh installs it)"
  cargo test --workspace --all-features
fi
