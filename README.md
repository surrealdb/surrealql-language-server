# SurrealQL Language Server

A Language Server Protocol (LSP) implementation for [SurrealQL](https://surrealdb.com/docs/surrealql), the query language of [SurrealDB](https://surrealdb.com).

## Features

- **Syntax diagnostics** — real-time parse error reporting via tree-sitter
- **Semantic analysis** — schema inference from DDL and query flow
- **Hover** — type info, permission posture, function signatures, and language badges (SurrealQL vs JavaScript)
- **Completions** — contextual `record<table>` types, field names, builtin functions, statement keywords
- **Go-to definition / references** — tables, fields, functions, params
- **Rename** — safe rename of local function definitions
- **Code actions** — missing `PERMISSIONS` clause suggestions
- **Signature help** — parameter hints for builtin and user-defined functions
- **Call hierarchy** — inbound/outbound function call tracking
- **Document symbols** — outline of tables, fields, events, indexes, functions
- **JavaScript scripting functions** — `function() { ... }` bodies parse cleanly with no false diagnostics; `DEFINE FUNCTION` bodies containing scripting functions are detected and labelled as JavaScript

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

```bash
cargo build --release
# binary at: target/release/surreal-language-server
```

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
│       ├── types.rs          # DocumentAnalysis, TableDef, FunctionDef, …
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

The `src/parser.c` is auto-generated — never edit it directly. JavaScript scripting function bodies (`function() { ... }`) are handled by an external C scanner at `src/scanner.c` which tracks brace depth, strings, template literals, and comments.

## CI

GitHub Actions runs `cargo fmt --check` and `cargo test` on every push and pull request. The grammar sibling repo is cloned automatically during CI.
