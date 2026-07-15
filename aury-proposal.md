# Aury: A Language Co-Designed with an LLM Repair Loop

## Thesis

The useful invention is not "AI writes LLVM" and not "a language for machines
instead of humans" in the abstract. It is a specific thing:

> **A language, a validator, and a repair protocol co-designed so that every
> rejection the validator emits comes with a structured set of admissible
> mechanical repairs the model can apply, and an intent-verification gate the
> program must pass before it is accepted.**

Everything else — MLIR lowering, native codegen, structured errors — is in
service of that. If the proposal only delivers "Rust but the AI writes it,"
it has failed. The deliverable is a closed loop:

```
intent → generate → validate → (reject + structured repairs) → patch → re-validate → accept-or-regenerate
```

with an *intent* gate (specs/property tests) sitting alongside the
*structural* gate (types/effects/safety). Both gates emit repair signals.

---

## 1. Representation: s-expressions with content-addressed nodes

### Decision

The AI emits **s-expressions**. Not free-form prose, not a Rust-like surface,
not JSON. s-expressions because:

- They parse unambiguously and trivially (no indent rules, no precedence,
  no semicolon debates).
- They carry structure natively — patching happens on nodes, not lines.
- They are compact relative to JSON (no quote/key noise) and readable.
- LLMs have substantial training exposure (Clojure, Scheme, Elisp, Racket,
  ISLISP, plus s-expr-style data formats). Transfer learning works.
- They admit metadata (node IDs, source provenance, intent tags) inline.

The parser is a one-screen recursive-descent reader. It assigns every node a
**Merkle ID** — a hash of the node's tag + its children's IDs — so IDs are
stable, reproducible, and content-addressed. Repair patches reference nodes
by Merkle path; the same source always yields the same IDs.

A thin **human-readable projection** is auto-generated for inspection and
diffs. Humans never author it. It exists only for debugging.

### Why not free-form text / a Rust-like surface

A Rust-like surface reintroduces every problem this proposal claims to solve:
parser ambiguity, indentation/precedence conventions, line-based errors that
don't map to AST nodes, and "canonical form" that exists only on paper
because the model can write the same thing five ways. The whole point is to
remove degrees of freedom the model can get wrong.

### Why not raw JSON via tool-call

JSON is more verbose for code (keys and quotes everywhere), LLMs are
slightly less reliable on deeply-nested JSON than on Lisp-like nesting, and
JSON's string-only semantics fight the symbolic structure. s-exprs keep it
symbolic. (Function-calling / structured-output mode is still used to enforce
the s-expr grammar — the model emits s-exprs *through* a constrained
decoder, not as free text.)

### Concrete shape

```scheme
(module calculator
  (spec
    (property add-commutes
      (forall ((a i64) (b i64))
        (= (call add a b) (call add b a))))
    (property diff-nonneg
      (forall ((a i64) (b i64))
        (>= (call positive-difference a b) 0))))

  (fn add
    (params (a i64) (b i64))
    (ret i64)
    (body
      (call i64.add (ref a) (ref b))))

  (fn positive-difference
    (params (a i64) (b i64))
    (ret i64)
    (body
      (if (call i64.gt (ref a) (ref b))
          (then (call i64.sub (ref a) (ref b)))
          (else (lit 0 i64))))))
```

After parsing, every `(fn ...)`, `(call ...)`, `(ref ...)`, `(lit ...)`
node has a Merkle ID. Errors and repairs speak in those IDs.

---

## 2. Semantics: the things the validator checks

Everything below is **explicit, no inference**. The defining cost of AI-friendly
vs. human-friendly is that we trade verbosity for mechanical checkability. We
take that trade deliberately.

- **Types are explicit and structural.** No inference, no subtyping beyond
  declared numeric casts. Every binding carries a type annotation. If the
  model writes the wrong type, that is a *local* error with a *local* repair
  (change this annotation), not a constraint-propagation mystery.

