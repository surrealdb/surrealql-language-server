# Changelog

## 0.4.0 — unreleased

Type-inference release. Parameter annotations were being discarded
wholesale — `FunctionParam.type_expr` was *always* `None` — so nothing
could type-check a call. This release makes declared types real and uses
them.

The checker's failure mode is **silence**: anything the inference engine
cannot pin down yields `Unknown`, and the assignability relation turns
any doubt into a verdict that reports nothing. A noisy checker gets
switched off and then protects nobody.

### Added

- **Argument type checking** on `fn::` calls (`argument-type`,
  `argument-count`), ranged at the offending argument rather than the
  whole statement. Object literals report per property, so one bad field
  in six names that field:
  ``Argument 2 of `fn::doc::add`: property `line` expects
  `record<orderLine>`, found `int`.``
- **`undefined-variable`** with a did-you-mean. SurrealDB substitutes
  `NONE` for an unset parameter rather than failing, so a typo silently
  changes what a query means and nothing at runtime tells you.
  `analysis.externalParams` declares names the caller binds at runtime
  (SDK `.bind(…)`, Surrealist's variables panel).
- **`let-type`** when a `LET $x: T = v` value cannot satisfy `T`.
- **A binding table** for `LET`, function/closure parameters and `FOR`
  loop variables, with block scoping and shadowing. `$variable` hover
  and completion, neither of which existed.
- **Structural types**: `TypeExpr` gained `Object`, `Tuple`, `Set` and
  `Literal`, read from the grammar's own nodes. Builtin return types are
  parsed from the signature table, with `type::record`/`type::thing`
  narrowed to the table their argument names.
- **Build provenance** in `serverInfo.version`
  (`0.4.0 (branch-sha, grammar rev)`). The bare crate version is
  identical on a branch and on the published release, so it could not
  identify which binary an editor was talking to.
- `analysis.enableTypeChecking` (default on) and
  `analysis.externalParams`.

### Fixed

Each of these was a silent no-op: a node-kind constant naming a node the
grammar never emits compiles fine and simply disables whatever matches
on it.

- Function **parameter types** were always `None` — `ParamDefinition`
  puts a named `Colon` between the name and the type, so positional
  adjacency never matched. Measured on the test fixture: **0/64 → 64/64**
  extracted.
- **Return types** looked for a `ReturnsClause` node that does not exist.
- **`TYPE 'a' | 'b'` and `TYPE [string, string]`** were dropped, because
  the type-payload lookup omitted `UnionType` and `LiteralType`.
- **`LET`/`FOR`/`IF`/`RETURN` bodies were never descended into**, so
  nested statements and bindings were invisible to analysis. Combined
  with 0.3.0's tight diagnostic ranges, those statements now get
  diagnostics for the first time.
- Bindings inside `DEFINE EVENT … THEN { }` were never registered — the
  block sits inside a `ThenClause`, not as a direct child.
- Expression-bodied closures (`|$x| $x != 'Done'`) bound no parameters.
- **`record<a | b>`** parsed as one table literally named `a | b`, which
  then registered a phantom table.
- **Inlay hints** looked up `model.functions` with the `fn::` prefix
  stripped from a map keyed *with* it, so they never fired.
- 17 dead node-kind constants removed. `node_kind.rs` now documents that
  every constant must exist in `node-types.json`.

### Compatibility

- LSP wire: capabilities unchanged (pinned by `tests/compat.rs`);
  `Diagnostic.source` and the syntax `parse` code unchanged. The four new
  codes are additions.
- New diagnostics are ERROR severity and on by default; the kill switch
  is `analysis.enableTypeChecking`.
- Config: both new keys accept camelCase and snake_case, like every
  existing key, and are registered in the unknown-key sweep.
- `serverInfo.version` now carries a build suffix after the semver. No
  test pinned it; clients parsing it strictly should read the leading
  version.
- **Still outstanding:** 0.3.0 promised the legacy message-text quick-fix
  fallback in `unknown_table_payload` would be removed in 0.4. It is
  still present — removing it changes behaviour for clients that strip
  `Diagnostic.data`, and no test covers that path, so it wants a
  deliberate decision rather than being folded into this release.

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
