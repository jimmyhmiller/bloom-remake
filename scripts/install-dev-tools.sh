#!/usr/bin/env bash
# Installs the development tools CI uses into the repository-local, git-ignored `.tools/` (PLAN §2.8).
# Nothing is installed globally. Re-running is cheap: a tool already present at its pinned version is skipped.
#
#   scripts/install-dev-tools.sh            install every tool
#   scripts/install-dev-tools.sh --check    report what is installed, install nothing (exit 1 if anything is missing)
#
# M13.3 extends this list (cargo-fuzz, cargo-mutants, cargo-auditable, an SBOM tool).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TOOLS="$ROOT/.tools"

# name version binary
PINNED=(
  "cargo-deny 0.20.2 cargo-deny"
  "cargo-hack 0.6.45 cargo-hack"
  "cargo-nextest 0.9.146 cargo-nextest"
  "wasm-bindgen-cli 0.2.114 wasm-bindgen" # the browser host's glue (scripts/build-web.sh); matches crates/blossom-web
)

check_only=0
case "${1:-}" in
  "") ;;
  --check) check_only=1 ;;
  -h|--help) sed -n '2,9p' "$0"; exit 0 ;;
  *) echo "install-dev-tools: unknown argument: $1" >&2; exit 2 ;;
esac

installed_version() {
  # Prints the installed version of $1 (a binary in .tools/bin), or nothing.
  local bin="$TOOLS/bin/$1"
  [ -x "$bin" ] || return 0
  # A cargo subcommand answers `<bin> <subcommand> --version`, a plain tool `<bin> --version`, with "<name> <version>".
  case "$1" in
    cargo-*) "$bin" "${1#cargo-}" --version 2>/dev/null | awk '{print $2}' | head -n1 ;;
    *) "$bin" --version 2>/dev/null | awk '{print $2}' | head -n1 ;;
  esac
}

missing=0
for entry in "${PINNED[@]}"; do
  read -r name version bin <<<"$entry"
  have="$(installed_version "$bin")"
  if [ "$have" = "$version" ]; then
    echo "install-dev-tools: $name $version already installed"
    continue
  fi
  if [ "$check_only" = 1 ]; then
    echo "install-dev-tools: $name $version is not installed (found: ${have:-nothing})"
    missing=1
    continue
  fi
  echo "install-dev-tools: installing $name $version into $TOOLS"
  cargo install --locked --root "$TOOLS" --version "$version" "$name"
done

if [ "$check_only" = 1 ] && [ "$missing" = 1 ]; then
  exit 1
fi
echo "install-dev-tools: done; ci.sh puts $TOOLS/bin first on PATH"
