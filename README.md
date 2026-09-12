# SurrealQL Language Server

A Language Server Protocol (LSP) implementation for [SurrealQL](https://surrealdb.com/docs/surrealql), the query language of [SurrealDB](https://surrealdb.com).

## Features

- Syntax diagnostics via tree-sitter
- Semantic analysis with schema inference from DDL and query flow
- Hover with type info, permission posture, function signatures, and language badges (SurrealQL vs JavaScript)
- Contextual completions for `record<table>` types, field names, builtin functions, and statement keywords
- Go-to definition and references for tables, fields, functions, and params
- Safe rename of local function definitions
- Code actions for missing `PERMISSIONS` clauses
- Signature help for builtin and user-defined functions
- Call hierarchy with inbound/outbound function call tracking
- Document symbols outlining tables, fields, events, indexes, and functions
- Folding ranges for statements, blocks, object/array literals and comment runs
- Selection ranges, so expand-selection walks the syntax tree
- `function() { ... }` bodies parse cleanly with no false diagnostics; `DEFINE FUNCTION` bodies containing scripting functions are detected and labelled as JavaScript

## Requirements

The language server compiles against a [tree-sitter SurrealQL grammar](https://github.com/surrealdb/surrealql-tree-sitter) that must be checked out as a sibling directory:

```
parent/
├── surrealql-language-server/   ← this repo
└── surrealql-tree-sitter/       ← grammar (sibling checkout)
```

Run the setup script to clone or update the grammar:

```bash
bash scripts/setup-grammar.sh
```

Or set `TREE_SITTER_SURREALQL_DIR` to point to an existing checkout:

```bash
TREE_SITTER_SURREALQL_DIR=/path/to/surrealql-tree-sitter cargo build
```

The grammar is **pinned**, because the analysis layer is coupled to its node
kinds. The revision lives in [`grammar.pin`](grammar.pin) and nowhere else: the
setup script, the CI checkout steps and `build.rs` all read it, and the build
fails with the fixing command when a checkout has drifted off it. Bump it
deliberately alongside any [`src/semantic/node_kind.rs`](src/semantic/node_kind.rs)
change, and run the conformance sweep as well as the suite.

The shapes the grammar still cannot parse are listed in
[`docs/grammar-gaps.md`](docs/grammar-gaps.md). None of them is valid SurrealQL
at the current pin: every `parse` error the server reports is a real syntax
error.

## Building

### Native binary

```bash
cargo build --release
# binary at: target/release/surrealql-language-server
```

### Browser WASM package

Build the wasm-bindgen npm package (outputs to `pkg/`):

```bash
bash scripts/build-wasm.sh
```

Requirements:

- `wasm-bindgen` CLI (`cargo install wasm-bindgen-cli --version 0.2.108`)
- `wasm-opt` (`cargo install wasm-opt`)
- On macOS, a wasm-capable clang (e.g. `brew install llvm`; the script auto-detects Homebrew LLVM)

#### Browser hosts (Surrealist, etc.)

Initialize the module with `fetch` + `arrayBuffer`, then pass the bytes to the default export — the same pattern used by [`@surrealdb/wasm`](https://github.com/surrealdb/surrealdb.js/tree/main/packages/wasm). Avoid passing a URL string directly to `init()` when the build pipeline pre-gzips `.wasm` assets in place: browsers only gunzip automatically when the response carries `Content-Encoding: gzip` (S3 production uploads do; many static preview servers do not).

```ts
import init, { WasmLanguageServer } from "@surrealdb/surrealql-language-server";
import wasmUrl from "@surrealdb/surrealql-language-server/surrealql_language_server_bg.wasm?url";

const wasmCode = await fetch(wasmUrl).then((response) => response.arrayBuffer());
await init({ module_or_path: wasmCode });

const server = new WasmLanguageServer({ /* callbacks */ });
```

The `./surrealql_language_server_bg.wasm` export is declared in `pkg/package.json` for bundlers that resolve deep imports.

## Testing

Tests need the grammar sibling checkout (see Requirements above) —
run `bash scripts/setup-grammar.sh` or set `TREE_SITTER_SURREALQL_DIR`
first, then:

```bash
cargo test
```

Two suites additionally want a SurrealDB checkout at the revision
[`surrealdb.pin`](surrealdb.pin) names: the catalogue freshness check and the
conformance sweep over SurrealDB's own corpus:

```bash
bash scripts/setup-surrealdb.sh   # or: SURREALDB_DIR=/path cargo test
cargo test --test conformance -- --ignored   # the ~1,900-file sweep, about 4s
```

Without a checkout both skip and say so. With one at a *different* revision they
report version skew as though it were a defect, so each prints which revision it
read when the two disagree.

## Repository Layout

```text
.
├── src/
│   ├── main.rs               # LSP stdio entry point (+ panic hook)
│   ├── config.rs             # workspace settings (+ validation warnings)
│   ├── grammar.rs            # tree-sitter language binding, curated builtin prose
│   ├── grammar_generated.rs  # @generated builtin catalogue — do not edit by hand
│   ├── core/
│   │   ├── server.rs         # transport-agnostic request handlers
│   │   ├── dispatch.rs       # JSON-RPC dispatch table (shared with WASM)
│   │   ├── client.rs         # LspNotifier / WorkspaceLoader / MetadataProvider traits
│   │   ├── state.rs          # shared server state
│   │   └── completion_context.rs
│   ├── native/               # tower-lsp adapter, walkdir loader, SurrealDB metadata,
│   │                         #   and the headless `check` subcommand (check.rs)
│   ├── wasm/                 # wasm-bindgen adapter (Surrealist)
│   └── semantic/
│       ├── analyzer.rs       # document analysis (parse + extract + syntax diagnostics)
│       ├── model.rs          # merged workspace model, semantic diagnostics, code actions
│       ├── codes.rs          # stable Diagnostic.code registry
│       ├── types.rs          # DocumentAnalysis, TableDef, FunctionDef, ...
│       ├── type_expr.rs      # SurrealQL type expression parser
│       └── text.rs           # LSP range utilities
├── xtask/                    # code generator (see Builtin Function Catalogue)
│   └── src/
│       ├── engine_tables.rs  # names, dispatch and rename tables
│       ├── signatures.rs     # argument types, from the `fnc/` implementations
│       ├── returns.rs        # return types, from the function registry
│       ├── methods.rs        # method receiver tables
│       ├── probe.rs          # `verify-returns`: checks the engine by running it
│       ├── emit.rs           # joins them and renders the catalogue as Rust
│       └── emit_json.rs      # renders the same catalogue as builtins.json
├── tests/
│   ├── lsp.rs                # analyzer/model integration tests
│   ├── core_server.rs        # end-to-end server tests (mock notifier)
│   ├── dispatch.rs           # JSON-RPC wire tests
│   ├── check.rs              # end-to-end tests for the check subcommand
│   ├── conformance.rs        # silence sweep over SurrealDB's own corpus
│   ├── generated_catalogue.rs # catalogue freshness + shape invariants
│   ├── compat.rs             # backwards-compatibility tripwires
│   └── common/               # shared mocks for the three boundary traits
├── docs/
│   ├── ai-plan.md            # opportunity map for the AI-agent surfaces
│   ├── grammar-gaps.md       # known gaps at the pinned grammar revision
│   ├── pain-points.md        # audited pain-point catalog + status
│   ├── perf-baseline.md      # latency baseline measurements
│   └── perf-plan.md          # latency targets `cargo bench` gates on
├── AGENTS.md                 # agent-facing contract: check loop, codes, gaps
├── llms.txt                  # machine-readable resource index
├── builtins.json             # @generated catalogue-as-data — do not edit by hand
├── build.rs                  # compiles tree-sitter grammar (C), enforces grammar.pin
├── grammar.pin               # the tree-sitter grammar revision: single source
├── surrealdb.pin             # the SurrealDB revision the catalogue + corpus come from
└── scripts/
    ├── setup-grammar.sh      # clones/updates the grammar sibling repo to the pin
    └── setup-surrealdb.sh    # fetches the SurrealDB checkout the tests read
```

## Editor Integration

The server communicates over `stdio` and works with any LSP-compatible editor.

### Settings

Settings arrive four ways: `initializationOptions` on `initialize`, a
`workspace/configuration` pull (which a `null` `didChangeConfiguration` payload
asks for), a `workspace/didChangeConfiguration` push, and (for the CLI)
`check --config file.json`, which accepts the same JSON. Either under a
`surrealql` key or at the root. Every key accepts both `camelCase`
and `snake_case`. An unknown key is reported through `window/logMessage` with a
did-you-mean suggestion rather than ignored.

A **partial** payload (which is what an editor sends when one setting changes)
only changes what it names. Keys it omits keep the values already in force.

#### `analysis.schemalessDiagnostics`

Decides which diagnostics apply to a table declared `SCHEMALESS`, where ad-hoc
fields are legal SurrealQL.

| Value | Behavior |
| --- | --- |
| `quiet` *(default)* | Report none of `unknown-field`, `field-type`, `unknown-type`, `permission-denied`, `permission-unknown` on such a table. |
| `errors` | Report only `field-type` and `unknown-type` — the two faults SurrealDB itself raises. The advisory three stay quiet. |
| `strict` | No exemption: check a `SCHEMALESS` table exactly as a `SCHEMAFULL` one. |

```jsonc
{ "surrealql": { "analysis": { "schemalessDiagnostics": "errors" } } }
```

`errors` is worth preferring over the default if you want a quiet editor without
hiding real failures. SurrealDB coerces `DEFAULT`, `VALUE` and `COMPUTED` to the
declared type on a `SCHEMALESS` table too, and it refuses to parse an unknown
type name at all — so under `quiet` a file that always fails can look clean.

The setting keys on the **keyword**. A bare `DEFINE TABLE t` is schemaless to the
engine, but it declares nothing, so it keeps the diagnostics it would have had
anyway.

#### `analysis.maxSyntaxDiagnostics`

Upper bound on **syntax** diagnostics (`parse`, `unknown-type`) per document, so
a pathological buffer cannot flood the problems panel. Default `2000`, raised
from `100`. Set it to `0` to report every one.

```jsonc
{ "surrealql": { "analysis": { "maxSyntaxDiagnostics": 0 } } }
```

This counts **diagnostics, not lines** — no setting limits how long a document
may be. Semantic and type diagnostics are uncapped; they are derived from the
definitions and query facts in the file, so the code itself bounds them.

Two unrelated limits do apply to the *workspace scan*, and neither is
configurable: files over 2 MB are skipped, and at most 5,000 `.surql` files are
indexed. Both are reported through `window/logMessage` when they bite. They
affect which files contribute schema, not the diagnostics on the file you have
open.

#### Other analysis settings

| Key | Default | Effect when `false` |
| --- | --- | --- |
| `analysis.enableTypeChecking` | `true` | Turns off the whole type pass: `argument-type`, `argument-count`, `let-type`, `return-type`, `operator-type`, `unknown-method`, `undefined-variable`, `field-type`, `renamed-function`, `not-callable`. `unknown-type` survives — it is a syntax fault. |
| `analysis.enablePermissionAnalysis` | `true` | Turns off `permission-denied` and `permission-unknown` on every table. |
| `analysis.enableCodeActions` | `true` | Stops offering quick fixes and refactors entirely. |
| `analysis.externalParams` | `[]` | Not a toggle: names the variables your caller binds at runtime (`db.query(sql).bind(("id", id))`, or Surrealist's variables panel) so `undefined-variable` does not flag them. |
| `analysis.diagnosticDebounceMs` | `200` | Not a toggle: how long a burst of keystrokes must settle before the document is re-analysed. `0` analyses every change. |

#### Connecting to a database

Live schema from a running SurrealDB, merged with whatever the workspace's
`.surql` files define. Every key is optional, and **`check` never connects**:
these affect the editor only.

| Key | Default | Meaning |
| --- | --- | --- |
| `connection.endpoint` | none | `ws://localhost:8000` or `http://…`. Nothing is fetched without it. |
| `connection.namespace` / `connection.database` | none | Selected after signing in. Also required for database-scoped credentials. |
| `connection.username` / `connection.password` | none | Tried as root first, then as database credentials. |
| `connection.token` | none | A bearer token, tried before username/password. |
| `connection.access` | none | **Accepted but not yet used.** Record/scope access is not wired into sign-in; the three routes above are what authenticate today. |

Each of the six may also come from the environment:
`SURREALDB_ENDPOINT`, `SURREALDB_NAMESPACE`, `SURREALDB_DATABASE`,
`SURREALDB_USERNAME`, `SURREALDB_PASSWORD`, `SURREALDB_TOKEN`, which is the
easier route for a shared machine. A value in the settings wins over the
environment. There is no `SURREALDB_ACCESS`.

#### `metadata.*`

| Key | Default | Meaning |
| --- | --- | --- |
| `metadata.mode` | `workspace+db` | Where schema comes from. `workspace` / `filesystem` reads only `.surql` files; `db` / `remote` reads only the live database; `both` / `workspace+db` reads both. An unknown value warns and falls back to the default. |
| `metadata.enableLiveMetadata` | `true` | Turns off the database fetch without clearing the connection settings. Ignored by the browser build, which has no connection of its own. |
| `metadata.refreshOnSave` | `true` | Re-fetches live schema on every `didSave`. |

#### `authContexts` / `activeAuthContext`

What the permission analysis assumes about who is running the query. Each context
has a `name`, a list of `roles`, and optionally an `authRecord` plus free-form
`claims`, `session` and `variables` objects. The default is a single `viewer`
context with the `viewer` role. `activeAuthContext` names the one in force; an
unknown name warns and the first context is used.

```jsonc
{ "surrealql": {
    "authContexts": [
      { "name": "viewer", "roles": ["viewer"] },
      { "name": "owner", "roles": ["owner"], "authRecord": "user:me" }
    ],
    "activeAuthContext": "owner"
} }
```

#### Accepted but not yet implemented

Two keys parse and validate, and are read by nothing. They are listed here rather
than removed because clients already send them:

- `connection.access`: see the connection table above.
- `analysis.enableAggressiveSchemaInference`: tables inferred from usage always
  count toward the model; setting this to `false` does not change that.

## Using with AI agents

The same analysis the editor shows is available headless, so coding agents,
CI jobs and pre-commit hooks can run the generate → check → repair loop:

```bash
surrealql-language-server check queries/
surrealql-language-server check --stdin --stdin-filename src/feed.surql --workspace schema/
surrealql-language-server check queries/ --format json --fail-on warning
```

| Exit | Meaning |
| --- | --- |
| 0 | Ran to completion; nothing at or above `--fail-on` (default `error`). |
| 1 | Ran to completion; diagnostics at or above the threshold. |
| 2 | Usage error, unreadable input, or a skipped target file. |

`--format json` prints exactly one JSON object on stdout for **every** exit
code. An exit-2 report carries an `error` object naming the kind, so a consumer
never has to treat empty output as a result:

```jsonc
{ "files": [], "summary": { … }, "exitCode": 2,
  "error": { "kind": "unreadable-input", "message": "cannot read `q.surql`: …" } }
```

`--format json` prints one object whose diagnostics are LSP wire objects
verbatim (stable codes, 0-based UTF-16 ranges, structured `data` hints),
plus a `summary`, the `scan` losses, and the `exitCode`:

```jsonc
{
  "files": [{ "path": "queries/feed.surql", "diagnostics": [ /* LSP Diagnostic */ ] }],
  "summary": { "filesChecked": 1, "errors": 0, "warnings": 1, "information": 0, "hints": 0 },
  "scan": { "walkErrors": 0, "skippedOversize": 0, "skippedUnreadable": 0, "fileCapHit": false },
  "exitCode": 0
}
```

[`AGENTS.md`](AGENTS.md) is the agent-facing contract: the check loop, the
diagnostic-code table, and the known grammar gaps an agent must not "fix".
[`llms.txt`](llms.txt) indexes the machine-consumable resources, including
[`builtins.json`](builtins.json).

Recipes:

- **CI**: run `check` over your `.surql` directories with
  `--fail-on warning`; exit codes 1 and 2 fail the job.
- **pre-commit**: `surrealql-language-server check $(git diff --cached --name-only -- '*.surql')`
  (skip when the list is empty).
- **Claude Code**: add "Run `surrealql-language-server check <file>` after
  every `.surql` edit" to `CLAUDE.md`, or point it at [`AGENTS.md`](AGENTS.md).
- **Cursor**: the same instruction in `.cursor/rules`.
- **Zed**: the extension already ships this server for editing; use `check`
  for the headless loop.

## Grammar Development

The tree-sitter grammar lives in the sibling [`surrealql-tree-sitter`](https://github.com/surrealdb/surrealql-tree-sitter) repo. After editing `grammar.js`:

```bash
cd ../surrealql-tree-sitter
npx tree-sitter generate
npx tree-sitter test
```

The `src/parser.c` is auto-generated and should not be edited directly. JavaScript scripting function bodies (`function() { ... }`) are handled by an external C scanner at `src/scanner.c` which tracks brace depth, strings, template literals, and comments.

## Builtin Function Catalogue

`src/grammar_generated.rs` holds every builtin SurrealDB accepts — 434 functions with their argument types, return types, arity and method receivers. It is committed, and generated from a SurrealDB checkout rather than written by hand:

```bash
make builtins            # or: cargo xtask generate-builtins --surrealdb ../surrealdb
make builtins-check      # compare without writing
```

Pass `--surrealdb <path>` or set `SURREALDB_DIR` (`make builtins SURREALDB=/path/to/surrealdb`). The checkout must be at the revision the catalogue header records. `--check` is what `tests/generated_catalogue.rs` runs. Never edit the generated files by hand.

The same generator run also writes [`builtins.json`](builtins.json) — the
catalogue as data, for anyone building SurrealQL tooling outside this crate.
It is committed, freshness-checked in CI alongside the Rust rendering,
attached to every GitHub release, and shipped in the npm package
(`@surrealdb/surrealql-language-server/builtins.json`). One encoding rule
matters to consumers: `params: null` means the signature is unknown —
never read it as zero-arity.

These targets do not need the grammar checkout: `cargo run --package xtask` never builds the root package, so `build.rs` does not run.

The generator reads four places in the engine: `syn/parser/builtin.rs` for the names, `fnc/mod.rs` for the dispatch and method tables, the `pub fn` signatures under `fnc/` for the argument types, and `exec/function/builtin/` for the return types.

SurrealDB never reads its own return-type registry, so a wrong declaration there would compile and ship. To check them by running them:

```bash
make verify-returns      # or: cargo run -p xtask --features probe -- verify-returns --surrealdb ../surrealdb
```

This boots an in-memory engine, calls every function with synthesised arguments, and compares the answer with what the catalogue records. It compiles the whole engine, hence the feature flag and the few minutes on a cold build. Run it after a SurrealDB version bump.

## CI

GitHub Actions runs `cargo fmt --check`, `cargo test` and `cargo test -p xtask` on every push and pull request. The grammar and SurrealDB sibling repos are cloned automatically, both pinned, so the catalogue freshness check runs in CI.

## Releases

### Native binaries and crates.io

Push a `v*` tag (e.g. `v0.1.5`). CI builds platform binaries, uploads them to the GitHub Release, and publishes the Rust crate to [crates.io](https://crates.io).

### Browser WASM npm package

The scoped package [`@surrealdb/surrealql-language-server`](https://www.npmjs.com/package/@surrealdb/surrealql-language-server) is built with `scripts/build-wasm.sh` (`cargo` → `wasm-bindgen` → `wasm-opt`) and published to npm on the same `v*` tag. A `.tgz` is also attached to the GitHub Release.

Release checklist:

1. Bump `version` in [`Cargo.toml`](Cargo.toml) and [`pkg/package.json`](pkg/package.json).
2. Push the tag: `git tag vX.Y.Z && git push origin vX.Y.Z`.
3. Confirm the `wasm` CI job succeeds and the package appears on npm.
4. Confirm the release assets include the platform binaries, the npm `.tgz`,
   and `builtins.json`.

npm publishing uses [Trusted Publishing](https://docs.npmjs.com/trusted-publishers/) (OIDC from GitHub Actions). Before the first publish, an `@surrealdb` org admin must configure a trusted publisher on the package's npm **Access** page ([`@surrealdb/surrealql-language-server`](https://www.npmjs.com/package/@surrealdb/surrealql-language-server)) with:

- Repository owner: `surrealdb`
- Repository name: `surrealql-language-server`
- Workflow filename: `ci.yml` (exact match, case-sensitive)

The CI workflow intentionally does **not** set `registry-url` on `actions/setup-node` — that option writes an `.npmrc` which forces token auth and breaks OIDC ([npm/cli#8730](https://github.com/npm/cli/issues/8730)). Do not add a `NODE_AUTH_TOKEN` secret for this job.

If the first CI publish still fails with a misleading `404`, an org admin can bootstrap the package once locally (`bash scripts/build-wasm.sh && npm publish --access public` from `pkg/`), then configure the trusted publisher for subsequent tag releases.
