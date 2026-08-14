# Make the language server faster

- **Base commit**: `c563398` (`perf/lsp-latency-fixes`, branched from `claude/lsp-schemaless-errors-3e7f6c`)
- **Source**: the research pass of 2026-08-12, which measured 8 bottlenecks
- **Language**: this plan follows the ASD-STE100 rules and the terms in the `simplify-plan` skill

## Purpose

The language server spends about 95 percent of its time in one function. That function converts a byte offset to a line and a column. It scans the document from byte 0 on every call.

## Result

The plan is complete when all of these statements are true:

- `offset_to_position` does not scan the document from byte 0.
- The semantic token pass on a file of 4000 lines takes less than 20 ms. It took 3845 ms.
- `analyze_document` on a file of 3200 lines takes less than 60 ms. It took 6383 ms.
- The ranged semantic token pass for 40 lines takes less than 5 ms. It took 3860 ms.
- `token_at` and `word_range` on a file of 832 KB take less than 1 ms. They took 12.6 ms.
- `table_completion_items` with 800 tables takes less than 2 ms. It took 24.7 ms.
- `semantic_diagnostics` for one document with 200 documents open takes less than 1 ms. It took 9.0 ms.
- The `didChange` handler does not block the async reactor thread.
- `cargo test` passes.
- `cargo fmt --check` passes.

## Vocabulary

| Term | Type | Meaning in this plan |
|------|------|----------------------|
| `finding` | technical name | One numbered defect from the research report of 2026-08-12. There are 8. |
| `line index` | technical name | A `Vec<usize>` of the byte offset of each line start, plus one flag for an ASCII-only document. |
| `reactor` | technical name | The `tokio` thread that reads and writes LSP messages. |
| `debounce` | technical name | A delay that drops all but the last event in a group. |
| `compat golden` | technical name | The exact-equality test at `tests/compat.rs:19`, which pins the advertised capabilities. |
| `corpus sweep` | technical name | The test `cargo test --test conformance -- --ignored`, which runs over about 1900 SurrealDB files. |
| `baseline` | technical name | The measured times before any change in this plan. |
| `parse` | technical name | The `tree_sitter::Tree` for one document. |
| `latency` | technical name | The time between a client request and the server answer. |
| `to parse` | technical verb | To build a `tree_sitter::Tree` from text. |
| `to commit` | technical verb | To record a change in `git`. |
| `to refactor` | technical verb | To change the structure of code without a change to its behaviour. |
| `to absorb` | technical verb | To read one document analysis into the merged model, as `absorb_analysis` does. |
| `to profile` | technical verb | To measure where a program spends its time. |

## Before you start

1. Make sure that the branch is `perf/lsp-latency-fixes`.
2. Make sure that `git status` reports a clean tree.
3. Clone the grammar at the pinned reference `826d0c2ca6733a1c201ea7015dd91f439f67b573`.
4. Set `TREE_SITTER_SURREALQL_DIR` to the grammar directory for every `cargo` command.
5. Make sure that a SurrealDB checkout is at `../surrealdb`, or set `SURREALDB_DIR`.
   - NOTE: Without this checkout the corpus sweep prints `skipping` and proves nothing.
6. Read the research report for the measured numbers and the file references.

---

## Procedure

### Phase A — Prepare the measurement

NOTE: Phase A adds no fix. Phase A makes every later win measurable and protects it against a regression.

#### A1. Add a benchmark harness

1. Create the file `benches/latency.rs`.
2. Write one benchmark for each of the 7 measured operations in **Result**.
3. Build the synthetic input in the benchmark at sizes 200, 800, 3200 lines.
4. Add a `[[bench]]` section for `latency` to `Cargo.toml`.
   - Check: `cargo bench --no-run` compiles.
5. Commit the harness alone.
   - Check: `git log` shows one commit that changes no file under `src/`.

#### A2. Record the baseline

1. Run `cargo bench` on the clean tree.
2. Write the times to `docs/perf-baseline.md`.
   - Check: the file holds one number for each operation in **Result**.
3. Compare each number to the research report.
   - NOTE: A large difference means the machine differs. Use your own baseline, not the report.

---

### Phase B — Correct the position conversion

NOTE: Phase B corrects finding 01 and finding 08. Phase B gives about 95 percent of the total win. Do Phase B before every other phase.

#### B1. Add the line index type

