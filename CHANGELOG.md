# Changelog

## 0.5.0 — unreleased

Engine-parity release. Two things the server claimed to do but did not:
completion ignored where the cursor was, and the builtin functions were
not type-checked at all. Both are now derived from the SurrealDB source
rather than from a hand-maintained table, and validated against
SurrealDB's own test corpus.

The last tag is `v0.3.0`, so 0.4.0 below never shipped on its own and
both sections land together. They are kept apart because 0.4.0 is a
self-contained story — declared types becoming real — that this release
builds directly on. The minor bump rather than a patch is required by the
breaking changes listed below.

The catalogue moves from 79 hand-written functions covering 2 of the 20
advertised namespaces to **434 generated from the engine**, with argument
types read from the implementations. `cargo xtask generate-builtins`
rebuilds it; a test fails when the committed file is stale.

The same doctrine as 0.4.0 applies, and mattered more here than
anywhere: a diagnostic that fires on working code costs far more than one
that never fires. Every new check was swept across all 1,897 files of
`language-tests/` before shipping. That sweep found five distinct
false-positive sources — four of them in code this release did not
otherwise touch — and the release ships with **zero false positives** and
51 diagnostics that match an error SurrealDB itself declares, in its own
words.

### Completion now respects the cursor

- `INFO FOR ` returned about 375 items, of which nine were legal: every
  keyword, every builtin and user function, and every table. It now
  returns exactly the nine targets the engine accepts. The cause was a
  single unguarded fallthrough in the completion handler, reached
  whenever the three positive gates above it missed — which was every
  statement form outside a nine-keyword allowlist.
- New `core::statement_shape`: a flat table of literal keyword prefixes
  covering the heads of `INFO FOR`, `USE`, `DEFINE`, `REMOVE`, `ALTER`,
  `REBUILD`, `SHOW CHANGES`, `ACCESS`, and the `ON`/`TYPE`/`PERMISSIONS`
  slots inside them, every entry transcribed from the parser that defines
  it.
- **No clause spine, deliberately.** Classifying a position inside
  `SELECT` needs to tell a clause keyword from a field of the same name,
  and SurrealQL accepts `SELECT order FROM t`. Guessing there hides
  fields, variables and functions in `WHERE … AND `, the busiest position
  in the language. Unrecognised positions keep the list they returned
  before, so the set of changed positions equals the set of table rows.
- `DEFINE PARAM` names and `DEFINE ANALYZER` names are offered. Both were
  already in the model — hover and go-to-definition resolved them — but
  nothing ever put them in the dropdown.

### Builtin functions are type-checked

- `string::len(42)` reports `argument-type`; `string::len('a', 'b')`
  reports `argument-count`. Neither reported anything before: the checker
  read `model.functions`, which holds only `DEFINE FUNCTION fn::…`, and
  returned early for every other name.
- Argument counts use the engine's own wording (`1 argument`,
  `2 to 3 arguments`, `zero or more arguments`).
- **An empty argument list is never reported.** `ArgumentList` is
  `seq('(', optional(…), ')')`, so `string::len()` parses clean, and every
  editor that closes brackets produces exactly that on the `(` keystroke.
- Method syntax is checked too — `{ a: 9 }.extend('9')` was previously
  invisible. The receiver counts as argument one, matching how the engine
  numbers the error.
- A signature the generator could not read is checked for nothing.
  `signature_known` keeps "unknown" distinct from "takes no arguments";
  without it every call to such a function would have been reported as
  expecting zero.

### What the structured parameters unlocked

- Hover answers for all 20 namespaces. `math::abs` used to answer nothing.
- Signature help covers all 434 functions, with parameters read from the
  catalogue instead of recovered by splitting a prose string on `,` —
  which covered 79 functions and broke on `array<string, 5>`.
- Inlay hints name builtin arguments. Suppressed for single-parameter
  calls, where `arg:` says nothing the reader cannot see.
- New `renamed-function` **warning** plus a rename quick fix, from the
  engine's own 62-pair table. `type::thing` → `type::record` is one.
