#!/usr/bin/env bash
# The Kafka comparison (SLICES.md, slice 10): the same workload against three Blossom brokers, three Apache Kafka
# brokers (KRaft) and three Redpanda brokers, on one machine, with the same client (Kafka's own perf tools) and the
# same durability (every acknowledged record fsynced on a majority of its replicas).
#
#   scripts/bench-kafka.sh up SYSTEM          start a three-broker cluster (SYSTEM: blossom | kafka | redpanda)
#   scripts/bench-kafka.sh down               stop every cluster this script started
#   scripts/bench-kafka.sh produce [ARGS…]    kafka-producer-perf-test against the running cluster
#   scripts/bench-kafka.sh consume [ARGS…]    kafka-consumer-perf-test against the running cluster
#   scripts/bench-kafka.sh e2e [N] [SIZE]     kafka-e2e-latency against the running cluster
#   scripts/bench-kafka.sh kill N             kill -9 broker N (1-3) of the running cluster
#   scripts/bench-kafka.sh restart N          start broker N again on its data (after `kill`)
#
# Environment: KAFKA_HOME (default: the repository's .tools/kafka_*), BENCH_DIR (scratch data, default
# /tmp/bench-kafka), BLOSSOM (the blossom binary, default target/release/blossom), REDPANDA_IMAGE, OUT; for Blossom,
# TAIL_CERT (strict | crc), TIERED (true | false: storage.tiered), HOT_ROWS (optional storage.hot_rows)
# and RECORD=1 (each broker records a trace into
# $BENCH_DIR/blossom/traces). BROKER_CPUS pins the brokers of Blossom and Kafka: three `taskset -c` lists separated by
# spaces, one per broker (e.g. "0-3,16-19 4-7,20-23 8-11,24-27"). PARTITIONS=0 creates no topic.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
KAFKA_HOME=${KAFKA_HOME:-$(ls -d "$ROOT"/.tools/kafka_* 2>/dev/null | head -1)}
K=$KAFKA_HOME/bin
BENCH_DIR=${BENCH_DIR:-/tmp/bench-kafka}
BLOSSOM=${BLOSSOM:-$ROOT/target/release/blossom}
REDPANDA_IMAGE=${REDPANDA_IMAGE:-mirror.gcr.io/redpandadata/redpanda:latest}
OUT=${OUT:-$BENCH_DIR/results}
BOOTSTRAP=127.0.0.1:19092,127.0.0.1:19093,127.0.0.1:19094
TOPIC=${TOPIC:-bench}
PARTITIONS=${PARTITIONS:-6}

die() { echo "bench-kafka: $*" >&2; exit 1; }
[ -x "$K/kafka-producer-perf-test.sh" ] || die "no Kafka distribution (KAFKA_HOME=$KAFKA_HOME)"

# ---- Blossom: examples/kafka/broker.bls, three `blossom run` processes
blossom_up() {
  [ -x "$BLOSSOM" ] || die "no blossom binary at $BLOSSOM (cargo build --release -p blossom-cli)"
  local d=$BENCH_DIR/blossom
  rm -rf "$d"; mkdir -p "$d"
  {
    printf 'format = 1\n\n[deployment]\nid = "bench"\nprogram = "kafka"\nversion = 1\n'
    printf 'source = "%s/examples/kafka/broker.bls"\nsecrets = "k.secrets"\n' "$ROOT"
    for i in 1 2 3; do
      local dial=""
      for j in 1 2 3; do
        [ "$i" != "$j" ] && dial="$dial${dial:+, }b$j = \"127.0.0.1:1919$((j + 1))\""
      done
      printf '\n[[node]]\nname = "b%s"\nrole = "Broker"\naddr = "127.0.0.1:1919%s"\n' "$i" "$((i + 1))"
      printf 'principal = "spiffe://bench/kafka/b%s"\nstreams = { kafka = "127.0.0.1:1909%s" }\ndial = { %s }\n' \
        "$i" "$((i + 1))" "$dial"
    done
    printf '\n[statics]\nbroker = [["b1", 1, "127.0.0.1", 19092], ["b2", 2, "127.0.0.1", 19093], '
    printf '["b3", 3, "127.0.0.1", 19094]]\n\n[security]\nmode = "insecure-dev"\n\n[storage]\ndata_dir = "data"\n'
    printf 'tail_certification = "%s"\n' "${TAIL_CERT:-strict}"
    printf 'tiered = %s\n' "${TIERED:-true}"
    if [ -n "${EVAL_WORKERS:-}" ]; then printf 'eval_workers = %s\n' "$EVAL_WORKERS"; fi
    if [ -n "${MAP_CACHE_ROWS:-}" ]; then printf 'map_cache_rows = %s\n' "$MAP_CACHE_ROWS"; fi
    if [ -n "${MAP_CACHE_JOINS:-}" ]; then printf 'map_cache_joins = %s\n' "$MAP_CACHE_JOINS"; fi
    if [ -n "${LAZY_INDEXES:-}" ]; then printf 'lazy_indexes = %s\n' "$LAZY_INDEXES"; fi
    if [ -n "${HOT_ROWS:-}" ]; then printf 'hot_rows = %s\n' "$HOT_ROWS"; fi
    if [ -n "${BLOCK_CACHE_BYTES:-}" ]; then printf 'cache_bytes = %s\n' "$BLOCK_CACHE_BYTES"; fi
  } > "$d/deploy.toml"
  printf 'seed = "00112233445566778899aabbccddeeff"\n' > "$d/k.secrets"
  chmod 600 "$d/k.secrets"
  echo blossom > "$BENCH_DIR/system"
  for i in 1 2 3; do blossom_start "$i" --init-fresh; done
}

