#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
project_dir="$(cd -- "$script_dir/.." && pwd)"

if [[ ! -x "$project_dir/target/release/aury" ]]; then
  cargo build --release --manifest-path "$project_dir/Cargo.toml"
fi

exec node "$script_dir/web/server.mjs"
