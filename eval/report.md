## Evaluation — repair convergence over the corpus

Deterministic (seed `0xC0FFEE`), reproduced by `aury eval eval/corpus.json`.

Columns: **First-shot** = the model's initial program passed type + intent checks with zero repairs; **Loop** = accepted (`✓`) / correctly rejected (`ø`, a deliberately-wrong spec) / failed (`✗`) after the closed loop; **Patches** = mechanical repairs the loop applied; **Oracle** = concrete input→output checks that passed.

| Task | First-shot | Loop | Patches | Oracle | Notes |
|------|:----------:|:----:|:-------:|:------:|-------|
| gcd | ✓ | ✓ | 0 | 2/2 |  |
| loop-factorial | ✓ | ✓ | 0 | 2/2 |  |
| mean-f64 | ✓ | ✓ | 0 | 1/1 |  |
| parse-classify | ✓ | ✓ | 0 | 2/2 |  |
| dice-effect | ✓ | ✓ | 0 | — |  |
| effect-leak | effect✗ | ✓ | 1 | 2/2 |  |
| vec-pipeline | ✓ | ✓ | 0 | 2/2 |  |
| alias-region | region✗ | ✓ | 1 | 1/1 |  |
| vec-use-after-move | region✗ | ✓ | 1 | 2/2 |  |
| calculator | ✓ | ✓ | 0 | 3/3 |  |
| unterminated | parse✗ | ✓ | 1 | 1/1 |  |
| false-property | intent✗ | ø | 0 | — | correctly rejected (intent gate) |

**12/12 outcomes as expected** · first-shot-valid 8 · rescued by repair 4 · oracle checks 18/18.

### First-shot failures by gate

**Converged** = the loop mechanically repaired the program to acceptance; **rejected✓** = a deliberately-wrong spec the loop correctly refused (true negative).

| Gate | first-shot fails | converged | rejected✓ |
|------|:----------------:|:---------:|:---------:|
| parse | 1 | 1 | 0 |
| effect | 1 | 1 | 0 |
| region | 2 | 2 | 0 |
| intent | 1 | 0 | 1 |
