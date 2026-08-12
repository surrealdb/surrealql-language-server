# Latency baseline

The measured times before any change from `docs/perf-plan.md`.

- **Commit**: `871c225` (`perf/lsp-latency-fixes`), no file under `src/` changed
- **Command**: `cargo bench` (the `bench` profile inherits `opt-level = 'z'` from `[profile.release]`, so these numbers match the shipped binary)
- **Machine**: Apple M4 Max, rustc 1.94.0
- **Grammar**: `826d0c2c`, the pinned `GRAMMAR_REF`

NOTE: Read the microseconds-per-line column, not only the milliseconds. A linear pass holds its cost per line as the file grows. A quadratic pass multiplies it. The input sizes 200, 800 and 3200 are two clean quadruplings.

## Baseline

| Operation | Small | Medium | Large | µs/line trend | Target |
|-----------|-------|--------|-------|---------------|--------|
| `semantic_tokens_full` | 10.59 ms | 158.87 ms | **2510.31 ms** | 53 → 199 → 784 | 16 ms |
| `analyze_document` | 28.66 ms | 405.07 ms | **6309.34 ms** | 143 → 506 → 1972 | 60 ms |
| `semantic_tokens_range` (40 lines) | 10.48 ms | 158.95 ms | **2512.54 ms** | 52 → 199 → 785 | 5 ms |
| `token_at` + `word_range` | 0.15 ms | 0.60 ms | **2.44 ms** | 0.75 → 0.75 → 0.76 | 1 ms |
| `table_completion_items("")` | 0.09 ms | 1.30 ms | **21.53 ms** | 40 → 200 → 800 tables | 2 ms |
| `semantic_diagnostics`, declared targets | 0.30 ms | 0.29 ms | 0.30 ms | flat | 1 ms |
| `semantic_diagnostics`, undeclared targets | 0.68 ms | 2.32 ms | **8.90 ms** | 10 → 50 → 200 docs | 1 ms |
| `MergedSemanticModel::build` | 0.08 ms | 0.35 ms | 1.43 ms | 10 → 50 → 200 docs | none |

Small, medium and large are 200, 800 and 3200 lines for the document operations, and 10, 50 and 200 documents for the model operations.

6 of the 7 targets fail at the baseline.

## What the numbers say

`semantic_tokens_full`, `analyze_document` and `semantic_tokens_range` each quadruple their cost per line over the two quadruplings of input. That is finding 01.

`semantic_tokens_range` costs the same as `semantic_tokens_full` at every size. It asks for 40 lines. That is finding 02.

`token_at` and `word_range` hold a flat cost per line, so the pair is linear, not quadratic. The cost is still proportional to the whole file for a read of one token. That is finding 08. This is the one row where the failure is a constant factor and not a growth rate.

`semantic_diagnostics` is flat when the document declares the tables it queries. The cost grows with the workspace only when a target is undeclared, because that branch calls `target_usage_count` over every query fact and then runs `jaro_winkler` against every table. That is finding 06.

NOTE: The first version of this benchmark measured only the declared-target case. It reported a flat 0.29 ms and hid finding 06 completely. A benchmark that does not reach a code path proves nothing about that path.

## Differences from the research report

The research report of 2026-08-12 measured `semantic_tokens_full` on 4000 lines at 3845 ms, and this baseline measures 3200 lines at 2510 ms. The two agree: 3200 lines is 64 percent of 4000 lines, and a quadratic cost at 64 percent of the input is 41 percent of the time.

`analyze_document` at 200 lines reads 28.66 ms here against 16.45 ms in the report. The benchmark reports a mean over many runs, and the report timed one run.