- **Effects are declared and capability-scoped** (§4). A function's effect
  signature is part of its type. Calling a function with a wider effect than
  the caller declares is a structural error.

- **Memory safety via affine ownership + explicit regions** (§3). No lifetime
  inference, no variance, no HRTB. References carry their region explicitly.

- **Control flow is structured.** `if`, `match`, `loop`, `return`. No `goto`,
  no labeled breaks, no exceptions. Lowering to basic blocks happens *after*
  validation, in MLIR — the AI never sees a basic block.

- **No undefined behavior by construction.** Integer overflow is a checked
  trapping op unless an explicit `(wrap)` variant is requested. Division by
  zero traps. Out-of-bounds access traps (and is statically rejected when
  statically detectable). The language's semantics are defined *before* LLVM
  — LLVM UB does not exist at the Aury layer.

- **Determinism defaults.** Iteration order over maps is sorted. The `rng`
  capability is the only nondeterminism source, and it is seeded explicitly
  so a given seed + program is reproducible.

- **Stable across LLVM versions.** The Aury spec is independent of LLVM.
  Upgrading LLVM is a compiler-internal concern; emitted Aury programs do
  not change meaning.

---

## 3. Memory safety: affine ownership + explicit regions, no inference

### Decision

Linear/affine ownership for unique resources; **region-based allocation**
for memory; references carry an explicit region; **no lifetime inference,
no variance, no elision**. Every borrow is written.

### Mechanism

- A **region** is an explicit scope introduced by `(region r ...)` or
  declared on a function. Allocations live in a region; the region frees on
  exit. No per-allocation free, no GC, no RAII ceremonies.
- A **reference** is `(ref r mut? t)` — it names its region, its
  mutability, and its pointee type. There is no `&'a T` style lifetime
  parameter; the region *is* the lifetime, written out.
- **Affine use**: a unique-owned value can be used at most once. Copies
  require an explicit `(copy ...)`. Moves are implicit on last use; the
  validator reports the move.
- **No shared mutable state across regions.** Two `mut` references to
  overlapping memory in the same region are statically rejected (an alias
  analysis pass, decidable because regions are explicit and disjoint by
  construction unless declared shared).
- **Shared regions** (for inter-task communication) require an explicit
  `(sync ...)` wrapper providing a mutex/channel. The capability system
  gates these.

### Why this and not Rust's borrow checker

Rust's borrow checker is hard largely because of *inference*: lifetimes are
elided, variance is implicit, higher-ranked bounds leak through closures.
Those exist for human ergonomics. We don't need them. With regions written
explicitly:

- The check is a local dataflow pass over each region's borrow graph.
- Errors are pointwise ("region `r` borrowed mut at #x, also borrowed at #y")
  and admit mechanical repairs ("split into two regions", "insert a copy",
  "demote one borrow to shared").
- No "lifetime doesn't satisfy" cascade that requires whole-program
  reasoning to repair.

### Why this and not GC

Deterministic memory, no runtime pause, region frees are bulk and trivial,
fits a systems-tools target. The cost is programmer (model) verbosity,
which is the accepted trade.

### Cost

Verbose. AI-generated code is fine with verbose. We explicitly do not
compete with hand-written Rust on concision.

---

## 4. Effects and capabilities

Effects are not a type-system afterthought; they are a first-class part of a
function's signature, enforced by a **capability** discipline.

- A **capability** is a first-class value: `fs`, `net`, `clock`, `rng`,
  `state<key>`, `log`. Capabilities are passed as arguments.
- A function's effect row lists the capabilities it requires:
  `(effects (fs read) (net) (clock))`.
- Calling a function requires the caller to *hold* the listed capabilities.
- The runtime harness **grants** a specific capability set to a generated
  program. The compiler verifies the program doesn't transitively require
  capabilities the harness didn't grant. Sandboxing is therefore
  **static and compositional**, not a runtime afterthought.

```
(fn read-config
  (params (path str) (fs (cap fs read)))
  (effects (fs read))
  ...)
```

