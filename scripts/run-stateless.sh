#!/usr/bin/env bash
# Runs an example app on stateless hosts (docs/design/STATELESS.md): N `blossom serve` instances over one state store,
# behind a round-robin proxy that sends each request to the next instance (scripts/rr-proxy.mjs).
#
#   scripts/run-stateless.sh                      # the keyed chat, 3 instances on SQLite: http://localhost:8080/?member=lunch
#   scripts/run-stateless.sh keyed_chat postgres  # on the Postgres of scripts/test-services.sh
#   scripts/run-stateless.sh keyed_chat s3 5      # 5 instances on its MinIO
#   STORE_URL=postgres://… scripts/run-stateless.sh keyed_chat custom
#
# Kill any instance (its pid is printed) and the pages carry on through the others. The SQLite store and the seed are
# kept under examples/web/.data/stateless-APP*, so a rerun keeps the rooms; `--fresh` as a fourth argument starts over.
# Builds the CLI and the page if they are missing. Ctrl-C stops everything.
set -euo pipefail
cd "$(dirname "$0")/.."
app="${1:-keyed_chat}"
store="${2:-sqlite}"
count="${3:-3}"
fresh="${4:-}"
port="${PORT:-8080}"
deploy="examples/web/$app.deploy.toml"
[ -f "$deploy" ] || { echo "run-stateless: no $deploy" >&2; exit 2; }
bin="${CARGO_TARGET_DIR:-target}/debug/blossom"
[ -x "$bin" ] || cargo build -q -p blossom-cli
[ -f web/pkg-member/blossom_web_bg.wasm ] || scripts/build-web.sh
data="examples/web/.data"
mkdir -p "$data"
seed_file="$data/stateless-$app.seed"
if [ "$fresh" = "--fresh" ]; then rm -f "$seed_file" "$data/stateless-$app.db"*; fi
[ -f "$seed_file" ] || openssl rand -hex 16 > "$seed_file"
case "$store" in
  sqlite) url="sqlite:$data/stateless-$app.db" ;;
  postgres | s3)
    scripts/test-services.sh start
    eval "$(scripts/test-services.sh env)"
    if [ "$store" = postgres ]; then
      url="$BLOSSOM_TEST_POSTGRES&schema=demo_$app"
    else
      url="${BLOSSOM_TEST_S3%%\?*}/demo-$app?${BLOSSOM_TEST_S3#*\?}"
      export AWS_ACCESS_KEY_ID="$BLOSSOM_TEST_S3_KEY" AWS_SECRET_ACCESS_KEY="$BLOSSOM_TEST_S3_SECRET"
    fi
    ;;
  custom) url="${STORE_URL:?STORE_URL names the store}" ;;
  *) echo "run-stateless: the store is sqlite, postgres, s3 or custom (STORE_URL)" >&2; exit 2 ;;
esac
pids=()
trap 'kill "${pids[@]}" 2>/dev/null' EXIT
backends=()
for i in $(seq 1 "$count"); do
  p=$((port + i))
  BLOSSOM_SEED="$(cat "$seed_file")" "$bin" serve --deploy "$deploy" --store "$url" --web "127.0.0.1:$p" \
    --web-root web --insecure-dev &
  pids+=($!)
  backends+=("127.0.0.1:$p")
  echo "run-stateless: instance $i on :$p (pid $!)"
done
node scripts/rr-proxy.mjs "$port" "${backends[@]}" &
pids+=($!)
echo "run-stateless: $app on $count stateless instances ($store): http://localhost:$port/?member=lunch (Ctrl-C to stop)"
wait