1. Open `src/semantic/text.rs`.
2. Add a `LineIndex` struct that holds the line start offsets and one `all_ascii` flag.
3. Add a constructor that reads the source one time and records each line start.
4. Add a method `position(&self, source: &str, offset: usize) -> Position`.
5. Find the line in the constructor output with a binary search.
6. If `all_ascii` is true, return the byte column as the character column.
7. If `all_ascii` is false, count the UTF-16 units in the line only.
   - Check: the method never reads a byte before the start of the line.
8. Add a method `range(&self, source: &str, start: usize, end: usize) -> Range`.
9. Add a method `offset(&self, source: &str, position: Position) -> usize`.
   - NOTE: This method replaces `position_to_offset`, which has the same defect.

#### B2. Test the line index against the old functions

CAUTION: Write these tests before you change any call site. The old functions are correct, and they are the only specification for the new one.

1. Add a test that compares `LineIndex::position` to `offset_to_position` at every offset.
2. Run the test on ASCII text.
3. Run the test on text that holds a 4-byte character.
   - NOTE: The test at `src/semantic/text.rs:181` uses the character `₹`. Use a character outside the basic multilingual plane too.
4. Run the test on text that ends without a newline.
5. Run the test on empty text.
6. Add the same comparison test for `LineIndex::offset` against `position_to_offset`.
   - Check: `cargo test text` passes.

#### B3. Store the line index on the document

1. Open `src/semantic/types.rs`.
2. Add a `line_index: LineIndex` field to `DocumentAnalysis`.
3. Open `src/semantic/analyzer.rs`.
4. Build the line index one time in `analyze_document_with_limit`, after the parse.
5. Pass the line index to the extraction walk.
   - Check: `cargo build` succeeds.

#### B4. Use the line index in the analyzer

1. Replace every call to `byte_range_to_lsp` in `src/semantic/analyzer.rs` with `LineIndex::range`.
   - NOTE: There are 14 call sites in this file.
2. Replace every call to `offset_to_position` in `src/semantic/analyzer.rs`.
3. Replace the 3 call sites in `src/semantic/infer.rs`.
   - Check: `grep offset_to_position src/semantic/analyzer.rs` returns nothing.
4. Run `cargo test`.
   - Check: all tests pass. No diagnostic range moves.

#### B5. Use the line index in the semantic token pass

1. Open `src/semantic/highlight.rs`.
2. Change `push_span` to take the line index instead of only the source.
3. Replace the call to `offset_to_position` at line 235.
4. Pass the line index through `collect_absolute` and `walk`.
   - Check: `cargo test` passes.
5. Run `cargo bench`.
   - Check: the semantic token pass on 3200 lines is more than 100 times faster.

#### B6. Correct the cursor helpers

1. Open `src/semantic/text.rs`.
2. Rewrite `token_at` to find the cursor offset with the line index.
3. Walk outward from the cursor offset over the slice around the cursor only.
4. Delete the `char_indices().collect()` call in `token_at`.
5. Rewrite `word_range` the same way.
6. Rewrite `token_prefix` the same way.
   - Check: no function in this file allocates a vector of the whole document.
7. Run `cargo test`.
   - Check: all tests pass.

#### B7. Correct the completion context scans

1. Open `src/core/completion_context.rs`.
2. Replace the 4 calls to `position_to_offset` at lines 46, 194, 260 and 349.
3. Delete the 2 calls to `chars().collect()` at lines 198 and 262.
4. Read only the slice before the cursor in each of these functions.
   - Check: `cargo test` passes.

#### B8. Measure Phase B

1. Run `cargo bench`.
2. Compare each number to `docs/perf-baseline.md`.
   - Check: every operation in **Result** meets its target, except the 3 in Phase C.
3. Run the corpus sweep `cargo test --test conformance -- --ignored`.
   - NOTE: The sweep takes about 130 seconds. It reports 5 failures on a clean tree.
   - Check: the sweep reports the same 5 failures and no more.
4. Commit Phase B.

---

### Phase C — Correct the request-path scans

NOTE: Phase C corrects finding 02, finding 06 and finding 07. Each step in Phase C is independent. You can do them in any order.

#### C1. Limit the ranged semantic token pass to its range

1. Open `src/semantic/highlight.rs`.
2. Change `walk` to take the requested byte range.
3. Do not descend into a node whose byte range does not touch the requested range.
4. Convert the requested `Range` to a byte range with the line index first.
   - Check: `collect_semantic_tokens_range` returns the same tokens as before.
5. Run `cargo bench`.
   - Check: the ranged pass for 40 lines is faster than the full pass by more than 20 times.

