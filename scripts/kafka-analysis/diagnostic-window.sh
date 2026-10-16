#!/usr/bin/env bash
# Called alongside one diagnostic soak; begin 15 s into its max phase.
set -euo pipefail
bench=${1:?bench directory}; out=${2:?output directory}; mode=${3:?bloom or kafka}
while ! grep -q ' max: ' "$out/events.log" 2>/dev/null; do sleep 0.2; done
sleep 15
pids=(); for n in 1 2 3; do pids+=("$(cat "$bench/pid.$n")"); done
pidlist=$(IFS=,; echo "${pids[*]}")
date -u +%s.%N > "$out/diagnostic-start-epoch.txt"
sudo -n bpftrace scripts/kafka-analysis/latencies.bt "${pids[@]}" > "$out/latencies.txt" 2> "$out/latencies.err" & bpf=$!
perf stat -x, -o "$out/perf-stat.csv" -e task-clock,cycles,instructions,branches,branch-misses,cache-references,cache-misses,context-switches,cpu-migrations,page-faults -p "$pidlist" -- sleep 30 & counters=$!
if [[ $mode == bloom ]]; then
 perf record -q -e cycles:u -F 199 --call-graph fp -p "$pidlist" -o "$out/cpu.perf" -- sleep 30 & stacks=$!
fi
wait "$bpf"; wait "$counters"
if [[ $mode == bloom ]]; then wait "$stacks"; fi
date -u +%s.%N > "$out/diagnostic-end-epoch.txt"
