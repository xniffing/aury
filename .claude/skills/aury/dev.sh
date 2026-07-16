#!/usr/bin/env bash
# dev.sh — the agent-facing Aury authoring/repair harness.
#
#   dev.sh <program.json|program.aury> [entry-fn arg...]
#
# Runs the closed loop an AI uses to author and correct a program:
#   ingest (--force)  ->  aury loop (auto-repair + property tests)
#   -> aury run  (interpreter)
#   -> aury compile  (native, when clang is present)
#   -> aury wasm     (wasm32-wasi, when a wasm runtime is present)
#   for a given entry fn, all asserted to agree.
#
# Each stage prints under a `=== STAGE ===` banner. The last line is a single
# machine-readable status the agent parses:
#
#   AURY_RESULT {"status":"accepted","patches_applied":N,"entry":"fn","run":"..",
#                "native":"..","native_matches":true,"wasm":"..","wasm_matches":true}
#   AURY_RESULT {"status":"rejected", ...}   # see the rejection JSON printed above
#   AURY_RESULT {"status":"error","message":".."}
#
# native/wasm fields appear only when their backend toolchain is available.
#
# See AURY-FOR-AGENTS.md (same directory) for the language + rejection/repair schema.

set -uo pipefail

banner() { printf '\n=== %s ===\n' "$1"; }
emit()   { printf '\nAURY_RESULT %s\n' "$1"; }
jstr()   { printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'; }

REPO_ROOT="$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel 2>/dev/null)"
[ -n "$REPO_ROOT" ] || { emit '{"status":"error","message":"not inside the aury git repo"}'; exit 1; }
cd "$REPO_ROOT" || exit 1

if [ $# -lt 1 ]; then
  echo "usage: dev.sh <program.json|program.aury> [entry-fn arg...]" >&2
  emit '{"status":"error","message":"no program given"}'
  exit 2
fi

INPUT="$1"; shift
ENTRY="${1:-}"; [ $# -gt 0 ] && shift || true
ARGS=("$@")

[ -f "$INPUT" ] || { emit "{\"status\":\"error\",\"message\":\"no such file: $(jstr "$INPUT")\"}"; exit 1; }

# --- Build the toolchain if needed -----------------------------------------
AURY="$REPO_ROOT/target/debug/aury"
if [ ! -x "$AURY" ]; then
  banner "BUILD"
  cargo build -q || { emit '{"status":"error","message":"cargo build failed"}'; exit 1; }
fi

STEM="${INPUT%.*}"
WORK="${STEM}.repaired.aury"   # gitignored; the canonical form we repair and run

# --- Stage 1: get a canonical .aury (ingest JSON, or copy .aury) ------------
case "$INPUT" in
  *.json)
    banner "INGEST (json -> canonical .aury, --force)"
    if ! "$AURY" ingest "$INPUT" "$WORK" --force; then
      emit '{"status":"error","message":"ingest failed (malformed JSON AST)"}'
      exit 1
    fi
    ;;
  *)
    cp "$INPUT" "$WORK"
    ;;
esac

# --- Stage 2: the repair loop (auto-repair + property/contract tests) -------
banner "LOOP (validate -> repair -> re-validate -> property tests)"
LOOP_OUT="$("$AURY" loop "$WORK" 2>&1)"
LOOP_CODE=$?
printf '%s\n' "$LOOP_OUT"

if ! printf '%s' "$LOOP_OUT" | grep -q '^=== ACCEPTED'; then
  # Not accepted: rejection JSON (with ranked repair menus) is printed above.
  PATCHES="$(printf '%s' "$LOOP_OUT" | sed -n 's/.*patches=\([0-9]*\).*/\1/p' | head -1)"
  REGEN="$(printf '%s' "$LOOP_OUT" | grep -q 'regenerate=true' && echo true || echo false)"
  emit "{\"status\":\"rejected\",\"patches_applied\":${PATCHES:-0},\"recommend_regenerate\":${REGEN}}"
  exit 1
fi

# Accepted: capture the repaired canonical source and write it back.
PATCHES="$(printf '%s' "$LOOP_OUT" | sed -n 's/=== ACCEPTED after \([0-9]*\) patches ===/\1/p' | head -1)"
printf '%s\n' "$LOOP_OUT" | sed -n '/^=== ACCEPTED/,$p' | tail -n +2 > "$WORK"

# --- Stage 3: run the entry fn (interpreter), and native if clang is present -
RUN_JSON=""
if [ -n "$ENTRY" ]; then
  banner "RUN (interpreter): $ENTRY ${ARGS[*]:-}"
  RUN_OUT="$("$AURY" run "$WORK" "$ENTRY" ${ARGS[@]+"${ARGS[@]}"} 2>&1)"
  if [ $? -ne 0 ]; then
    printf '%s\n' "$RUN_OUT"
    emit "{\"status\":\"error\",\"message\":\"accepted but run failed\",\"entry\":\"$(jstr "$ENTRY")\"}"
    exit 1
  fi
  printf '%s\n' "$RUN_OUT"
  RUN_JSON=",\"entry\":\"$(jstr "$ENTRY")\",\"run\":\"$(jstr "$RUN_OUT")\""

  if command -v clang >/dev/null 2>&1; then
    banner "COMPILE (native; must equal the interpreter result)"
    NATIVE_OUT="$("$AURY" compile "$WORK" "$ENTRY" ${ARGS[@]+"${ARGS[@]}"} 2>&1)"
    printf '%s\n' "$NATIVE_OUT"
    NATIVE_LAST="$(printf '%s' "$NATIVE_OUT" | tail -1)"
    RUN_JSON="${RUN_JSON},\"native\":\"$(jstr "$NATIVE_LAST")\""
    if [ "$NATIVE_LAST" != "$RUN_OUT" ]; then
      RUN_JSON="${RUN_JSON},\"native_matches\":false"
    else
      RUN_JSON="${RUN_JSON},\"native_matches\":true"
    fi
  fi

  # Optional wasm32-wasi parity: build+run the same entry through `aury wasm`
  # and assert it equals the interpreter. Gated on a wasm runtime being present;
  # if the wasm toolchain is incomplete the build fails and the stage is skipped
  # (never fatal) — set WASI_SDK_PATH or AURY_WASM_CLANG/WASI_SYSROOT + wasm-ld.
  if command -v wasmtime >/dev/null 2>&1 || command -v wasmer >/dev/null 2>&1; then
    banner "WASM (wasm32-wasi; must equal the interpreter result)"
    WASM_MODULE="$(mktemp -u).wasm"
    WASM_OUT="$("$AURY" wasm "$WORK" "$ENTRY" ${ARGS[@]+"${ARGS[@]}"} -o "$WASM_MODULE" 2>/dev/null)"
    WASM_CODE=$?
    rm -f "$WASM_MODULE"
    if [ $WASM_CODE -eq 0 ] && [ -n "$WASM_OUT" ]; then
      printf '%s\n' "$WASM_OUT"
      WASM_LAST="$(printf '%s' "$WASM_OUT" | tail -1)"
      RUN_JSON="${RUN_JSON},\"wasm\":\"$(jstr "$WASM_LAST")\""
      if [ "$WASM_LAST" != "$RUN_OUT" ]; then
        RUN_JSON="${RUN_JSON},\"wasm_matches\":false"
      else
        RUN_JSON="${RUN_JSON},\"wasm_matches\":true"
      fi
    else
      echo "skipped: wasm32-wasi toolchain unavailable"
    fi
  fi
fi

emit "{\"status\":\"accepted\",\"patches_applied\":${PATCHES:-0}${RUN_JSON}}"
