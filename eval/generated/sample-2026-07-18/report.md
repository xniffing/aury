## Generation-reliability baseline — Aury vs Python

Recorded model **sample** on **2026-07-18**, 2 generation(s) per task per language. Generation is non-hermetic (a one-time offline model run); **scoring below is deterministic** — a replay of the committed fixtures through the gates.

| Metric | Aury | Python |
|------|:----:|:----:|
| First-shot valid (compiles/checks, 0 fixes) | 3/4 | 3/4 |
| Converged (accepted after ≥1 mechanical repair) | 1/4 | — |
| **Final oracle-correct** | **3/4** | **3/4** |

### Per task (Aury first-shot / converged / correct · Python correct)

| Task | Aury fs | Aury conv | Aury ok | Python ok |
|------|:------:|:--------:|:------:|:--------:|
| add | 1/2 | 1/2 | 2/2 | 1/2 |
| gcd | 2/2 | 0/2 | 1/2 | 2/2 |

### Threats to validity

- **Small sample, single model/date.** n = 4 Aury and 4 Python generations, model `sample`, 2026-07-18. Read as evidence with provenance, not a universal rate.
- **Familiarity bias favors Python.** Models have seen vast amounts of Python and almost no Aury, so this is a *conservative* test for Aury — a win despite the bias is a strong signal; a loss is honest.
- **The repair loop is Aury's treatment.** Python is scored first-shot only (no feedback round), isolating structured mechanical repair as the intervention.
