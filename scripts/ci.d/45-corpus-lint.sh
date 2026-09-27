#!/usr/bin/env bash
# tier: fast
# Validates every golden-corpus manifest against schema v1 (PLAN §5) with tests/corpus/tools/check_manifests.py,
# for every corpus area that has cases. WP M5.2 replaces the script with `cargo run -p xtask -- corpus --lint`
# (run by its fragment 50-corpus.sh) and deletes it; this fragment then reports that and passes, but only while
# 50-corpus.sh exists and runs the lint, so the manifests are never left unchecked. Owned by M1.1.
set -uo pipefail
checker=tests/corpus/tools/check_manifests.py
successor=scripts/ci.d/50-corpus.sh
if [ ! -f "$checker" ]; then
  if [ -f "$successor" ] && grep -q -- 'corpus --lint' "$successor"; then
    echo "45-corpus-lint: $checker is gone; manifests are linted by 'xtask corpus --lint' ($successor)"
    exit 0
  fi
  echo "45-corpus-lint: $checker is gone and $successor does not run 'xtask corpus --lint':" >&2
  echo "45-corpus-lint: nothing validates the corpus manifests" >&2
  exit 1
fi
status=0
areas=0
for area in tests/corpus/*/; do
  name="$(basename "$area")"
  [ "$name" = tools ] && continue
  if [ -z "$(find "$area" -name manifest.toml -print -quit)" ]; then
    continue
  fi
  areas=$((areas + 1))
  python3 "$checker" "$name" || status=1
done
echo "45-corpus-lint: $areas corpus area(s) checked"
exit "$status"
