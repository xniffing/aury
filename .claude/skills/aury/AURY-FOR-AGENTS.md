# Aury for agents

This is the reference an AI consults **before authoring an Aury program and
while correcting one**. Aury is co-designed with an LLM repair loop: you author
in a structured JSON AST, the toolchain returns machine-readable rejections that
each carry a *ranked, cost-annotated repair menu*, and you apply repairs until
the program is accepted.

The correction loop you run:

```
author JSON  →  ingest (--force)  →  loop (auto-repair) + test (counterexamples)
     ↑                                          │
     └──────── read rejections / repairs ───────┘   (apply lowest-cost admissible
                                                      repair, or regenerate)
```

Run every step with the harness: `./.claude/skills/aury/dev.sh <program.json>`.
See "The dev.sh contract" at the bottom for its output shape.

---

## 1. Author in JSON (never count parentheses)

Emit a **typed-object JSON AST**. Every node is `{"kind": "...", ...}`. This is
the authoring surface — `ingest` converts it to the canonical s-expr IR with
identical Merkle node ids, so JSON is a true front-end for the one canonical
form, not a second format. Because the JSON is `kind`-tagged, you never miscount
delimiters — the class of error that structured generation exists to remove.

A whole module:

```json
{
  "kind": "module",
  "name": "gcd",
  "items": [ <item>, <item>, ... ]
}
```

`items` is a list of `fn`, `struct`, `extern`, and `spec` nodes (below).

---

## 2. Types (the `"type"` / `"ret"` string fields)

Types are written as **strings**, using the s-expr type syntax:

| Type | String | Notes |
|------|--------|-------|
| 64-bit int | `"i64"` | the numeric core |
| 64-bit float | `"f64"` | IEEE-754 double; see `f64.*` builtins |
| Boolean | `"bool"` | |
| String | `"str"` | |
| Unit | `"unit"` | no value |
| Vector | `"(vec i64)"` | homogeneous, `(vec T)` |
| Struct | `"(struct Vec2)"` | must match a declared `struct` |
| Result | `"(result i64 str)"` | `(result Ok Err)` — from `i64.from_str` etc. |
| Region handle | `"region"` | rarely written directly |
| Reference | `"(ref r i64)"` / `"(mut r i64)"` | region-scoped borrow |

---

## 3. Module items

### `fn`
```json
{
  "kind": "fn",
  "name": "gcd",
  "params": [ {"name": "a", "type": "i64"}, {"name": "b", "type": "i64"} ],
  "ret": "i64",
  "effects": ["rng"],          // optional; omit for a pure function
  "body": <expr>
}
```
The body is a **single expression** (use `let`, `if`, `block` to sequence).

### `struct`
```json
{ "kind": "struct", "name": "Vec2",
  "fields": [ {"name": "x", "type": "i64"}, {"name": "y", "type": "i64"} ] }
```

### `extern`
An externally-provided function signature (no body) that Aury may call:
```json
{ "kind": "extern", "name": "host_time",
  "params": [], "ret": "i64", "effects": ["time"] }
```

### `spec` — the part an agent checks against
A spec block carries **contracts** and **properties**. This is where intent is
verified; `aury test` runs it and hands back shrunk counterexamples.

```json
{
  "kind": "spec",
  "contracts": [
    { "pre": <bool-expr>, "post": <bool-expr> }   // requires / ensures
  ],
  "properties": [
    {
      "name": "gcd-commutative",
      "forall": [ {"name": "a", "type": "i64"}, {"name": "b", "type": "i64"} ],
      "body": <bool-expr>        // must evaluate to bool for all bindings
    }
  ]
}
```
A property `body` must be `bool`. Guard partial properties with `if`:
`if (precondition) then (real check) else true` — an unguarded property that is
never meaningfully exercised is flagged as **vacuous**.

---

## 4. Expression nodes (every `kind`)

