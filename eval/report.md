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
| effect-leak | type✗ | ✓ | 1 | 2/2 |  |
| vec-pipeline | ✓ | ✓ | 0 | 2/2 |  |
| calculator | ✓ | ✓ | 0 | 3/3 |  |
| unterminated | parse✗ | ✓ | 1 | 1/1 |  |
| false-property | intent✗ | ø | 0 | — | correctly rejected (intent gate) |

**10/10 outcomes as expected** · first-shot-valid 8 · rescued by repair 2 · oracle checks 15/15.
