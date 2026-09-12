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

---

# Result

Measured on the same machine and command after phases A to C, plus D1 and D2.

| Operation | Baseline | Now | Change | Target |
|-----------|----------|-----|--------|--------|
| `semantic_tokens_full`, 3200 lines | 2510.31 ms | **13.12 ms** | 191x | 16 ms ✅ |
| `analyze_document`, 3200 lines | 6309.34 ms | **69.70 ms** | 91x | 60 ms ❌ |
| `semantic_tokens_range`, 40 lines | 2512.54 ms | **0.65 ms** | 3849x | 5 ms ✅ |
| `token_at` + `word_range` | 2.44 ms | **0.0001 ms** | ~24000x | 1 ms ✅ |
| `table_completion_items`, 800 tables | 21.53 ms | **0.23 ms** | 93x | 2 ms ✅ |
| `semantic_diagnostics`, declared | 0.30 ms | 0.31 ms | — | 1 ms ✅ |
| `semantic_diagnostics`, undeclared | 8.90 ms | **9.47 ms** | none | 1 ms ❌ |
| `MergedSemanticModel::build`, 200 docs | 1.43 ms | 1.92 ms | — | none |
| Corpus sweep, 1897 files | ~130 s | **4.7 s** | 28x | none |

5 of the 7 targets pass. The cost per line is now flat for every document
operation, so the quadratic term is gone rather than reduced:

| Operation | µs/line at 200 / 800 / 3200 lines |
|-----------|-----------------------------------|
| `semantic_tokens_full` | 3.93 → 4.06 → 4.10 (was 53 → 199 → 784) |
| `analyze_document` | 21.17 → 21.69 → 21.78 (was 143 → 506 → 1972) |
| `semantic_tokens_range` | 0.91 → 0.34 → 0.20 (falls, because the walk is pruned) |

NOTE: `MergedSemanticModel::build` and the declared-target diagnostics read
slightly slower than at the baseline. Both now maintain an extra index
(`target_usage`), which costs a little on build to save a lot on lookup —
`table_completion_items` is 93 times faster.

NOTE: A field index was part of that trade too, as a separate
`fields_by_table` map. It no longer exists. `MergedSemanticModel::fields` is
now keyed `table → field → definition`, so the grouping it provided is the
outer map and a lookup allocates nothing. That, plus cloning a merge candidate
only when it wins, took `MergedSemanticModel::build` at 200 documents from
1.96 ms to 1.49 ms.

## All targets met

Both failing targets were closed. `cargo bench` exits 0.

| Operation | Baseline | After phases A-D | Final | Target |
|-----------|----------|------------------|-------|--------|
| `analyze_document`, 3200 lines | 6309.34 ms | 69.70 ms | **59.31 ms** | 60 ms ✅ |
| `semantic_diagnostics`, undeclared | 8.90 ms | 9.47 ms | **0.80 ms** | 1 ms ✅ |
| `analyze_document`, schema shape | not measured | not measured | **32.50 ms** | 60 ms ✅ |

CAUTION: `analyze_document` clears its budget by about 1 ms — measured 58.8, 58.8,
59.6 and 59.3 ms across four runs. That is a real gate on a 60 ms budget, so a
slower machine can fail it. If CI reports a failure there, the next lever is
`collect_node_diagnostics`, which still allocates one tree-sitter cursor per
non-leaf node; converting it to the reused-cursor form used by
`collect_descendants` is worth an estimated 2-3 ms. Do not relax the target
without doing that first.

## What closed the typo sweep

Two changes, and the second one corrects a claim this document used to make.

`find_nearest_explicit_table` read `tables.values()` and filtered on `explicit`
afterwards, so with most tables inferred from usage it walked 5000 ~230-byte
entries to reach 1000 candidates. `explicit_tables` holds the candidates
contiguously.

The previous text here said no *sound* prefilter prunes this corpus. **That was
wrong.** It assumed the worst-case Winkler prefix bonus. Winkler is
`J + 0.1p(1-J)` for `p = min(4, common prefix)`, strictly increasing in `J`, so
`JW > 0.86` iff `J > T(p)` — and `T(0) = 0.860` against `T(4) = 0.767`. A pair can
only pass if `m(a+b) > (3T(p)-1)ab`, with `m` bounded by `min(a,b)` and then by
the character-multiset intersection. Computing `p` exactly is what does the work:
on the benchmark cross-product survivors go from 20,000 to 88.

