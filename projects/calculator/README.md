# Aury Calculator → wasm → Vite

A full calculator written in **Aury**, compiled to a **wasm32-wasi reactor**
module, and driven by a **Vite** frontend. Every arithmetic operation the UI
performs is executed inside the wasm module — the JavaScript only marshals
values and renders.

```
calculator.json ──▶ (aury loop) ──▶ calculator.repaired.aury
   (authored JSON AST)                     │
                                           ▼  aury wasm-lib --export …
                             web/public/calculator.wasm
                                           │  fetch + instantiate
                                           ▼
                                 web/  (Vite frontend)
```

## What's in the Aury program

`calculator.json` is the authoring surface — the skill's typed-object JSON AST,
edited directly (not generated). `aury loop` auto-applies any repairs to it.
Most iterative functions are expressed as **recursion** with `*_iter` /
`*_check` accumulator helpers (internal, not exported). `factorial` is the
exception: as of Track 2 (mutable loops) it is written with a mutable
accumulator and a `loop` / `break` — no helper — demonstrating the new form
while keeping the exact same exported `(i64) -> i64` behavior.

| Kind | Functions |
|------|-----------|
| Binary `(i64,i64)->i64` | `add` `subtract` `multiply` `divide` `modulo` `percent` `average` `maximum` `minimum` `gcd` `lcm` `power` |
| Unary `(i64)->i64` | `negate` `absolute` `square` `increment` `decrement` `double` `factorial` `fibonacci` `isqrt` |
| Predicate `(i64)->bool` | `is_even` `is_prime` |

`divide`/`modulo` are guarded against divide-by-zero (return `0`) so the module
never traps. `factorial`/`fibonacci`/`power` are bounded to stay within `i64`
range (and, for the recursion-based ones, to keep recursion shallow).

The `spec` block carries **8 properties** (commutativity, add/sub inverse,
negate involution, gcd divides both, …) that Aury property-tests during
`aury loop`. The program is accepted in **0 patches** and the native backend is
asserted to agree with the interpreter.

## The wasm boundary

Every export is scalar, so it crosses as a wasm `i64` — a JS **BigInt** — with
no linear-memory marshaling. The program is reachable from `i64`-only, so the
module links with **zero imports**: the browser needs no WASI shim and
instantiates it from a plain `ArrayBuffer`.

Exports are named `aury__<fn>` (plus `_initialize`). `web/src/calculator.js`
resolves each one and wraps the BigInt marshaling.

## Edit and rebuild

`calculator.json` is the source of truth — edit it directly. To validate,
property-test, and recompile the wasm:

```bash
./build-wasm.sh        # calculator.json -> validate/property-test -> web/public/calculator.wasm
```

`build-wasm.sh` is a thin wrapper: it declares the export list and output path,
then calls the aury skill's `dev.sh` (author/repair loop) and `wasm-lib.sh`
(reactor builder). All the reusable work — ingest, the repair loop, and
`wasm32-wasi` toolchain detection — lives in the skill. To iterate on the AST
alone, run the loop directly:

```bash
./.claude/skills/aury/dev.sh projects/calculator/calculator.json factorial 10
```

The wasm build needs a `wasm32-wasi` toolchain (clang w/ the wasm target, a
wasi-libc sysroot, `wasm-ld`); the skill's bundled `wasm-toolchain.sh` locates
it (wasi-sdk or Homebrew llvm+lld+wasi-libc).

## Run the frontend

```bash
cd web
npm install
npm run dev        # http://localhost:5173  (or `npm run build` for a static dist/)
```

Vite serves `web/public/calculator.wasm` at `/calculator.wasm`. The page loads
it, calls `_initialize`, and every keypad/function button invokes an
`aury__*` export.