- New `not-callable` **warning** for the nine names the parser accepts
  that no implementation backs in call form.

### Fixed

- **`DEFINE ACCESS` was never indexed.** The grammar wraps it in an
  `AccessDefinition` node, so the "second keyword child" lookup returned
  `None` and the extraction arm was unreachable — despite the arm, the
  type and the merge path all existing. `DEFINE SCOPE` was dead the same
  way.
- **`MIDDLEWARE fn::x()` was reported as a wrong argument count.** It
  registers a function; the API runtime supplies `(request, next)`. 33
  occurrences in SurrealDB's own corpus.
- **A parameter typed `any` was treated as required.** SurrealDB
  substitutes `NONE` for a missing argument when the declared type admits
  it, so `fn::any_arg()` is legal where `fn::one_arg()` is not.
- **An unparseable argument was counted as an argument.** The pinned
  grammar cannot read a closure (`|| 'x'`) or a signed decimal suffix
  (`-1.5dec`), both valid SurrealQL, so an `ERROR` node inflated the
  count. A call whose arguments contain a parse error is no longer
  checked.
- `TypeExpr::parse` had no `set<>` case, and read the length of
  `array<string, 5>` as part of the element type. Both silently disabled
  the check for those types.
- `docs/pain-points.md` listed three gaps that commit 345cc7a had already
  closed; corrected.

### Breaking

- `ColumnSlot::Loose` is removed. The completion handler never matched on
  it, so `WHERE`, `AND`, `OR` and `BY` already behaved as `None` and
  their behaviour is unchanged.
- `DocumentAnalysis` and `MergedSemanticModel` each gained an `analyzers`
  field. Both are constructed with struct-literal syntax, so downstream
  code that builds one needs the new field.
- `assign.rs` now reports a scalar flowing **into** a collection as
  unknown rather than incompatible. An aggregate hands a function the
  whole group where the source names one field, so `math::sum(price)`
  inside `AS SELECT … GROUP BY …` is valid; reporting it flagged
  SurrealDB's own view tests. The reverse direction is unchanged.
- Two new diagnostic codes, `renamed-function` and `not-callable`. Codes
  are wire-visible; clients matching on the existing set are unaffected.

### Testing

- `tests/conformance.rs` sweeps the whole SurrealDB corpus and asserts the
  exact set of diagnostics in both directions, so a new false positive and
  a check that silently stops working both fail. Ignored by default at
  about two minutes: `cargo test --test conformance -- --ignored`.
- `tests/fixtures/builtin_calls_valid.surql` holds 137 calls across 20
  namespaces, lifted verbatim from corpus files that expect no error, and
  runs everywhere — including where there is no SurrealDB checkout.
- `tests/generated_catalogue.rs` pins the catalogue's freshness against
  the generator, and the invariants the argument checks depend on.
- The suite grew from 245 tests to 340 in the crate, plus 28 for the
  generator and one ignored corpus sweep. That includes the first
  end-to-end completion tests: the handler previously had none.

### Known gaps

- The pinned tree-sitter grammar cannot parse `DEFINE SEQUENCE` at all,
  and has no `INFO FOR USER` or `INFO FOR INDEX` branch. Completion
  follows the engine, so six offered keywords (`GRAPHQL_ALIAS`,
  `GRAPHQL_DEPRECATED`, `SYSTEM`, `DISKANN`, `RETRY`, `MAXDEPTH`) draw a
  syntax error from the grammar if selected. Declared in
  `OFFERS_THE_GRAMMAR_CANNOT_PARSE`, with tests keeping the list honest.
- Ten callable functions have no readable signature and are checked for
  nothing: the seven `api::` middleware functions, whose leading arguments
  the runtime supplies, and `rand::float`/`int`/`time`, whose
  zero-or-two arity this catalogue cannot express.
- Method calls resolve by the convention `<receiver type>::<method>`, so
  the remapped ones (`<number>.round()` is `math::round`) are not checked.
  Generating the engine's 11 receiver tables would widen this.

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