| kind | shape | meaning |
|------|-------|---------|
| `lit` | `{"kind":"lit","value": 0 \| 1.5 \| true \| "hi"}` | i64 / f64 / bool / str literal — a **number with a decimal point is an f64** (`1.5`, `0.0`), without one it is an i64 |
| `ref` | `{"kind":"ref","name":"a"}` | read a param / let binding |
| `let` | `{"kind":"let","name":"h","type":"i64","init":<e>,"body":<e>}` | bind then continue |
| `call` | `{"kind":"call","op":"i64.add","args":[<e>,...]}` | builtin **or** user fn by name |
| `if` | `{"kind":"if","cond":<e>,"then":<e>,"else":<e>}` | all three required |
| `match` | `{"kind":"match","scrut":<e>,"arms":[{"pattern":<p>,"body":<e>}]}` | see patterns below |
| `loop` | `{"kind":"loop","body":<e>}` | repeat `body`; a `break` inside exits and gives the loop its value, else it runs until a `return` (diverges) |
| `break` | `{"kind":"break","value":<e>}` | exit the nearest enclosing `loop`; the loop evaluates to `value` (`value` optional → unit) |
| `set` | `{"kind":"set","name":"acc","value":<e>}` | reassign a mutable `let` local (not a param); yields unit |
| `return` | `{"kind":"return","value":<e>}` | early return from the **function** (unwinds past loops) |
| `block` | `{"kind":"block","stmts":[<e>,...],"tail":<e>}` | sequence the `stmts`, then the value is `tail` |
| `region` | `{"kind":"region","name":"r","body":<e>}` | open an allocation/effect region |
| `copy` | `{"kind":"copy","value":<e>}` | explicit copy of a value |
| `vec-new` | `{"kind":"vec-new","type":"(vec i64)","elems":[<e>,...]}` | build a vector |
| `idx` | `{"kind":"idx","target":<e>,"index":<e>}` | vector element (bounds-checked) |
| `len` | `{"kind":"len","target":<e>}` | vector length → i64 |
| `new-struct` | `{"kind":"new-struct","name":"Vec2","fields":[{"name":"x","value":<e>}]}` | construct |
| `get` | `{"kind":"get","target":<e>,"field":"x"}` | read a struct field |
| `cast` | `{"kind":"cast","type":"i64","value":<e>}` | str→i64 parse (traps on bad input); `i64`↔`f64` numeric casts; `f64`→`str` formatting |

### Match patterns (`"pattern"`)
| kind | shape | matches |
|------|-------|---------|
| `wild` | `{"kind":"wild"}` | anything (`_`) |
| `bind` | `{"kind":"bind","name":"x"}` | anything, binds `x` |
| `lit` | `{"kind":"lit","value": 0}` | that exact literal |

### Mutable loops (`set` + `loop` + `break`)

Iteration can use a **mutable accumulator** instead of a recursive helper. Bind
the state with `let`, mutate it with `set`, and exit the `loop` with `break`,
which makes the whole `loop` expression evaluate to the break value. Iterative
factorial:

```json
{"kind":"let","name":"acc","type":"i64","init":{"kind":"lit","value":1},
 "body":{"kind":"let","name":"i","type":"i64","init":{"kind":"ref","name":"n"},
  "body":{"kind":"loop","body":{
    "kind":"if",
    "cond":{"kind":"call","op":"i64.le","args":[{"kind":"ref","name":"i"},{"kind":"lit","value":1}]},
    "then":{"kind":"break","value":{"kind":"ref","name":"acc"}},
    "else":{"kind":"block","stmts":[
      {"kind":"set","name":"acc","value":{"kind":"call","op":"i64.mul","args":[{"kind":"ref","name":"acc"},{"kind":"ref","name":"i"}]}},
      {"kind":"set","name":"i","value":{"kind":"call","op":"i64.sub","args":[{"kind":"ref","name":"i"},{"kind":"lit","value":1}]}}],
      "tail":{"kind":"lit","value":null}}}}}}
```

Rules the validator enforces (each is a rejection with a repair menu):

