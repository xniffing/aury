You are writing a program in **Aury**, a small typed s-expression language.

Output **only** a single canonical Aury `(module ...)` form — no prose, no
markdown fences, no explanation. It must define a function with the exact
signature given below so an automated harness can call it.

## Task

{{INTENT}}

Define exactly this function:

- name: `{{FN}}`
- parameters: {{PARAMS}}
- returns: `{{RET}}`

## Aury syntax (everything you need)

A module wraps function definitions:

```
(module m
  (fn <name> (params (<p> <type>) ...) (ret <type>)
    (body <expr>)))
```

Types are `i64`, `bool`. Expressions:

- literal: `(lit 42)`  ·  variable: `(ref x)`
- integer ops (all `i64`): `(call i64.add a b)`, `i64.sub`, `i64.mul`, `i64.div`,
  `i64.mod`, `i64.neg`, `i64.abs`
- integer comparisons (return `bool`): `(call i64.eq a b)`, `i64.neq`, `i64.lt`,
  `i64.le`, `i64.gt`, `i64.ge`
- call your own function: `(call <fn> arg ...)`
- conditional: `(if <bool> (then <expr>) (else <expr>))`
- local binding: `(let <name> <type> <init> <body>)`
- mutable loop: bind with `let`, reassign with `(set <name> <value>)`, loop with
  `(loop <body>)`, exit with `(break <value>)`. A `set` yields unit, so sequence
  statements in a `(block <stmt> ... <tail>)`. Example accumulator:

```
(fn sum-to (params (n i64)) (ret i64)
  (body
    (let acc i64 (lit 0)
      (let i i64 (lit 1)
        (loop
          (if (call i64.gt (ref i) (ref n))
              (then (break (ref acc)))
              (else (block
                (set acc (call i64.add (ref acc) (ref i)))
                (set i (call i64.add (ref i) (lit 1)))
                unit))))))))
```

- early return from the function: `(return <expr>)`  ·  unit value: `unit`

There is no `while`, no `for`, no operator syntax — every operation is a
`(call ...)`. Recursion is allowed. Booleans come only from comparisons.

Output the module now.
