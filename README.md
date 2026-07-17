# Aury

### A repair-oriented intermediate language for reliable AI-generated programs

> **Research prototype · v0 · Rust implementation · LLVM native backend**

Aury is a small, strongly typed, s-expression intermediate language designed
around a closed LLM repair loop. Its central claim is not that language models
can emit low-level code. The claim is that a language, validator, repair
protocol, and intent-verification harness can be **co-designed** so that model
errors become bounded, structured, and mechanically repairable.

The system implements the following acceptance loop:

```text
intent
  ↓
structured generation
  ↓
parse → type/effect/region validation → intent verification
  ↓ accepted                                      ↓ rejected
interpreter / LLVM backend       ranked admissible repairs + counterexamples
  ↑                                                   ↓
  └──────────────── patch or regenerate ──────────────┘
```

The companion document [`aury-proposal.md`](aury-proposal.md) presents the
broader research programme. This README describes the **implemented prototype**,
its thesis, architecture, evidence, and limitations.

---

## Contents

1. [Abstract](#abstract)
2. [Research thesis](#research-thesis)
3. [Contributions](#implemented-contributions)
4. [System architecture](#system-architecture)
5. [Language and semantics](#language-and-semantics)
6. [Structural validation](#structural-validation)
7. [Repair protocol](#repair-protocol)
8. [Intent verification](#intent-verification)
9. [AI authoring surface](#ai-authoring-surface)
10. [Interpreter and native backend](#interpreter-and-native-backend)
11. [Case study: calculator in the browser](#case-study-calculator-in-the-browser)
12. [Build and use](#build-and-use)
13. [Evaluation](#evaluation-and-evidence)
14. [Scope and limitations](#scope-limitations-and-threats-to-validity)
15. [Repository map](#repository-map)

---

## Abstract

General-purpose compilers are optimized to help humans author programs. They
accept flexible surfaces, infer omitted information, and explain failures in
natural language. An LLM-based coding loop has a different failure profile: it
benefits from canonical structure, explicit semantics, stable node identity,
and a finite set of valid next actions.

Aury explores that alternative design point. Programs are represented as a
small typed IR. Every expression has a content-derived identity. Types,
effects, and regions are explicit. Validation failures are emitted as
structured rejection objects containing ranked repairs that the validator has
already determined to be locally admissible. Separately, generated properties
exercise the implementation and return shrunk counterexamples when behavior
violates the stated intent.

Accepted programs can run in a tree-walking interpreter or lower through a
static, type-aware LLVM backend. Differential tests require both backends to
produce the same observable value for scalar and aggregate programs. The
prototype therefore studies reliability at the point of acceptance rather
than first-shot generation accuracy.

---

## Research thesis

The useful unit of design is not the language alone. It is the combined system:

```text
language + validator + repair protocol + intent gate + execution backends
```

Aury investigates four questions:

- **RQ1 — Repairability:** Can a language constrain common generation failures
  to local, machine-addressable rejections with a small set of valid repairs?
- **RQ2 — Intent:** Can executable, non-vacuous properties reduce the gap
  between “well typed” and “does what was requested”?
- **RQ3 — Semantic stability:** Can interpreter/native differential testing
  preserve language semantics across lowering to LLVM?
- **RQ4 — Authoring:** Can a structured JSON generation surface eliminate
  delimiter and grammar failures while preserving one canonical IR?

The prototype does not yet provide a statistical answer to these questions.
It provides an executable artifact in which the mechanisms can be tested and
measured.

### Non-goals

Aury is not intended to replace Rust, C++, Python, or their ecosystems. It is
not currently self-hosted, does not attempt general-purpose application
development, and does not claim that generated specifications perfectly encode
human intent. The target is a compact experimental substrate for AI-generated
algorithms, transformations, and small tools.

---

## Implemented contributions

| Component | Prototype status |
|---|---|
| Canonical s-expression reader | Implemented |
| Typed-object JSON authoring surface | Implemented |
| Content-addressed SHA-256 node IDs | Implemented |
| Explicit type/effect/region checker | Implemented |
| Structured rejection objects | Implemented |
| Ranked repair candidates | Implemented for core validation failures |
| Closed repair loop with budgets | Implemented |
| Seeded property testing | Implemented |
| Counterexample shrinking | Implemented |
| Structural vacuity detection | Implemented |
| Executable function contracts (`requires`/`ensures`) | Implemented (runtime + intent gate) |
| Mutable loops (`set` / `loop` / `break`) | Implemented (interp + native parity) |
| Tree-walking interpreter | Implemented |
| Static, type-aware LLVM lowering | Implemented |
| Native vectors, structs, results, strings, and RNG | Implemented |
| Interpreter/native differential tests | Implemented |
| Full arena lifetime semantics | Deferred; v0 `region`/`copy` are explicit no-ops |
| SMT contract discharge | Not implemented |
| Real MLIR dialect pipeline | Not implemented; v0 emits LLVM IR directly |
| General FFI and OS capability surface | Not implemented |

This distinction matters: [`aury-proposal.md`](aury-proposal.md) is a design
proposal; the table above is the honest implementation boundary.

---

## System architecture

### Compilation and execution pipeline

```text
Typed-object JSON ──┐
                    ├──> canonical Sexpr ──> typed AST + Merkle IDs
Aury s-expression ──┘                           │
                                                ├──> structural validator
                                                │      ├─ accepted
                                                │      └─ rejection + repairs
                                                │
                                                ├──> property-test harness
                                                │      ├─ accepted
                                                │      └─ shrunk counterexample
                                                │
                                                ├──> interpreter
                                                │
                                                └──> typed LLVM IR
                                                       + generated entry wrapper
                                                       + embedded C runtime
                                                       └──> clang ──> executable
```

### Acceptance protocol

1. **Generate** a typed-object JSON tree or canonical Aury expression.
2. **Parse** into one canonical s-expression representation.
3. **Validate** types, effects, affine use, regions, calls, and control flow.
4. **Repair** rejected nodes using ranked structured candidates, or regenerate
   when the local repair budget is exhausted.
5. **Verify intent** by running seeded properties and shrinking failures.
6. **Execute** only after the structural and intent gates accept the module.
7. **Compare backends** where native execution is requested.

The separation between structural and intent gates is deliberate. Structural
validation can prove that a program is internally coherent; it cannot prove
that the specification matches what a user meant.

---

## Language and semantics

Aury is intentionally small and explicit. There is no type inference and no
operator precedence to recover.

### Types

```text
i64
f64
bool
str
unit
(vec T)
(struct Name)
(result OkT ErrT)
(ref region mut|ref T)
region
```

### Core forms

```scheme
(lit 42)
(ref name)
(let name i64 (lit 1) body)
(call i64.add left right)
(if condition (then value) (else value))
(match value (pattern arm) ...)
(block statement ... tail)
(loop body)
(break value)
(set name value)
(return value)
(vec-new (vec i64) ...)
(idx vector index)
(len vector)
(new-struct Name (field value) ...)
(get struct-value field)
(region r body)
(copy value)
(cast str value)
```

### Builtins

- **Integer:** `add`, `sub`, `mul`, `div`, `mod`, comparisons, `neg`, `abs`,
  `to_str`, and `parse`/`from_str`
- **Float:** `f64.add`, `sub`, `mul`, `div`, comparisons, `neg`, `abs`, and
  `to_str`; `i64`↔`f64` and `f64`→`str` via `cast`
- **Boolean:** `and`, `or`, `not`, and equality
- **String:** equality, inequality, concatenation, and length
- **Result:** `is_ok`
- **RNG:** deterministic `rng.next`, gated by the `rng` effect

### Defined edge behavior

Aury semantics are defined before LLVM lowering:

- integer addition, subtraction, multiplication, negation, and absolute value
  wrap in two’s-complement arithmetic;
- `i64::MIN / -1` and the corresponding remainder have defined behavior;
- integer division or remainder by zero traps;
- `f64` arithmetic is IEEE-754 and **never traps**: `f64.div` by zero yields
  `±inf` (or `NaN` for `0.0/0.0`), every ordered comparison with `NaN` is false,
  and `f64.neq` with `NaN` is true; floats render in a canonical
  17-significant-digit scientific form (`1.5` → `1.5000000000000000e+00`,
  plus `NaN`/`inf`/`-inf`) that is byte-identical across interpreter, native,
  and wasm — `f64`→`i64` casts truncate toward zero and saturate (`NaN`→0);
- vector indices are bounds checked and trap on negative or excessive indices;
- RNG uses seeded SplitMix64, so identical seed and execution order produce
  identical values;
- immutable aggregate display is deterministic and field-ordered.

These rules prevent LLVM undefined behavior from silently becoming Aury
language behavior.

### Mutable loops

Iteration is expressed with a mutable accumulator rather than only recursion. A
`let` binding is reassigned with `(set name value)`, and `(break value)` exits
the nearest enclosing `loop`, making the loop expression evaluate to that value
(a loop with no reachable `break` diverges, as before). Iterative factorial:

```scheme
(fn factorial (params (n i64)) (ret i64)
  (body
    (let acc i64 1
      (let i i64 1
        (loop
          (if (call i64.gt (ref i) (ref n))
              (break (ref acc))
              (block
                (set acc (call i64.mul (ref acc) (ref i)))
                (set i   (call i64.add (ref i) (lit 1)))
                unit)))))))
```

Each construct is also a repair opportunity, checked with no inference:

- `set` of a parameter is rejected (`SET_OF_PARAM`) — parameters are values;
  reassign a `let`-bound local instead;
- `set` of an unbound name (`SET_UNBOUND`) or a value whose type disagrees with
  the binding (`SET_TYPE_MISMATCH`);
- `break` outside any loop (`BREAK_OUTSIDE_LOOP`), or two `break`s in one loop
  whose value types disagree (`BREAK_TYPE_MISMATCH`).

The interpreter defines the semantics and the native and wasm backends must
match it observably; an iterative accumulator runs identically across all three
(differential parity is a test failure, not a warning). The calculator example's
`factorial` is written this way as a demonstration.

---

## Structural validation

The validator operates over the explicit AST and emits machine-readable
[`Rejection`](src/repair.rs) values rather than prose-only diagnostics.
Implemented checks include:

- function call arity and argument types;
- declared and explicit return types;
- branch and match-arm agreement with divergence-aware control flow;
- duplicate functions, structs, and fields;
- vector element, index, and field-access types;
- effect-row containment, including `rng` use from pure functions;
- affine move tracking and explicit copy checks;
- region names and region scope;
- local binding scope and nested shadow restoration.

A rejection identifies the gate, rejection kind, Merkle node, structural path,
expected and received values, context, and candidate repairs.

```json
{
  "gate": "type",
  "kind": "ARG_TYPE_MISMATCH",
  "node": "content-derived-node-id",
  "expected": "i64",
  "received": "str",
  "repairs": [
    {
      "id": "r1",
      "action": "replace_node",
      "cost": 2,
      "preserves_effects": true
    }
  ]
}
```

Merkle IDs make repairs node-oriented rather than line-oriented. The same
canonical form produces the same IDs, allowing a model or tool to address a
semantic node without reconstructing a textual diff.

---

## Repair protocol

Aury’s differentiating mechanism is that the compiler participates in repair
selection.

### Admissibility

A repair is offered only when the validator knows the replacement is locally
valid. For example, a conversion is not suggested merely because its name
sounds plausible; its output type must agree with the rejected position.
This prevents repair chains in which one guessed conversion creates a larger
mismatch elsewhere.

### Ranking

Candidates carry a cost and preservation metadata. A local literal replacement
is cheaper than changing a public parameter type and propagating that change
to every caller. The loop can therefore prefer small, semantics-preserving
changes while still exposing broader regeneration choices.

### Termination

The repair driver tracks attempts and applies finite budgets. If a repair does
not close the rejection, the system escalates to another candidate or asks for
regeneration. “Keep asking the model until the compiler stops complaining” is
not considered a termination strategy.

### Parse repair

The parse gate is inside the loop. Unterminated s-expression lists can be
repaired mechanically by appending the known number of missing delimiters.
For normal AI authoring, the typed-object JSON surface avoids this class of
failure entirely.

---

## Intent verification

Structural validity is necessary but insufficient. Aury modules may include
properties generated alongside the implementation:

```scheme
(spec
  (property add-commutes
    (forall ((a i64) (b i64))
      (call i64.eq
        (call add (ref a) (ref b))
        (call add (ref b) (ref a))))))
```

The v0 harness provides:

1. **Seeded generation** for integers, booleans, strings, and vectors.
2. **Property execution** in the interpreter.
3. **Failure shrinking** to produce smaller counterexamples.
4. **Structural vacuity detection:** a property that calls no user-defined
   function is rejected because it does not exercise an implementation.
5. **Structured property failures** that re-enter the repair workflow.

The vacuity check is intentionally conservative and structural. A property
that exercises a function and always passes may be a correct invariant; random
attempts to make it fail cannot distinguish correctness from vacuity.

### Function contracts

Functions may also carry executable **preconditions and postconditions** as
`requires`/`ensures` clauses between the effect row and the body. `ensures`
binds the reserved name `result` to the return value:

```scheme
(fn abs (params (x i64)) (ret i64)
  (ensures (call i64.ge (ref result) (lit 0)))
  (body (if (call i64.lt (ref x) (lit 0)) (call i64.neg (ref x)) (ref x))))
```

Contracts are enforced two ways:

1. **Runtime enforcement** — the interpreter checks `requires` on entry and
   `ensures` on exit; a violation traps like any other runtime error, so it
   surfaces on every concrete execution (`aury run`).
2. **Intent gate** — `aury test`/`aury loop` actively generate inputs, keep
   those satisfying the preconditions, run the function, and report any
   postcondition violation as a shrunk, in-domain counterexample that re-enters
   the repair loop (gate `contract`, kind `POSTCONDITION_FALSIFIED`).

The type gate additionally requires each clause to be a pure `bool` predicate
(`CONTRACT_NOT_BOOL` / `CONTRACT_IMPURE`), and a postcondition that never
references `result` is flagged `VACUOUS_CONTRACT`. `result` is in scope only for
`ensures`, so using it in a `requires` clause is an ordinary unbound reference.
Implication checking and SMT discharge remain future work; v0.1 contracts are
executable runtime assertions, not proofs.

### Intent boundary

Passing properties proves only that the implementation satisfies those
properties for the tested inputs. It does not prove that the properties encode
the user’s full intent. Wrong specifications remain possible. Aury makes the
specification executable, inspectable, reproducible, and harder to make
trivially meaningless; the user remains the final oracle.

Executable contracts close part of this gap by making the specification run on
every execution, but a passing contract is still only evidence over the tested
and concretely-executed inputs, not a proof. Implication checking and SMT
discharge remain future work.

---

## AI authoring surface

Although the canonical stored language is an s-expression IR, the recommended
model-facing surface is typed-object JSON:

```json
{
  "kind": "call",
  "op": "i64.add",
  "args": [
    { "kind": "ref", "name": "a" },
    { "kind": "lit", "value": 1 }
  ]
}
```

Every node has an explicit `kind`; calls, bindings, branches, and types do not
depend on delimiter balancing or precedence. `aury ingest` converts this tree
to the canonical s-expression path and then runs the same AST builder and
validator used by textual Aury.

```bash
target/release/aury ingest input.json output.aury
target/release/aury emit-json output.aury
```

The JSON and s-expression paths are tested to produce identical canonical IR
and identical node IDs. JSON is therefore an authoring interface, not a second
language with different semantics.

---

## Interpreter and native backend

### Interpreter

[`src/interp.rs`](src/interp.rs) is the executable semantic reference for v0.
It evaluates typed values, structured control flow, vectors, structs, results,
strings, region/copy pass-through behavior, and deterministic RNG.

### LLVM lowering

[`src/lower.rs`](src/lower.rs) emits typed LLVM IR for the reachable call graph
of a selected entry function. It does not lower every function in the module,
which keeps unsupported or irrelevant code outside a native build.

| Aury value | Native representation |
|---|---|
| `i64`, `bool`, `unit` | LLVM `i64` |
| `str` | boxed pointer containing length and byte data |
| `(vec T)` | boxed `{ i64 len, ptr slots }` |
| struct | boxed contiguous 8-byte slots in declaration order |
| result | boxed tag and payload slots |

Pointer-valued aggregate members are converted to and from 64-bit slot bits.
Type descriptors allow the runtime to print arbitrarily nested vectors,
structs, and results in exactly the interpreter’s display format.

The native runtime in [`runtime/aury_rt.c`](runtime/aury_rt.c) provides:

- boxed string and aggregate allocation;
- checked vector access;
- string operations and integer parsing;
- deterministic SplitMix64;
- edge-defined integer division and remainder;
- recursive descriptor-driven value printing.

Its source is embedded in the CLI binary with `include_str!`, so an installed
`aury` executable does not depend on the source checkout when invoking clang.

### Native entry values

Scalar CLI syntax remains direct:

```text
42  true  hello
```

Composite values are type-directed JSON:

```text
[1, 2, 3]
{"name":"sample","values":[1,2]}
{"ok":[1,2]}
{"err":"message"}
```

Unknown fields, missing fields, invalid result shapes, malformed JSON, and
duplicate object keys are rejected before lowering.

### Differential parity

Native correctness is treated as equivalence with the interpreter, not merely
successful compilation. Integration tests compare both backends across:

- strings and Unicode-sensitive parsing;
- vectors, nested vectors, and bounds failures;
- structs and nested aggregate fields;
- both result variants;
- typed branches and nested returns;
- deterministic RNG sequences;
- integer overflow and `MIN / -1` edges;
- generic aggregate output formatting.

---

## Case study: calculator in the browser

[`projects/calculator/`](projects/calculator/) is an end-to-end Aury program
compiled to a WebAssembly library and driven by a Vite frontend.

The Aury core is a full calculator written in pure `i64`/`bool` arithmetic — 23
public functions across binary ops (`add`, `divide`, `gcd`, `lcm`, `power`, …),
unary ops (`square`, `isqrt`, `factorial`, `fibonacci`, …), and predicates
(`is_even`, `is_prime`). Aury v0 has no mutable loops, so every iterative
function is expressed as recursion. A `spec` block carries eight properties
(commutativity, add/sub inverse, negate involution, gcd-divides-both, …) that
are property-tested during `aury loop`; the program is accepted in **0 patches**
and the native backend is asserted to match the interpreter.

```bash
./projects/calculator/build-wasm.sh   # JSON AST → validate/property-test → wasm-lib
```

The `build-wasm.sh` pipeline runs the repair loop over the hand-authored JSON AST
and builds a `wasm32-wasi` **reactor** module exporting `aury__<fn>`. Each
export is scalar, so it crosses the JS boundary as a `BigInt` with no
marshaling, and the module links with **zero imports** — the browser needs no
WASI shim and instantiates it straight from an `ArrayBuffer`.

```bash
cd projects/calculator/web
npm install
npm run dev        # http://localhost:5173
```

The Vite frontend serves the module at `/calculator.wasm`; every keypad and
function button invokes an `aury__*` export directly in the browser.

See [`projects/calculator/README.md`](projects/calculator/README.md) for the
full pipeline and details.

---

## Build and use

### Requirements

- Rust toolchain with Cargo
- `clang` for native compilation
- Node.js only for the optional calculator frontend (Vite)

### Build

```bash
cargo build --release
cargo test
```

The binary is written to `target/release/aury`.

### Quick start

```bash
AURY=target/release/aury

$AURY validate projects/calculator/calculator.repaired.aury
$AURY test projects/calculator/calculator.repaired.aury 12345
$AURY run projects/calculator/calculator.repaired.aury factorial 10
$AURY compile projects/calculator/calculator.repaired.aury gcd 48 36 -o /tmp/aury-gcd
```

### CLI

```text
aury validate <file>             run type/effect/region validation
aury json <file>                 emit one JSON rejection per line
aury run <file> <fn> [args...]  execute with the interpreter
aury test <file> [seed]          run seeded properties + contracts and shrinking
aury loop <file> [seed]          run the closed repair loop
aury lower <file>                print the structural MLIR sketch
aury ll <file> [out.ll]          emit validated LLVM IR
aury compile <file> <fn> [args...] [-o out]
                                 lower, compile with clang, execute, and print
aury wasm <file> <fn> [args...] [-o out.wasm] [--no-run]
                                 lower, build a wasm32-wasi module, run it
aury wasm-lib <file> --export <fn>[,<fn>...] [-o out.wasm]
                                 build a reusable wasm32-wasi reactor module
                                 exporting the named functions (for a browser)
aury ingest <file.json> [out]    typed-object/array JSON → canonical Aury
aury emit-json <file.aury>       canonical Aury → array-form JSON
```

`aury compile` currently generates a native `main` with the supplied entry
arguments embedded in the LLVM module, compiles it, and immediately runs it.
This is suitable for differential testing and small tools; a reusable dynamic
native library ABI is future work.

### WebAssembly (wasm32-wasi)

`aury wasm` reuses the same LLVM lowering, retargeting clang at `wasm32-wasi`
and linking the C runtime against wasi-libc. The generated entry is named
`__main_void` (raw IR bypasses clang's C frontend, so wasi-libc's `_start`
finds the entry only under its own symbol). If a wasm runtime is on `PATH`
(`wasmtime` or `wasmer`), the module is executed and its result printed — it
must match `aury run` and `aury compile`; pass `--no-run` to only build.

`aury wasm-lib` instead builds a **reactor** module (`-mexec-model=reactor`, no
`main`) that exports the named functions under the symbol `aury__<name>`, plus
`_initialize`. A host — a browser via `WebAssembly.instantiate`, or wasmtime —
calls them directly. Functions whose parameters and result are scalars (`i64`,
`bool`) cross the boundary as wasm `i64` with no marshaling; aggregate types are
linear-memory pointers and are flagged. The
[calculator example](projects/calculator/README.md) compiles its functions this
way and runs them in the browser. Both wasm commands share the same toolchain:

It needs a clang with the WebAssembly target plus a wasi-libc sysroot,
`wasm-ld`, and the wasm32 `compiler-rt` builtins. The self-contained
[wasi-sdk](https://github.com/WebAssembly/wasi-sdk) provides all of these and is
auto-detected via `WASI_SDK_PATH` (or `/opt/wasi-sdk`). To assemble the
toolchain from Homebrew instead:

```bash
brew install llvm lld wasi-libc wasmtime      # clang+wasm target, wasm-ld, sysroot, runtime
export AURY_WASM_CLANG="$(brew --prefix llvm)/bin/clang"
export WASI_SYSROOT="$(brew --prefix wasi-libc)/share/wasi-sysroot"
export PATH="$(brew --prefix lld)/bin:$PATH"   # so clang finds wasm-ld
```

Homebrew's `llvm` omits the wasm32 `compiler-rt` builtins; fetch the prebuilt
`libclang_rt.builtins-wasm32.a` from a wasi-sdk release and place it where clang
expects it (`$(brew --prefix llvm)/lib/clang/<ver>/lib/wasm32-unknown-wasip1/libclang_rt.builtins.a`),
or just use wasi-sdk.

---

## Evaluation and evidence

The present evaluation is regression-oriented rather than a published language
benchmark.

### Automated evidence

`cargo test` covers:

- parser stability and content-addressed IDs;
- JSON/s-expression round trips;
- validator rejection kinds and repair ranking;
- effect violations and repair-loop closure;
- property failures, shrinking, and vacuity detection;
- interpreter behavior;
- clang-backed native parity;
- malformed aggregate CLI input;
- recursive descriptor rejection;
- duplicate definition rejection;
- output-path and embedded-runtime behavior.

The C runtime is also compiled independently with strict warning settings
during development.

### Acceptance criterion

For supported native programs, acceptance requires more than producing valid
LLVM. The observable native result must equal the interpreter result. This
turns the interpreter into an executable semantics and makes backend drift a
test failure.

### Repair convergence corpus

`aury eval eval/corpus.json` runs the same closed loop an agent uses over a
corpus of `(intent, program)` tasks and reports, per task and in aggregate:
whether the initial program passed type **and** intent checks with zero repairs
(*first-shot*), whether the loop reached an accepted state and after how many
mechanical patches, and whether concrete reference-oracle checks confirm correct
behavior. Tasks whose spec is deliberately wrong assert the opposite outcome —
the loop must *refuse* to accept. The run is deterministic (fixed seed) and is a
`cargo test` gate (`evaluation_corpus_converges_as_expected`).

The honest baseline this answers is **first-shot vs post-repair** — how many
tasks the loop rescues that would otherwise be rejected — over the v0.2 language
range (contracts, mutable loops, `f64`, effects, growable vecs, regions):

| Task | First-shot | Loop | Patches | Oracle |
|------|:----------:|:----:|:-------:|:------:|
| gcd | ✓ | ✓ | 0 | 2/2 |
| loop-factorial | ✓ | ✓ | 0 | 2/2 |
| mean-f64 | ✓ | ✓ | 0 | 1/1 |
| parse-classify | ✓ | ✓ | 0 | 2/2 |
| dice-effect | ✓ | ✓ | 0 | — |
| effect-leak | effect✗ | ✓ | 1 | 2/2 |
| vec-pipeline | ✓ | ✓ | 0 | 2/2 |
| alias-region | region✗ | ✓ | 1 | 1/1 |
| vec-use-after-move | region✗ | ✓ | 1 | 2/2 |
| calculator | ✓ | ✓ | 0 | 3/3 |
| unterminated | parse✗ | ✓ | 1 | 1/1 |
| false-property | intent✗ | ø (rejected) | 0 | — |

**12/12 outcomes as expected** · 8 first-shot-valid · 4 rescued by repair · 1
deliberately-wrong spec correctly rejected by the intent gate · 18/18 oracle
checks. Regenerate the table and a CSV with
`aury eval eval/corpus.json --md eval/report.md --csv eval/report.csv`.

**First-shot failures by gate — and which the loop mechanically converges.** The
v0.2 loop closes *structural* gates, not just parse: an under-declared effect row
is widened, a use-after-move gets a copy, and an aliasing conflict is split into
disjoint regions — all applied mechanically and re-validated.

| Gate | first-shot fails | converged | rejected✓ |
|------|:----------------:|:---------:|:---------:|
| parse | 1 | 1 | 0 |
| effect | 1 | 1 | 0 |
| region | 2 | 2 | 0 |
| intent | 1 | 0 | 1 |

The corpus is intentionally small and its programs are curated (so most pass
first-shot); it demonstrates the loop's mechanics and per-gate convergence end to
end, not a large-sample success rate.

Tasks may also carry an independent **reference implementation** (a Python
script under `eval/baseline/`) run against the same oracle inputs. This measures
cross-implementation *agreement* — whether a hand-written program in another
language computes the same outputs (currently 5/5 across `gcd` and `calculator`)
— and is skipped hermetically when no interpreter is present. It is **not** a
generation-reliability baseline.

### What has not yet been measured

- first-shot generation success against Python, Rust, or a baseline IR (the
  corpus above measures repair convergence *within* Aury — including a per-gate
  convergence breakdown — and cross-implementation *agreement* against a
  reference impl, but not a cross-language *generation* comparison, which
  requires model-generated programs in another language on a matched task set);
- repair-loop convergence at scale over a large, uncurated intent corpus;
- semantic preservation under large-program optimization;
- user comprehension of generated properties;
- performance relative to handwritten implementations;
- effectiveness of admissible repairs versus ordinary compiler diagnostics.

These measurements are necessary before making empirical claims about Aury’s
advantage for model-generated software.

---

## Scope, limitations, and threats to validity

### Region semantics

The validator checks region **aliasing**: a `mut` reference is exclusive to its
region, so sharing a region with any other reference is a pointwise
`ALIAS_CONFLICT` with a mechanical `split_region` repair. Affine move-tracking is
also enforced (`vec-push` consumes its target; a later use is `USE_AFTER_MOVE`,
repaired by inserting a copy).

Regions are a **real arena** when the native backend can prove them escape-free —
a scalar result and no `set`/`return`/`break` in the body — in which case every
allocation made inside is bulk-freed at region exit (verified by allocation
accounting; the observable result is unchanged, so interpreter, native, and wasm
still agree). Regions that could let an aggregate escape (an aggregate result, or
control flow leaving the region) conservatively fall back to process-lifetime
allocation. Unconditional freeing with a static `REGION_ESCAPE` check, and
result-relocation for escaping aggregates, are not yet implemented; `copy`
remains an immutable-value pass-through.

### Contracts and proof

Function contracts (`requires`/`ensures`) are executable: they are enforced as
runtime assertions and actively exercised by the intent gate. Implication
checking and SMT proof are not implemented. Both property testing and contract
testing are evidence over sampled (and concretely executed) inputs, not formal
verification.

### Effects and capabilities

Effect rows and deterministic RNG gating are implemented. The proposal’s
broader first-class `fs`, `net`, `clock`, `state`, and synchronization
capability system is not. Extern execution and audited OS shims are deferred.

### Numerical and platform assumptions

The aggregate ABI uses 8-byte slots and pointer-to-`i64` conversion, targeting
64-bit platforms. The backend invokes `clang` and currently assumes an LLVM IR
version accepted by the installed toolchain.

### Recursive aggregates

Runtime value descriptors are finite strings. Recursive struct entry types are
therefore rejected clearly rather than generating an infinite descriptor.

### Specification risk

A property can be non-vacuous and still be incomplete or wrong. Structural
vacuity detection prevents properties that exercise no implementation; it does
not establish correspondence with natural-language intent.

### Training-data risk

Aury has little direct pretraining representation compared with mainstream
languages. Structured JSON authoring and repair menus reduce generation
freedom, but a broader corpus and controlled comparison are still required.

---

## Repository map

```text
Cargo.toml                 Rust package and CLI definition
aury-proposal.md           broader research proposal
runtime/aury_rt.c          native allocation, arithmetic, RNG, and printing

src/sexpr.rs               canonical s-expression parser
src/json.rs                model-facing JSON authoring conversion
src/id.rs                  content-addressed node IDs
src/ast.rs                 typed AST construction
src/types.rs               explicit types and effect rows
src/validate.rs            structural validation and rejection generation
src/repair.rs              rejection and repair data model
src/loop_driver.rs         bounded repair-loop driver
src/spec.rs                property generation, execution, vacuity, shrinking
src/interp.rs              reference interpreter
src/value_io.rs            typed CLI JSON parsing and deterministic display
src/lower.rs               static LLVM lowering and native entry generation
src/lower_sketch.rs        structural MLIR preview
src/eval.rs                evaluation harness (repair convergence over a corpus)
src/main.rs                command-line interface and embedded-runtime driver

eval/corpus.json           evaluation corpus: (intent, program, oracle) tasks
tests/integration.rs       end-to-end and differential regression suite
tests/native_parity.aury   aggregate/RNG/control-flow parity fixture

.claude/skills/aury/dev.sh            author/repair loop harness
.claude/skills/aury/wasm-lib.sh       wasm32-wasi reactor builder
.claude/skills/aury/wasm-toolchain.sh wasm toolchain detection
projects/calculator/       calculator compiled to wasm-lib
projects/calculator/web/   Vite frontend calling the wasm exports
```

Generated build output, native executables, LLVM scratch files, and local agent
artifacts are excluded through [`.gitignore`](.gitignore).

---

## Project status

Aury is an experimental prototype intended for research, learning, and design
iteration. It demonstrates that a repair-oriented language can be executed end
to end—from structured generation and validation through intent checks and a
native LLVM backend—while keeping its unimplemented claims explicit.

A first evaluation corpus now exists (`aury eval eval/corpus.json`, see
[Evaluation](#evaluation-and-evidence)): it measures repair convergence and
intent-gate behavior end to end over the v0.1 language range. The next
meaningful milestone is to scale it — a larger, uncurated task set with a
first-shot success rate and a controlled cross-language baseline.