# Broker $1 of the Blossom cluster; `--init-fresh` (for a new cluster) as $2, or nothing to recover from its store.
blossom_start() {
  local d=$BENCH_DIR/blossom i=$1
  nohup $(pin "$i") "$BLOSSOM" run --deploy "$d/deploy.toml" --node "b$i" --insecure-dev ${2:-} \
    --stats "$d/b$i.stats" ${RECORD:+--record "$d/traces"} >> "$d/b$i.log" 2>&1 &
  echo $! > "$BENCH_DIR/pid.$i"
}

# `taskset -c LIST` for broker $1 when BROKER_CPUS is set.
pin() {
  [ -n "${BROKER_CPUS:-}" ] || return 0
  local cpus
  read -ra cpus <<< "$BROKER_CPUS"
  [ "${#cpus[@]}" = 3 ] || die "BROKER_CPUS needs three CPU lists"
  echo "taskset -c ${cpus[$(($1 - 1))]}"
}

# ---- Apache Kafka: three KRaft nodes, each broker and controller (a static quorum). `log.flush.interval.messages=1`:
# every append is fsynced on every replica before it is acknowledged or fetched further (Kafka's default leaves the
# flush to the OS, relying on replication alone; `KAFKA_FSYNC=default` measures that too).
kafka_up() {
  local d=$BENCH_DIR/kafka
  rm -rf "$d"; mkdir -p "$d"
  local id
  id=$("$K/kafka-storage.sh" random-uuid)
  for i in 1 2 3; do
    cat > "$d/k$i.properties" <<P
process.roles=broker,controller
node.id=$i
controller.quorum.voters=1@127.0.0.1:19292,2@127.0.0.1:19293,3@127.0.0.1:19294
listeners=PLAINTEXT://127.0.0.1:1909$((i + 1)),CONTROLLER://127.0.0.1:1929$((i + 1))
advertised.listeners=PLAINTEXT://127.0.0.1:1909$((i + 1))
controller.listener.names=CONTROLLER
inter.broker.listener.name=PLAINTEXT
listener.security.protocol.map=PLAINTEXT:PLAINTEXT,CONTROLLER:PLAINTEXT
log.dirs=$d/data$i
offsets.topic.replication.factor=3
transaction.state.log.replication.factor=3
P
    if [ "${KAFKA_FSYNC:-every}" = every ]; then
      echo "log.flush.interval.messages=1" >> "$d/k$i.properties"
    fi
    "$K/kafka-storage.sh" format -t "$id" -c "$d/k$i.properties" > /dev/null
  done
  echo kafka > "$BENCH_DIR/system"
  for i in 1 2 3; do kafka_start "$i"; done
}

kafka_start() {
  local d=$BENCH_DIR/kafka i=$1
  LOG_DIR=$d/logs$i nohup $(pin "$i") "$K/kafka-server-start.sh" "$d/k$i.properties" >> "$d/k$i.out" 2>&1 &
  echo $! > "$BENCH_DIR/pid.$i"
}

