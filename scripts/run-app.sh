#!/usr/bin/env bash
# Runs one of the shared example apps (examples/web/APP.bls with APP.deploy.toml) on a node that serves its page:
#
#   scripts/run-app.sh polls            # then open http://localhost:8080/ in two tabs (or two browsers)
#   scripts/run-app.sh board 8081
#   scripts/run-app.sh pixels 8082 --fresh   # start over with an empty store
#
# The node's store is examples/web/.data/APP and its seed examples/web/.data/APP.seed (made on the first run), so a
# restart keeps everything and the tabs keep their identities. Builds the CLI and the page if they are missing.
set -euo pipefail
cd "$(dirname "$0")/.."
app="${1:?usage: scripts/run-app.sh APP [PORT] [--fresh] (APP: polls, board, tictactoe, pixels, chat, todos_shared)}"
port="${2:-8080}"
fresh="${3:-}"
deploy="examples/web/$app.deploy.toml"
[ -f "$deploy" ] || { echo "run-app: no $deploy" >&2; exit 2; }
bin="${CARGO_TARGET_DIR:-target}/debug/blossom"
[ -x "$bin" ] || cargo build -q -p blossom-cli
[ -f web/pkg-member/blossom_web_bg.wasm ] || scripts/build-web.sh
data="examples/web/.data"
mkdir -p "$data"
seed_file="$data/$app.seed"
init=()
if [ "$fresh" = "--fresh" ] || [ ! -f "$seed_file" ]; then
  rm -rf "$data/$app"
  openssl rand -hex 16 > "$seed_file"
  init=(--init-fresh)
fi
echo "run-app: $app on http://localhost:$port/ (Ctrl-C to stop)"
BLOSSOM_SEED="$(cat "$seed_file")" exec "$bin" run --deploy "$deploy" --node s --insecure-dev "${init[@]}" \
  --web "127.0.0.1:$port" --web-root web
