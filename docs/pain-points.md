# Pain-Points Audit

- **Date**: 2026-07-24
- **Base commit**: `25dfce1714c2131b2c53495436b2768614e7f56d` (file/line references below are relative to this commit)
- **Method**: 6 area explorations (diagnostics pipeline, LSP surface, native runtime, WASM runtime, semantic coverage, tests/docs/CI), each finding adversarially re-verified against the code, plus a completeness pass. 96 findings confirmed, 9 candidates refuted.
- **Status column**: ✅ fixed in the 0.3.0 error-handling change · ⏳ deferred (tracked here) · ❌ won't-fix / by design.

## Resolved since this audit

This document was written at `25dfce17`. The cycle that added the rule registry,
the command-line mode, the formatter and incremental sync closed most of it. The
tables below keep their original wording with the status corrected inline; this
list is the summary.

**High severity.** H10 (WASM never built in PR CI) ✅. H12 (release jobs not
gated on tests) ✅. H15 (incremental sync needs ordered edits) ✅ — the edit is
applied on the reactor and only the analysis is spawned. H7's remaining rows D4
(incremental sync) and D5 (incremental parse) ✅. H11 partly: `metadata_db` and
`workspace_fs` now have unit tests; the WASM JS surface still has none. H14 (the
merged-model rebuild) ⏳ **measured and deliberately not changed** — the build is
1.5 ms at 200 documents, and it is the change whose failure mode is a silently
wrong hover type.

**Also fixed, and not in this document because nobody had found them yet:**

- **Every `PERMISSIONS` clause mis-parsed.** `parse_permission_rule` read only
  the direct children of `PermissionsForClause`, but the actions and the mode
  live inside a `PermissionGroup`; `FULL` is a `Literal` node and `NONE` a
  `None` node, neither of which is a keyword. Every form resolved to
  `actions: [Execute], mode: Expression(<whole clause>)`, so `permission-denied`
  could never fire, `PERMISSIONS FULL` never granted, and every table with a
  clause reported `permission-unknown`. The existing unit test passed because it
  hand-built the `TableDef` — the same shape as H1's dead typo detection.
- **`unknown-field` reached only `SET` and `CONTENT`.** A misspelled column in a
  `SELECT`, `WHERE`, `GROUP BY`, `ORDER BY`, `SPLIT`, `FETCH` or `OMIT` was
  silent.
- **`fn::does_not_exist()` was silent**, as was any misspelled builtin.
- **`REMOVE TABLE person` left `person` in the model**, so every later query
  against it looked fine.
- **A client sending only `rootUri`** got an empty workspace-folder list, so
  nothing was walked and no `surrealql.toml` was ever found.
- **`didChangeConfiguration` discarded the project config file** entirely.
- **Call hierarchy listed a caller twice** if it called twice.

**Low severity, resolved:** `positionEncoding` is stated explicitly ·
`DiagnosticTag::UNNECESSARY` is used by the unused-code rules ·
`codeDescription` links every diagnostic to its rule page · formatting, folding
range, selection range and pull diagnostics all exist · client capabilities are
read · clippy runs in CI. **Still open:** code lens · `connection.access` unused
· `shutdown()` no-op · symlinked directories not traversed · WASM ignores
`enable_live_metadata` · grammar load failure is a silent total outage.

## High severity

