## Review

- **Correct:** `src/lower.rs:197-203` has the right architectural intent: string globals are buffered and emitted at module scope rather than inside function definitions.
- **Blocker:** `src/lower.rs:201-203` mutates `l` through `out_str` while iterating over `&l.str_literals`. `cargo test` fails with two `E0502` borrow errors.
- **Blocker:** `src/lower.rs:261-262` and `src/lower.rs:445-446` use `struct_name` after moving it into a tuple. These produce two `E0382` errors. The crate currently does not compile.
- **Blocker:** The new API takes `args: &[String]` at `src/lower.rs:210`, but integration was not updated:
  - `src/main.rs:270-277` still parses every argument into `Vec<i64>` and passes `&iargs`.
  - `tests/integration.rs:282` still passes an integer array.
  Once the library ownership errors are fixed, both callers will have type errors. The CLI pre-parse also prevents string arguments from reaching the new lowering.
- **High:** String entry results are printed using the boxed string object as a C string at `src/lower.rs:283-284`. The value is a pointer to `{ i64 len, ptr data }`, confirmed by `runtime/aury_rt.c:18-20`, not a `char *`. The generated main must load field 1 before passing it to `%s`; otherwise output is garbage or empty and may read beyond the object.
- **High:** Even after loading the data pointer, native string output would not satisfy the documented interpreter-equivalence contract. `show_value` prints quoted/escaped strings with a newline, while `src/lower.rs:174-176,283-284` uses raw `%s` without a newline. Embedded NUL strings also cannot be represented correctly through `%s`.
- **High:** Native parsing differs materially from interpreter semantics:
  - Interpreter: `src/interp.rs:298-301` trims whitespace, requires the entire value to parse, and rejects overflow.
  - Runtime: `runtime/aury_rt.c:55-69` rejects leading whitespace, accepts digit prefixes such as `12x`, and performs unchecked signed arithmetic that can overflow with undefined behavior.
  
  Thus `result.is_ok(i64.parse("12x"))`, whitespace inputs, and boundary values can produce different native and interpreter results.
- **High:** `lower_match` ignores the scrutinee LLVM/Aury type at `src/lower.rs:737-743`; bind patterns hardcode an `i64` slot and `Type::I64` at `src/lower.rs:753-759`. A bind over a pointer-backed value such as `str` would emit `store i64 <ptr-value>`, which is invalid LLVM IR and loses the bound type.
- **Medium:** The compile argument scanner drops every argument beginning with `--` at `src/main.rs:246-253`. With string parameters, a legitimate value such as `--help` is silently removed and subsequently causes an arity error. Only recognized CLI options should be consumed.
- **Medium:** Bool parameters are grouped with integers and parsed as `i64` at `src/lower.rs:240-266`; the existing run path accepts `true`/`false` in `src/main.rs:331-336`. Type-directed compile argument parsing and validation are needed.
- **Note:** `runtime/aury_rt.c` and `src/main.rs` are unchanged in the current source diff despite requiring integration work. No lowering tests were added or updated.
- **Note:** Numerous tracked `target/` build artifacts are modified/deleted. These should not be included with the source change.

### Tests needed

1. Update the numeric lowering test for the new string argument API.
2. End-to-end clang tests linking `runtime/aury_rt.c` for:
   - string literal return;
   - string parameter round-trip;
   - concat, equality, inequality, and length;
   - string-valued `if` and function calls.
3. Assert string globals occur outside function bodies and generated IR assembles.
4. Test quotes, backslashes, newlines, UTF-8, empty strings, and embedded NUL behavior.
5. Compare interpreter/native parsing for whitespace, trailing junk, sign-only input, `i64::MIN/MAX`, and overflow.
6. Test bool CLI arguments using `true` and `false`.
7. Test string arguments beginning with `--`.
8. Test bind-pattern matching over each supported scrutinee type, especially `str`, and assemble the resulting IR.
9. Compare exact native/interpreter stdout for i64, bool, and string entry results.