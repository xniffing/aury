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
| `unterminated.aury` | Parse-gate repair: missing parens auto-closed in 1 patch |
| `false-property.json` | A false property → `PROPERTY_FALSIFIED` with a shrunk counterexample and `recommend_regenerate` |

Try: `./.claude/skills/aury/dev.sh examples/agent/gcd.json gcd 48 36`

## Raw CLI (what dev.sh orchestrates)

`aury validate|json <f>` (structured rejections) · `aury test <f> [seed]`
(properties + shrinking + vacuity) · `aury loop <f> [seed]` (closed repair) ·
`aury ingest <f.json> [out] [--force]` · `aury run|compile <f> <fn> [args]` ·
`aury ll <f> [out.ll]` (LLVM IR). Prefer `dev.sh` — it chains the loop for you.