You cannot `read-config` without holding `(cap fs read)`. The capability is
proof of permission, threaded through the call graph.

This subsumes an effect system *and* a sandboxing policy *and* a
dependency-audit surface ("this program needs `net` + `fs read` only").

---

## 5. Intent verification: the gate that actually matters

Structural validation proves the program is well-formed. It does not prove
the program does what the user wants. The proposal treats intent
verification as a **first-class deliverable**, not a closing caveat.

Three layers, all generated by the AI alongside the implementation, all
checked before acceptance:

### 5.1 Contracts (preconditions, postconditions, invariants)

```scheme
(spec
  (pre (>= n 0))
  (post (<= (result) n))
  (invariant (forall ((i i64)) (in i arr) -> (>= (load arr i) 0))))
```

Discharged by SMT where the fragment is decidable (linear arithmetic,
uninterpreted functions, arrays, quantifier-free Booleans); compiled to
runtime assertions where not. Decidability is per-check, reported in the
diagnostic.

### 5.2 Property tests

```scheme
(spec
  (property sort-idempotent
    (forall ((xs (vec i64)))
      (= (call sort (call sort xs)) (call sort xs))))
  (gen (vec i64) ...))   ; generator the AI also writes
```

The harness runs N random cases (QuickCheck-style), **shrinks** failures to
minimal counterexamples, and feeds the shrunk counterexample back as a
repair signal. Generators are themselves Aury code; if the generator is
too narrow to ever falsify the property, a **vacuity check** flags it.

### 5.3 Spec vacuity and implication

A spec the program cannot fail is worthless. The validator:
- runs the property with a deliberately adversarial generator;
- checks the property can *fail* on some input (non-vacuity);
- for changes, checks the new spec *implies* the old (regression).

### 5.4 Differential testing (where a reference exists)

If the user supplies a reference (an existing script, a previous version, an
oracle), the harness runs both on random inputs and reports divergences.
This is the only layer that can catch "compiles, specs pass, still wrong
because the spec is also wrong" — by anchoring to an external oracle.

### What this does *not* solve

None of this proves the spec matches user intent. The user is still the
final oracle. But the loop shrinks the gap: instead of "trust the AI's
code," it's "the AI's code passes its own spec, which the user can read and
edit, and which the harness shows is non-vacuous and regression-stable."
That is a meaningful, inspectable reduction in the intent gap. It is not a
proof, and the proposal doesn't claim it is.

---

## 6. The repair protocol (the centerpiece)

This is the part that distinguishes Aury from "Rust + JSON errors." Every
rejection from any gate — types, effects, memory, contracts, properties — is
emitted as a **structured repair object**, not a message.

### Shape

```json
{
  "gate": "type",
  "kind": "ARG_TYPE_MISMATCH",
  "node": "m1:3f2a...",
  "path": "module/fn@add/body/call@i64.add/arg[0]",
  "expected": "i64",
  "received": "string",
  "context": {"param_name": "a", "caller_node": "m1:8b41..."},
  "repairs": [
    {"id": "r1", "action": "wrap",
     "with": "(call i64.parse (ref x))",
     "cost": 1, "preserves_effects": true},
    {"id": "r2", "action": "replace_node",
     "with": "(lit 0 i64)",
     "cost": 2, "preserves_effects": true},
    {"id": "r3", "action": "change_param_type",
     "param": "a", "from": "i64", "to": "string",
     "cost": 5, "propagates": ["call sites of add"],
     "preserves_effects": false}
  ]
}
```

### Properties the protocol guarantees

- **Every repair is a structured patch** keyed by Merkle node ID, not "edit
  line 18." The model applies `r1` and resubmits; no diff-merge ambiguity.
- **Repairs are ranked by cost** and tagged with whether they preserve
  effects/contracts, so the model can prefer repairs that don't cascade.
- **Repairs are *admissible by construction***: each one names a known-valid
  replacement the validator has already checked. The model cannot pick an
  invalid repair from the menu; it can only refuse all of them (in which
  case it should regenerate, not loop).
