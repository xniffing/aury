# Aury v0

A working prototype of the language proposed in [`aury-proposal.md`](aury-proposal.md):
**a small, strongly-typed, s-expression IR co-designed around an LLM repair
loop, with an intent-verification gate, lowering (sketch) to MLIR/LLVM.**

The interesting invention is *not* "AI writes LLVM." It is the **validated
semantic layer + repair protocol** sitting between the model and LLVM. This
repo implements that layer and the closed loop:

```
generate → validate → (reject + structured admissible repairs) → patch → re-validate → accept-or-regenerate
```

…with an **intent gate** (property tests + contracts + vacuity check) sitting
next to the **structural gate** (types / effects / regions). Both gates emit
repair signals.

## What v0 actually implements

| Part | Status |
|---|---|
| s-expression reader (one-screen, unambiguous) | ✅ `src/sexpr.rs` |
| Content-addressed Merkle node ids (SHA-256 of raw form) | ✅ `src/id.rs` |
| Typed AST + conversion from s-exprs | ✅ `src/ast.rs` |
| Explicit types + effect rows (**no inference**) | ✅ `src/types.rs` |
| Type / effect / region checker → structured rejections | ✅ `src/validate.rs` |
| **Repair protocol**: ranked, *admissible-by-construction* patches | ✅ `src/repair.rs` |
| Contracts + property tests + shrinking + vacuity check | ✅ `src/spec.rs` |
| Tree-walking interpreter (v0 execution backend) | ✅ `src/interp.rs` |
| Closed repair loop (auto-apply lowest-cost admissible patch) | ✅ `src/loop_driver.rs` |
| **Native lowering: Aury → LLVM IR → executable (via clang)** | ✅ `src/lower.rs` |

The MLIR/LLVM codegen is deliberately stubbed — per the proposal, codegen is
the part we *don't* build ourselves, and it requires LLVM installed. The
interpreter is sufficient to demonstrate the whole generate→repair→test loop.

## Build & test

```bash
cargo build
cargo test        # 3 unit + 9 integration tests, 0 warnings
```

## The headline demo: the loop closes automatically

```bash
$ aury validate examples/broken.aury      # a type error
rejected: 1 rejection(s)
{ "gate": "type", "kind": "ARG_TYPE_MISMATCH",
  "node": "6ba6f2e6...", "expected": "i64", "received": "str",
  "repairs": [
    { "id": "r1", "action": "replace_node", "with": "(lit 0)", "cost": 2, ... },
    { "id": "r2", "action": "change_param_type", ... "cost": 5, ... } ] }

$ aury loop examples/broken.aury 12345     # auto-repair + re-validate
[loop] applied repair `r1` (action=replace_node) to node 6ba6f2e6...; patch #1
[loop] accepted: type/effect/region checks pass; property tests pass
=== ACCEPTED after 1 patches ===
(module broken (fn add (params (a i64) (b i64)) (ret i64) (body (lit 0))) ...)
```

The validator proposes only **admissible** repairs — replacements it has
already checked are locally valid. The model picks from a menu of known-good
fixes; it cannot pick an invalid one. A `wrap` conversion is only offered if it
*returns the expected type* (this guard is what prevents the runaway repair
chain you'd otherwise get from, e.g., wrapping `i64.parse`, which returns
`result`, around an `i64` slot).

## Intent verification

```bash
$ aury test examples/buggy-max.aury 12345
{ "gate": "property-test", "kind": "PROPERTY_FALSIFIED",
  "path": "max-at-least-a",
  "received": "falsified for: a = 0i64, b = -2i64",   # <-- shrunk, minimal
  "repairs": [ { "action": "fix_impl_or_spec", ... } ] }
```

A correct implementation that genuinely exercises a function is **not** flagged
vacuous (`correct_impl_is_not_flagged_vacuous`). A property that doesn't
exercise any implementation is flagged vacuous
(`vacuity_check_flags_property_that_does_not_exercise_impl`). The vacuity check
is sound — no false positives on correct code — because it's structural
("does the body call any user fn?"), not a random "can it fail" probe.

## Design decisions that make it AI-friendly (and where it diverges from Rust)

