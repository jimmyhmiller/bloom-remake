#!/usr/bin/env bash
# The services the state store adapters' tests need (docs/design/STATELESS.md §10), on this machine, under
# target/services: a Postgres cluster (pg_ctl; TLS on, with a certificate authority made here) and MinIO (S3).
#
#   scripts/test-services.sh start   # starts both (again, if they run already: a no-op)
#   scripts/test-services.sh env     # prints the variables the tests read, for `eval "$(… env)"`
#   scripts/test-services.sh stop
#
# Needs Postgres's server binaries (initdb, pg_ctl; on macOS `brew install postgresql@17`; set PG_BIN to their
# directory if they are not on PATH), openssl, and a `minio` binary (on macOS `brew install minio`; MinIO no longer
# publishes community builds, so elsewhere build RELEASE.2025-10-15T17-29-55Z from source and set MINIO_BIN).
set -euo pipefail
cd "$(dirname "$0")/.."
dir="${CARGO_TARGET_DIR:-$PWD/target}/services"
pg_port="${BLOSSOM_TEST_PG_PORT:-55432}"
s3_port="${BLOSSOM_TEST_S3_PORT:-59000}"
s3_user=blossom
s3_secret=blossom-test-secret

find_pg() {
  if [ -n "${PG_BIN:-}" ]; then echo "$PG_BIN"; return; fi
  if command -v pg_ctl >/dev/null; then dirname "$(command -v pg_ctl)"; return; fi
  for d in /opt/homebrew/opt/postgresql@17/bin /usr/local/opt/postgresql@17/bin /usr/lib/postgresql/*/bin; do
    if [ -x "$d/pg_ctl" ]; then echo "$d"; return; fi
  done
  echo "no Postgres server binaries (pg_ctl): install them or set PG_BIN" >&2
  exit 1
}

find_minio() {
  if [ -n "${MINIO_BIN:-}" ]; then echo "$MINIO_BIN"; return; fi
  if command -v minio >/dev/null; then command -v minio; return; fi
  echo "no minio binary: install it (brew install minio) or set MINIO_BIN" >&2
  exit 1
}

certs() {
  local c="$dir/certs"
  [ -f "$c/server.crt" ] && return
  mkdir -p "$c"
  openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -subj "/CN=blossom test CA" \
    -keyout "$c/ca.key" -out "$c/ca.pem" 2>/dev/null
  openssl req -newkey rsa:2048 -nodes -subj "/CN=localhost" -keyout "$c/server.key" -out "$c/server.csr" 2>/dev/null
  printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\nbasicConstraints=CA:FALSE\n' >"$c/ext.cnf"
  openssl x509 -req -in "$c/server.csr" -CA "$c/ca.pem" -CAkey "$c/ca.key" -CAcreateserial -days 3650 \
    -extfile "$c/ext.cnf" -out "$c/server.crt" 2>/dev/null
  chmod 600 "$c/server.key"
}

start_pg() {
  local bin data
  bin="$(find_pg)"
  data="$dir/pg"
  if [ ! -f "$data/PG_VERSION" ]; then
    mkdir -p "$data"
    "$bin/initdb" -D "$data" -A trust -U blossom >/dev/null
    certs
    cat >>"$data/postgresql.conf" <<EOF
listen_addresses = '127.0.0.1'
port = $pg_port
unix_socket_directories = ''
ssl = on
ssl_cert_file = '$dir/certs/server.crt'
ssl_key_file = '$dir/certs/server.key'
max_connections = 300
fsync = on
EOF
  fi
  if ! "$bin/pg_ctl" -D "$data" status >/dev/null 2>&1; then
    "$bin/pg_ctl" -D "$data" -l "$dir/pg.log" -w start >/dev/null
  fi
  "$bin/psql" -h 127.0.0.1 -p "$pg_port" -U blossom -d postgres -tAc \
    "select 1 from pg_database where datname = 'blossom_test'" | grep -q 1 ||
    "$bin/createdb" -h 127.0.0.1 -p "$pg_port" -U blossom blossom_test
}

start_minio() {
  local bin pid
  bin="$(find_minio)"
  mkdir -p "$dir/minio"
  if [ -f "$dir/minio.pid" ] && kill -0 "$(cat "$dir/minio.pid")" 2>/dev/null; then return; fi
  MINIO_ROOT_USER="$s3_user" MINIO_ROOT_PASSWORD="$s3_secret" nohup "$bin" server --quiet \
    --address "127.0.0.1:$s3_port" --console-address "127.0.0.1:$((s3_port + 1))" "$dir/minio" \
    >"$dir/minio.log" 2>&1 &
  pid=$!
  echo "$pid" >"$dir/minio.pid"
  for _ in $(seq 1 100); do
    if curl -sf "http://127.0.0.1:$s3_port/minio/health/live" >/dev/null; then return; fi
    sleep 0.1
  done
  echo "minio did not come up; see $dir/minio.log" >&2
  exit 1
}

stop() {
  local bin
  if [ -f "$dir/pg/PG_VERSION" ]; then
    bin="$(find_pg)"
    "$bin/pg_ctl" -D "$dir/pg" -m fast stop >/dev/null 2>&1 || true
  fi
  if [ -f "$dir/minio.pid" ]; then
    kill "$(cat "$dir/minio.pid")" 2>/dev/null || true
    rm -f "$dir/minio.pid"
  fi
}

env_vars() {
  cat <<EOF
export BLOSSOM_TEST_POSTGRES='postgres://blossom@127.0.0.1:$pg_port/blossom_test?sslmode=disable'
export BLOSSOM_TEST_POSTGRES_TLS='postgres://blossom@localhost:$pg_port/blossom_test?sslmode=require&sslrootcert=$dir/certs/ca.pem'
export BLOSSOM_TEST_S3='s3://blossom-test/run?endpoint=http://127.0.0.1:$s3_port&region=us-east-1&path_style=true'
export BLOSSOM_TEST_S3_KEY='$s3_user'
export BLOSSOM_TEST_S3_SECRET='$s3_secret'
EOF
}

case "${1:-}" in
  start)
    mkdir -p "$dir"
    start_pg
    start_minio
    ;;
  stop) stop ;;
  env) env_vars ;;
  *)
    echo "usage: $0 start|stop|env" >&2
    exit 2
    ;;
esac
