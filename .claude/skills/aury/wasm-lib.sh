#!/usr/bin/env bash
# wasm-lib.sh — the Aury skill's reactor builder: turn an accepted program into a
# wasm32-wasi library a host (a browser, wasmtime, …) can call.
#
#   wasm-lib.sh <program.json|program.aury> --export <fn>[,<fn>...] [-o out.wasm]
#
# It is the shipping counterpart to dev.sh: dev.sh authors/repairs/parity-checks
# a program; wasm-lib.sh emits the browser-facing artifact. If given JSON it
# ingests to a canonical .aury first (via --force), so the same file you author
# with dev.sh works here. The bundled wasm-toolchain.sh locates clang / wasi
# sysroot / wasm-ld.
#
# Exports are named `aury__<fn>` (plus `_initialize`). Scalar params/results
# (`i64`, `bool`) cross the boundary as wasm `i64`; aggregates are linear-memory
# pointers and are flagged by `aury wasm-lib`.
#
# Prints the built path on success; a one-line `WASM_LIB_ERROR <msg>` on failure.
set -uo pipefail

err() { printf 'WASM_LIB_ERROR %s\n' "$1" >&2; exit 1; }

SKILL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SKILL_DIR" rev-parse --show-toplevel 2>/dev/null)"
[ -n "$REPO_ROOT" ] || err "not inside the aury git repo"

# --- args ------------------------------------------------------------------
PROGRAM=""; EXPORTS=""; OUT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --export) EXPORTS="${2:-}"; shift 2 || err "--export needs a value" ;;
    --export=*) EXPORTS="${1#--export=}"; shift ;;
    -o) OUT="${2:-}"; shift 2 || err "-o needs a value" ;;
    -o*) OUT="${1#-o}"; shift ;;
    -*) err "unknown flag: $1" ;;
    *) [ -z "$PROGRAM" ] && PROGRAM="$1" || err "unexpected argument: $1"; shift ;;
  esac
done

[ -n "$PROGRAM" ]  || err "usage: wasm-lib.sh <program> --export <fns> [-o out.wasm]"
[ -f "$PROGRAM" ]  || err "no such file: $PROGRAM"
[ -n "$EXPORTS" ]  || err "no --export list given"
# Normalise: strip whitespace/newlines a caller may have wrapped the list with.
EXPORTS="$(printf '%s' "$EXPORTS" | tr -d '[:space:]')"

# --- aury binary (build once if missing, like dev.sh) ----------------------
AURY="$REPO_ROOT/target/debug/aury"
if [ ! -x "$AURY" ]; then
  echo "building aury…" >&2
  ( cd "$REPO_ROOT" && cargo build -q ) || err "cargo build failed"
fi

# --- get a canonical .aury (ingest JSON, or use the .aury as-is) -----------
case "$PROGRAM" in
  *.json)
    WORK="${PROGRAM%.*}.repaired.aury"
    "$AURY" ingest "$PROGRAM" "$WORK" --force >&2 || err "ingest failed (malformed JSON AST)"
    ;;
  *) WORK="$PROGRAM" ;;
esac

# --- default output path ---------------------------------------------------
[ -n "$OUT" ] || OUT="${WORK%.*}.wasm"
mkdir -p "$(dirname "$OUT")"

# --- resolve the wasm toolchain, then build --------------------------------
# shellcheck source=wasm-toolchain.sh
source "$SKILL_DIR/wasm-toolchain.sh"

"$AURY" wasm-lib "$WORK" --export "$EXPORTS" -o "$OUT" >&2 \
  || err "wasm-lib build failed (is the wasm32-wasi toolchain installed? see SKILL.md)"

printf '%s\n' "$OUT"