- **s-expressions, not a Rust-like surface.** Unambiguous to parse; node ids
  are content-addressed hashes of the raw form, so repair patches address
  *nodes*, not source lines.
- **No inference anywhere.** Types, effects, and regions are written out. A
  wrong annotation is a *local* error with a *local* repair, not a
  constraint-propagation mystery.
- **Affine ownership + explicit regions**, with no lifetime inference / no
  variance / no elision — the things in Rust's borrow checker that are hard
  for humans *and* models.
- **Effects as capability-gated effect rows.** A pure function calling an
  effectful op is statically rejected with an `add_capability` repair.
- **No undefined behavior at the Aury layer.** Integer overflow traps unless
  explicitly wrapped; div/mod by zero traps; OOB traps. Semantics are defined
  *before* LLVM.

## AI authoring surface: JSON ingest

The proposal says the model should emit a *structured tree*, not free text it
has to hand-balance. v0 implements this: author Aury as a typed-object JSON
(`"kind"` tags, no delimiter counting), and `aury ingest` converts it to the
canonical s-expr IR, validates it, and writes `.aury`.

```
$ aury ingest examples/gcd.json examples/gcd.aury
ingested examples/gcd.json → examples/gcd.aury (validated)
$ aury run examples/gcd.aury gcd 48 36
12
```

`aury emit-json <file.aury>` converts the other way (array-form JSON), so the two
paths round-trip. The headline guarantee — proven by `json_and_sexpr_paths_
produce_identical_ir` — is that a JSON-authored program and a text-authored
program produce **byte-identical IR (identical Merkle node ids)**. The JSON
form is an authoring surface; the s-expr form stays canonical on disk.

The repair loop now also covers the **parse gate**: an unterminated list (the
exact error that hand-authoring deep s-exprs produces) is repaired by
appending the missing closing parens, bringing parse errors *inside* the
generate→validate→repair loop instead of outside it:

```
$ printf '(module m (fn fact ... (call fact (call i64.sub (ref n) 1)' | aury loop /dev/stdin 0
[loop] parse repair: appended 7 closing paren(s)
[loop] accepted: type/effect/region checks pass; property tests pass
```

## CLI

```
aury validate <file>          type/effect/region checks; print rejections as JSON
aury run <file> <fn> [args]   validate then run <fn> with i64/bool/str args
aury test <file> [seed]        validate then run property tests (shrinking + vacuity)
aury loop <file> [seed]        the closed repair loop (auto-apply admissible patches;
                              also repairs parse errors by closing unterminated lists)
aury ll <file> [out.ll]       Aury → LLVM IR text (the real native backend)
aury compile <file> <fn> [args] [-o out]  → native executable (clang -O2) and run it
aury lower <file>             MLIR lowering sketch (structural preview)
aury ingest <file.json> [out] JSON AST → canonical .aury (the AI authoring surface)
aury emit-json <file.aury>    .aury → array-form JSON (round-trip)
```

## What v0 explicitly does *not* do (honest scope)

- Real MLIR/LLVM codegen (stubbed; the interpreter is the backend).
- Self-hosting (the compiler is Rust).
- Shared regions + sync primitives, async, traits, parametric types (v1).
- General-purpose stdlib / ecosystem (not a goal — Aury targets
  AI-generated tools and small services, not replacing Rust/C++).

## Layout

```
src/sexpr.rs       canonical s-expression reader
src/id.rs          content-addressed Merkle node ids (SHA-256)
src/ast.rs         typed AST + s-expr conversion, assigning ids
src/types.rs       explicit types + effect rows (no inference)
src/repair.rs      the repair protocol: rejections + ranked admissible patches
src/validate.rs    type/effect/region checker emitting rejections
src/spec.rs        contracts, property tests, shrinking, vacuity check
src/json.rs        JSON authoring surface → canonical Sexpr (the AI interface)
src/interp.rs      tree-walking interpreter (v0 backend)
src/lower.rs       MLIR lowering sketch (swappable for real MLIR/LLVM)
src/loop_driver.rs the closed repair loop (now covers parse errors too)
examples/*.aury     calculator, broken, effects-bad, buggy-max, structs, rng-demo
tests/integration.rs  end-to-end tests of the whole loop
```