# Vendored SurrealQL grammar

Generated: do not edit. Refresh with `make vendor-grammar`.

Revision: `373e7cd52e3beabfbe8339f2cbf6a0cfb6a35d0e` (see [`grammar.pin`](../../grammar.pin))

These are the four build inputs `build.rs` compiles, copied from
[surrealql-tree-sitter](https://github.com/surrealdb/surrealql-tree-sitter) so
the published crate builds without a sibling checkout. A local checkout still
wins: `build.rs` prefers `TREE_SITTER_SURREALQL_DIR`, then
`../surrealql-tree-sitter`, and falls back here.
