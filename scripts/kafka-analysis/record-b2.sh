#!/usr/bin/env bash
# Record only b2 to bound diagnostic disk overhead. Do not use for throughput controls.
set -euo pipefail
args=("$@"); record=false
for ((i=0; i+1<${#args[@]}; i++)); do
 if [[ ${args[$i]} == --node && ${args[$((i+1))]} == b2 ]]; then record=true; fi
done
if $record; then args+=(--record "${BENCH_DIR:?}/blossom/traces"); fi
exec "$HOME/soak-results/blossom-fused-maintenance" "${args[@]}"
