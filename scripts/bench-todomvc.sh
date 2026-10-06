#!/usr/bin/env bash
# Benchmarks the Blossom TodoMVC (examples/web/todomvc.bls in the browser host) against the TodoMVC suites of
# Speedometer 3, with Speedometer's own runner and steps, in headless Chromium.
#
#   scripts/bench-todomvc.sh [--iterations N] [--suites A,B,…] [--json FILE]
#
# It builds the browser host (scripts/build-web.sh), fetches Speedometer at a pinned commit into
# target/bench/speedometer, copies web/ in as suites/blossom, appends the Blossom suites (tests/web/bench/
# blossom-suites.mjs) to Speedometer's suite list, and runs tests/web/bench/todomvc.mjs.
set -euo pipefail
cd "$(dirname "$0")/.."
SPEEDOMETER_REPO=https://github.com/WebKit/Speedometer.git
SPEEDOMETER_COMMIT=b0bc16e
dir=target/bench/speedometer

scripts/build-web.sh
if [ ! -d "$dir/.git" ]; then
  rm -rf "$dir"
  git clone --quiet "$SPEEDOMETER_REPO" "$dir"
fi
git -C "$dir" fetch --quiet origin
git -C "$dir" checkout --quiet --force "$SPEEDOMETER_COMMIT"
git -C "$dir" clean --quiet -fd

rm -rf "$dir/suites/blossom"
mkdir -p "$dir/suites/blossom"
cp -R web/index.html web/host.js web/host.css web/todomvc.css web/pkg "$dir/suites/blossom/"

# The Blossom suites go at the end of Speedometer's list.
suites="$dir/suites/default-suites.mjs"
node - "$suites" tests/web/bench/blossom-suites.mjs <<'NODE'
const fs = require("fs");
const [file, extra] = process.argv.slice(2);
let src = fs.readFileSync(file, "utf8");
const end = src.lastIndexOf("]);");
if (end < 0) throw new Error(`${file}: the suite list's end was not found`);
const added = [
  `blossomSuite("TodoMVC-Blossom", "suites/blossom/index.html?bench"),`,
  `blossomSuite("TodoMVC-Blossom-Persist", "suites/blossom/index.html?bench=persist"),`,
].join("\n    ");
src = src.slice(0, end) + `    ${added}\n` + src.slice(end);
fs.writeFileSync(file, src + "\n" + fs.readFileSync(extra, "utf8"));
NODE

(cd tests/web && npm ci --silent && node bench/todomvc.mjs "../../$dir" "$@")