# ---- Redpanda: three containers on the host network, one core and 2 GiB each, production mode (fsync on; the
# dev-container mode would bypass it).
redpanda_up() {
  local d=$BENCH_DIR/redpanda
  podman unshare rm -rf "$d" 2> /dev/null || rm -rf "$d"
  mkdir -p "$d"
  for i in 1 2 3; do
    mkdir -p "$d/conf$i" "$d/data$i"
    cat > "$d/conf$i/redpanda.yaml" <<Y
redpanda:
  data_directory: /var/lib/redpanda/data
  empty_seed_starts_cluster: false
  seed_servers:
    - host: {address: 127.0.0.1, port: 19192}
    - host: {address: 127.0.0.1, port: 19193}
    - host: {address: 127.0.0.1, port: 19194}
  rpc_server: {address: 127.0.0.1, port: 1919$((i + 1))}
  advertised_rpc_api: {address: 127.0.0.1, port: 1919$((i + 1))}
  kafka_api:
    - {address: 127.0.0.1, port: 1909$((i + 1))}
  advertised_kafka_api:
    - {address: 127.0.0.1, port: 1909$((i + 1))}
  admin:
    - {address: 127.0.0.1, port: 1964$((i + 1))}
  developer_mode: false
pandaproxy:
  pandaproxy_api:
    - {address: 127.0.0.1, port: 1808$((i + 1))}
schema_registry:
  schema_registry_api:
    - {address: 127.0.0.1, port: 1818$((i + 1))}
rpk:
  additional_start_flags:
    - "--smp=${REDPANDA_SMP:-1}"
    - "--memory=2G"
    - "--reserve-memory=0M"
Y
    podman unshare chown -R 101:101 "$d/conf$i" "$d/data$i"
    podman run -d --name "bench-rp$i" --network host \
      -v "$d/conf$i:/etc/redpanda" -v "$d/data$i:/var/lib/redpanda/data" \
      "$REDPANDA_IMAGE" redpanda start --overprovisioned > /dev/null
  done
}

wait_ready() {
  local i pid
  for i in $(seq 1 60); do
    for f in "$BENCH_DIR"/pid.*; do
      [ -f "$f" ] || continue
      if ! kill -0 "$(cat "$f")" 2> /dev/null; then
        tail -n 20 "$BENCH_DIR"/*/b1.log "$BENCH_DIR"/*/k1.out 2> /dev/null >&2
        die "a broker exited while starting"
      fi
    done
    if timeout 10 "$K/kafka-topics.sh" --bootstrap-server "$BOOTSTRAP" --list > /dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  die "the cluster did not answer within a minute"
}

create_topic() {
  [ "$PARTITIONS" != 0 ] || return 0
  timeout 60 "$K/kafka-topics.sh" --bootstrap-server "$BOOTSTRAP" --create --topic "$TOPIC" \
    --partitions "$PARTITIONS" --replication-factor 3 > /dev/null
}

down() {
  local f
  for f in "$BENCH_DIR"/pid.*; do
    [ -f "$f" ] || continue
    kill "$(cat "$f")" 2> /dev/null || true
    rm -f "$f"
  done
  if command -v podman > /dev/null; then
    for i in 1 2 3; do podman rm -f "bench-rp$i" > /dev/null 2>&1 || true; done
  fi
  sleep 1
}

cmd=${1:-}
shift || true
mkdir -p "$BENCH_DIR"
case "$cmd" in
  up)
    down
    case "${1:-}" in
      blossom) blossom_up ;;
      kafka) kafka_up ;;
      redpanda) redpanda_up ;;
      *) die "up: blossom | kafka | redpanda" ;;
    esac
    wait_ready
    create_topic
    ;;
  down) down ;;
  produce)
    "$K/kafka-producer-perf-test.sh" --topic "$TOPIC" --producer-props bootstrap.servers="$BOOTSTRAP" acks=all "$@"
    ;;
  consume)
    # A consumer in a group (the coordinator, joins and offset commits included) reading the topic from the start.
    "$K/kafka-consumer-perf-test.sh" --bootstrap-server "$BOOTSTRAP" --topic "$TOPIC" --timeout 60000 "$@"
    ;;
  e2e)
    # One record at a time, produced with acks=all and consumed: the time from send to receipt.
    # Arguments: the number of records and their size.
    "$K/kafka-e2e-latency.sh" "$BOOTSTRAP" "$TOPIC" "${1:-2000}" all "${2:-1024}"
    ;;
  kill)
    [ -f "$BENCH_DIR/pid.${1:-}" ] || die "kill: no broker ${1:-} running"
    kill -9 "$(cat "$BENCH_DIR/pid.$1")"
    rm -f "$BENCH_DIR/pid.$1"
    ;;
  restart)
    case "${1:-}" in 1 | 2 | 3) ;; *) die "restart: a broker, 1-3" ;; esac
    [ ! -f "$BENCH_DIR/pid.$1" ] || die "restart: broker $1 is running"
    case "$(cat "$BENCH_DIR/system")" in
      blossom) blossom_start "$1" ;;
      kafka) kafka_start "$1" ;;
      *) die "restart: only Blossom and Kafka brokers" ;;
    esac
    ;;
  *) sed -n '2,15p' "$0" >&2; exit 2 ;;
esac
