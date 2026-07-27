# Pain-Points Audit

- **Date**: 2026-07-24
- **Base commit**: `25dfce1714c2131b2c53495436b2768614e7f56d` (file/line references below are relative to this commit)
- **Method**: 6 area explorations (diagnostics pipeline, LSP surface, native runtime, WASM runtime, semantic coverage, tests/docs/CI), each finding adversarially re-verified against the code, plus a completeness pass. 96 findings confirmed, 9 candidates refuted.
- **Status column**: ✅ fixed in the 0.3.0 error-handling change · ⏳ deferred (tracked here) · ❌ won't-fix / by design.

## High severity

| # | Finding | Where | Status |
|---|---------|-------|--------|
| H1 | **Typo detection was dead code.** The analyzer infers a table/field from the very statement that misuses it, so the merged model always "knew" the typo'd name — the "Unknown table/field" diagnostics and the replace quick fix could never fire in the real pipeline. Unit tests passed only because they hand-built models without inference. | `src/semantic/model.rs:505`, `src/semantic/analyzer.rs:1224` | ✅ provenance-aware check + did-you-mean + relatedInformation + e2e test; hardened per PR #18 review (plural-sibling guard, single-use heuristic, stands down while metadata is degraded) |
| H2 | **SurrealDB connection/auth/timeout errors were never surfaced.** `LiveMetadataSnapshot.errors` was populated but read by nothing — a bad endpoint looked identical to an empty schema. | `src/native/metadata_db.rs:51` | ✅ `window/showMessage` toast (deduped per failure set) + per-error logs + recovery log |
| H3 | **Generic/leaky syntax errors.** Everything was "Invalid SurrealQL syntax near …"; MISSING nodes leaked grammar rule names with zero-width ranges; one typo smeared a single squiggle over the rest of the file. | `src/semantic/analyzer.rs:1261` | ✅ keyword did-you-mean hints, human `Expected …` names, ≥1-char MISSING ranges, first-line clamping + relatedInformation, nested-error surfacing, 100-diagnostic cap |
| H4 | **`panic = 'abort'` with no native panic hook.** Any panic killed the server with no trace (wasm had `console_error_panic_hook`; native printed nothing). | `Cargo.toml:63`, `src/main.rs` | ✅ stderr panic hook; `panic='abort'` kept deliberately (see comment in Cargo.toml); remaining production panic sites audited — none reachable from user input |
| H5 | **`didChangeConfiguration` wiped connection settings.** Partial payloads rebuilt settings from scratch instead of merging, silently killing live metadata until restart. | `src/core/server.rs:297` | ✅ merges over in-flight settings; `null` payload triggers a configuration pull |
| H6 | **WASM `onRequestConfiguration` failures were swallowed** — a throwing/rejecting/garbage-returning host left the server on defaults with zero feedback. | `src/wasm/notifier.rs:109` | ✅ distinct warning per failure mode via `onLogMessage` |
| H7 | Full re-parse + full workspace model rebuild on every keystroke; no debounce; tree-sitter incremental parsing unused (`parser.parse(text, None)`). | `src/core/server.rs:71` | ⏳ perf epic — needs incremental sync + per-document model invalidation |
| H8 | Builtin function catalog covers ~2 of 20 namespaces (`string::`, type functions); no `math::`, `array::`, `time::`, `rand::`, `crypto::`, `vector::`, … Hover/completion silent for most builtins. | `src/grammar.rs:60` | ⏳ generate the catalog from an authoritative SurrealDB source |
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
- Unknown-field warnings fired on explicit SCHEMALESS tables where ad-hoc fields are legal → restricted to SCHEMAFULL.

### Correctness / robustness (⏳ deferred unless noted)

- `signature_help` brittle text scan (`rfind('(')` + comma count) — breaks on nested calls/strings (`core/server.rs:564`).
- CRUD statements inside FOR/IF blocks never analyzed (`analyzer.rs:145`); INSERT produces no query facts (`analyzer.rs:116`); LET/FOR `$variable` scoping untracked (`analyzer.rs:565`); DEFINE ANALYZER/USER/NAMESPACE/DATABASE/MODEL/TOKEN/CONFIG unanalyzed (`analyzer.rs:61`).
- Rename/references/document-highlight only cover custom functions (`core/server.rs:524,536`); call-hierarchy `fromRanges` point at definitions, not call sites (`core/server.rs:712`); call hierarchy resolves items by bare name.
- Dead `analysis.*` config flags: `enable_permission_analysis`, `enable_code_actions`, `enable_aggressive_schema_inference` are accepted but never checked — the settings UI lies (`model.rs:481`, `core/server.rs:632`).
- `connection.access` accepted but never used for authentication (`config.rs:48`).
- WASM ignores `enable_live_metadata`/db mode where native honors them (`wasm/host_data.rs:103`); a db-only metadata mode wipes host-pushed workspace documents in the browser (`core/server.rs:203`).
- No `didChangeWatchedFiles` — externally created/deleted `.surql` files invisible until restart; live metadata reconnects and re-walks the whole DB on every save (`metadata_db.rs:73`).
- node-kind constants have no grammar-drift check (`node_kind.rs:14`); the grammar SHA is pinned in four places with no consistency check; setup script doesn't verify the checkout matches the pin.
- ✅ Cargo/npm version skew (0.2.0 vs 0.2.1) realigned at 0.3.0.
- ✅ README references to nonexistent files fixed; this document and `docs/grammar-gaps.md` created.
- ✅ No LSP-pipeline integration tests → `tests/core_server.rs` + `tests/dispatch.rs` + `tests/compat.rs`.
- TypeExpr drops record links inside unsupported generic/literal types (`type_expr.rs:27`).

## Low severity (one-liners, ⏳ unless marked)

Zero-width MISSING ranges ✅ · DiagnosticTag/codeDescription unused · no positionEncoding negotiation (UTF-16 assumed) · no request cancellation · missing formatting/folding/selection-range/pull-diagnostics/code-lens · text helpers allocate a per-call char-index Vec · client capabilities ignored at initialize · `shutdown()` no-op · symlinked workspace dirs never traversed · wasm outcome chosen by method not id · all-three-callbacks required in the wasm constructor (new `onShowMessage` is optional ✅) · grammar load failure is a silent total outage · code actions keyed to message text ✅ (code-based now) · DEFINE ACCESS keeps only the name · generic statements shown as EVENT symbols in the outline · stale comments in node_kind.rs/highlight.rs · precedence-guarded `.expect("checked above")` in highlight.rs ✅ (removed) · hand-maintained SPECIAL_VARIABLES · build.rs error omits the one-command fix · self-defeating tests that assert nothing · no clippy in CI · Cargo.toml lacks crates.io metadata · duplicate CI runs on PRs · README testing section omits the grammar prerequisite ✅ · `--no-default-features` passed but no features declared · over-broad token chars break hover on record ids.

## Refuted candidates (for the record)

Nine plausible findings did not survive adversarial verification, including: "analyze_document failure silently drops documents" (unreachable — tree-sitter always returns a tree), "the unknown-table quick fix replaces the whole statement" (the edit range is the diagnostic range, now token-tight), and "JSON-RPC batch requests unsupported" (batching is out of LSP scope).
