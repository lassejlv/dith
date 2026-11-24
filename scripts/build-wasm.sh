#!/usr/bin/env bash
# Build the dith WebAssembly module into npm/wasm/ (used by the npm package and web/ playground).
#
# Needs: rustup target wasm32-unknown-unknown, wasm-bindgen-cli matching the wasm-bindgen version
# in Cargo.lock, and optionally binaryen's wasm-opt (skipped with a warning when missing).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="$root/npm/wasm"
target_dir="${CARGO_TARGET_DIR:-$root/target}"

cd "$root"
cargo rustc --locked --lib \
  --target wasm32-unknown-unknown --profile wasm-release \
  --no-default-features --features wasm --crate-type cdylib

rm -rf "$out"
wasm-bindgen "$target_dir/wasm32-unknown-unknown/wasm-release/dith.wasm" \
  --out-dir "$out" --out-name dith --target web --omit-default-module-path

if command -v wasm-opt >/dev/null 2>&1; then
  wasm-opt "$out/dith_bg.wasm" -O3 --enable-bulk-memory --enable-nontrapping-float-to-int \
    --enable-sign-ext --enable-mutable-globals --enable-reference-types --enable-multivalue \
    --strip-debug --strip-producers -o "$out/dith_bg.wasm"
else
  echo "warning: wasm-opt not found; skipping size optimisation" >&2
fi

raw=$(wc -c <"$out/dith_bg.wasm")
gz=$(gzip -9 -c "$out/dith_bg.wasm" | wc -c)
echo "dith_bg.wasm: $raw bytes ($gz gzipped)"
