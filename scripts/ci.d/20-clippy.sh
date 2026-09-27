#!/usr/bin/env bash
# tier: fast
# Lints with every feature enabled; warnings are errors (ARCHITECTURE §11.10, §12.1). Owned by M1.1.
set -euo pipefail
cargo clippy --workspace --all-targets --all-features -- -D warnings
