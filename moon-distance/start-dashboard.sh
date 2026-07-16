#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
project_dir="$(cd -- "$script_dir/.." && pwd)"

if [[ ! -x "$project_dir/target/release/aury" ]]; then
  cargo build --release --manifest-path "$project_dir/Cargo.toml"
fi

# Export the wasm32-wasi toolchain env (wasi-sdk or Homebrew) so the server's
# `aury wasm` subprocess can build modules. No-op if already configured.
# shellcheck source=wasm-toolchain.sh
source "$script_dir/wasm-toolchain.sh"

exec node "$script_dir/web/server.mjs"
