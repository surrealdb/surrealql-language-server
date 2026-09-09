# Grammar Gaps

The language server compiles against the tree-sitter SurrealQL grammar
pinned to commit `cb2e6b5f77de5de4e59aa4e1a72ccac7606d7d3b`
(`GRAMMAR_REF` in [`scripts/setup-grammar.sh`](../scripts/setup-grammar.sh)
and the checkout steps in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml)).
The analysis layer in [`src/semantic/node_kind.rs`](../src/semantic/node_kind.rs)
is coupled to that revision's node kinds — bump the pin and the
constants together.

## Fixed by the `cb2e6b5` pin

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

## Fixed by the `df12d94` pin

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

- **No `%` operator.** Nothing in `grammar.js` holds `'%'`, so `8 % 3`
  does not parse. The engine supports it at `MulDiv` precedence and
  rejects `"8" % "3"`, which the arithmetic check therefore cannot reach.
- **No unary minus.** A sign belongs to the `Number` token
  (`optional(choice('-', '+'))`) and `PrefixExpression` accepts `!`
  alone, so `-[1,2,3]` does not parse. The engine rejects it with
  `Cannot negate the value 'array'`.
- **Mock syntax does not parse.** `|test:1..4|` yields `ERROR` nodes
  *around* a `BinaryExpression` rather than inside one, so a guard that
  only inspects a subtree sees a well-formed fragment. `has_broken_sibling`
  in `semantic::infer` exists for exactly this shape.
- **A union type does not parse in a `LET` annotation.** The
  `ParamDefinition` type slot takes a single type expression, so
  `LET $a: int | float = 2;` raises a false `parse` diagnostic on
  SurrealQL the engine accepts. Tracked by
  `adds_nothing_where_the_grammar_already_fails_to_parse` in
  [`tests/lsp.rs`](../tests/lsp.rs), which also pins that the type
  checker adds no second diagnostic on top of the failed parse.
- **A sized collection type does not parse.** `LET $b: array<float, 10> = 2;`
  is valid SurrealQL (the second argument bounds the length), but the
  pinned grammar rejects it, so it too surfaces as a false `parse`
  error — at least in the `ParamDefinition` slot the tracked examples
  use. Same tracker test as the union gap; like the nested `SET`
  target below, the real fix is cross-repo in `surrealql-tree-sitter`,
  after which the pin moves here.
- **A union in a `ParamDefinition` does not parse.** `_safeType` is
  `choice($._singleType, seq('<', $._type, '>'))` and omits `UnionType`,
  so `LET $a: int | float = 2` and a closure or function parameter typed
  the same way leave an `ERROR` node; the bracketed `<int | float>` form
  parses. `DEFINE FIELD … TYPE int | float` is unaffected, because
  `TypeClause` uses `_type`. `semantic::infer` refuses to type a closure
  whose parameter list holds an `ERROR`, rather than invent an arity.
- **A sized collection does not parse.** `array<float, 10>` and
  `set<int, 3>` produce `ParameterizedType(TypeName, TypeName,
  ERROR(Int))` — `ParameterizedType` has no comma list. Both are valid
  engine kinds.
- **A signed decimal suffix does not parse.** `math::ceil(-102023.1dec)`
  leaves an `ERROR` in the argument list, which is why the call checks
  refuse to count arguments in a list that holds one.

- **A `SET` target cannot be a nested field.** `FieldAssignment` is
  `seq($.Ident, alias($._assignmentOp, $.Operator), choice($.IfElseStatement, $._value))`, so the
  assigned-to side is a *single* identifier. `CREATE person SET
  name.first = 'John'` therefore reports ``Invalid SurrealQL syntax near
  `.first`.`` on valid SurrealQL — the `.first` becomes an `ERROR` sibling
  inside the `FieldAssignment`. The engine accepts nested targets, and
  `DEFINE FIELD name.first ON person` already parses (that rule uses
  `Idiom`), so a schema can declare a field that no `SET` can assign.

  Fix is one token in `grammar.js` — `$.Ident` → `$.Idiom` in
  `FieldAssignment` — and it regenerates with no new conflicts. Verified
  against this repo's suite and the SurrealDB corpus sweep. It is a
  *cross-repo* change: it lands in `surrealql-tree-sitter`, then the pin
  moves here.

  The analyzer already reads both shapes, so the pin can move without a
  matching code change: `field_assignment_target` in
  [`src/semantic/analyzer.rs`](../src/semantic/analyzer.rs) accepts an
  `Ident` (pinned revision) or an `Idiom` (fixed revision). Note the
  fixed grammar wraps *every* target in an `Idiom`, including the plain
  `SET age = 29` case.

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
