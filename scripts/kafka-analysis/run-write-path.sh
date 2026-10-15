#!/usr/bin/env bash
set -euo pipefail
cd "${ANALYSIS_ROOT:-$HOME/bloom-kafka-stages}"
export KAFKA_HOME="$HOME/bloom-scale/.tools/kafka_2.13-4.0.0"
export BLOSSOM="$HOME/soak-results/blossom-fused-maintenance"
export BROKER_CPUS='0-1,16-17 2-3,18-19 4-5,20-21' CLIENT_CPUS='6-7,22-23'
export RATE=2000 MAX_SECONDS=60 FIXED_SECONDS=60 KILL_AT=20 DOWN_SECONDS=10
export HOT_ROWS=262144 BLOCK_CACHE_BYTES=33554432 TIERED=true TAIL_CERT=strict
export MAP_CACHE_ROWS=8192 MAP_CACHE_JOINS=true LAZY_INDEXES=true EVAL_WORKERS=1
export COMPILED_JOINS=true PACKED_INDEXES=true FUSE_RELATIONS=true
unset PIPELINE_BATCHES INFER_KEYS RECORD
export BENCH_DIR=/tmp/gap-write-path OUT="$HOME/soak-results/gap-write-path"
test ! -e "$OUT"; mkdir -p "$OUT"
python3 scripts/kafka-analysis/sample-processes.py --bench-dir "$BENCH_DIR" --out "$OUT" & sampler=$!
(
 while ! grep -q ' max: ' "$OUT/events.log" 2>/dev/null; do sleep 0.2; done
 sleep 15
 pids=(); for n in 1 2 3; do pids+=("$(cat "$BENCH_DIR/pid.$n")"); done
 date -u +%s.%N > "$OUT/write-start-epoch.txt"
 sudo -n bpftrace scripts/kafka-analysis/write-path.bt "${pids[@]}" > "$OUT/write-path.txt" 2> "$OUT/write-path.err"
 date -u +%s.%N > "$OUT/write-end-epoch.txt"
) & meta=$!
trap 'kill "$sampler" "$meta" 2>/dev/null || true' EXIT
timeout 900 nice -n 10 scripts/bench-kafka-soak.sh blossom > "$OUT/run.log" 2>&1
kill "$sampler"; wait "$sampler" || true; wait "$meta"; trap - EXIT
python3 - "$OUT" <<'PY'
import pathlib,re,sys
p=pathlib.Path(sys.argv[1]);s=(p/'summary.txt').read_text()
assert re.search(r'# verify OK acked=\d+ lost=0 misplaced=0 duplicates=0 present_unacknowledged=0 records=\d+ foreign=0',s),s
PY
cp "$BENCH_DIR/blossom/deploy.toml" "$OUT/"
cp "$BENCH_DIR/blossom"/*.stats "$OUT/"
rm -rf "$BENCH_DIR"
echo 'Write-path diagnostic verified and retained'
