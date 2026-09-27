#!/usr/bin/env bash
# tier: fast
# Formatting (ARCHITECTURE §11.10, lint job). Owned by M1.1.
set -euo pipefail
cargo fmt --all --check
