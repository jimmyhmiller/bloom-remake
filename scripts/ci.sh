#!/usr/bin/env bash
# The CI entry point (ARCHITECTURE §11.10, PLAN §3).
#
#   scripts/ci.sh [fast|gate|nightly]      (default: fast)
#
# Runs every fragment scripts/ci.d/NN-*.sh whose tier matches, in lexical order, from the repository root:
#   fast     fragments marked `# tier: fast`
#   gate     fragments marked `# tier: fast` or `# tier: gate`, then cargo-deny (the milestone gate, PLAN §3)
#   nightly  fragments marked `# tier: nightly`
# Every fragment declares its tier on a line `# tier: <tier>`; a fragment without one is an error. Each fragment is
# owned by the WP that adds it (PLAN §2.4). All fragments run even when one fails; the exit status is non-zero if
# any failed. `.tools/bin` (scripts/install-dev-tools.sh) is put first on PATH.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIER="${1:-fast}"
case "$TIER" in
  fast|gate|nightly) ;;
  -h|--help) sed -n '2,13p' "$0"; exit 0 ;;
  *) echo "ci: unknown tier '$TIER' (expected fast, gate or nightly)" >&2; exit 2 ;;
esac

export PATH="$ROOT/.tools/bin:$PATH"
export CI_TIER="$TIER"
cd "$ROOT" || exit 1

runs() {
  # Whether a fragment of tier $1 runs in tier $TIER.
  case "$TIER:$1" in
    fast:fast|gate:fast|gate:gate|nightly:nightly) return 0 ;;
    *) return 1 ;;
  esac
}

failed=()
ran=0
shopt -s nullglob
for fragment in scripts/ci.d/[0-9][0-9]-*.sh; do
  tier="$(sed -n 's/^# tier: *\([a-z]*\).*$/\1/p' "$fragment" | head -n1)"
  case "$tier" in
    fast|gate|nightly) ;;
    *) echo "ci: $fragment has no valid '# tier: fast|gate|nightly' line" >&2; failed+=("$fragment (no tier)"); continue ;;
  esac
  runs "$tier" || continue
  echo "==> $fragment (tier $tier)"
  ran=$((ran + 1))
  if ! bash "$fragment"; then
    echo "ci: FAILED: $fragment" >&2
    failed+=("$fragment")
  fi
done

if [ "$TIER" = gate ]; then
  if command -v cargo-deny >/dev/null 2>&1; then
    echo "==> cargo deny check"
    ran=$((ran + 1))
    cargo deny check || failed+=("cargo deny check")
  else
    echo "ci: cargo-deny is not installed, so licenses, advisories and bans were NOT checked;"
    echo "ci: run scripts/install-dev-tools.sh to install it into .tools/"
  fi
fi

if [ "${#failed[@]}" -gt 0 ]; then
  echo "ci: $TIER tier FAILED (${#failed[@]} of $ran):" >&2
  printf '  %s\n' "${failed[@]}" >&2
  exit 1
fi
echo "ci: $TIER tier passed ($ran steps)"
