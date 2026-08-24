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

The grammar is **pinned** to a specific commit (`GRAMMAR_REF` in
[`scripts/setup-grammar.sh`](scripts/setup-grammar.sh) and the checkout steps
in CI) because the analysis layer is coupled to the grammar's node kinds.
Bump it deliberately alongside any [`src/semantic/node_kind.rs`](src/semantic/node_kind.rs)
change. Known grammar parse gaps (and the tests that track them) are listed in
[`docs/grammar-gaps.md`](docs/grammar-gaps.md).

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
│   ├── native/               # tower-lsp adapter, walkdir loader, SurrealDB metadata
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
│       └── emit.rs           # joins them and renders the catalogue
├── tests/
│   ├── lsp.rs                # analyzer/model integration tests
│   ├── core_server.rs        # end-to-end server tests (mock notifier)
│   ├── dispatch.rs           # JSON-RPC wire tests
│   ├── conformance.rs        # silence sweep over SurrealDB's own corpus
│   ├── generated_catalogue.rs # catalogue freshness + shape invariants
│   ├── compat.rs             # backwards-compatibility tripwires
│   └── common/               # shared mocks for the three boundary traits
├── docs/
│   ├── pain-points.md        # audited pain-point catalog + status
│   └── grammar-gaps.md       # known gaps at the pinned grammar revision
├── build.rs                  # compiles tree-sitter grammar (C)
└── scripts/
    └── setup-grammar.sh      # clones/updates the grammar sibling repo
```

## Editor Integration

The server communicates over `stdio` and works with any LSP-compatible editor.

### Settings

Settings arrive via `initializationOptions` or `workspace/didChangeConfiguration`,
either under a `surrealql` key or at the root. Every key accepts both `camelCase`
and `snake_case`. An unknown key is reported through `window/logMessage` with a
did-you-mean suggestion rather than ignored.

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
| `analysis.enableCodeActions` | `true` | `textDocument/codeAction` returns an empty list. The capability stays advertised — a client reads that once, at `initialize`. |
| `analysis.enableAggressiveSchemaInference` | `true` | Nothing. Accepted for compatibility and has no effect. |
| `analysis.externalParams` | `[]` | Not a toggle: names the variables your caller binds at runtime (`db.query(sql).bind(("id", id))`, or Surrealist's variables panel) so `undefined-variable` does not flag them. |

## Formatting

`textDocument/formatting` and `textDocument/rangeFormatting`, plus
`surrealql-language-server format` on the command line.

The formatter walks the parse tree's leaves and decides only what goes *between*
them. It cannot reflow a long line, and it also cannot lose, reorder or invent a
token — the property that matters in a tool that rewrites a schema.

**A file the grammar cannot parse is returned exactly as it was.** Format-on-save
fires while you are still typing, and text nobody can safely read is text nobody
should rewrite. Across SurrealDB's own 1,897-file corpus the formatter changes
588 files, leaves 703 already-canonical, and refuses 606 that do not parse at the
pinned grammar.

Range formatting widens the request to whole statements first; half a statement
is not something a token-stream formatter can lay out.

## Editing model

`textDocument/didChange` is incremental. An edit is applied to the authoritative
buffer synchronously and in arrival order; only the analysis that follows is
spawned and debounced (`analysis.diagnosticDebounceMs`, default 200 ms). Under
incremental sync a change carries a range into the previous text, so two
notifications landing out of order would corrupt the document.

tree-sitter reuses the previous parse tree: edits since that tree was produced
are accumulated and replayed against it, and the accumulator is cleared in the
same critical section that stores a new tree — so an analysis dropped as
superseded leaves the two in step.

`textDocument/semanticTokens/full/delta` returns the changed run rather than
every token in the file. `$/progress` reports the workspace walk and the
`INFO FOR DB` fetch, both of which can take seconds.

## Command line

The same analysis the editor gets, from a terminal — so a build pipeline can
fail on it.

```bash
surrealql-language-server check                 # the working directory
surrealql-language-server check schema/ q.surql # directories and files
surrealql-language-server check --format json   # machine-readable
surrealql-language-server rules                 # every rule
surrealql-language-server explain unknown-table # one rule, in full
surrealql-language-server format schema/        # format in place
surrealql-language-server format --check        # exit 1 if anything would change
```

Run with no arguments it serves LSP over stdio, exactly as before — no editor
integration changes.

`check` exits `0` when nothing is reported, `1` when any finding is an error,
and `2` on a usage or file error. It reads `surrealql.toml` from the first
directory argument unless `--no-config` is given, and `--rule ID=SEVERITY`
overrides one rule above everything else.

Human output is 1-based, the way an editor shows a position. JSON output is
0-based, matching the Language Server Protocol.

A command-line run has no database connection, so rules needing live metadata
stand down rather than guessing: `unknown-table` does not fire there. Point the
editor at a database for those. Everything else runs, including the permission
rules — the auth context comes from the settings, which a command-line run has
as much as the editor does.

#### `surrealql.toml`

A `surrealql.toml` in the workspace root sets analysis policy for everyone who
opens the repository, and for anything that reads the same file in a build
pipeline.

```toml
[analysis]
schemalessDiagnostics = "errors"
externalParams = ["id", "limit"]

[analysis.ruleSeverity]
unknown-field = "error"
permission-unknown = "off"
```

The keys are the LSP settings keys, so one reference covers both. `surrealql.toml`
is tried first and `.surrealql.toml` second; only the first workspace folder is
consulted. A file that is not valid TOML is refused whole, with a warning —
applying the half that parsed would leave nobody able to say which policy is in
force.

**Settings precedence**, lowest first: built-in defaults, then `surrealql.toml`,
then the editor's LSP settings, then the `SURREALDB_*` environment variables for
connection fields that are still unset. The merge is per key, so an editor
setting for one rule does not discard the rest of the committed policy.

#### Suppressing a rule in the source

A comment silences a rule where it sits, without changing anything for the rest
of the workspace.

```surql
-- surql-ignore-file: permission-unknown, dynamic-target

