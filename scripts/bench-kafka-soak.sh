#!/usr/bin/env bash
# The Kafka soak: a sustained, wider workload than bench-kafka.sh's runs, the same against each system, with a broker
# killed (kill -9) in the middle and every acknowledged record checked afterwards. One system per run:
#
#   scripts/bench-kafka-soak.sh SYSTEM        SYSTEM: blossom | kafka (three brokers each, via bench-kafka.sh)
#
# The workload (scripts/kafka-soak/Soak.java, Kafka's own Java client): $TOPICS topics of $PARTS partitions, replicated
# three times; $PRODUCERS producers (acks=all, idempotent) and a group of $CONSUMERS consumers running throughout.
#   1. max     the producers unthrottled for $MAX_SECONDS
#   2. drain   until the group has read everything (its lag is 0)
#   3. fixed   the producers paced at $RATE records/s in all for $FIXED_SECONDS; at $KILL_AT seconds the broker leading
#              the most partitions is killed with kill -9, and started again $DOWN_SECONDS later
#   4. catchup a new group of $CONSUMERS consumers reads every topic from the start
#   5. verify  every acknowledged record is read back at the offset its acknowledgement gave, and none twice
#
# Brokers are pinned to $BROKER_CPUS (three taskset lists) and the clients to $CLIENT_CPUS. Results in $OUT: one CSV
# per client (a line a second), resources.csv (each broker's CPU and RSS every 5 s), disk.csv, events.log (phase and
# fault times, seconds since the start) and summary.txt.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
SYSTEM=${1:?usage: bench-kafka-soak.sh blossom|kafka}
KAFKA_HOME=${KAFKA_HOME:-$(ls -d "$ROOT"/.tools/kafka_* 2> /dev/null | head -1)}
K=$KAFKA_HOME/bin
export KAFKA_HOME
export BENCH_DIR=${BENCH_DIR:-/tmp/bench-soak}
export BROKER_CPUS=${BROKER_CPUS:-"0-3,16-19 4-7,20-23 8-11,24-27"}
CLIENT_CPUS=${CLIENT_CPUS:-"12-15,28-31"}
TOPICS=${TOPICS:-10}
PARTS=${PARTS:-12}
PRODUCERS=${PRODUCERS:-4}
CONSUMERS=${CONSUMERS:-4}
SIZE=${SIZE:-1024}
MAX_SECONDS=${MAX_SECONDS:-300}
RATE=${RATE:?RATE: the records/s of the fixed phase, all producers together}
FIXED_SECONDS=${FIXED_SECONDS:-1800}
KILL_AT=${KILL_AT:-600}
DOWN_SECONDS=${DOWN_SECONDS:-60}
OUT=${OUT:-$BENCH_DIR/results-$SYSTEM-$(date +%Y%m%d-%H%M%S)}
BOOTSTRAP=127.0.0.1:19092,127.0.0.1:19093,127.0.0.1:19094
BENCH="$ROOT/scripts/bench-kafka.sh"

die() { echo "bench-kafka-soak: $*" >&2; exit 1; }
[ -x "$K/kafka-topics.sh" ] || die "no Kafka distribution (KAFKA_HOME=$KAFKA_HOME)"
command -v taskset > /dev/null || die "taskset not found"
mkdir -p "$OUT"
T0=$(date +%s)
since() { echo $(($(date +%s) - T0)); }
event() { echo "$(since) $*" | tee -a "$OUT/events.log"; }

topic_list=$(seq -s, -f "soak-%g" 0 $((TOPICS - 1)))
classes=$BENCH_DIR/soak-classes
rm -rf "$classes"
mkdir -p "$classes"
javac -nowarn -cp "$KAFKA_HOME/libs/*" -d "$classes" "$ROOT/scripts/kafka-soak/Soak.java"
# Run in the background (`soak … &`): the exec makes $! the client's own pid.
# The client logs at WARN (retries, NOT_LEADER, out-of-order sequences) into each client's .err/.csv.
soak() {
  exec taskset -c "$CLIENT_CPUS" java -Dlog4j2.configurationFile="$KAFKA_HOME/config/tools-log4j2.yaml" \
    -cp "$classes:$KAFKA_HOME/libs/*" Soak "$@"
}