- **The validator tracks applied repairs** per node to detect cycles. If a
  node is rejected with the same kind twice, the validator escalates
  ("patch r1 was applied and did not resolve — try r3 or regenerate").

### Termination

- Per-node repair budget (e.g., 3 attempts). On exhaustion, that node is
  marked *failed* and the model is asked to **regenerate the containing
  function** with the failure as context.
- Per-program repair budget (e.g., 20 total patches). On exhaustion, the
  whole artifact is regenerated with a summary of unresolved errors.
- No silent infinite loops.

### Why this is the differentiator

`rustc --error-format json` gives you structured *diagnostics*. It does not
give you admissible, validated, ranked *patches*. The model still has to
invent the fix. Aury's protocol offloads fix-invention onto the compiler
where the fix space is known, and leaves the model to choose among them
and to regenerate when the local fix space is exhausted. That is a
qualitative change in the generate/repair loop, not a quantitative one.

---

## 7. MLIR lowering pipeline

The Aury AST lowers through MLIR. Each level's verifier encodes a layer of
the language's semantics, so validation isn't a separate ad-hoc pass — it
*is* the dialect invariants.

```
Aury AST
  → aury dialect      (types, effects, regions, contracts attached as attrs)
  → scf dialect          (structured control flow: if/loop/match)
  → mem dialect          (region allocs, affine borrows → alloc/load/store)
  → arith + llvm dialect (scalar ops, calls, ABI)
  → LLVM IR              (verifier run; passes; codegen)
```

- The **aury dialect** owns type-checking, effect-checking, region/borrow
  checking, and contract attachment. Its `verify()` is the structural gate.
- Lowering passes are standard MLIR; we do **not** write a codegen, an
  optimizer, or an ABI layer. We inherit them.
- A failing `verify()` at any level emits a repair object (the dialect
  knows the failing op and the violated invariant; it can enumerate
  admissible repairs because the dialect is small and closed).
- Upgrading LLVM is internal; Aury source is stable.

### What we explicitly do *not* build

- No custom register allocator, no custom optimizer, no custom ABI. LLVM
  owns those.
- No self-hosting. The Aury compiler is written in Rust (or OCaml); Aury
  is not mature enough to host itself and shouldn't pretend to be.

---

## 8. FFI and OS interaction

C ABI imports with explicit, *audited* effect and capability declarations:

```scheme
(extern libc.read
  (abi c)
  (params (fd i32) (buf (ref r mut u8)) (count usize))
  (ret (result usize i32))
  (effects (io)))
```

- The compiler cannot see inside C, so declared effects are **trusted but
  tracked**. Imports live in audited runtime shims (libc, a small
  sockets layer, a small fs layer). Arbitrary C without a shim is not
  allowed in v0.
- Calling an extern requires holding its declared capabilities.
- The OS surface is intentionally **small and explicit**. There is no
  "discovering the stdlib" — there is a declared, versioned set of externs.

---

## 9. Training-data strategy (the question the original proposal ducked)

A novel language is a language the model has near-zero direct training
exposure to. This is the proposal's biggest practical risk. Strategy:

1. **Token overlap by design.** Aury tokens overlap heavily with Rust
   (types, `fn`, `ref`, `mut`, `result`), MLIR (op names, attributes), and
   Lisp (s-exprs, `let`, `lambda`-style bindings). Transfer learning from
   pretraining is real, not zero.
2. **Synthetic corpus from existing IRs.** Lift the MLIR and LLVM test
   suites (thousands of small, correct programs) into Aury. Train on
   (intent description → Aury) pairs constructed from commit messages and
   test names. Cheap, large, correct-by-construction.
