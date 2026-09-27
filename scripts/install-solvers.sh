#!/usr/bin/env bash
# Install pinned SMT/ASP solvers into this checkout's ignored .tools directory only.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tools="$root/.tools"
mkdir -p "$tools/bin"
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) asset=cvc5-macOS-arm64-static.zip; digest=0ad2df5de1b35c0fda6afa9ca9f7b542a615c2137e1ec678a45deccdda1871b2 ;;
  Darwin-x86_64) asset=cvc5-macOS-x86_64-static.zip; digest=45e4156e9285162ae7e43504fa451ca2f618994f0d83fdd661d945f208d75f14 ;;
  Linux-aarch64) asset=cvc5-Linux-arm64-static.zip; digest=2572d01b142a6bfebdcb259f5a395f6228d2db5609f7dcc9a60851a5f1a58655 ;;
  Linux-x86_64) asset=cvc5-Linux-x86_64-static.zip; digest=413f56f01f3a7374105c654581e67249eb66d4e430e748b17962d595cd4861b6 ;;
  *) echo "install-solvers: unsupported platform $(uname -s)-$(uname -m)" >&2; exit 2 ;;
esac
if [ ! -x "$tools/bin/cvc5" ] || ! "$tools/bin/cvc5" --version 2>/dev/null | head -n1 | grep -q '1.3.3'; then
  scratch="$(mktemp -d "$tools/solver-download.XXXXXX")"
  trap 'rm -rf "$scratch"' EXIT
  curl -fL --retry 3 "https://github.com/cvc5/cvc5/releases/download/cvc5-1.3.3/$asset" -o "$scratch/$asset"
  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$scratch/$asset" | awk '{print $1}')"
  else
    actual="$(shasum -a 256 "$scratch/$asset" | awk '{print $1}')"
  fi
  if [ "$actual" != "$digest" ]; then echo "install-solvers: cvc5 checksum mismatch" >&2; exit 1; fi
  unzip -q "$scratch/$asset" -d "$scratch/unpacked"
  binary="$(find "$scratch/unpacked" -type f -name cvc5 -print -quit)"
  if [ -z "$binary" ]; then echo "install-solvers: cvc5 binary missing from release" >&2; exit 1; fi
  install -m 755 "$binary" "$tools/bin/cvc5"
  rm -rf "$scratch"
  trap - EXIT
fi
if [ ! -x "$tools/venv/bin/python" ]; then
  python3 -m venv "$tools/venv"
fi
if ! "$tools/venv/bin/python" -m clingo --version 2>/dev/null | head -n1 | grep -q '5.8.0'; then
  "$tools/venv/bin/python" -m pip install --disable-pip-version-check 'clingo==5.8.0'
fi
cat > "$tools/bin/clingo" <<SH
#!/usr/bin/env bash
exec "$tools/venv/bin/python" -m clingo "\$@"
SH
chmod 755 "$tools/bin/clingo"
echo "install-solvers: cvc5 1.3.3 and clingo 5.8.0 ready in $tools/bin"
