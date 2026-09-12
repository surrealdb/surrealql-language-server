# Grammar Gaps

The language server compiles against the tree-sitter SurrealQL grammar at the
revision [`grammar.pin`](../grammar.pin) names: the single source, read by
[`scripts/setup-grammar.sh`](../scripts/setup-grammar.sh), by the CI checkout
steps, and by [`build.rs`](../build.rs), which fails the build when the checkout
has drifted off it. The analysis layer in
[`src/semantic/node_kind.rs`](../src/semantic/node_kind.rs) is coupled to that
revision's node kinds: bump the pin and the constants together.

## Fixed by the `373e7cd` pin

The pin moved from `cb2e6b5` (an unmerged PR branch) to upstream `master`, which
carries `surrealql-tree-sitter#16`. That revision closed **every false-positive
shape this document had recorded**: seven shapes of valid SurrealQL that used
to surface as `parse` errors:

| Shape | Example |
| --- | --- |
| The remainder operator | `RETURN 8 % 3` |
| A prefix sign on a non-literal | `RETURN -$x`, `RETURN -[1, 2, 3]` |
| A sized collection type | `LET $b: array<float, 10> = [1.0]` |
| A union in a `ParamDefinition` | `LET $a: int \| float = 2` |
| A decimal with a fraction or exponent | `RETURN 102023.1dec` |
| A nested `SET` target | `CREATE person SET name.first = 'John'` |
| Mock syntax in a value position | `RETURN \|test:1..4\|` |

Verified shape by shape through `check` at the new pin: all seven parse clean,
and the whole suite passes unchanged. `AGENTS.md` no longer lists any false
positive.

Two of them needed a matching change on this side, because the shapes had never
reached the type checker before and it was wrong about both:

- **`PrefixExpression` was typed `bool` unconditionally**
  ([`src/semantic/infer.rs`](../src/semantic/infer.rs)). True while `!` was the
  only prefix operator; with `-x` and `+x` parsing it produced `argument-type`
  and `operator-type` errors on SurrealQL the engine runs: caught by the corpus
  sweep on `bench/executor/rt_import.surql`, which writes
  `vector::divide([$viewportWidth, -$viewportHeight], …)`. `!` still answers
  `bool`; `+` is the engine's identity, so the operand's type passes through;
  `-` follows `TryNeg`, which accepts only numbers.
- **`ArithOp` had no `%`** ([`src/semantic/operate.rs`](../src/semantic/operate.rs)).
  `binding_power` already ranked it at MulDiv, so only the operand table was
  missing. `TryRem for Value` has exactly one arm, `(Number, Number)`, so
  `"8" % "3"` is now reported in the engine's own words, and the corpus file
  `language/expression/operators/modulo.surql`, which declares that very error,
  is now in the sweep's expected set.

## Fixed by the `cb2e6b5` pin (earlier)

The pin moved from `df12d94` to `cb2e6b5` for the `DEFINE INDEX` kinds
SurrealDB 3 reads. At `df12d94`, `IndexClause` was
`choice(UniqueClause, SearchAnalyzerClause, MtreeClause, HnswClause)` — the
full-text form keyed on the pre-3.0 `SEARCH` keyword — so
`DEFINE INDEX … FULLTEXT ANALYZER english BM25` reported ``Invalid SurrealQL
syntax near `FULLTEXT ANALYZER english BM25`.`` on valid SurrealQL, and every
`COUNT` and `DISKANN` index failed the same way. The new revision mirrors the
engine's `parse_define_index` (`syn/parser/stmt/define.rs`):

- **`FullTextClause`** — `FULLTEXT [ANALYZER <name>] [BM25 [(k1, b)]]
  [HIGHLIGHTS]`, the options in any order and none required. The statement
  above is `IndexClause(FullTextClause(Keyword, Keyword, Ident,
  Bm25Clause(Keyword)))`; the analyzer name is the `Ident` child, as in
  `SearchAnalyzerClause`.
- **`CountClause`** — `COUNT [WHERE <condition>]`; the condition is an
  ordinary `WhereClause` child.
- **`DiskAnnClause`** — `DISKANN DIMENSION <n>` followed, in any order, by
  `DiskAnnDistClause`, `IndexTypeClause`, `IndexDegreeClause`,
  `IndexLBuildClause`, `IndexAlphaClause` and `IndexHashedVectorClause`.
- **`HnswClause`** gains `IndexHashedVectorClause`, and both vector `DIST`
  clauses accept the `DISTANCE` spelling the engine lexes as the same
  keyword.
- **`IndexTypeClause`** covers the engine's `VectorTypeKind` set (`F16`,
  `I8`, `U8` were missing) and **`Distance`** its `DistanceKind` set
  (`COSINE_NORMALIZED`, `INNER_PRODUCT` were missing).

