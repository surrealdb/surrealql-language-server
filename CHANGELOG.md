# Changelog

## 0.3.0 — 2026-07-24

Error-handling and diagnostics release. Everything the server used to
swallow silently — connection failures, malformed configuration,
workspace-scan truncation, host-callback failures — now reaches the
IDE, and syntax/semantic diagnostics carry precise ranges, stable
codes, and actionable wording. Full audit trail in
[`docs/pain-points.md`](docs/pain-points.md).

### Added

- **Stable diagnostic codes** on every diagnostic
  (`src/semantic/codes.rs`): `parse` (pre-existing), `unknown-table`,
  `unknown-field`, `permission-denied`, `permission-unknown`,
  `dynamic-target`. `Diagnostic.data` carries structured payloads
  (`{table, field?, suggestion?}`) and `relatedInformation` points at
  the relevant definition.
- **Typo detection that actually fires.** `CREATE prson …` next to
  `DEFINE TABLE person` now yields ``Unknown table `prson`. Did you
  mean `person`?`` with a one-click replace quick fix — this path was
  previously dead code because schema inference masked it. Unknown
  fields on SCHEMAFULL tables get the same treatment. Guarded against
  false positives (PR #18 review): singular/plural sibling names
  (`orders`/`order`) never trigger, names used in more than one
  statement are treated as deliberate tables, and detection stands
  down entirely while the live-metadata connection is failing (remote
  tables missing from the model would otherwise light up local
  near-misses in bulk).
- **Syntax-error hints**: misspelled keywords are detected against the
  grammar's own keyword list (``… Did you mean `FROM`?``); MISSING
  tokens name what was expected (``Expected `)`.``, ``Expected
  `THEN`.``) with visible (non-zero-width) squiggles.
- **Live-metadata failure surfacing**: SurrealDB connection, auth, and
  timeout errors now produce a `window/showMessage` toast (once per
  distinct failure set) plus per-error `window/logMessage` entries,
  and an INFO log when the connection recovers.
- **Configuration validation**: malformed `surrealql` settings
  payloads, unknown `metadata.mode` values, and unknown
  `activeAuthContext` names are reported via `window/logMessage`
  instead of being silently ignored. Misspelled setting *keys* are
  detected too (serde otherwise ignores unknown fields):
  ``unknown setting `connection.endpint` — did you mean `endpoint`?``.
  Warning sets are deduplicated — a persistently bad configuration
  logs once per distinct set, with an INFO line when the warnings
  resolve.
- **Workspace-scan reporting**: unreadable entries, oversized files,
  and the file-count cap are counted and summarized in the log; a
  toast fires when the cap truncates indexing.
- **`window/showMessage` support**: new `LspNotifier::show_message`
  (default impl falls back to `log_message`); WASM hosts can pass an
  optional `onShowMessage` callback — the existing three-callback
  constructor keeps working unchanged.
- **Native panic hook**: panics are printed to stderr (visible in the
  editor's output panel) before the process aborts.
- New test infrastructure: end-to-end core-server suite with recording
  mocks (`tests/core_server.rs`), native JSON-RPC wire tests
  (`tests/dispatch.rs`), and backwards-compatibility tripwires
  (`tests/compat.rs` — capabilities golden, config shape/alias/env
  tables, diagnostic identity).

### Changed

- **Semantic diagnostics anchor to the offending token** (the `prson`
  in `CREATE prson`), not the whole statement. Quick-fix edits
  therefore replace exactly the token.
- **Giant syntax squiggles are clamped**: a multi-line ERROR region is
  reported on its first line with the full extent in
  `relatedInformation`, and nested errors inside it are surfaced
  separately (capped at 100 diagnostics per document).
- **"Target could not be resolved statically" is quieter**: suppressed
  for `$parameter` and expression (function call / subquery / block)
  targets, and SELECT target extraction now understands the current
  grammar shape (previously *every* SELECT warned).
- **`workspace/didChangeConfiguration` merges** partial payloads over
  the in-flight settings instead of resetting them — a payload that
  omits the connection block no longer wipes the endpoint configured
  via `initializationOptions`. A `null` settings payload triggers a
  configuration pull. *(Behavior change for clients that relied on
  partial payloads resetting everything.)*
- **Unknown `metadata.mode` strings repair to the default**
  (`workspace+db`) with a warning instead of silently disabling all
  schema sources. *(Behavior change; the supported off-switches remain
  `enableLiveMetadata: false` and the explicit mode strings.)*
- **Unknown-field checks are restricted to SCHEMAFULL tables** —
  explicit SCHEMALESS tables legitimately accept ad-hoc fields. The
  builtin `id`/`in`/`out` fields are always allowed, and RELATE
  statements are exempt (their SET fields belong to the edge table,
  not the subject tables).
- **Settings and metadata application is serialized** behind an
  internal lock — concurrently spawned `didChangeConfiguration` /
  `didSave` handlers can no longer lose each other's updates or
  invert the reported metadata-connection status.
- WASM: diagnostics that fail to serialize are no longer published as
  `NULL` (the host keeps its previous set and the failure is logged);
  malformed notifications and skipped `replaceWorkspace` entries are
  logged; `onRequestConfiguration` failures are logged per cause.
- Versions realigned: crate and npm package both ship 0.3.0 (they had
  drifted to 0.2.0 / 0.2.1), so `serverInfo.version` matches npm.

### Compatibility

- LSP wire: capabilities unchanged; `Diagnostic.source` remains
  `"surreal-language-server"`; syntax code remains `"parse"`; the
  severity mapping is unchanged. New `code`/`data`/
  `relatedInformation` fields are standard optional LSP fields.
- Quick fixes now match on `Diagnostic.code` + `data`; the legacy
  message-text fallback is retained for **one release** and will be
  removed in 0.4.
- WASM JS API: constructor, method names, and JSON-RPC response
  strings are unchanged; `onShowMessage` is optional.
- Config: all existing shapes, aliases, defaults, and `SURREALDB_*`
  environment fallbacks are preserved (pinned by `tests/compat.rs`).
- Crate API: `LspNotifier` gained `show_message` with a default impl;
  `QueryFact` gained `#[serde(default)]` fields (`target_refs`,
  `field_refs`, `target_resolution`); `WorkspaceIndex` gained
  `scan_stats`. Exhaustive struct literals of these types need
  updating — hence the 0.3.0 bump.
