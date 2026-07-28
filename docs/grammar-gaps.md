# Grammar Gaps

The language server compiles against the tree-sitter SurrealQL grammar
pinned to commit `826d0c2ca6733a1c201ea7015dd91f439f67b573`
(`GRAMMAR_REF` in [`scripts/setup-grammar.sh`](../scripts/setup-grammar.sh)
and the checkout steps in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml)).
The analysis layer in [`src/semantic/node_kind.rs`](../src/semantic/node_kind.rs)
is coupled to that revision's node kinds — bump the pin and the
constants together.

## Known parse/shape gaps at the pinned revision

- **No `FromClause` node.** `SELECT … FROM target` lays the targets
  out as direct children after the bare `FROM` keyword. The analyzer's
  target extraction handles both shapes
  (`target_nodes_for_statement` in
  [`src/semantic/analyzer.rs`](../src/semantic/analyzer.rs)).
- **Keyword tokens are aliased.** Every keyword is a hidden `_kw_<word>`
  token aliased to the public `Keyword` kind. `Node::grammar_name()`
  recovers the concrete keyword for MISSING-node diagnostics, but the
  lookahead table only exposes the alias — "expected *which* keyword"
  cannot be derived from parser states (that's why syntax hints use
  the build-generated `KEYWORDS` list instead).
- **Error recovery is coarse.** A single typo often produces one ERROR
  node spanning the rest of the statement (or file); nested statements
  inside the error region may re-parse. The diagnostics layer clamps
  those spans to the first line and surfaces nested errors separately.
- **Statement coverage.** DEFINE ANALYZER / USER / NAMESPACE /
  DATABASE / MODEL / TOKEN / CONFIG parse but get no structured
  analysis; CRUD statements nested in FOR/IF blocks and INSERT
  statements produce no query facts (see
  [`docs/pain-points.md`](pain-points.md)).

## Tracking tests

`cargo test --test lsp` carries the guard tests that pin known-good
parses (the `*_no_diagnostic` / `*_produces_no_syntax_diagnostics`
cases). When bumping the grammar pin, run the full suite: those tests
are the drift alarm for node-kind changes.
