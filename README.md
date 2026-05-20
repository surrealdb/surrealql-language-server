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

## Testing

```bash
cargo test
```

## Repository Layout

```text
.
├── src/
│   ├── main.rs               # LSP stdio entry point
│   ├── backend.rs            # tower-lsp request handlers
│   ├── config.rs             # workspace settings
│   ├── grammar.rs            # tree-sitter language binding
│   ├── providers/            # completion, hover, rename, etc.
│   └── semantic/
│       ├── analyzer.rs       # document analysis (parse + extract)
│       ├── model.rs          # merged workspace model, code actions
│       ├── types.rs          # DocumentAnalysis, TableDef, FunctionDef, ...
│       ├── type_expr.rs      # SurrealQL type expression parser
│       └── text.rs           # LSP range utilities
├── tests/
│   └── lsp.rs                # integration tests
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

## CI

GitHub Actions runs `cargo fmt --check` and `cargo test` on every push and pull request. The grammar sibling repo is cloned automatically during CI.

## Releases

### Native binaries and crates.io

Push a `v*` tag (e.g. `v0.1.5`). CI builds platform binaries, uploads them to the GitHub Release, and publishes the Rust crate to [crates.io](https://crates.io).

### Browser WASM npm package

The scoped package [`@surrealdb/surrealql-language-server`](https://www.npmjs.com/package/@surrealdb/surrealql-language-server) is built with `scripts/build-wasm.sh` (`cargo` → `wasm-bindgen` → `wasm-opt`) and published to npm on the same `v*` tag. A `.tgz` is also attached to the GitHub Release.

Release checklist:

1. Bump `version` in [`Cargo.toml`](Cargo.toml) and [`pkg/package.json`](pkg/package.json).
2. Push the tag: `git tag vX.Y.Z && git push origin vX.Y.Z`.
3. Confirm the `wasm` CI job succeeds and the package appears on npm.

npm publishing uses [Trusted Publishing](https://docs.npmjs.com/trusted-publishers/) (OIDC from GitHub Actions). Before the first publish, an `@surrealdb` org admin must configure a trusted publisher on npmjs.com:

- Repository: `surrealdb/surrealql-language-server`
- Workflow filename: `ci.yml`