NOTE: A bound on the *difference* of the lengths is still unsound at any `p`.
`person` and `personaddress` differ by 7 characters and score 0.892. That
counterexample is pinned by a test.

## What closed analyze_document

The cost was never the parse (23.8 ms of the original 69.7) and it was not the
allocations either. Measured directly: a 70,000-node tree walks in 7.53 ms when
each node allocates its own cursor via `node.walk()`, and 4.25 ms with one reused
cursor — 3.3 ms per traversal, and the extraction path made several.

`collect_descendants` now drives one cursor instead of recursing with a fresh one
per node, and the helpers that iterate children return early for a node that has
none. Together: 69.70 -> 59.31 ms.

NOTE: Two estimates that did not survive measurement, recorded so nobody spends
the time again. Swapping `kind()` for `kind_id()` was projected at 4-8 ms; it
measured **0.36 ms** per traversal, so it was not done — `kind()` is far cheaper
than its strlen-plus-UTF-8-validation description suggests. And the isolated
allocation fixes (the discarded `TableDef`, the duplicated preview, the redundant
range conversions, the clones, `with_capacity`) were projected at 5-8 ms
*combined* and moved the number by less than the run-to-run noise. They are kept
because they are strictly less work, not because they were measurable.

## The schema-shape row

The all-SELECT rows could not see `leading_comment_text`, which ran
`source.lines().collect::<Vec<_>>()` — a whole-document scan — for every DEFINE
without a `COMMENT` clause, the common case in real schema files. The new
`analyze_document/schema` row covers that shape, and it is linear: 10.24 / 10.04 /
10.16 µs per line at 200 / 800 / 3200 lines.

## What phase D still owes

D1 and D2 are done. D3, D4 and D5 are not — see the entries in
`docs/pain-points.md`. Neither failing target needed them.


---

# Against master (0.7)

Both branches measured on the same machine with the same harness (`benches/latency.rs`
is byte-identical between them) each in its own shipped configuration: master on
grammar `cb2e6b5` at `opt-level = 'z'`, this branch on `373e7cd` at `opt-level = 3`.

| Operation | master | this branch | change |
|-----------|--------|-------------|--------|
| `analyze_document`, 3200 lines | 47.57 ms | **30.77 ms** | 1.55x |
| `analyze_document/schema`, 3200 lines | 27.28 ms | **16.37 ms** | 1.67x |
| `semantic_tokens_full`, 3200 lines | 10.21 ms | **6.57 ms** | 1.55x |
| `semantic_tokens_range`, 40 lines | 0.454 ms | **0.304 ms** | 1.49x |
| `table_completion_items`, 800 tables | 0.183 ms | **0.147 ms** | 1.24x |
| `semantic_diagnostics`, declared | 0.225 ms | **0.142 ms** | 1.58x |
| `semantic_diagnostics`, undeclared | 0.719 ms | **0.524 ms** | 1.37x |

## Where that came from, honestly

Almost all of it is the optimisation level. Running **this branch** at master's
`opt-level = 'z'` isolates the code changes:

| Operation | master (z) | this branch (z) | this branch (3) |
|-----------|-----------|-----------------|-----------------|
| `analyze_document` | 47.57 ms | 47.53 ms | 30.77 ms |
| `analyze_document/schema` | 27.28 ms | 27.06 ms | 16.37 ms |
| `semantic_tokens_full` | 10.21 ms | 9.86 ms | 6.57 ms |
| `semantic_tokens_range` | 0.454 ms | 0.469 ms | 0.304 ms |
| `table_completion_items` | 0.183 ms | 0.194 ms | 0.147 ms |
| `semantic_diagnostics` | 0.225 ms | 0.230 ms | 0.142 ms |
| `semantic_diagnostics`, undeclared | 0.719 ms | 0.684 ms | 0.524 ms |

