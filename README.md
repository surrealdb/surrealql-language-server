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