#### C2. Add a field index to the merged model

NOTE: This step is done, but not as written here. It first added a separate
`fields_by_table: HashMap<String, Vec<String>>` map beside a flat `fields` map
keyed by a `(table, field)` tuple. That map is gone. `MergedSemanticModel::fields`
is now nested — `HashMap<String, HashMap<String, FieldDef>>` — so the outer map
*is* the index, and no separate map has to be kept in step. A tuple key also
cannot be borrowed from a pair of `&str`, so the flat map allocated two `String`s
on every read; the nested one allocates nothing. Read the current code before you
follow the steps below.

1. Open `src/semantic/types.rs`.
2. Add a `fields_by_table: HashMap<String, Vec<FieldDef>>` field to `MergedSemanticModel`.
3. Open `src/semantic/model.rs`.
4. Fill the field index in `absorb_analysis`, which already walks every field.
5. Change `fields_for_table` at line 220 to read the field index.
6. Keep the sort order that `fields_for_table` uses now: origin priority, then name.
   - Check: `cargo test` passes. Completion order does not change.

#### C3. Add a usage-count index to the merged model

1. Open `src/semantic/types.rs`.
2. Add a `target_usage: HashMap<String, usize>` field to `MergedSemanticModel`.
3. Open `src/semantic/model.rs`.
4. Count each target table one time in `MergedSemanticModel::build`.
5. Change `target_usage_count` at line 513 to read the index.
   - Check: `cargo test typo` passes. The single-use heuristic still fires.

CAUTION: Choose the length limit in step 7 so that no typo test fails. `jaro_winkler` scores above 0.86 only for close names.

6. Add a length filter before the `jaro_winkler` call at line 489.
7. Skip a candidate whose name length differs from the unknown name by more than 3.
   - Check: `cargo test` passes.

#### C4. Move the completion documentation to a resolve handler

CAUTION: This step changes the advertised capabilities. Update the compat golden at `tests/compat.rs:19` in the same commit. A separate commit leaves the test suite red.

1. Open `src/core/server.rs`.
2. Set `resolve_provider` to `Some(true)` at line 101.
3. Add a `completion_resolve` method to `LanguageServerCore`.
4. Open `src/semantic/model.rs`.
5. Remove the `documentation` field from the items in `table_completion_items` at line 162.
6. Put the table name in the item `data` field instead.
7. Build the hover text in `completion_resolve` for one item only.
8. Add the resolve handler to `src/native/backend.rs`.
9. Add the `completionItem/resolve` method to `src/core/dispatch.rs`.
10. Change `"resolveProvider": false` to `true` in the compat golden.
    - Check: `cargo test --test compat` passes.
11. Run `cargo bench`.
    - Check: `table_completion_items` with 800 tables takes less than 2 ms.
12. Commit Phase C.

---

### Phase D — Correct the architecture

NOTE: Phase D corrects finding 03, finding 04 and finding 05. The row H7 in `docs/pain-points.md` already tracks these 3 findings. Do Phase D last. The measurements are easier to read after Phase B removes the quadratic cost.

#### D1. Move the analysis off the reactor thread

CAUTION: Do not hold the `RwLock` write guard across an `await`. The guard is not `Send`, so `spawn_blocking` cannot take it.

1. Open `src/native/backend.rs`.
2. Wrap the body of `did_change` at line 65 in `tokio::task::spawn_blocking`.
   - NOTE: `initialized` at line 51 and `did_save` at line 71 already use this pattern.
3. Keep the state write on the async side of the call.
4. Run `cargo test --test core_server`.
   - Check: all tests pass.
5. Leave `src/core/dispatch.rs` unchanged.
   - NOTE: The `wasm32` target has one thread. There is nothing to move the work to.

#### D2. Add a debounce for the diagnostics

1. Open `src/config.rs`.
2. Add a `diagnostic_debounce_ms: u64` field to `AnalysisSettings` at line 82.
3. Set the default value to 200.
4. Open `src/core/server.rs`.
5. Store a document version and a generation counter for each open document.
6. Add the debounce delay to `upsert_open_document` before the publish call.
7. If a newer version arrives during the delay, drop the older result.
   - Check: a burst of 10 changes publishes diagnostics one time.
8. Add a test that sends 5 `didChange` messages and counts the published diagnostics.
   - Check: the count is less than 5.

#### D3. Make the merged model incremental

CAUTION: Add a test for a changed function body before step 7. The inference at `src/semantic/infer.rs:757` runs to a fixed point over 8 rounds. A stale entry gives a wrong hover type.

