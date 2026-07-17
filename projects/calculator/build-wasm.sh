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

# Public functions exported to the browser (internal helpers stay private).
# Integer (i64/bool) exports cross the wasm boundary as JS BigInt; the f64
# exports cross as JS Number (no linear-memory marshaling — f64 is a wasm scalar).
EXPORTS="add,subtract,multiply,divide,modulo,percent,average,maximum,minimum,\
negate,absolute,square,increment,decrement,double,gcd,lcm,power,factorial,\
fibonacci,isqrt,is_even,is_prime,\
fadd,fsubtract,fmultiply,fdivide,fmaximum,fminimum,fpower,fnegate,fabs,fsquare,\
freciprocal,fsqrt,to_float,to_int,is_nan"

echo "==> validate + property-test (skill: dev.sh)"
"$SKILL/dev.sh" "$HERE/calculator.json" | tail -3

echo "==> compile wasm-lib reactor (skill: wasm-lib.sh)"
"$SKILL/wasm-lib.sh" "$HERE/calculator.json" \
  --export "$EXPORTS" \
  -o "$HERE/web/public/calculator.wasm"

echo "done -> web/public/calculator.wasm"