children=()
cleanup() {
  for p in "${children[@]}"; do kill "$p" 2> /dev/null || true; done
  "$BENCH" down > /dev/null 2>&1 || true
}
trap cleanup EXIT

# ---- the cluster and its topics
event "up $SYSTEM"
PARTITIONS=0 "$BENCH" up "$SYSTEM"
for t in ${topic_list//,/ }; do
  timeout 120 "$K/kafka-topics.sh" --bootstrap-server "$BOOTSTRAP" --create --topic "$t" --partitions "$PARTS" \
    --replication-factor 3 > /dev/null
done
event "topics created: $TOPICS x $PARTS"

# ---- each broker's CPU (percent of one core) and RSS every 5 s; the data directories' size every minute
sample() {
  local hz prev=() now pid ticks rss
  hz=$(getconf CLK_TCK)
  echo "t,broker,cpu_pct,rss_mb" > "$OUT/resources.csv"
  echo "t,mb" > "$OUT/disk.csv"
  local n=0
  while true; do
    for i in 1 2 3; do
      pid=$(cat "$BENCH_DIR/pid.$i" 2> /dev/null || true)
      if [ -n "$pid" ] && [ -r "/proc/$pid/stat" ]; then
        read -r ticks rss < <(awk '{print $14 + $15, $24}' "/proc/$pid/stat")
        if [ -n "${prev[$i]:-}" ] && [ "${prev[$i]%:*}" = "$pid" ]; then
          echo "$(since),$i,$(((ticks - ${prev[$i]#*:}) * 100 / (hz * 5))),$((rss * 4096 / 1048576))" >> "$OUT/resources.csv"
        fi
        prev[$i]="$pid:$ticks"
      else
        prev[$i]=""
      fi
    done
    if [ $((n % 12)) = 0 ]; then
      echo "$(since),$(du -sm "$BENCH_DIR/$SYSTEM" | cut -f1)" >> "$OUT/disk.csv"
    fi
    n=$((n + 1))
    sleep 5
  done
}
sample &
children+=($!)

# ---- the group reading throughout
for c in $(seq 1 "$CONSUMERS"); do
  soak consume --bootstrap "$BOOTSTRAP" --topics "$topic_list" --group soak-live --id "live$c" \
    --stop "$OUT/stop-live" > "$OUT/consumer-live-$c.csv" 2> "$OUT/consumer-live-$c.err" &
  children+=($!)
done
live=("${children[@]: -$CONSUMERS}")

# The group's lag: what the topics hold less what the group has committed.
lag() {
  timeout 60 "$K/kafka-consumer-groups.sh" --bootstrap-server "$BOOTSTRAP" --describe --group "$1" 2> /dev/null \
    | awk '$1 == "'"$1"'" && $6 ~ /^[0-9]+$/ { s += $6; n++ }
           $1 == "'"$1"'" && $6 == "-" && $5 > 0 { open = 1 }
           END { if (n == 0 || open) print "unknown"; else print s }'
}

# The broker leading the most partitions of the soak topics.
busiest() {
  timeout 60 "$K/kafka-topics.sh" --bootstrap-server "$BOOTSTRAP" --describe --topic 'soak-.*' \
    | awk '{ for (i = 1; i < NF; i++) if ($i == "Leader:") n[$(i + 1)]++ }
           END { for (b in n) if (n[b] > best) { best = n[b]; who = b }; print who }'
}

# The records the topics hold: end offsets less start offsets.
held() {
  local e s
  e=$("$K/kafka-get-offsets.sh" --bootstrap-server "$BOOTSTRAP" --topic 'soak-.*' --time -1 | awk -F: '{ s += $3 } END { print s }')
  s=$("$K/kafka-get-offsets.sh" --bootstrap-server "$BOOTSTRAP" --topic 'soak-.*' --time -2 | awk -F: '{ s += $3 } END { print s }')
  echo $((e - s))
}

# Starts the producers of a phase: name, first id less one, records/s each (-1: unthrottled), seconds. Their pids in
# `prod`.
produce_phase() {
  local p
  prod=()
  for p in $(seq 1 "$PRODUCERS"); do
    soak produce --bootstrap "$BOOTSTRAP" --topics "$topic_list" --id "$(($2 + p))" --rate "$3" --seconds "$4" \
      --size "$SIZE" --out "$OUT" > "$OUT/producer-$1-$p.csv" 2> "$OUT/producer-$1-$p.err" &
    prod+=($!)
    children+=($!)
  done
}

# ---- 1. max
event "max: $PRODUCERS producers unthrottled for $MAX_SECONDS s"
produce_phase max 100 -1 "$MAX_SECONDS"
for p in "${prod[@]}"; do wait "$p" || event "a max producer exited $?"; done
event "max done"

# ---- 2. drain
for i in $(seq 1 120); do
  l=$(lag soak-live)
  [ "$l" = 0 ] && break
  sleep 5
done
event "drained (lag $l)"

# ---- 3. fixed, with a broker killed
event "fixed: $RATE records/s for $FIXED_SECONDS s"
produce_phase fixed 200 $((RATE / PRODUCERS)) "$FIXED_SECONDS"
sleep "$KILL_AT"
victim=$(busiest)
[ -n "$victim" ] || die "no leader found for the soak topics"
"$BENCH" kill "$victim"
event "killed broker $victim (kill -9)"
sleep "$DOWN_SECONDS"
"$BENCH" restart "$victim"
event "restarted broker $victim"
for p in "${prod[@]}"; do wait "$p" || event "a fixed producer exited $?"; done
event "fixed done"
for i in $(seq 1 120); do
  l=$(lag soak-live)
  [ "$l" = 0 ] && break
  sleep 5
done
event "drained (lag $l)"
touch "$OUT/stop-live"
for p in "${live[@]}"; do wait "$p" || true; done

# ---- 4. catchup
total=$(held)
event "catchup: a new group of $CONSUMERS reads $total records"
cpids=()
for c in $(seq 1 "$CONSUMERS"); do
  soak consume --bootstrap "$BOOTSTRAP" --topics "$topic_list" --group soak-catchup --id "catchup$c" \
    --stop "$OUT/stop-catchup" --prop max.poll.records=2000 > "$OUT/consumer-catchup-$c.csv" \
    2> "$OUT/consumer-catchup-$c.err" &
  cpids+=($!)
  children+=($!)
done
cstart=$(date +%s%N)
deadline=$(($(date +%s) + 3600))
while true; do
  read_n=0
  for c in $(seq 1 "$CONSUMERS"); do
    n=$(tail -n 1 "$OUT/consumer-catchup-$c.csv" 2> /dev/null | awk -F, '/^[0-9]/ { print $2 }')
    read_n=$((read_n + ${n:-0}))
  done
  [ "$read_n" -ge "$total" ] && break
  [ "$(date +%s)" -lt "$deadline" ] || { event "catchup gave up at $read_n of $total"; break; }
  sleep 1
done
cend=$(date +%s%N)
touch "$OUT/stop-catchup"
for p in "${cpids[@]}"; do wait "$p" || true; done
event "catchup done: $read_n records in $(awk "BEGIN { printf \"%.1f\", ($cend - $cstart) / 1e9 }") s"

# ---- 5. verify
event "verify"
taskset -c "$CLIENT_CPUS" java -Xmx16g -cp "$classes:$KAFKA_HOME/libs/*" Soak verify --bootstrap "$BOOTSTRAP" \
  --topics "$topic_list" --out "$OUT" --prop max.poll.records=5000 > "$OUT/verify.txt" 2>&1 || true
tail -n 1 "$OUT/verify.txt" | tee -a "$OUT/events.log"
echo "$(since),$(du -sm "$BENCH_DIR/$SYSTEM" | cut -f1)" >> "$OUT/disk.csv"

# ---- summary
{
  echo "system=$SYSTEM topics=$TOPICS partitions=$PARTS producers=$PRODUCERS consumers=$CONSUMERS size=$SIZE"
  echo "max_seconds=$MAX_SECONDS rate=$RATE fixed_seconds=$FIXED_SECONDS kill_at=$KILL_AT down=$DOWN_SECONDS"
  echo "broker_cpus=$BROKER_CPUS client_cpus=$CLIENT_CPUS"
  cat "$OUT/events.log"
  for f in "$OUT"/producer-*.csv "$OUT"/consumer-*.csv; do
    echo "$(basename "$f" .csv): $(grep '^#' "$f" | tail -n 1)"
  done
} > "$OUT/summary.txt"
event "done: $OUT"
