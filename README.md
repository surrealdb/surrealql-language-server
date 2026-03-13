# Surreal Monorepo

This repository contains the SurrealQL language tooling stack in one place:

- [`crates/language-server`](./crates/language-server): generic `stdio` LSP server for SurrealQL
- [`extensions/zed-surrealql`](./extensions/zed-surrealql): Zed extension and bundled grammar assets
- sibling checkout at `../tree-sitter-surrealql`: canonical tree-sitter grammar source

The grammar stays in the monorepo even with the LSP in place:

- `Zed` uses tree-sitter directly for syntax highlighting, structure, and editor-native UX.
- `Neovim` can pair the same LSP with `nvim-treesitter` for highlighting, folds, and textobjects.
- `VSCode` uses the same LSP over `stdio`; the server still parses SurrealQL with the sibling tree-sitter grammar checkout.

The language server now includes a semantic layer for:

- permission-aware diagnostics and hover
- function indexing, references, rename, signature help, and call hierarchy
- schema inference from DDL and query flow
- merged local, remote, and inferred metadata
- contextual `record<table>` completion

The Zed extension now launches the same monorepo `surreal-language-server` binary. It resolves the command from:

- `lsp.surreal-language-server.binary.path` if configured in Zed
- `target/release/surreal-language-server` in this repo when the monorepo itself is the opened worktree
- `target/debug/surreal-language-server`
- `surreal-language-server` from `PATH`

## Repository Layout

```text
.
├── crates/
│   └── language-server/
├── extensions/
│   └── zed-surrealql/
```

## Development

Rust workspace:

```bash
cargo test
```

Tree-sitter grammar:

```bash
cd ../tree-sitter-surrealql
bun install --frozen-lockfile
bun run check
```

The language server now consumes the sibling `../tree-sitter-surrealql` checkout by default. Set `TREE_SITTER_SURREALQL_DIR` if your grammar lives elsewhere.

To refresh the Zed extension's vendored grammar after package changes:

```bash
bash scripts/sync-zed-grammar.sh
```

## CI

GitHub Actions runs Rust formatting/tests and Zed extension asset/build checks in this repo. Grammar tests now live in the standalone [`../tree-sitter-surrealql`](../tree-sitter-surrealql) repo.
