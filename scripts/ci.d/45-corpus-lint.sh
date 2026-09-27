#!/usr/bin/env bash
# tier: fast
# Validates every golden-corpus manifest against schema v1 (PLAN §5) with tests/corpus/tools/check_manifests.py,
# for every corpus area that has cases. WP M5.2 replaces the script with `cargo run -p xtask -- corpus --lint`
# (run by its fragment 50-corpus.sh) and deletes it; this fragment then reports that and passes. Owned by M1.1.
set -uo pipefail
checker=tests/corpus/tools/check_manifests.py
if [ ! -f "$checker" ]; then
  echo "45-corpus-lint: $checker is gone; manifests are linted by 'xtask corpus --lint' (50-corpus.sh)"
  exit 0
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