- `set` targets a **`let`-bound local**, not a parameter (`SET_OF_PARAM`) or an
  unbound name (`SET_UNBOUND`); the value type must match the binding
  (`SET_TYPE_MISMATCH`). To "mutate" a parameter, first `let`-bind a copy.
- `break` must sit inside a `loop` (`BREAK_OUTSIDE_LOOP`), and all breaks in one
  loop must agree on a value type (`BREAK_TYPE_MISMATCH`) — that type is the
  loop's type. A loop with **no** `break` diverges (it must be exited by
  `return`), so it can't be used as a value.
- `set` yields unit, so it belongs in a `block`'s `stmts`, with a real value
  (often `{"kind":"lit","value":null}` for unit) as the `tail`.

A `set` inside a `match`/`if` arm mutates the enclosing binding (arms are not
isolated scopes). Semantics are identical on the interpreter and native backend.

---

## 5. Builtins (exact signatures)

Call as `{"kind":"call","op":"<name>","args":[...]}`. Arity and types are
checked; a mismatch comes back as a rejection with a repair.

**Integer** (`i64`):
| op | signature |
|----|-----------|
| `i64.add` `i64.sub` `i64.mul` `i64.div` `i64.mod` | `(i64, i64) -> i64` |
| `i64.eq` `i64.neq` `i64.lt` `i64.le` `i64.gt` `i64.ge` | `(i64, i64) -> bool` |
| `i64.neg` `i64.abs` | `(i64) -> i64` |
| `i64.to_str` | `(i64) -> str` |
| `i64.from_str` `i64.parse` | `(str) -> (result i64 str)` |

**Float** (`f64`, IEEE-754 — arithmetic **never traps**):
| op | signature |
|----|-----------|
| `f64.add` `f64.sub` `f64.mul` `f64.div` | `(f64, f64) -> f64` |
| `f64.eq` `f64.neq` `f64.lt` `f64.le` `f64.gt` `f64.ge` | `(f64, f64) -> bool` |
| `f64.neg` `f64.abs` | `(f64) -> f64` |
| `f64.to_str` | `(f64) -> str` |

`f64.div` by zero yields `inf` / `-inf` (`0.0/0.0` → `NaN`) — it does **not**
trap like `i64.div`. Any comparison involving `NaN` is false, except `f64.neq`,
which is true, so `(f64.eq NaN NaN)` is `false` and `(f64.neq NaN NaN)` is `true`.
There are no `f64` literals for `inf`/`NaN`: obtain them from arithmetic
(`(f64.div 1.0 0.0)`, `(f64.div 0.0 0.0)`). Get a `NaN`/`inf` or plain float as a
string with `f64.to_str`; convert between numbers with `cast` (`(cast f64 <i64>)`,
`(cast i64 <f64>)` truncates toward zero and saturates, `NaN`→0). Floats print in
a **canonical 17-significant-digit scientific form** (`1.5` → `1.5000000000000000e+00`)
that is byte-identical across the interpreter, native, and wasm backends.

**Boolean** (`bool`):
| op | signature |
|----|-----------|
| `bool.and` `bool.or` | `(bool, bool) -> bool` |
| `bool.not` | `(bool) -> bool` |
| `bool.eq` | `(bool, bool) -> bool` |

**String** (`str`):
| op | signature |
|----|-----------|
| `str.concat` | `(str, str) -> str` |
| `str.eq` `str.neq` | `(str, str) -> bool` |
| `str.len` | `(str) -> i64` |

**Result** (`(result i64 str)`):
| op | signature |
|----|-----------|
| `result.is_ok` | `((result i64 str)) -> bool` |

A `call` whose `op` is not a builtin is a **call to a user `fn`** of that name;
the argument types must match the callee's `params`.

### Working with `result` (important: it is testable-only in v0)

