# surreal-language-server

Generic Language Server Protocol implementation for SurrealQL.

This server speaks standard LSP over `stdio`, so the same binary can be used by Zed, VSCode, Neovim, Vim, or any other editor with an LSP client.

## Features

- Parse diagnostics powered by the existing `tree-sitter-surrealql` grammar
- Semantic completions for tables, functions, keywords, special variables, and `record<table>` positions
- Hover for tables, functions, params, access definitions, keywords, builtin namespaces, and typed `record<table>` expressions
- Permission-aware diagnostics and hover summaries using configured auth contexts
- Workspace and remote SurrealDB metadata merging with local-first precedence
- Function definition, references, rename, signature help, document highlights, call hierarchy, and workspace symbols
- Schema inference from `DEFINE`, CRUD/query flow, object literals, and nested `record<table>` type expressions
- Document symbols for top-level SurrealQL statements and definitions

## Requirements

- Rust toolchain with Cargo
- The sibling grammar checkout at [`../../../tree-sitter-surrealql`](../../../tree-sitter-surrealql)

By default the build expects the grammar repo to live at `../../../tree-sitter-surrealql` relative to this crate. If you want to build against a different checkout, set `TREE_SITTER_SURREALQL_DIR` before building.

## Build

```bash
cargo build --release
```

With an explicit grammar path:

```bash
TREE_SITTER_SURREALQL_DIR=/absolute/path/to/tree-sitter-surrealql cargo build --release
```

The binary will be available at `target/release/surreal-language-server`.

## Language Identity

- LSP language id: `surrealql`
- File extensions: `.surql`, `.surrealql`

## Editor Integration

The server remains a single generic `stdio` LSP binary. Tree-sitter stays in the stack:

- Zed keeps the bundled tree-sitter grammar for highlighting, structure, and editor-native UX.
- Neovim can use the same LSP with optional `nvim-treesitter` for structural features.
- VSCode does not need tree-sitter on the client side, but the server still parses with the sibling grammar repo.

### Zed

Use the monorepo Zed extension in [`../../extensions/zed-surrealql`](../../extensions/zed-surrealql) and configure it to launch `surreal-language-server` over `stdio`.

The Zed extension now resolves the language server binary in this order:

- `lsp.surreal-language-server.binary.path`
- `target/release/surreal-language-server` in the opened worktree
- `target/debug/surreal-language-server` in the opened worktree
- `surreal-language-server` from `PATH`

The current grammar metadata already lines up with this server:

- grammar: `surrealql`
- language id: `surrealql`
- file suffixes: `surql`, `surrealql`

### VSCode

A minimal `vscode-languageclient` wiring example lives at [`examples/vscode-client.ts`](./examples/vscode-client.ts).

Core server settings:

- command: `surreal-language-server`
- transport: `stdio`
- document selector language: `surrealql`

### Neovim / Vim

A minimal `nvim-lspconfig` example lives at [`examples/neovim.lua`](./examples/neovim.lua).

Core server settings:

- command: `surreal-language-server`
- filetypes: `surql`, `surrealql`

For Vim, the same `cmd` and filetype mapping can be used through any LSP client plugin.

## Configuration

All editor clients should pass the same `surrealql` settings object through standard LSP configuration:

```json
{
  "surrealql": {
    "connection": {
      "endpoint": "ws://127.0.0.1:8000/rpc",
      "namespace": "app",
      "database": "app",
      "username": "root",
      "password": "root",
      "token": null,
      "access": null
    },
    "metadata": {
      "mode": "workspace+db",
      "enableLiveMetadata": true,
      "refreshOnSave": true
    },
    "analysis": {
      "enablePermissionAnalysis": true,
      "enableAggressiveSchemaInference": true,
      "enableCodeActions": true
    },
    "authContexts": [
      {
        "name": "viewer",
        "roles": ["viewer"],
        "authRecord": "user:viewer",
        "claims": {},
        "session": {},
        "variables": {}
      }
    ],
    "activeAuthContext": "viewer"
  }
}
```

Environment fallback is supported when editor settings omit connection fields:

- `SURREALDB_ENDPOINT`
- `SURREALDB_NAMESPACE`
- `SURREALDB_DATABASE`
- `SURREALDB_USERNAME`
- `SURREALDB_PASSWORD`
- `SURREALDB_TOKEN`

## Development

Run tests with:

```bash
cargo test
```