3. **Few-shot pattern library.** A curated, indexed set of canonical
   Aury patterns ("sort a vector", "parse CSV", "HTTP GET", "read file
   line by line"). Retrieved by intent similarity at generation time.
4. **Fine-tune once v0 is stable.** Run the generate/validate/repair loop
   at scale on a benchmark of intents; collect accepted (intent, Aury,
   spec) triples; fine-tune. The loop generates its own training data.
5. **Keep the language small.** No historical cruft means the surface a
   model must master is bounded. Coverage is achievable with modest data.
6. **The repair protocol absorbs residual error.** Even a mediocre Aury
   model becomes usable because the validator catches and patches most
   mistakes. The bar for model quality is lower than for "free-form Rust"
   because the loop is tighter.

The honest claim: v0 models will be worse at Aury than at Python. The
honest counter: Aury v0 programs that pass the gates are more reliable
than Python programs that "look right," because the gates are real. The
proposition is *reliability at acceptance*, not *first-shot accuracy*.

---

## 10. Scope and milestones

### v0 — pure core (prototype, ~2–3 months)

- s-expression parser + Merkle IDs.
- i64, bool, str, vec, structs.
- `if`, `match`, `loop`, `return`.
- Affine ownership + regions, no shared regions.
- Effects + capabilities (`fs`, `net`, `clock`, `rng`, `log`).
- Contracts (SMT-decidable fragment + runtime assertions).
- Property tests + vacuity check + shrinking.
- Repair protocol for type/effect/region/contract errors.
- MLIR lowering through scf/mem/arith/llvm. Native binaries.
- C FFI for a small audited libc subset.

**Target: AI-generated CLI tools and data transformations compile, pass
specs, and run.**

### v1 — real systems tools (~6–9 months)

- Modules, traits, parametric types (no lifetime params — regions are
  values).
- Shared regions + sync primitives (mutex, channel) under capability gating.
- Async via a structured task dialect (capabilities still apply).
- Differential testing harness.
- Larger audited FFI surface (sockets, paths, time, env).
- Self-hosted benchmark suite; first fine-tune.

**Target: AI-generated small services and simulations.**

### v2 — usable ecosystem (~12–18 months)

- Standard library (in Aury where possible, FFI where not).
- Package + capability manifest format.
- IDE-free tooling: a TUI that shows the AST, specs, and the repair menu.
- Cross-module spec implication + regression suite.
- Public benchmark + leaderboard for (intent → passing Aury).

### Explicitly *not* a goal

- Replacing Rust or C++.
- A general-purpose ecosystem on the scale of Cargo or crates.io.
- Performance competitiveness with hand-tuned C.
- Self-hosting.
- Browser/WASM as a first-class target (a later addition, not v0).

The realistic framing is **a special-purpose language for AI-generated
tools and small services**, not a general-purpose systems language. The
writeup's own "feasible for a small prototype" tier is the actual product
surface; the "replacement for Rust" tier is a multi-year distraction this
proposal declines to chase.

---

## 11. What this proposal explicitly defers or admits

- **Intent is not provable.** The verification gate narrows the intent gap;
  it doesn't close it. The user is the final oracle.
- **The model can write a wrong spec.** Vacuity + differential testing catch
  some of this; the rest is the user's job, made easier by readable specs.
- **Memory safety is not information security.** Affine + regions prevent
  memory unsafety; they don't prevent logic flaws or capability misuse.
- **C FFI is a trusted boundary.** Bugs in the audited shims are out of
  scope for Aury's guarantees.
- **LLVM bugs are out of scope.** Same as for every language on LLVM.
- **v0 will not be ergonomic for humans.** That is the point, and the
  projection exists only for inspection.

---

## 12. The one-sentence summary

> Build a small, explicit, no-inference, s-expression language whose
> validator emits ranked, mechanically-applicable, admissible repairs and
> whose acceptance gate requires non-vacuous property tests and contracts,
> lowering through MLIR to native code — co-designed around an LLM repair
> loop, scoped to AI-generated tools, not general-purpose systems
> programming.

The interesting invention is not "AI writes LLVM," and not "a language for
machines." It is the **validated, ranked, admissible-repair surface plus an
intent-verification gate**, sitting between the model and LLVM. That surface
is what makes AI-generated software safer. The language exists to make that
surface possible.