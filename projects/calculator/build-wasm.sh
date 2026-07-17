#!/usr/bin/env bash
# Build the calculator into a browser-ready wasm module.
#
#   ./build-wasm.sh
#
# All the reusable work lives in the aury skill: dev.sh runs the author/repair
# loop over the hand-authored JSON AST; wasm-lib.sh (bundling toolchain
# detection) emits the wasm32-wasi reactor. This script only declares the
# project-specific bits — which functions to export and where to write the wasm.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SKILL="$(git -C "$HERE" rev-parse --show-toplevel)/.claude/skills/aury"

# Public functions exported to the browser (internal *_iter / *_check helpers stay private).
EXPORTS="add,subtract,multiply,divide,modulo,percent,average,maximum,minimum,\
negate,absolute,square,increment,decrement,double,gcd,lcm,power,factorial,\
fibonacci,isqrt,is_even,is_prime"

echo "==> validate + property-test (skill: dev.sh)"
"$SKILL/dev.sh" "$HERE/calculator.json" | tail -3

echo "==> compile wasm-lib reactor (skill: wasm-lib.sh)"
"$SKILL/wasm-lib.sh" "$HERE/calculator.json" \
  --export "$EXPORTS" \
  -o "$HERE/web/public/calculator.wasm"

echo "done -> web/public/calculator.wasm"