-- surql-ignore: unknown-field
CREATE person SET nickname = "b";

CREATE person SET nickname = "b";  -- surql-ignore: unknown-field
```

- A directive on its own line covers the next line that holds code. It may sit
  above other comments and blank lines and still reach the statement.
- A directive at the end of a line covers that line.
- `surql-ignore-file` covers the whole document. It is only honoured above the
  first statement — below that a reader would have to scroll past code to
  discover the file is muted, so it is inert there.
- A comma-separated list covers several rules. A bare `-- surql-ignore` with no
  list covers every rule on the line.
- All four comment forms work: `--`, `//`, `#` and `/* … */`.

Directives are read from the parse tree, not by scanning text, so a directive
inside a string literal is just a string.

A directive that silences nothing is reported as `unused-suppression` — a hint,
tagged so an editor greys it out. That catches a comment which outlived the fault
it covered, and a rule id with a typo in it. For a file that keeps directives
deliberately, `-- surql-ignore-file: unused-suppression` turns it off; a line
directive cannot, because a line directive's scope is the next line of *code*.

#### Diagnostic rules

Every diagnostic carries a stable `code`. [`docs/rules.md`](docs/rules.md) has a
page for each one, and clients that support `codeDescription` link straight to
it from the problems panel.

| Rule | Category | Default | Fix |
| --- | --- | --- | --- |
| [`argument-count`](#argument-count) | types | error | — |
| [`argument-type`](#argument-type) | types | error | — |
| [`duplicate-definition`](#duplicate-definition) | schema | warning | — |
| [`dynamic-target`](#dynamic-target) | schema | warning | — |
| [`field-type`](#field-type) | types | error | — |
| [`let-type`](#let-type) | types | error | — |
| [`not-callable`](#not-callable) | types | warning | — |
| [`operator-type`](#operator-type) | types | error | — |
| [`permission-denied`](#permission-denied) | permissions | error | — |
| [`permission-unknown`](#permission-unknown) | permissions | warning | — |
| [`renamed-function`](#renamed-function) | types | warning | automatic |
| [`return-type`](#return-type) | types | error | — |
| [`undefined-variable`](#undefined-variable) | types | error | — |
| [`unknown-field`](#unknown-field) | schema | warning | — |
| [`unknown-table`](#unknown-table) | schema | warning | suggested |
| [`unknown-index-field`](#unknown-index-field) | schema | warning | — |
| [`unknown-analyzer`](#unknown-analyzer) | schema | warning | — |
| [`unknown-function`](#unknown-function) | types | error | suggested |
| [`relation-endpoint`](#relation-endpoint) | schema | error | — |
| [`unused-binding`](#unused-binding) | types | hint | — |
| [`unused-suppression`](#unused-suppression) | syntax | hint | — |
| [`unknown-method`](#unknown-method) | types | error | — |
| [`unknown-type`](#unknown-type) | syntax | error | suggested |
| [`parse`](#parse) | syntax | error | — |

#### `analysis.ruleSeverity`

Sets the severity of one rule, keyed by the id that appears in
`Diagnostic.code`. Accepted values are `off`, `hint`, `info`, `warning` and
`error`.

```json
{
  "surrealql": {
    "analysis": {
      "ruleSeverity": {
        "unknown-field": "error",
        "permission-unknown": "off"
      }
    }
  }
}
```

**Precedence**, lowest first:

1. The rule's default severity.
2. `analysis.enableTypeChecking` and `analysis.enablePermissionAnalysis`, which
   turn a whole category off.
3. `analysis.ruleSeverity`, which wins over both. An explicit severity here
   re-enables a single rule inside a category a switch turned off — so
   `enableTypeChecking: false` plus `{"let-type": "warning"}` reports
   `let-type` and nothing else from the type pass.
4. What the run has available. A rule that needs information this run does not
   have stays off whatever the settings say: `unknown-table` needs live
   database metadata, so it does not fire in a run without a database, however
   it is configured.

An unknown rule id or an unknown severity is dropped with a warning through
`window/logMessage`, with a did-you-mean hint where one is close enough. A bad
entry never changes another rule.

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

Pass `--surrealdb <path>` or set `SURREALDB_DIR` (`make builtins SURREALDB=/path/to/surrealdb`). The checkout must be at the revision the catalogue header records. `--check` is what `tests/generated_catalogue.rs` runs. Never edit the generated file by hand.

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

npm publishing uses [Trusted Publishing](https://docs.npmjs.com/trusted-publishers/) (OIDC from GitHub Actions). Before the first publish, an `@surrealdb` org admin must configure a trusted publisher on the package's npm **Access** page ([`@surrealdb/surrealql-language-server`](https://www.npmjs.com/package/@surrealdb/surrealql-language-server)) with:

- Repository owner: `surrealdb`
- Repository name: `surrealql-language-server`
- Workflow filename: `ci.yml` (exact match, case-sensitive)

The CI workflow intentionally does **not** set `registry-url` on `actions/setup-node` — that option writes an `.npmrc` which forces token auth and breaks OIDC ([npm/cli#8730](https://github.com/npm/cli/issues/8730)). Do not add a `NODE_AUTH_TOKEN` secret for this job.

If the first CI publish still fails with a misleading `404`, an org admin can bootstrap the package once locally (`bash scripts/build-wasm.sh && npm publish --access public` from `pkg/`), then configure the trusted publisher for subsequent tag releases.
