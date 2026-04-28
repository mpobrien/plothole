#!/usr/bin/env bash
# Vercel build entry point. Installs rustup + wasm-pack, builds the wasm bundle,
# and stages the static files into ./dist for deployment.
set -euo pipefail

# Always install via rustup. Vercel ships a system rustc (without rustup, no wasm32 target),
# so we ignore it and put $HOME/.cargo/bin first on PATH to override.
if [ ! -x "$HOME/.cargo/bin/rustup" ]; then
  echo ">>> installing rustup"
  # Vercel pre-installs a Rust at /rust; rustup-init refuses unless we tell it to skip the check.
  export RUSTUP_INIT_SKIP_PATH_CHECK=yes
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain stable --profile minimal --target wasm32-unknown-unknown
fi
export PATH="$HOME/.cargo/bin:$PATH"

# Idempotent — no-op if already added.
rustup target add wasm32-unknown-unknown

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
