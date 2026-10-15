#!/usr/bin/env bash
# Corroborate syscall attribution with the exact metadata-write function.
set -euo pipefail
out=${1:?output}; binary="$HOME/soak-results/blossom-fused-maintenance"
symbol=$(nm --defined-only "$binary" | awk '$3 ~ /MetaStore5write/ {print $3}')
test -n "$symbol"
cat > "$out/meta.bt" <<EOF
uprobe:$binary:$symbol /comm == "engine"/ { @start[tid] = nsecs; @calls[pid] = count(); }
uretprobe:$binary:$symbol /@start[tid]/ { @ns[pid] = sum(nsecs - @start[tid]); @us[pid] = hist((nsecs - @start[tid]) / 1000); delete(@start[tid]); }
interval:s:30 { exit(); }
END { clear(@start); }
EOF
while ! grep -q ' max: ' "$out/events.log" 2>/dev/null; do sleep 0.2; done
sleep 15
date -u +%s.%N > "$out/meta-start-epoch.txt"
sudo -n bpftrace "$out/meta.bt" > "$out/meta-latency.txt" 2> "$out/meta-latency.err"
date -u +%s.%N > "$out/meta-end-epoch.txt"