| # | Finding | Where | Status |
|---|---------|-------|--------|
| H1 | **Typo detection was dead code.** The analyzer infers a table/field from the very statement that misuses it, so the merged model always "knew" the typo'd name — the "Unknown table/field" diagnostics and the replace quick fix could never fire in the real pipeline. Unit tests passed only because they hand-built models without inference. | `src/semantic/model.rs:505`, `src/semantic/analyzer.rs:1224` | ✅ provenance-aware check + did-you-mean + relatedInformation + e2e test; hardened per PR #18 review (plural-sibling guard, single-use heuristic, stands down while metadata is degraded) |
| H2 | **SurrealDB connection/auth/timeout errors were never surfaced.** `LiveMetadataSnapshot.errors` was populated but read by nothing — a bad endpoint looked identical to an empty schema. | `src/native/metadata_db.rs:51` | ✅ `window/showMessage` toast (deduped per failure set) + per-error logs + recovery log |
| H3 | **Generic/leaky syntax errors.** Everything was "Invalid SurrealQL syntax near …"; MISSING nodes leaked grammar rule names with zero-width ranges; one typo smeared a single squiggle over the rest of the file. | `src/semantic/analyzer.rs:1261` | ✅ keyword did-you-mean hints, human `Expected …` names, ≥1-char MISSING ranges, first-line clamping + relatedInformation, nested-error surfacing, 100-diagnostic cap |
| H4 | **`panic = 'abort'` with no native panic hook.** Any panic killed the server with no trace (wasm had `console_error_panic_hook`; native printed nothing). | `Cargo.toml:63`, `src/main.rs` | ✅ stderr panic hook; `panic='abort'` kept deliberately (see comment in Cargo.toml); remaining production panic sites audited — none reachable from user input |
| H5 | **`didChangeConfiguration` wiped connection settings.** Partial payloads rebuilt settings from scratch instead of merging, silently killing live metadata until restart. | `src/core/server.rs:297` | ✅ merges over in-flight settings; `null` payload triggers a configuration pull |
| H6 | **WASM `onRequestConfiguration` failures were swallowed** — a throwing/rejecting/garbage-returning host left the server on defaults with zero feedback. | `src/wasm/notifier.rs:109` | ✅ distinct warning per failure mode via `onLogMessage` |
| H7 | Full re-parse + full workspace model rebuild on every keystroke; no debounce; tree-sitter incremental parsing unused (`parser.parse(text, None)`). | `src/core/server.rs:380` (`did_change`), `src/semantic/analyzer.rs:36` | 🟡 partly fixed — **and this row named the wrong causes.** Measured on c563398: the full re-parse is 23.8 ms for a 166 KB file and the model rebuild 1.9 ms at 200 documents, against **6309 ms** for one `analyze_document` on that file. The dominant cost was never here; it was H13. ✅ debounce (`analysis.diagnosticDebounceMs`, default 200) + per-edit analysis moved to `spawn_blocking` + a per-document version so a superseded edit is dropped. ⏳ still open: incremental sync (D4), incremental parse (D5), incremental model (D3) — see H14, H15 |
| H13 | **Position conversion rescanned the document from byte 0 on every call.** `offset_to_position` walked `char_indices()` from the start of the file to convert one offset, and it is called once per emitted semantic token and about ten times per extracted query fact. That made `collect_semantic_tokens` and `analyze_document` quadratic in file size: a 3200-line file took 2510 ms to highlight and 6309 ms to analyze, walking 2.5 GB of text to answer conversions alone. H7 did not mention it. | `src/semantic/text.rs:13` | ✅ `LineIndex` — line starts recorded once, binary search, ASCII fast path, cached on `DocumentAnalysis`. 191x on the highlight pass, 91x on `analyze_document`, corpus sweep 130 s → 4.7 s. Differential tests against the old functions at every offset and position |
| H14 | The merged-model rebuild cannot be made incremental without dependency tracking. `infer_function_return_types` is **89%** of `MergedSemanticModel::build` (9.7 ms of 9.7 ms at 200 documents with functions, against 1.1 ms for the same corpus with none), and it reads the model in 5 places — so every function's inferred return type depends on the whole model, which depends on every document. Caching it per document is unsound: the failure mode is a wrong hover type, silently. | `src/semantic/infer.rs:757`, `src/semantic/model.rs:29` | ⏳ needs a design decision, not a mechanical change. Either track per-function dependencies (salsa-style) or accept the rebuild. Do not ship a document-keyed cache |
| H15 | Incremental text sync (`TextDocumentSyncKind::INCREMENTAL`) requires the notification path to apply edits **in order**, but `did_change` is now spawned so the debounce can coalesce a burst. With full sync, out-of-order delivery is harmless because every message carries the whole document and the version decides the winner. With incremental sync it corrupts the buffer. | `src/core/server.rs:99`, `src/native/backend.rs:65` | ⏳ split the path: apply the edit synchronously and in order, then spawn only the debounced analysis. Needs the authoritative buffer text held separately from `DocumentAnalysis`. Prerequisite for D5 |
| H8 | Builtin function catalog covers ~2 of 20 namespaces (`string::`, type functions); no `math::`, `array::`, `time::`, `rand::`, `crypto::`, `vector::`, … Hover/completion silent for most builtins. | `src/grammar.rs:146` (`BUILTIN_FUNCTIONS`) | ✅ generated from the engine: 434 functions with argument types (`1e24b8c`) and return types (this release), read out of `syn/parser/builtin.rs`, `fnc/` and `exec/function/builtin/`. `cargo xtask generate-builtins` rebuilds; CI now runs the freshness check. The 79 curated entries stay for prose and for the types the engine's macros cannot spell |
| H9 | crates.io crate unbuildable downstream — the grammar isn't vendored into the published crate, so `cargo install surrealql-language-server` hits the build.rs panic. | `build.rs:15` | ⏳ vendor pinned grammar sources or stop publishing the crate |
| H10 | WASM target never compiled in PR CI (only on release tags) — npm-package breakage is invisible until release. | `.github/workflows/ci.yml:108` | ⏳ add a `wasm-check` job to the PR path |
| H11 | Zero tests for wasm dispatch, metadata_db, workspace_fs, notifiers, core server. | `tests/` | ✅ partially: shared mock harness (`tests/common/`), end-to-end core-server suite, native JSON-RPC dispatch suite, compat suite. ⏳ remaining: `wasm-bindgen-test` for the JS surface, metadata_db/workspace_fs unit tests |
| H12 | Release jobs not gated on tests/fmt — a red test job doesn't block publishing binaries/crate/npm. | `.github/workflows/ci.yml:35` | ⏳ add `needs: rust` to release/publish/wasm jobs |