`i64.from_str` / `i64.parse` produce `(result i64 str)`. There is **no `unwrap`
builtin and no `Ok`/`Err` match pattern** (patterns are only `wild`/`bind`/`lit`,
which compare i64/bool/str/unit). So you cannot pattern-match a `result` apart.
The idiom is: test it with `result.is_ok`, and extract the payload separately by
casting the original string — `cast i64 <str>` (traps on genuinely bad input):

```
(let r (result i64 str) (call i64.from_str (ref s))
  (if (call result.is_ok (ref r))
    (then (cast i64 (ref s)))     ; ok: parse succeeded, cast the same str
    (else (ref fallback))))       ; err: use a fallback
```
See `examples/agent/parse-classify.json` for the full pattern with properties.

---

## 6. Effects and regions

- A function with no `effects` field is **pure** and may only call pure things.
- Declaring `"effects": ["rng"]` lets the body use `rng.next` (→ `i64`) and call
  other `rng` functions. Capabilities that parse: `rng`, `io`, `net`, `time`
  (only `rng` has an interpreter/native builtin in v0).
- Effectful/allocating work happens inside a `region` node; `rng.next` is used
  inside one. See `examples/agent/dice.aury` for the canonical shape.
- The validator **rejects effect leaks**: using `rng.next` in a pure function is
  a rejection whose repair either adds the effect to the signature or removes the
  call.

---

## 7. Reading a rejection and correcting it

Every rejected gate — parse, type, effect, region, contract, property — comes
back as one JSON object:

```json
{
  "gate": "type",
  "kind": "TYPE_MISMATCH",
  "node": "5af1f8fcee45a7f0",          // Merkle id of the offending node
  "path": "gcd.body.if.cond",
  "expected": "bool",
  "received": "i64",
  "context": { ... },
  "repairs": [
    {
      "id": "r1",
      "action": "wrap_call",
      "with": "(call i64.eq ... (lit 0))",
      "cost": 2,                        // lower = prefer this one
      "preserves_effects": true,
      "preserves_contracts": true,
      "propagates": [],                 // sibling nodes that also need editing
      "note": "human-readable explanation of the fix"
    }
  ]
}
```

**Correction protocol:**
1. Read `repairs`, which are already sorted by `cost` (lowest first).
2. Prefer a repair with `preserves_effects` **and** `preserves_contracts` true.
3. Apply it (edit that node in your JSON), then re-run `dev.sh`.
4. `aury loop` will auto-apply *admissible* repairs for you; only intervene on
   what it leaves in `remaining`, or when `recommend_regenerate` is set.
5. For a `PROPERTY_FALSIFIED` / contract failure, `received` is the **shrunk
   minimal counterexample** (e.g. `"falsified for: a = 0i64, b = 0i64"`). Decide
   whether the **implementation** is wrong or the **property/contract** is wrong,
   fix that one, and re-run. Don't weaken a true property to pass.

---

## The dev.sh contract

`./.claude/skills/aury/dev.sh <program.json|program.aury> [entry-fn arg...]`

It runs: ingest (`--force`, so an invalid program is still written for repair) →
`aury loop` (auto-repair + property tests) → and, if an entry fn is given and the
program is accepted, `aury run` (and `aury compile` when `clang` is present, to
check the native result equals the interpreter). It prints each stage under a
clear `=== STAGE ===` banner and ends with a single machine-readable line:

```
AURY_RESULT {"status":"accepted","patches_applied":1,"entry":"gcd","run":"12","native":"12"}
```
or, on failure, `status` is `rejected` (see the printed rejection JSON above the
line) or `error`. Parse that final line; read the rejection JSON above it to pick
your next repair.

Beyond the interpreter and native (`aury compile`) backends, an accepted program
can be built for `wasm32-wasi` with the **same** LLVM lowering: `aury wasm` (an
executable module, run via wasmtime/wasmer) and `aury wasm-lib … --export <fn>`
(a reactor module exporting `aury__<fn>` for a host such as a browser). These are
backends only — authoring and repair never require the wasm toolchain. See
`SKILL.md` for the flags and `projects/calculator/` for a browser example.