At equal optimisation level the two branches are the same speed. The work added
in 0.7 (the depth guard, the pre-parse bracket count, two more reference
indexes, a `codeDescription` per diagnostic) costs nothing measurable. Three
rows read very slightly slower on this branch (0.454 → 0.469, 0.183 → 0.194,
0.225 → 0.230); all three are sub-millisecond operations where the difference is
within run-to-run variance, and all three are faster than master at the profile
that actually ships.

## What this harness cannot see

It parses every document from scratch, so it measures a **cold** analysis and
says nothing about the two changes users feel most:

* **Incremental parse.** One settled edit on an already-open buffer, measured
  through the real `analyze_document_incremental` path:

  | Document | Fresh analysis | After one edit | Saved |
  |----------|----------------|----------------|-------|
  | 200 lines (9 KB) | 1.60 ms | **1.13 ms** | 29% |
  | 800 lines (38 KB) | 6.21 ms | **4.07 ms** | 34% |
  | 3200 lines (156 KB) | 25.19 ms | **15.81 ms** | 37% |

  NOTE: the parse alone drops by about 95% (16.4 ms to 0.77 ms at 3200 lines),
  but the parse is only part of `analyze_document`: extraction and the syntax
  walk are full-document and gain nothing. **37% is the end-to-end number**, and
  it is the one to quote.

* **Incremental sync.** Not measurable here at all: it removes a 166 KB document
  crossing the wire and being JSON-unescaped on the reactor for every keystroke,
  which is paid before any code in this repository runs.

---

# Incremental sync and parse (0.7)

Measured on the same machine and harness, after the Phase 2 work.

## The release profile was the largest single win

`[profile.release]` carried `opt-level = 'z'`, which the benchmark inherited,
so every number this document recorded described a binary optimised for size,
which is not what the native binary wants. Same harness, same machine, the only
change being the optimisation level:

| Operation | `opt-level = 'z'` | `opt-level = 3` | Gain |
|-----------|-------------------|-----------------|------|
| `analyze_document`, 3200 lines | 46.39 ms | **28.68 ms** | 1.62x |
| `analyze_document/schema` | 26.21 ms | **15.73 ms** | 1.67x |
| `semantic_tokens_full` | 10.26 ms | **6.63 ms** | 1.55x |
| `semantic_diagnostics`, 200 docs | 0.228 ms | **0.144 ms** | 1.58x |

Larger than anything left in `docs/perf-plan.md`, for a one-line change. The
default now optimises for speed; `scripts/build-wasm.sh` sets
`CARGO_PROFILE_RELEASE_OPT_LEVEL=z` for the browser module, where a download is
a real cost. The native binary grows to about 9.3 MB.

## Incremental parse

A reparse against the previous tree, against a fresh parse of the same text:

| Document | Fresh parse | Incremental | Saved |
|----------|-------------|-------------|-------|
| 200 lines (9 KB) | 1.054 ms | **0.263 ms** | 0.79 ms (75%) |
| 800 lines (38 KB) | 4.024 ms | **0.408 ms** | 3.62 ms (90%) |
| 3200 lines (156 KB) | 16.449 ms | **0.765 ms** | 15.68 ms (95%) |

The parse is the part of `analyze_document` that incremental sync can remove;
the extraction and syntax walks are full-document and gain nothing.

NOTE: The plan gated this on a differential test, and was right to. Tree-sitter's
incremental reparse is not *guaranteed* to reproduce a fresh parse when the
previous tree held ERROR nodes, and ERROR/MISSING nodes are the `parse`
diagnostics, this server's primary output. Measured over SurrealDB's 1,894
parseable corpus files with ten random single-character edits each, including
inserted quotes and parens: **zero mismatches**. Pinned as
`incremental_reparse_matches_a_fresh_parse` in `tests/conformance.rs`; re-run it
after a grammar bump.

## What incremental sync actually saves

Not measured here, because no harness in this repository can: a 166 KB document
used to cross the wire and be JSON-unescaped into a fresh `String` on the reactor
thread for every keystroke. At ten characters a second that is 1.6 MB/s of
decoding before the debounce sees the message, and in the browser a full
JS-to-wasm string copy each time. It is paid before any code here runs.