## Medium severity

### Error handling & messages (all ✅ fixed in 0.3.0)

- Whole-statement diagnostic ranges → token-tight ranges via `QueryFact.target_refs`/`field_refs`.
- No `Diagnostic.code` on semantic diagnostics; quick fixes matched on message *text* → stable code registry (`src/semantic/codes.rs`) + `data` payloads; code actions match code+data with the legacy string fallback kept for one release (remove in 0.4).
- No relatedInformation / did-you-mean → both added for unknown-table/unknown-field.
- Malformed settings JSON silently dropped (`config.rs:235`) → parse errors reported via `window/logMessage`; misspelled setting *keys* swept against known-key lists with did-you-mean (PR #18 review); warning sets deduplicated per distinct signature.
- Unknown `metadata.mode` silently disabled all schema sources (`config.rs:55`) → warns and repairs to the default (`workspace+db`). **Deliberate behavior change**, see CHANGELOG.
- Unknown `activeAuthContext` silently fell back to the first context (`config.rs:217`) → warns.
- Workspace scan swallowed walkdir errors and silently skipped >2 MB files / the 5,000-file cap / non-UTF8 reads (`workspace_fs.rs:63,74`) → counted in `WorkspaceScanStats`, summarized in the log, toast when the cap is hit.
- WASM published `NULL` diagnostics on serialization failure (`wasm/notifier.rs:73`) → keeps the previous set and logs; throwing host callbacks logged.
- Malformed notification params vanished (`wasm/dispatch.rs:63`) → logged with the method name (dispatch now lives in `src/core/dispatch.rs`, natively tested).
- `replaceWorkspace` silently skipped invalid URIs (`wasm/server.rs:115`) → logged per entry.
- "Target could not be resolved statically" fired on legitimate `$param`/expression targets (`model.rs:491`) → `TargetResolution` classification suppresses those. Also fixed: **SELECT target extraction found nothing on the current grammar** (the `FromClause` node no longer exists), so *every* real SELECT warned — extraction now reads the post-`FROM` region directly.
- Unknown-field warnings fired on explicit SCHEMALESS tables where ad-hoc fields are legal → restricted to SCHEMAFULL. Superseded: `analysis.schemalessDiagnostics` now decides this, plus `field-type`/`unknown-type`/`permission-*`, across three values (`quiet` default, `errors`, `strict`). The SCHEMAFULL-only rule is what `strict` restores.

### Correctness / robustness (⏳ deferred unless noted)

- ~~`signature_help` brittle text scan~~ ✅ replaced by a bracket-and-string-aware scan; the callee name is read by scanning back over name characters rather than splitting on whitespace, which used to pick up the enclosing call's text. Still a scan rather than a tree walk, deliberately: signature help is most useful on the `(` keystroke, and the grammar has no call node yet at that moment.
- ~~CRUD statements inside FOR/IF blocks never analyzed~~ ✅ **stale row** — `collect_statements` descends into `LET`/`FOR`/`IF`/`RETURN`/`THROW` bodies. ~~INSERT produces no query facts~~ ✅ **stale row** — it has a `QueryAction::Create` arm. ~~LET/FOR `$variable` scoping untracked~~ ✅ **stale row** — `BindingTable` scopes `LET`, `FOR`, function and closure parameters by byte range. DEFINE ANALYZER/USER/NAMESPACE/DATABASE/MODEL/TOKEN/CONFIG unanalyzed — ✅ each now gets a named outline entry with a real `SymbolKind`; still no structured analysis of their bodies.
- ~~Rename/references/document-highlight only cover custom functions~~ ✅ they reach tables, fields and parameters, honour `includeDeclaration`, distinguish WRITE from READ, and refuse to rename a `Remote` symbol. ~~Call-hierarchy `fromRanges` point at definitions~~ ✅ they point at the call sites, items carry identity in `data`, and a caller that calls twice is listed once with two sites.
- Dead `analysis.*` config flags — ✅ resolved. `enable_code_actions` empties the code-action response (the capability stays advertised, because a client reads that once). `enable_aggressive_schema_inference` is documented as inert and kept, because `tests/compat.rs` pins its parsing, its default and its zero-warning behaviour.
- `connection.access` accepted but never used for authentication (`config.rs:48`).
- WASM ignores `enable_live_metadata`/db mode where native honors them (`wasm/host_data.rs:103`); a db-only metadata mode wipes host-pushed workspace documents in the browser (`core/server.rs:203`).
- ~~No `didChangeWatchedFiles`~~ ✅ registered at startup for `**/*.surql` and `**/*.surrealql`, with an open buffer always winning. ⏳ live metadata still reconnects and re-walks the whole DB on every save.
- node-kind constants have no grammar-drift check (`node_kind.rs:14`). ~~The grammar SHA is pinned in four places with no consistency check~~ ✅ `the_grammar_pin_is_the_same_everywhere` in `tests/compat.rs` checks all six. ⏳ the setup script still does not verify the checkout matches the pin.
- ✅ Cargo/npm version skew (0.2.0 vs 0.2.1) realigned at 0.3.0.
- ✅ README references to nonexistent files fixed; this document and `docs/grammar-gaps.md` created.
- ✅ No LSP-pipeline integration tests → `tests/core_server.rs` + `tests/dispatch.rs` + `tests/compat.rs`.
- TypeExpr drops record links inside unsupported generic/literal types (`type_expr.rs:27`).

## Low severity (one-liners, ⏳ unless marked)

Zero-width MISSING ranges ✅ · DiagnosticTag/codeDescription unused · no positionEncoding negotiation (UTF-16 assumed) · no request cancellation · missing formatting/folding/selection-range/pull-diagnostics/code-lens · text helpers allocate a per-call char-index Vec · client capabilities ignored at initialize · `shutdown()` no-op · symlinked workspace dirs never traversed · wasm outcome chosen by method not id · all-three-callbacks required in the wasm constructor (new `onShowMessage` is optional ✅) · grammar load failure is a silent total outage · code actions keyed to message text ✅ (code-based now) · DEFINE ACCESS keeps only the name · generic statements shown as EVENT symbols in the outline · stale comments in node_kind.rs/highlight.rs · precedence-guarded `.expect("checked above")` in highlight.rs ✅ (removed) · hand-maintained SPECIAL_VARIABLES · build.rs error omits the one-command fix · self-defeating tests that assert nothing · no clippy in CI · Cargo.toml lacks crates.io metadata · duplicate CI runs on PRs · README testing section omits the grammar prerequisite ✅ · `--no-default-features` passed but no features declared · over-broad token chars break hover on record ids.

## Refuted candidates (for the record)

Nine plausible findings did not survive adversarial verification, including: "analyze_document failure silently drops documents" (unreachable — tree-sitter always returns a tree), "the unknown-table quick fix replaces the whole statement" (the edit range is the diagnostic range, now token-tight), and "JSON-RPC batch requests unsupported" (batching is out of LSP scope).
