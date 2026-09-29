#!/usr/bin/env bash
# The flagship comparison: the same closed-loop key-value workload, from the same client (`blossom-kv`), against
#   - the Blossom Raft KV (examples/e11_raft_kv.bls) as three `blossom run` processes, and
#   - a three-member etcd cluster,
# on this machine, both durable (each commit is fsynced before it is acknowledged: F_FULLFSYNC on macOS for both).
# Blossom runs twice: with `tail_certification = "crc"` (one fsync per group commit, etcd's model) and "strict" (four:
# the data, a sync marker, and an acknowledgement receipt that lets recovery tell corruption from a torn tail).
# Each run records its history and checks it for linearizability.
#
# Usage: ETCD_BIN=/path/to/etcd [ETCD_BENCH=/path/to/etcd/benchmark] scripts/bench-raft-vs-etcd.sh
# Knobs (environment): CLIENTS (16), RUN_SECONDS (20), KEYS (1000), MIX (put:get:del, 50:50:0), VALUE (16 bytes),
# BASE_PORT (27100), OUT (a fresh temporary directory), CHECK (1: check linearizability), TAILS ("crc strict").
set -euo pipefail

ETCD_BIN=${ETCD_BIN:?set ETCD_BIN to an etcd binary (v3.6)}
CLIENTS=${CLIENTS:-16}
RUN_SECONDS=${RUN_SECONDS:-20}
KEYS=${KEYS:-1000}
MIX=${MIX:-50:50:0}
VALUE=${VALUE:-16}
BASE_PORT=${BASE_PORT:-27100}
CHECK=${CHECK:-1}
TAILS=${TAILS:-crc strict}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=${OUT:-$(mktemp -d -t blossom-bench)}
mkdir -p "$OUT"

cargo build --quiet --release --manifest-path "$ROOT/Cargo.toml" -p blossom-cli -p blossom-bench
BLOSSOM="$ROOT/target/release/blossom"
KV="$ROOT/target/release/blossom-kv"
check_flag=()
if [ "$CHECK" = 1 ]; then check_flag=(--check); fi
common=(--clients "$CLIENTS" --seconds "$RUN_SECONDS" --keys "$KEYS" --mix "$MIX" --value-size "$VALUE" "${check_flag[@]}")

pids=()
cleanup() {
    for p in "${pids[@]:-}"; do
        [ -n "$p" ] && kill -9 "$p" 2>/dev/null || true
    done
}
trap cleanup EXIT

echo "== machine: $(uname -sm), $(sysctl -n machdep.cpu.brand_string 2>/dev/null || true), $(sysctl -n hw.ncpu 2>/dev/null || nproc) cpus"
echo "== workload: $CLIENTS clients, ${RUN_SECONDS}s, $KEYS keys, mix $MIX, ${VALUE}-byte values"

# ---- Blossom: three `blossom run` processes of e11, once per tail certification.
for tail in $TAILS; do
bdir="$OUT/blossom-$tail"
mkdir -p "$bdir"
deploy="$bdir/deploy.toml"
{
    echo 'format = 1'
    echo
    echo '[deployment]'
    echo 'id = "bench"'
    echo 'program = "raft_kv"'
    echo 'version = 1'
    echo "source = \"$ROOT/examples/e11_raft_kv.bls\""
    echo 'secrets = "raft.secrets"'
    for i in 1 2 3; do
        echo
        echo '[[node]]'
        echo "name = \"s$i\""
        echo 'role = "Server"'
        echo "addr = \"127.0.0.1:$((BASE_PORT + i))\""
        echo "client_addr = \"127.0.0.1:$((BASE_PORT + 10 + i))\""
        echo "principal = \"spiffe://bench/raft/Server/s$i\""
    done
    echo
    echo '[security]'
    echo 'mode = "insecure-dev"'
    echo
    echo '[storage]'
    echo 'data_dir = "data"'
    echo "tail_certification = \"$tail\""
} > "$deploy"
printf 'seed = "0f0e0d0c0b0a09080706050403020100"\n' > "$bdir/raft.secrets"
chmod 600 "$bdir/raft.secrets"
for i in 1 2 3; do
    "$BLOSSOM" run --deploy "$deploy" --node "s$i" --insecure-dev --init-fresh > "$bdir/s$i.out" 2> "$bdir/s$i.err" &
    pids+=($!)
done
for i in 1 2 3; do
    for _ in $(seq 100); do
        grep -q ready "$bdir/s$i.out" 2>/dev/null && break
        sleep 0.1
    done
    grep -q ready "$bdir/s$i.out" || { echo "s$i did not start:"; cat "$bdir/s$i.err"; exit 1; }
done
sleep 2 # a leader
echo "== Blossom (e11 Raft KV, 3 processes, tail certification $tail)"
"$KV" load --deploy "$deploy" --principal spiffe://bench/raft/client "${common[@]}" | tee "$bdir/report.txt"
cleanup 2>/dev/null
wait 2>/dev/null || true
pids=()
done

# ---- etcd: three members.
edir="$OUT/etcd"
mkdir -p "$edir"
cluster=""
for i in 1 2 3; do
    cluster+="e$i=http://127.0.0.1:$((BASE_PORT + 20 + i)),"
done
cluster=${cluster%,}
endpoints=""
for i in 1 2 3; do
    "$ETCD_BIN" --name "e$i" --data-dir "$edir/e$i" \
        --listen-client-urls "http://127.0.0.1:$((BASE_PORT + 30 + i))" \
        --advertise-client-urls "http://127.0.0.1:$((BASE_PORT + 30 + i))" \
        --listen-peer-urls "http://127.0.0.1:$((BASE_PORT + 20 + i))" \
        --initial-advertise-peer-urls "http://127.0.0.1:$((BASE_PORT + 20 + i))" \
        --initial-cluster "$cluster" --initial-cluster-state new --initial-cluster-token bench \
        --log-level error > "$edir/e$i.log" 2>&1 &
    pids+=($!)
    endpoints+="127.0.0.1:$((BASE_PORT + 30 + i)),"
done
endpoints=${endpoints%,}
for i in 1 2 3; do
    for _ in $(seq 100); do
        curl -fs "http://127.0.0.1:$((BASE_PORT + 30 + i))/health" | grep -q '"health":"true"' && break
        sleep 0.1
    done
done
echo "== etcd $("$ETCD_BIN" --version | head -1 | awk '{print $3}') (3 members)"
"$KV" etcd --endpoints "$endpoints" "${common[@]}" | tee "$edir/report.txt"

if [ -n "${ETCD_BENCH:-}" ]; then
    echo "== etcd's own gRPC benchmark (for reference: a different client)"
    grpc=$(echo "$endpoints" | sed 's/[^,]*/http:\/\/&/g')
    total=$((CLIENTS * 2000))
    "$ETCD_BENCH" --endpoints="$grpc" --conns="$CLIENTS" --clients="$CLIENTS" put --key-size=8 --val-size="$VALUE" \
        --total="$total" 2>&1 | grep -E "Requests/sec|Average|Slowest|Fastest|99%" | tee "$edir/grpc-put.txt"
fi
echo "== reports in $OUT"
