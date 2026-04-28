#!/usr/bin/env bash
# Vercel build entry point. Installs rustup + wasm-pack, builds the wasm bundle,
# and stages the static files into ./dist for deployment.
set -euo pipefail

if ! command -v rustc >/dev/null 2>&1; then
  echo ">>> installing rustup"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain stable --profile minimal
fi
export PATH="$HOME/.cargo/bin:$PATH"

if ! command -v wasm-pack >/dev/null 2>&1; then
  echo ">>> installing wasm-pack"
  curl --proto '=https' --tlsv1.2 -sSf https://rustwasm.github.io/wasm-pack/installer/init.sh | sh
fi

echo ">>> building wasm"
wasm-pack build --target web --no-default-features --features wasm

echo ">>> staging static files"
rm -rf dist
mkdir -p dist
cp index.html dist/
cp -r pkg dist/

echo ">>> done; output in ./dist"
