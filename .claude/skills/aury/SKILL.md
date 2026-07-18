---
name: aury
description: >
  Author, validate, test, and repair Aury programs through the LLM repair loop.
  Use whenever writing or fixing an Aury program (.aury or the JSON AST authoring
  surface), reading Aury validation/type/effect/property rejections, or applying
  the ranked repair menus Aury returns. Triggers: "write an aury program",
  "fix this aury program", "the aury validator rejected", "add a property/spec",
  "why is aury rejecting", authoring an aury module/fn/struct/spec.
---

# Authoring and repairing Aury programs

Aury is co-designed with this loop: you author in a **structured JSON AST** (no
parenthesis counting), and every rejected gate — parse, type, effect, region,
contract, property — returns a **ranked, cost-annotated repair menu** plus, for
falsified specs, a **shrunk counterexample**. You correct by applying repairs
until the program is accepted, then it runs on both the interpreter and the LLVM
native backend (which must agree).

## Before you write anything

Read **`AURY-FOR-AGENTS.md`** (this directory). It is the authoritative reference
for: the JSON node vocabulary (`kind`-tagged), every type, the exact builtin
signatures, `spec`/`property`/`contract` syntax, effects/regions, and the
rejection + repair JSON schema. Do not guess builtin names or signatures — they
are checked and a wrong one just costs a repair round.

## The workflow

Author a `<name>.json` module (typed-object AST), then run the harness:

```bash
./.claude/skills/aury/dev.sh <program.json> [entry-fn arg...]
```

It ingests (`--force`), runs `aury loop` (auto-repair + property/contract tests),
and — if you pass an entry fn and the program is accepted — runs it on the
interpreter and, when `clang` is present, the native backend (asserting the two
results match). The final line is machine-readable:

```
AURY_RESULT {"status":"accepted","patches_applied":1,"entry":"gcd","run":"12","native":"12","native_matches":true}
```

`status` is `accepted`, `rejected`, or `error`. You may also pass an existing
`.aury` file instead of JSON.

## The correction protocol (when status is not `accepted`)

1. Read the rejection JSON printed above the `AURY_RESULT` line. Each carries
   `gate`, `kind`, `node` (Merkle id), `path`, `expected`, `received`, and a
   `repairs` array **already sorted by `cost`** (lowest first).
2. `aury loop` has already auto-applied every *admissible* repair. Only act on
   what remains, or when the result sets `recommend_regenerate`.
3. Prefer the lowest-cost repair with `preserves_effects` **and**
   `preserves_contracts` true. Edit that node in your JSON and re-run `dev.sh`.
4. For `PROPERTY_FALSIFIED` / contract failures, `received` is the **minimal
   counterexample** (e.g. `a = 0i64, b = 0i64`). Decide whether the
   *implementation* or the *property* is wrong and fix that one — never weaken a
   true property just to make it pass.
5. Repeat until `status` is `accepted`.

## Worked examples (in `examples/agent/`)

| File | Shows |
|------|-------|
| `gcd.json` | Clean JSON authoring: fn + property; accepts in 0 patches, runs interp≡native |
| `dice.aury` | Effects + regions: `(effects rng)` with `rng.next` inside a `region` |
| `log-scope.aury` | Scoped capability: bare `log.i64` → `CAPABILITY_NOT_IN_SCOPE`, loop wraps it in `(with (log) …)` |
| `clock-stamp.aury` | Scoped `clock.now` (deterministic tick) wrapped in `(with (clock) …)`; runs interp≡native |
| `unterminated.aury` | Parse-gate repair: missing parens auto-closed in 1 patch |
| `false-property.json` | A false property → `PROPERTY_FALSIFIED` with a shrunk counterexample and `recommend_regenerate` |
| `parse-classify.json` | `result` (`i64.from_str` + `result.is_ok`) and `match` (lit + bind patterns); 5 properties |
| `loop-factorial.json` | Mutable loop: `let` + `set` + `loop`/`break` accumulator, with `requires`/`ensures` contracts; interp≡native≡wasm |

Try: `./.claude/skills/aury/dev.sh examples/agent/gcd.json gcd 48 36`

## Raw CLI (what dev.sh orchestrates)

`aury validate|json <f>` (structured rejections) · `aury test <f> [seed]`
(properties + shrinking + vacuity) · `aury loop <f> [seed]` (closed repair) ·
`aury ingest <f.json> [out] [--force]` · `aury run|compile <f> <fn> [args]` ·
`aury ll <f> [out.ll]` (LLVM IR). Prefer `dev.sh` — it chains the loop for you.

## WebAssembly targets (same LLVM lowering)

Once a program is accepted it can also be built for `wasm32-wasi`:

- `aury wasm <f> <fn> [args...] [-o out.wasm] [--no-run]` — build an executable
  module with the args embedded and run it via `wasmtime`/`wasmer` (if present).
  stdout is the program result, identical to `aury run`/`aury compile`; the
  build banner goes to stderr. `--no-run` only builds.
- `aury wasm-lib <f> --export <fn>[,<fn>...] [-o out.wasm]` — build a **reactor**
  module that exports the named functions (as `aury__<name>`, plus
  `_initialize`) for a host to call — e.g. a browser via
  `WebAssembly.instantiate`. Scalar (`i64`/`bool`) params and results cross the
  boundary as wasm `i64` (JS `BigInt`) with no marshaling; aggregate types are
  linear-memory pointers and are flagged. Reachable-from-`i64`-only programs
  link with **zero imports**, so the host needs no WASI shim.

### `wasm-lib.sh` — the reactor builder (skill helper)

Prefer the bundled helper over calling `aury wasm-lib` by hand — it is the
shipping counterpart to `dev.sh` and handles ingest + toolchain detection:

```bash
./.claude/skills/aury/wasm-lib.sh <program.json|program.aury> \
  --export <fn>[,<fn>...] [-o out.wasm]
```

It ingests JSON to a canonical `.aury` (`--force`) if needed, sources the
bundled `wasm-toolchain.sh` to locate clang / wasi sysroot / `wasm-ld`, builds
the reactor, and prints the output path (or `WASM_LIB_ERROR <msg>`). Defaults
the output to `<stem>.wasm`. A project's own build script then only declares its
export list and output path — see `projects/calculator/build-wasm.sh`.

Both wasm commands need a `wasm32-wasi` toolchain (clang with the WebAssembly
target, a wasi-libc sysroot, `wasm-ld`). `wasm-toolchain.sh` (bundled with this
skill) resolves it from `WASI_SDK_PATH` / `AURY_WASM_CLANG` / `WASI_SYSROOT`, a
wasi-sdk install, or a Homebrew `llvm`+`lld`+`wasi-libc` assembly; `dev.sh` and
`wasm-lib.sh` source it automatically. See `projects/calculator/` for a program
compiled with `wasm-lib` and run in the browser. Authoring/repair never require
any of this — the wasm targets are backends only.
