#!/usr/bin/env bash
# Same workload, resource placement, and program for fresh interleaved controls.
set -euo pipefail
cd "${ANALYSIS_ROOT:-$HOME/bloom-kafka-stages}"
export KAFKA_HOME="$HOME/bloom-scale/.tools/kafka_2.13-4.0.0"
export BLOSSOM="${BLOSSOM:-$HOME/soak-results/blossom-fused-maintenance}"
export BROKER_CPUS='0-1,16-17 2-3,18-19 4-5,20-21' CLIENT_CPUS='6-7,22-23'
export RATE=2000 MAX_SECONDS=60 FIXED_SECONDS=60 KILL_AT=20 DOWN_SECONDS=10
export HOT_ROWS=262144 BLOCK_CACHE_BYTES=33554432 TIERED=true TAIL_CERT=strict
export MAP_CACHE_ROWS=8192 MAP_CACHE_JOINS=true LAZY_INDEXES=true EVAL_WORKERS=1
export COMPILED_JOINS=true PACKED_INDEXES=true FUSE_RELATIONS=true
unset PIPELINE_BATCHES INFER_KEYS
for trial in ${TRIALS:-kafka-1 bloom-1 bloom-2 kafka-2}; do
 export BENCH_DIR="${BENCH_ROOT:-/tmp}/${RUN_PREFIX:-gap-control}-$trial" OUT="${RESULT_ROOT:-$HOME/soak-results}/${RUN_PREFIX:-gap-control}-$trial"
 test ! -e "$OUT" || { echo "Refusing to overwrite $OUT" >&2; exit 1; }
 mkdir -p "${BENCH_ROOT:-/tmp}"
 available_kib=$(df -Pk "${BENCH_ROOT:-/tmp}" | awk 'NR == 2 { print $4 }')
 required_kib=$(( ${MIN_FREE_GIB:-16} * 1024 * 1024 ))
 if (( available_kib < required_kib )); then
  echo "Insufficient benchmark disk headroom: ${available_kib} KiB free; ${required_kib} KiB required. Set BENCH_ROOT to a roomy physical filesystem." >&2
  exit 1
 fi
 mkdir -p "$OUT"
 python3 scripts/kafka-analysis/sample-processes.py --bench-dir "$BENCH_DIR" --out "$OUT" & sampler=$!
 iostat -x -y 1 > "$OUT/iostat.txt" & iostat_pid=$!
 cleanup_monitors() { kill "$sampler" "$iostat_pid" 2>/dev/null || true; wait "$sampler" "$iostat_pid" 2>/dev/null || true; }
 trap cleanup_monitors EXIT
 system=kafka; [[ $trial == bloom* ]] && system=blossom
 timeout 900 nice -n 10 scripts/bench-kafka-soak.sh "$system" > "$OUT/run.log" 2>&1
 cleanup_monitors
 trap - EXIT
 python3 - "$OUT" <<'PY'
import pathlib,re,sys
p=pathlib.Path(sys.argv[1]); s=(p/'summary.txt').read_text()
assert re.search(r'# verify OK acked=\d+ lost=0 misplaced=0 duplicates=0 present_unacknowledged=0 records=\d+ foreign=0',s),s
print(p.name, '\n'.join(l for l in s.splitlines() if l.startswith('"max"') or '# verify OK' in l),flush=True)
PY
 cp -a "$BENCH_DIR/$system"/*.properties "$OUT/" 2>/dev/null || true
 if [[ $system == blossom ]]; then cp "$BENCH_DIR/blossom/deploy.toml" "$OUT/"; fi
 rm -rf "$BENCH_DIR"
done
