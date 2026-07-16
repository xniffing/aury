#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
project_dir="$(cd -- "$script_dir/.." && pwd)"
aury_bin="${AURY_BIN:-$project_dir/target/release/aury}"
program="$script_dir/moon-distance.aury"

if [[ ! -x "$aury_bin" ]]; then
  cargo build --release --manifest-path "$project_dir/Cargo.toml"
fi

timestamp="$(date -u +%s)"
printf 'UTC: %s\n' "$(date -u -d "@$timestamp" +%Y-%m-%dT%H:%M:%SZ)"
printf 'Unix timestamp: %s\n' "$timestamp"

case "${1:-}" in
  --native)
    executable="${TMPDIR:-/tmp}/aury-moon-distance-$$"
    trap 'rm -f "$executable"' EXIT
    "$aury_bin" compile "$program" moon-report "$timestamp" -o "$executable"
    ;;
  --wasm)
    # Build a wasm32-wasi module and run it through wasmtime/wasmer. The shared
    # helper exports the toolchain env (wasi-sdk or Homebrew) if not already set.
    # shellcheck source=wasm-toolchain.sh
    source "$script_dir/wasm-toolchain.sh"
    module="${TMPDIR:-/tmp}/aury-moon-distance-$$.wasm"
    trap 'rm -f "$module"' EXIT
    "$aury_bin" wasm "$program" moon-report "$timestamp" -o "$module"
    ;;
  *)
    "$aury_bin" run "$program" moon-report "$timestamp"
    ;;
esac