1. Open `src/semantic/model.rs`.
2. Split `MergedSemanticModel::build` into a per-document absorb step and a link step.
3. Keep the result of the absorb step for each document, keyed by `Uri`.
4. Re-absorb only the edited document in `recompute_model`.
5. Run the link step over the stored results.
   - NOTE: The link step builds `function_references`, `function_callers` and the inferred return types.
6. Keep `inferred_function_returns` between edits.
7. Clear the inferred return type of a function only when its definition changes.
8. Run `cargo test`.
   - Check: all tests pass.
9. Run `cargo bench`.
   - Check: the rebuild time does not grow with the document count.

#### D4. Change to incremental text sync

CAUTION: This step changes the advertised capabilities. Update the compat golden at `tests/compat.rs:19` in the same commit.

1. Open `src/core/server.rs`.
2. Change `TextDocumentSyncKind::FULL` to `TextDocumentSyncKind::INCREMENTAL` at line 99.
3. Change `did_change` to apply each content change to the stored text in order.
4. Convert each change range to a byte range with the line index.
5. Change `"textDocumentSync": 1` to `2` in the compat golden.
   - Check: `cargo test --test compat` passes.
6. Add a test that sends 3 partial changes, then reads the stored text.
   - Check: the text matches the same edits applied as one full change.

#### D5. Use the incremental parse

1. Open `src/semantic/analyzer.rs`.
2. Move the `Parser` out of `analyze_document_with_limit` at line 34.
3. Store one `Parser` for each open document, or one for each thread.
4. Call `Tree::edit` for each content change before you parse.
5. Pass the old tree as the second argument to `parser.parse` at line 36.
   - Check: `cargo test` passes.
6. Run the corpus sweep.
   - Check: the sweep reports the same 5 failures as the baseline.
7. Run `cargo bench`.
8. Commit Phase D.

---

### Phase E — Close out

#### E1. Update the audit document

1. Open `docs/pain-points.md`.
2. Change the status of row H7 from deferred to fixed.
3. Correct the file reference of row H7. It reads `src/core/server.rs:71`, which predates the core and adapter split.
4. Add a row for the position conversion defect, which H7 does not name.

#### E2. Record the result

1. Run `cargo bench` one last time.
2. Write the final times beside the baseline in `docs/perf-baseline.md`.
3. Add an entry to `CHANGELOG.md`.
   - Check: the entry names the measured change for each of the 8 findings.

#### E3. Open the pull request

1. Run `cargo fmt --check`.
2. Run `cargo test`.
3. Run `cargo test -p xtask`.
4. Build the `wasm32` target with `scripts/build-wasm.sh`.
   - NOTE: Pull request CI does not build `wasm32`. Row H10 records this gap.
   - Check: the build succeeds.
5. Open one pull request for each phase, in the order A, B, C, D, E.
   - NOTE: Phase B alone is worth a release. Do not wait for Phase D.

---

## If a problem occurs

- If a diagnostic range moves after Phase B, compare `LineIndex::position` to `offset_to_position` at the offset that fails.
- If the corpus sweep reports more than 5 failures, run `git stash` and measure the baseline again.
- If a hover type is wrong after step D3, clear the whole inferred return type map and measure the cost.
- If `cargo test --test compat` fails, read the failure message. The message tells you to update the golden only for a reviewed change.
- If the benchmark numbers do not improve after Phase B, profile one operation and count the calls to `offset_to_position`.

## Open questions

- Which debounce delay do you want? This plan uses 200 ms as the default. The value needs a real editor test.
- Do you want the line index on `DocumentAnalysis`, or in a separate cache? The field adds about 8 bytes for each line of every open document and every workspace document.
- Does the `wasm32` host need a debounce on its own side? The single thread makes the block worse there, and step D1 cannot help.
- Must the ranged semantic token pass return the same tokens as the full pass? Step C1 keeps the current behaviour, which includes a token whole when it overlaps the range.

## Terms not simplified

- Every path, command, flag, type name and function name stays exactly as it is. A changed literal is a wrong instruction.
- `quadratic` — the mathematical term for the growth this plan removes. There is no shorter accurate word.
- `binary search` — the name of the algorithm in step B1.
- `UTF-16` and `ASCII` — character encoding names fixed by the LSP specification.
- `async`, `reactor`, `debounce`, `resolve handler` — declared in **Vocabulary**.
- `jaro_winkler` — the exact function name in `src/semantic/model.rs`.
