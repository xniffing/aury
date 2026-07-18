# Calculator — module map

The call graph of the browser calculator module, produced by `aury diagram` — an
exact read-only walk of the typed AST. Each node is a function; each edge is a
resolved call to another function in the module. Builtin operations
(`i64.add`, `f64.mul`, …) are ops, not nodes.

**38 functions · all pure · `i64` + `f64` · 2 call edges.**

## Call graph

```mermaid
%% call graph for module `calculator`
graph TD
  add["add(a: i64, b: i64) -&gt; i64"]
  subtract["subtract(a: i64, b: i64) -&gt; i64"]
  multiply["multiply(a: i64, b: i64) -&gt; i64"]
  divide["divide(a: i64, b: i64) -&gt; i64"]
  modulo["modulo(a: i64, b: i64) -&gt; i64"]
  percent["percent(a: i64, b: i64) -&gt; i64"]
  average["average(a: i64, b: i64) -&gt; i64"]
  maximum["maximum(a: i64, b: i64) -&gt; i64"]
  minimum["minimum(a: i64, b: i64) -&gt; i64"]
  negate["negate(a: i64) -&gt; i64"]
  absolute["absolute(a: i64) -&gt; i64"]
  square["square(a: i64) -&gt; i64"]
  increment["increment(a: i64) -&gt; i64"]
  decrement["decrement(a: i64) -&gt; i64"]
  double["double(a: i64) -&gt; i64"]
  gcd["gcd(a: i64, b: i64) -&gt; i64"]
  lcm["lcm(a: i64, b: i64) -&gt; i64"]
  power["power(base: i64, exp: i64) -&gt; i64"]
  factorial["factorial(n: i64) -&gt; i64"]
  fibonacci["fibonacci(n: i64) -&gt; i64"]
  isqrt["isqrt(n: i64) -&gt; i64"]
  is_even["is_even(n: i64) -&gt; bool"]
  is_prime["is_prime(n: i64) -&gt; bool"]
  fadd["fadd(a: f64, b: f64) -&gt; f64"]
  fsubtract["fsubtract(a: f64, b: f64) -&gt; f64"]
  fmultiply["fmultiply(a: f64, b: f64) -&gt; f64"]
  fdivide["fdivide(a: f64, b: f64) -&gt; f64"]
  fmaximum["fmaximum(a: f64, b: f64) -&gt; f64"]
  fminimum["fminimum(a: f64, b: f64) -&gt; f64"]
  fpower["fpower(base: f64, exp: i64) -&gt; f64"]
  fnegate["fnegate(a: f64) -&gt; f64"]
  fabs["fabs(a: f64) -&gt; f64"]
  fsquare["fsquare(a: f64) -&gt; f64"]
  freciprocal["freciprocal(a: f64) -&gt; f64"]
  fsqrt["fsqrt(x: f64) -&gt; f64"]
  to_float["to_float(a: i64) -&gt; f64"]
  to_int["to_int(a: f64) -&gt; i64"]
  is_nan["is_nan(a: f64) -&gt; bool"]
  lcm --> gcd
  is_prime --> is_even
```

## Call structure

36 of the 38 functions are leaf primitives that compose only builtin operations.
All inter-function structure lives in the number-theory group:

- `lcm` reduces through `gcd`
- `is_prime` screens with `is_even`

Every function's effect row is pure — no capability (`rng`, `fs`, …) is used — so
the graph carries no effect badges. (For contrast, `examples/agent/dice.aury`
renders `roll-even() -> i64 · ⚡ rng`.)

The data-model view (`aury diagram … --kind types`) is empty for this module:
calculator defines no structs.

## Regenerate

```sh
aury ingest projects/calculator/calculator.json calculator.aury
aury diagram calculator.aury                 # this call graph
aury diagram calculator.aury --kind types    # data model (none here)
```
