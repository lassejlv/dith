#!/usr/bin/env bash
# Install the wasm-bindgen CLI matching Cargo.lock and binaryen's wasm-opt (Linux x86_64) into
# ~/.local/bin, for scripts/build-wasm.sh. On GitHub Actions the directory is added to PATH.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="$HOME/.local/bin"
binaryen_version="${BINARYEN_VERSION:-133}"
wasm_bindgen_version="$(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/"/, "", $3); print $3; exit }' "$root/Cargo.lock")"
work="$(mktemp -d)"
mkdir -p "$bin"

curl -fsSL "https://github.com/wasm-bindgen/wasm-bindgen/releases/download/$wasm_bindgen_version/wasm-bindgen-$wasm_bindgen_version-x86_64-unknown-linux-musl.tar.gz" |
  tar -xz -C "$work"
install "$work/wasm-bindgen-$wasm_bindgen_version-x86_64-unknown-linux-musl/wasm-bindgen" "$bin/"

curl -fsSL "https://github.com/WebAssembly/binaryen/releases/download/version_$binaryen_version/binaryen-version_$binaryen_version-x86_64-linux.tar.gz" |
  tar -xz -C "$work"
mkdir -p "$HOME/.local/lib"
cp -r "$work/binaryen-version_$binaryen_version/lib/." "$HOME/.local/lib/" 2>/dev/null || true
install "$work/binaryen-version_$binaryen_version/bin/wasm-opt" "$bin/"

if [ -n "${GITHUB_PATH:-}" ]; then
  echo "$bin" >>"$GITHUB_PATH"
fi
"$bin/wasm-bindgen" --version
"$bin/wasm-opt" --version