`SearchAnalyzerClause` and `MtreeClause` are unchanged, so 2.x schemas still
parse, and no existing node is renamed or reshaped — `node_kind.rs` needed no
change, and `extract_index` in
[`src/semantic/analyzer.rs`](../src/semantic/analyzer.rs) captures the new
clauses verbatim as index options, as it did for `HNSW`. Guard tests:
`tests/lsp.rs` (the *Grammar pin `cb2e6b5`* section) and the
`accepts_*_index_variants` tests in `src/semantic/analyzer.rs`. `DISKANN`
leaves `OFFERS_THE_GRAMMAR_CANNOT_PARSE` in
[`src/core/statement_shape.rs`](../src/core/statement_shape.rs).

Across SurrealDB's own `language-tests/` corpus the move fixed the parse of
42 files and regressed none.

## Fixed by the `df12d94` pin (earlier)

The pin moved from `826d0c2` to `df12d94` (upstream `master`) for four
shapes the earlier revision rejected or mis-nested on valid SurrealQL.
Each produced a false `parse` diagnostic, or a wrong tree, and each has a
guard test in `tests/lsp.rs` (the *Grammar pin* section):

- **A closure may have a bare-expression body.** `|$x: int| $x * 2` parses
  as `Closure(Pipe, ParamDefinition, Pipe, BinaryExpression)`; before, only
  a `Block` body was accepted and the expression form was an `ERROR`. This
  is what lets `semantic::infer` type a `LET`-bound closure at all.
- **`UNSET` takes a field list.** `UPDATE person:tobie UNSET name, email`
  is `UnsetClause(Keyword, Predicate(Ident)…)`, the same shape as `OMIT`.
  Before, `UNSET` was read as a list of `FieldAssignment`s, so the first
  bare field name broke the statement.
- **`SHOW CHANGES … SINCE` accepts a versionstamp.** `SINCE 1` is a
  `Number` child; before, only a `String` was accepted.
- **Binary operators carry the engine's precedence.** `BinaryExpression`
  is split into tiers mirroring `BindingPower`, so `1 + 1 * 3` nests as
  `1 + (1 * 3)`. The re-grouping in `semantic::infer` is kept regardless:
  it flattens a chain on *both* sides and regroups it with the engine's
  binding powers, which reproduces this grammar's tree and would correct a
  future one, and it is what judges a nested chain exactly once.
- **Mock syntax parses.** `|test:1..4|` is a `RangeRecordId`. The
  `has_broken_sibling` guard in `semantic::infer` that this shape motivated
  is kept, because the failure mode is tree-sitter's recovery rather than
  one rule.

Across SurrealDB's own `language-tests/` corpus the move fixed the parse
of 91 files and regressed none.

## Known parse/shape gaps at the pinned revision

None of these is a false positive: they are shapes the analyzer has to work
*around*, not valid SurrealQL the grammar rejects. The false-positive list is
empty: see the `373e7cd` section above.

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

- **A graph hop names no fields.** `Lookup` is
  `seq(choice($.LookupRight, $.LookupLeft, $.LookupBoth), choice($.Ident,
  $.Any, $.LookupSelection))`, and `node-types.json` gives it `"fields": {}`.
  The arrow and the thing it reaches are therefore *siblings*, and direction
  has to be read by scanning the children for an arrow kind rather than by a
  field lookup. `LookupLeft` covers both `<-` and `<~`. The same is true of
  `TableTypeClause`, where `RELATION`, `IN`, `OUT`, `FROM` and `TO` all arrive
  as generic `Keyword` nodes and only their order says which table list
  follows — see `parse_relation_clause` in
  [`src/semantic/analyzer.rs`](../src/semantic/analyzer.rs).

  Two further shape notes, both load-bearing:

  * A `Path` may *start* with a `Lookup`. `SELECT ->knows->person FROM person`
    writes no base, so the first child is a hop and the anchor table is the
    statement's own `FROM` target (`anchor_type` in
    [`src/semantic/infer.rs`](../src/semantic/infer.rs)).
  * A `RelateStatement` does **not** wrap its arrows in a `Lookup`. There the
    `LookupRight` / `LookupLeft` tokens are direct children of the statement,
    between the three subjects, so `RELATE` needs its own walk
    (`relate_edge_observation`).

- **An empty argument list is a field access.** `'abc'.slice(` parses as
  `Path(String, Subscript(Ident))` with the `(` left over as an `ERROR`
  sibling; only once an argument is typed does it become
  `Subscript(IdiomFunction(FunctionName, ArgumentList))`. Signature help is
  most useful on the `(` keystroke, so it resolves the receiver from the text
  rather than waiting for the tree — see `signature_help` in `core/server.rs`.

## Tracking tests

`cargo test --test lsp` carries the guard tests that pin known-good
parses (the `*_no_diagnostic` / `*_produces_no_syntax_diagnostics`
cases). When bumping the grammar pin, run the full suite: those tests
are the drift alarm for node-kind changes.
