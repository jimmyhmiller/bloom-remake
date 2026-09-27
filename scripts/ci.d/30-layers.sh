#!/usr/bin/env bash
# tier: fast
# Structural checks: the layer table, the sans-IO node and the diagnostic code registry (ARCH-01, ARCH-03,
# ARCHITECTURE §12.1). Owned by M1.1.
set -uo pipefail
status=0
cargo run -q -p xtask -- check-layers || status=1
cargo run -q -p xtask -- check-sans-io || status=1
cargo run -q -p xtask -- check-codes || status=1
exit "$status"
