# Changelog

## 0.5.0 — unreleased

Engine-parity release. Two things the server claimed to do but did not:
completion ignored where the cursor was, and the builtin functions were
not type-checked at all. Both are now derived from the SurrealDB source
rather than from a hand-maintained table, and validated against
SurrealDB's own test corpus.

The last tag is `v0.3.0`, so 0.4.0 below never shipped on its own and
both sections land together. They are kept apart because 0.4.0 is a
self-contained story — declared types becoming real — that this release
builds directly on. The minor bump rather than a patch is required by the
breaking changes listed below.

The catalogue moves from 79 hand-written functions covering 2 of the 20
advertised namespaces to **434 generated from the engine**, with argument
types read from the implementations. `cargo xtask generate-builtins`
rebuilds it; a test fails when the committed file is stale.

The same doctrine as 0.4.0 applies, and mattered more here than
anywhere: a diagnostic that fires on working code costs far more than one
that never fires. Every new check was swept across all 1,897 files of
`language-tests/` before shipping. That sweep found five distinct
false-positive sources — four of them in code this release did not
otherwise touch — and the release ships with **zero false positives** and
51 diagnostics that match an error SurrealDB itself declares, in its own
words.

### Completion now respects the cursor

- `INFO FOR ` returned about 375 items, of which nine were legal: every
  keyword, every builtin and user function, and every table. It now
  returns exactly the nine targets the engine accepts. The cause was a
  single unguarded fallthrough in the completion handler, reached
  whenever the three positive gates above it missed — which was every
  statement form outside a nine-keyword allowlist.
- New `core::statement_shape`: a flat table of literal keyword prefixes
  covering the heads of `INFO FOR`, `USE`, `DEFINE`, `REMOVE`, `ALTER`,
  `REBUILD`, `SHOW CHANGES`, `ACCESS`, and the `ON`/`TYPE`/`PERMISSIONS`
  slots inside them, every entry transcribed from the parser that defines
  it.
- **No clause spine, deliberately.** Classifying a position inside
  `SELECT` needs to tell a clause keyword from a field of the same name,
  and SurrealQL accepts `SELECT order FROM t`. Guessing there hides
  fields, variables and functions in `WHERE … AND `, the busiest position
  in the language. Unrecognised positions keep the list they returned
  before, so the set of changed positions equals the set of table rows.
- `DEFINE PARAM` names and `DEFINE ANALYZER` names are offered. Both were
  already in the model — hover and go-to-definition resolved them — but
  nothing ever put them in the dropdown.

### Builtin functions are type-checked

- `string::len(42)` reports `argument-type`; `string::len('a', 'b')`
  reports `argument-count`. Neither reported anything before: the checker
  read `model.functions`, which holds only `DEFINE FUNCTION fn::…`, and
  returned early for every other name.
- Argument counts use the engine's own wording (`1 argument`,
  `2 to 3 arguments`, `zero or more arguments`).
- **An empty argument list is never reported.** `ArgumentList` is
  `seq('(', optional(…), ')')`, so `string::len()` parses clean, and every
  editor that closes brackets produces exactly that on the `(` keystroke.
- Method syntax is checked too — `{ a: 9 }.extend('9')` was previously
  invisible. The receiver counts as argument one, matching how the engine
  numbers the error.
- A signature the generator could not read is checked for nothing.
  `signature_known` keeps "unknown" distinct from "takes no arguments";
  without it every call to such a function would have been reported as
  expecting zero.

### Declared function return types are checked

`DEFINE FUNCTION … -> T` declared a return type that nothing verified, so
this reported nothing:

```surql
DEFINE FUNCTION fn::beau::number($input: int) -> int {
    RETURN "";
};
```

It now reports ``fn::beau::number` returns `int`, but this value is
`string`.`` under the `""`, with the new `return-type` code. The engine
coerces a function's result to its declared type and fails with
`Couldn't coerce return value from function …`
(`expr/function.rs:330`), using the same coercion relation the argument
checks already model — so this needed no new type machinery.

- A body ending in a bare expression returns it, so
  `DEFINE FUNCTION fn::x() -> int { '' }` is reported too.
- **`RETURN`s inside `IF` branches and `FOR` bodies are checked**, at any
  nesting depth. A `RETURN` there returns from the enclosing function, not
  from the branch — SurrealDB's own `fn::fib($n: int) -> int` is written
  that way, and its recursion would not terminate otherwise.
- The walk descends only through constructs that propagate a return, as an
  allowlist rather than a blocklist: the two directions do not cost the
  same. Descending somewhere it should not reports against a value the
  function never returns, while failing to descend merely misses one. So a
  `RETURN` inside a closure, inside a nested `DEFINE FUNCTION`, or inside a
  block bound as a value (`LET $y = { RETURN 5 }`, which returns from that
  block) is not the function's return and is left alone.
- A body the parser could not read is not checked; a syntax diagnostic
  already covers it.

### Undeclared function return types are inferred

The other half of the same problem. `DEFINE FUNCTION` without `-> T` made
every call site `unknown`, which switched off every downstream check and left
hover with nothing to say — and most functions carry no annotation. Of the 29
definitions in `tests/fixtures/adversarial.surql`, 25 declare no return type.

```surql
DEFINE FUNCTION fn::custom::slug($input: string) {
    RETURN string::slug($input);
};

LET $slug = fn::custom::slug("some random string");
```

`$slug` now hovers as `string`, completes with `string` as its detail, and is
checked wherever it flows. The return type is read from the body — every
`RETURN` plus the block's trailing expression — and it reaches the argument
check exactly as a declared type does.

- **Across files, and along chains.** The pass runs in
  `MergedSemanticModel::build`, the only place holding every document's tree
  *and* every symbol, and a `fn::` definition routinely lives in another file.
  A body returning another unannotated function's result resolves on the next
  round, up to eight deep.
- **It cannot recurse.** A round reads what earlier rounds wrote out of a map;
  it never descends into a callee. An unannotated `fn::fib`, or a mutually
  recursive pair, simply never resolves and stays silent — no visited set, no
  depth guard, no stack risk. A whole round is computed before it is written,
  so the answer does not depend on `HashMap` order.
- **A declared type always wins**, and an inferred one is never checked
  against the body it came from: `check_one_function_body` reads the
  annotation from the tree, not from `FunctionDef`.
- **One unresolvable return path makes the whole answer unknown.** An inferred
  type feeds a diagnostic, so a type *narrower* than the truth would report
  against a value the function really returns.
  `{ IF $n > 0 { RETURN 'yes'; }; }` yields NONE when the branch does not
  fire, so it infers nothing rather than `string`.
- **Return paths that disagree infer a union.** A union on the value side of
  the assignability relation can never come back incompatible, so it informs
  hover and stays silent in the checker.
- **A body sees only its own parameters**, which is a correctness requirement
  and not an optimisation. Given `LET $g = 'hi';` then
  `DEFINE FUNCTION fn::f() { RETURN $g; }`, `$g` is unset inside the body and
  the engine yields NONE — so resolving bindings over the whole document would
  infer a type the function cannot produce.
- Hover writes `Return type inferred from the body.` beside the signature. The
  `->` alone cannot distinguish an inference from an annotation.
- No new diagnostic code and no new configuration key. The new reports go out
  under the existing `argument-type`, `let-type` and `return-type` codes —
  which makes the blast radius *wider* than a new code would be, not narrower.
  Swept across all 1,897 files of `language-tests/` with **zero** new
  diagnostics.
- `tests/conformance.rs` now sweeps `let-type` too. It was outside the filter,
  so this feature's likeliest false positive would have been invisible to the
  one test that reads real SurrealQL at scale. Widening it surfaced two true
  positives, both matching an `error =` those corpus files declare themselves.

### Arithmetic operands are checked

`RETURN "" + "222" + 3;` fails in SurrealDB with `Cannot perform addition
with 'string' and 'int'`, and the server said nothing: `infer_expr_type`
had no `BinaryExpression` arm, so every operator expression was `unknown`
and every check downstream of it stayed silent.

```surql
RETURN "" + "222" + 3;            -- reported
RETURN "" + "222" + <string>3;    -- silent, gives "2223"
RETURN <int>"0" + <int>"222" + 3; -- silent, gives 225
```

The operand rules are transcribed from the engine's own `TryAdd` /
`TrySub` / `TryMul` / `TryDiv` / `TryPow` impls for `Value` into the new
`semantic::operate`, with the file and lines cited on each table. They had
to be read rather than reasoned about, because they are irregular per
operator: `+` concatenates two strings but rejects a string and an int,
`*` scales a duration in one direction only (`1s * 2` works, `2 * 1s`
does not), `array + set` yields an array while `set + array` yields a
set, and `/` never fails at all.

**The documentation cannot answer this.** It never states a rule for
`string + int`, and never states one for numeric promotion in arithmetic —
its operators page shows same-type examples only. It also puts `??`/`?:`
*above* `**` in precedence, which the engine's own
`language/expression/operators/precedence.surql` disproves.

- **The tree is re-grouped before it is checked.** The pinned grammar puts
  every binary operator on one left-associative precedence level, so it
  parses `1 + 1 * 3` as `(1 + 1) * 3` while SurrealDB answers `4`. Reading
  the tree as parsed would name operand pairs the engine never formed, so
  the chain is flattened back to its written sequence and re-grouped with
  the engine's binding powers. `RETURN "" + 1 * 2;` reports `string` and
  `int`, not `int` and `int`.
- **Three operators are excluded, each for a different reason.** `/` and
  `÷` because the engine wraps their failures as
  `unwrap_or(f64::NAN.into())`, so `[1,2,3] / 1` evaluates to `NaN`;
  `+=`/`-=` because they take the looser `increment` path; and every
  comparison, containment and logical operator because none of them can
  fail — `1 < "a"` has a defined answer.
- **The right side of a short circuit is never reported.** `?:`, `??`,
  `&&` and `||` may leave it unevaluated, and `precedence.surql` relies on
  it: `2 + 1 ?: true + 1` is `3`, so the `true + 1` that would fail never
  runs.
- **`value_kind` is the gate**, and it is the only way this can produce a
  false positive. It answers "not provably one concrete kind" for
  `unknown`, `any`, `value`, an `option<T>`, a union, and any type name it
  has not been taught, and a single such operand silences the pair. An
  `option<int>` is deliberately *not* treated as a number: it may hold
  NONE.
- **A concrete kind that appears in no arm is reported**, which is what
  makes `true + 1` and `person:tobie + 1` errors rather than shrugs.
- The message follows the engine's wording, with the operand's type where
  the engine prints a value. Note `**` has its own sentence there:
  `Cannot raise the value …`.
- No new configuration key. One new wire-visible code, `operator-type`.
- **An arithmetic expression now has a type**, so this closes the first
  gap the return-type-inference change above recorded: a body of
  `RETURN $a + $b` is inferrable, and `LET $n: string = 1 + 2` is caught.
  Two existing tests asserted that `1 + 2` stays untyped; both were pinning
  a miss rather than a policy, and both now assert the opposite.
- Swept across all 1,897 files of `language-tests/`. It found **two** false
  positives, both now fixed and both regression-tested: the short circuit
  above, and a fragment left beside `ERROR` nodes by mock syntax
  (`|test:1..4|`) that the grammar cannot parse, where
  `test:..=-9223372036854775806` looks like a record id minus an int.
  `tests/conformance.rs` also sweeps `operator-type` now, and four corpus
  files gained expected entries — every one of them declaring the matching
  `Cannot perform …` in its own front matter.

### Method calls resolve against the engine's own receiver tables

SurrealQL lets most builtins be called as a method, and the mapping is **not**
`<receiver type>::<method>`. The server guessed that convention, which is right
for the eight receivers whose namespace happens to be their type name and wrong
for everything else. Of the 236 distinct method names SurrealDB's own
`method_syntax.surql` exercises, it resolved **124**.

```surql
RETURN (5).round();          -- math::round
RETURN 123.to_float();       -- type::float
RETURN "abc".is_alphanum();  -- string::is_alphanum
RETURN $point.area();        -- geo::area
```

`cargo xtask generate-builtins` now also reads `fnc::idiom` — one engine function
holding a `match` on the receiver's `Value` variant, **11 typed tables plus a
catch-all, 820 arms**. The receiver grouping sits outside the `dispatch!` macro,
so `syn` reads the twelve keys as typed patterns and the scrape depends on no
indentation. Each table is emitted in full rather than layered over the shared
block, because layering would be wrong — see below.

- **Three receivers use a foreign namespace**: `Number` (which covers int, float
  and decimal) dispatches into `math::`, `Geometry` into `geo::`, `Datetime` into
  `time::`.
- **52 method names flatten a path**, so `is_alphanum` is `string::is::alphanum`
  and `sort_asc` is `array::sort::asc`. Another 42 in the shared block do the
  same, such as `to_string_lossy` for `type::string_lossy`.
- **`String` shadows four shared arms and drops one.** `.repeat()` is
  `string::repeat`, not `array::repeat`, and `.is_datetime()` /`.is_uuid()` /
  `.is_record()` come from `string::is::…` with a *different arity* —
  `string::is::datetime` takes an optional format argument. `.is_set()` is absent
  from `String` and is a hard error there. This is why the tables are emitted
  whole.
- **The catch-all table is emitted too.** It serves `bool`, `uuid`, `regex`,
  `range`, `none` and `null` with 48 methods, and without it `true.to_string()`
  would report a false error.
- **A method past link one still resolves nothing**, except an optional chain.
  `$v.?.trim()` reads the same value as `$v.trim()`, and that shape appears six
  times across the two test fixtures; `$a.b.trim()` needs field resolution the
  server does not have.
- **An unknown method is now reported**, under the new `unknown-method` code.
  `"abc".nonsense()` was silent, because a miss in the old name guess was
  indistinguishable from a remapped name.
- **An object is exempt from that report.** When method dispatch fails on an
  object the engine retries the name as a *closure-valued field*
  (`val/value/get.rs`), so `{ a: |$x| $x }.a(1)` is legal and the field can be
  called anything. Three corpus files rely on it.
- **A GeoJSON object literal reaches the geometry table too.**
  `{ type: "Point", coordinates: […] }` is a `Value::Geometry` to the engine but
  an object to the lattice. `Object` and `Geometry` share no method name outside
  the shared block, so both tables are tried rather than one guessed — the same
  resolution `xtask/src/kinds.rs` already records for geometry *parameters*.
- **`value::chain` is method-only.** It is in the parser's `PATHS` but has no
  callable dispatch arm, so `value::chain(x, f)` parses and then fails while
  `x.chain(f)` works. The method path deliberately does not check `not_callable`.
- **A method call now has a type**, folded link by link, so
  `"019535d9-…".to_uuid().is_uuid()` is `bool`. Return types stay **partial**:
  the catalogue holds none, because every builtin returns `Value` in Rust. Three
  sources fill the gap — the 79 curated signatures, the `type::is_*` predicates,
  and an explicit list of the `type::` conversions plus the `math::` and
  `duration::`/`time::` accessors. Everything else stays `unknown`.
- Swept across all 1,897 files of `language-tests/`. It found **one** false
  positive — the closure-field fallback above — now fixed and regression-tested.
  `tests/conformance.rs` also sweeps `unknown-method`, and
  `language/functions/method_syntax.surql` is committed as a fixture: 198 calls
  the engine itself declares error-free.

### Completion offers every builtin, not just the curated 79

Reported from the Surrealist query editor: typing `rand::` completed the
namespace and then nothing inside it, and the same for every other namespace.

The cause was that completion iterated `BUILTIN_FUNCTIONS`, the **curated**
table, which carries prose and a docs link for 79 entries and covers exactly two
namespaces — `string::` and `type::`. The 434-entry generated catalogue, added in
this release for type checking, was never wired into the dropdown. So **355 of
the 434 functions the parser accepts were invisible**: all 62 of `array::`, 42 of
`math::`, 37 of `time::`, 24 each of `set::`, `string::` and `duration::`, and
all 12 of `rand::` including `rand::uuid::v4`.

- Completion now reads both tables, and a curated entry wins so it keeps its
  summary and docs link. `rand::` offers 15 items, `rand::uuid::` narrows to
  three, `math::` offers 66.
- **Constants are offered too.** `math::PI` and the other 26 take no arguments,
  so they are not function entries at all and nothing had ever suggested them.
  They are matched case-insensitively, since a constant is spelled in upper case
  while the prefix is lowered.
- A name the parser accepts but nothing implements — the nine such names,
  `object::matches` among them — is marked deprecated rather than offered
  silently, so the dropdown cannot hand out a query that parses and then fails.

### Three wrong answers this replaced

- **`$s.` returned an empty completion popup.** `$` is not a table-qualifier
  character, so the scan stopped just after it and `$s.` yielded the *table* name
  `s`; no fields were found on a table called `s`, and the handler answered with
  an empty list rather than falling through. This was the sharpest completion
  defect in the server.
- **`.at()` and `.split()` hovered as the `AT` and `SPLIT` keywords.** `token_at`
  treats `.` as a boundary, so hover saw the bare word and fell through to the
  keyword table. Method hover now runs first and resolves through the receiver.
- **Namespace completion offered `not::` and `sleep::`**, which are bare
  functions and not namespaces, and hid eight real ones: `api::`, `bytes::`,
  `eval::`, `file::`, `schema::`, `sequence::`, `set::` and `value::`. `set::`
  was the costly one — 24 functions the method checker already resolved but
  completion never suggested. The correct list had been generated as
  `GENERATED_NAMESPACES` all along, with a test in
  `tests/generated_catalogue.rs` describing the defect; no code ever read it.

### Methods reach the rest of the LSP

- **Completion after a `.`** offers that receiver's methods, with the resolved
  function and its parameters as the detail, and marks the twelve `file::`
  methods experimental. When the receiver's type is unknown — still common, since
  a field access types as `unknown` — every method is offered instead, ranked
  below the type-matched ones. An empty list there would read as a broken
  feature. Field names the position already offered are kept: a `.` admits both.
- **Hover** names the function a method resolves to, its return type where one is
  known, and its experimental target.
- **Signature help** works inside a method's argument list and drops parameter
  zero, since the receiver fills it. It resolves the receiver from the text
  rather than from the tree, because on the `(` keystroke there is no
  `IdiomFunction` node yet — with an empty argument list the grammar reads
  `.slice` as a field access and leaves the `(` as an ERROR sibling.

### What the structured parameters unlocked

- Hover answers for all 20 namespaces. `math::abs` used to answer nothing.
- Signature help covers all 434 functions, with parameters read from the
  catalogue instead of recovered by splitting a prose string on `,` —
  which covered 79 functions and broke on `array<string, 5>`.
- Inlay hints name builtin arguments. Suppressed for single-parameter
  calls, where `arg:` says nothing the reader cannot see.
- New `renamed-function` **warning** plus a rename quick fix, from the
  engine's own 62-pair table. `type::thing` → `type::record` is one.
- New `not-callable` **warning** for the nine names the parser accepts
  that no implementation backs in call form.

### Fixed

- **`DEFINE ACCESS` was never indexed.** The grammar wraps it in an
  `AccessDefinition` node, so the "second keyword child" lookup returned
  `None` and the extraction arm was unreachable — despite the arm, the
  type and the merge path all existing. `DEFINE SCOPE` was dead the same
  way.
- **`MIDDLEWARE fn::x()` was reported as a wrong argument count.** It
  registers a function; the API runtime supplies `(request, next)`. 33
  occurrences in SurrealDB's own corpus.
- **A parameter typed `any` was treated as required.** SurrealDB
  substitutes `NONE` for a missing argument when the declared type admits
  it, so `fn::any_arg()` is legal where `fn::one_arg()` is not.
- **An unparseable argument was counted as an argument.** The pinned
  grammar cannot read a closure (`|| 'x'`) or a signed decimal suffix
  (`-1.5dec`), both valid SurrealQL, so an `ERROR` node inflated the
  count. A call whose arguments contain a parse error is no longer
  checked.
- `TypeExpr::parse` had no `set<>` case, and read the length of
  `array<string, 5>` as part of the element type. Both silently disabled
  the check for those types.
- `docs/pain-points.md` listed three gaps that commit 345cc7a had already
  closed; corrected.

### Breaking

- `ColumnSlot::Loose` is removed. The completion handler never matched on
  it, so `WHERE`, `AND`, `OR` and `BY` already behaved as `None` and
  their behaviour is unchanged.
- `DocumentAnalysis` and `MergedSemanticModel` each gained an `analyzers`
  field. Both are constructed with struct-literal syntax, so downstream
  code that builds one needs the new field.
- `assign.rs` now reports a scalar flowing **into** a collection as
  unknown rather than incompatible. An aggregate hands a function the
  whole group where the source names one field, so `math::sum(price)`
  inside `AS SELECT … GROUP BY …` is valid; reporting it flagged
  SurrealDB's own view tests. The reverse direction is unchanged.
- Three new diagnostic codes: `renamed-function`, `not-callable` and
  `return-type`. Codes are wire-visible; clients matching on the existing
  set are unaffected.

### Testing

- `tests/conformance.rs` sweeps the whole SurrealDB corpus and asserts the
  exact set of diagnostics in both directions, so a new false positive and
  a check that silently stops working both fail. Ignored by default at
  about two minutes: `cargo test --test conformance -- --ignored`.
- `tests/fixtures/builtin_calls_valid.surql` holds 137 calls across 20
  namespaces, lifted verbatim from corpus files that expect no error, and
  runs everywhere — including where there is no SurrealDB checkout.
- `tests/generated_catalogue.rs` pins the catalogue's freshness against
  the generator, and the invariants the argument checks depend on.
- The suite grew from 245 tests to 354 in the crate, plus 28 for the
  generator and one ignored corpus sweep. That includes the first
  end-to-end completion tests: the handler previously had none.

### Known gaps

- The pinned tree-sitter grammar cannot parse `DEFINE SEQUENCE` at all,
  and has no `INFO FOR USER` or `INFO FOR INDEX` branch. Completion
  follows the engine, so six offered keywords (`GRAPHQL_ALIAS`,
  `GRAPHQL_DEPRECATED`, `SYSTEM`, `DISKANN`, `RETRY`, `MAXDEPTH`) draw a
  syntax error from the grammar if selected. Declared in
  `OFFERS_THE_GRAMMAR_CANNOT_PARSE`, with tests keeping the list honest.
- Ten callable functions have no readable signature and are checked for
  nothing: the seven `api::` middleware functions, whose leading arguments
  the runtime supplies, and `rand::float`/`int`/`time`, whose
  zero-or-two arity this catalogue cannot express.
- A method past link one of a path resolves nothing, apart from an optional
  chain: in `$a.b.trim()` the receiver is `$a.b`, which needs field resolution.
- A method's return type is known only for the 79 curated signatures, the
  `type::is_*` predicates, the `type::` conversions, and the `math::` and
  `duration::`/`time::` accessors. Everything else types as `unknown`, because
  the engine's Rust signature returns `Value` and carries no SurrealQL type.
- Too few arguments on a method is never reported. The check bails on an empty
  argument list, because an editor that closes brackets writes `.at()` on the
  `(` keystroke and this server has no debounce.
- Nine names parse but have no implementation, `object::matches` and the seven
  `duration::set_*` among them. They are recorded but not flagged in method form.
- Return-type inference reaches a body only as far as expression typing does,
  so it declines more often than it succeeds. A body returning a `SELECT`,
  `CREATE`, `UPDATE` or `DELETE` result infers nothing, because no statement
  kind has a type yet; nor does a `BinaryExpression` (`RETURN $a + $b`) or
  field access (`RETURN $x.name`). Those three arms are where the remaining
  value is — every unannotated function in `adversarial.surql` returns a CRUD
  result and so infers nothing today.
- The arithmetic check cannot reach `%` or unary minus, because the pinned
  grammar parses neither (see [`docs/grammar-gaps.md`](docs/grammar-gaps.md)).
  The engine rejects `"8" % "3"` and `-[1,2,3]`, and a grammar bump would
  unlock both with no change to the tables.
- An arithmetic operand is judged only when its type is provably one
  concrete kind, so field access (`$x.count + 1`), a method call, and any
  statement result still type as `unknown` and stay silent.
- Return-type inference also declines a body containing `IF … THEN … END`.
  The return walk does not descend through that form, and a `RETURN` it cannot
  see would make the inferred type too narrow. The brace form of `IF` is
  handled.
- `MergedSemanticModel::build` takes no `ServerSettings`, so return-type
  inference runs even when `analysis.enableTypeChecking` is false. It only
  feeds hover and completion in that case, which is the intent, but the work
  is still done.

## 0.4.0 — unreleased

Type-inference release. Parameter annotations were being discarded
wholesale — `FunctionParam.type_expr` was *always* `None` — so nothing
could type-check a call. This release makes declared types real and uses
them.

The checker's failure mode is **silence**: anything the inference engine
cannot pin down yields `Unknown`, and the assignability relation turns
any doubt into a verdict that reports nothing. A noisy checker gets
switched off and then protects nobody.

### Added

- **Argument type checking** on `fn::` calls (`argument-type`,
  `argument-count`), ranged at the offending argument rather than the
  whole statement. Object literals report per property, so one bad field
  in six names that field:
  ``Argument 2 of `fn::doc::add`: property `line` expects
  `record<orderLine>`, found `int`.``
- **`undefined-variable`** with a did-you-mean. SurrealDB substitutes
  `NONE` for an unset parameter rather than failing, so a typo silently
  changes what a query means and nothing at runtime tells you.
  `analysis.externalParams` declares names the caller binds at runtime
  (SDK `.bind(…)`, Surrealist's variables panel).
- **`let-type`** when a `LET $x: T = v` value cannot satisfy `T`.
- **A binding table** for `LET`, function/closure parameters and `FOR`
  loop variables, with block scoping and shadowing. `$variable` hover
  and completion, neither of which existed.
- **Structural types**: `TypeExpr` gained `Object`, `Tuple`, `Set` and
  `Literal`, read from the grammar's own nodes. Builtin return types are
  parsed from the signature table, with `type::record`/`type::thing`
  narrowed to the table their argument names.
- **Build provenance** in `serverInfo.version`
  (`0.4.0 (branch-sha, grammar rev)`). The bare crate version is
  identical on a branch and on the published release, so it could not
  identify which binary an editor was talking to.
- `analysis.enableTypeChecking` (default on) and
  `analysis.externalParams`.

### Fixed

Each of these was a silent no-op: a node-kind constant naming a node the
grammar never emits compiles fine and simply disables whatever matches
on it.

- Function **parameter types** were always `None` — `ParamDefinition`
  puts a named `Colon` between the name and the type, so positional
  adjacency never matched. Measured on the test fixture: **0/64 → 64/64**
  extracted.
- **Return types** looked for a `ReturnsClause` node that does not exist.
- **`TYPE 'a' | 'b'` and `TYPE [string, string]`** were dropped, because
  the type-payload lookup omitted `UnionType` and `LiteralType`.
- **`LET`/`FOR`/`IF`/`RETURN` bodies were never descended into**, so
  nested statements and bindings were invisible to analysis. Combined
  with 0.3.0's tight diagnostic ranges, those statements now get
  diagnostics for the first time.
- Bindings inside `DEFINE EVENT … THEN { }` were never registered — the
  block sits inside a `ThenClause`, not as a direct child.
- Expression-bodied closures (`|$x| $x != 'Done'`) bound no parameters.
- **`record<a | b>`** parsed as one table literally named `a | b`, which
  then registered a phantom table.
- **Inlay hints** looked up `model.functions` with the `fn::` prefix
  stripped from a map keyed *with* it, so they never fired.
- 17 dead node-kind constants removed. `node_kind.rs` now documents that
  every constant must exist in `node-types.json`.

### Compatibility

- LSP wire: capabilities unchanged (pinned by `tests/compat.rs`);
  `Diagnostic.source` and the syntax `parse` code unchanged. The four new
  codes are additions.
- New diagnostics are ERROR severity and on by default; the kill switch
  is `analysis.enableTypeChecking`.
- Config: both new keys accept camelCase and snake_case, like every
  existing key, and are registered in the unknown-key sweep.
- `serverInfo.version` now carries a build suffix after the semver. No
  test pinned it; clients parsing it strictly should read the leading
  version.
- **Still outstanding:** 0.3.0 promised the legacy message-text quick-fix
  fallback in `unknown_table_payload` would be removed in 0.4. It is
  still present — removing it changes behaviour for clients that strip
  `Diagnostic.data`, and no test covers that path, so it wants a
  deliberate decision rather than being folded into this release.

## 0.3.0 — 2026-07-24

Error-handling and diagnostics release. Everything the server used to
swallow silently — connection failures, malformed configuration,
workspace-scan truncation, host-callback failures — now reaches the
IDE, and syntax/semantic diagnostics carry precise ranges, stable
codes, and actionable wording. Full audit trail in
[`docs/pain-points.md`](docs/pain-points.md).

### Added

- **Stable diagnostic codes** on every diagnostic
  (`src/semantic/codes.rs`): `parse` (pre-existing), `unknown-table`,
  `unknown-field`, `permission-denied`, `permission-unknown`,
  `dynamic-target`. `Diagnostic.data` carries structured payloads
  (`{table, field?, suggestion?}`) and `relatedInformation` points at
  the relevant definition.
- **Typo detection that actually fires.** `CREATE prson …` next to
  `DEFINE TABLE person` now yields ``Unknown table `prson`. Did you
  mean `person`?`` with a one-click replace quick fix — this path was
  previously dead code because schema inference masked it. Unknown
  fields on SCHEMAFULL tables get the same treatment. Guarded against
  false positives (PR #18 review): singular/plural sibling names
  (`orders`/`order`) never trigger, names used in more than one
  statement are treated as deliberate tables, and detection stands
  down entirely while the live-metadata connection is failing (remote
  tables missing from the model would otherwise light up local
  near-misses in bulk).
- **Syntax-error hints**: misspelled keywords are detected against the
  grammar's own keyword list (``… Did you mean `FROM`?``); MISSING
  tokens name what was expected (``Expected `)`.``, ``Expected
  `THEN`.``) with visible (non-zero-width) squiggles.
- **Live-metadata failure surfacing**: SurrealDB connection, auth, and
  timeout errors now produce a `window/showMessage` toast (once per
  distinct failure set) plus per-error `window/logMessage` entries,
  and an INFO log when the connection recovers.
- **Configuration validation**: malformed `surrealql` settings
  payloads, unknown `metadata.mode` values, and unknown
  `activeAuthContext` names are reported via `window/logMessage`
  instead of being silently ignored. Misspelled setting *keys* are
  detected too (serde otherwise ignores unknown fields):
  ``unknown setting `connection.endpint` — did you mean `endpoint`?``.
  Warning sets are deduplicated — a persistently bad configuration
  logs once per distinct set, with an INFO line when the warnings
  resolve.
- **Workspace-scan reporting**: unreadable entries, oversized files,
  and the file-count cap are counted and summarized in the log; a
  toast fires when the cap truncates indexing.
- **`window/showMessage` support**: new `LspNotifier::show_message`
  (default impl falls back to `log_message`); WASM hosts can pass an
  optional `onShowMessage` callback — the existing three-callback
  constructor keeps working unchanged.
- **Native panic hook**: panics are printed to stderr (visible in the
  editor's output panel) before the process aborts.
- New test infrastructure: end-to-end core-server suite with recording
  mocks (`tests/core_server.rs`), native JSON-RPC wire tests
  (`tests/dispatch.rs`), and backwards-compatibility tripwires
  (`tests/compat.rs` — capabilities golden, config shape/alias/env
  tables, diagnostic identity).

### Changed

- **Semantic diagnostics anchor to the offending token** (the `prson`
  in `CREATE prson`), not the whole statement. Quick-fix edits
  therefore replace exactly the token.
- **Giant syntax squiggles are clamped**: a multi-line ERROR region is
  reported on its first line with the full extent in
  `relatedInformation`, and nested errors inside it are surfaced
  separately (capped at 100 diagnostics per document).
- **"Target could not be resolved statically" is quieter**: suppressed
  for `$parameter` and expression (function call / subquery / block)
  targets, and SELECT target extraction now understands the current
  grammar shape (previously *every* SELECT warned).
- **`workspace/didChangeConfiguration` merges** partial payloads over
  the in-flight settings instead of resetting them — a payload that
  omits the connection block no longer wipes the endpoint configured
  via `initializationOptions`. A `null` settings payload triggers a
  configuration pull. *(Behavior change for clients that relied on
  partial payloads resetting everything.)*
- **Unknown `metadata.mode` strings repair to the default**
  (`workspace+db`) with a warning instead of silently disabling all
  schema sources. *(Behavior change; the supported off-switches remain
  `enableLiveMetadata: false` and the explicit mode strings.)*
- **Unknown-field checks are restricted to SCHEMAFULL tables** —
  explicit SCHEMALESS tables legitimately accept ad-hoc fields. The
  builtin `id`/`in`/`out` fields are always allowed, and RELATE
  statements are exempt (their SET fields belong to the edge table,
  not the subject tables).
- **Settings and metadata application is serialized** behind an
  internal lock — concurrently spawned `didChangeConfiguration` /
  `didSave` handlers can no longer lose each other's updates or
  invert the reported metadata-connection status.
- WASM: diagnostics that fail to serialize are no longer published as
  `NULL` (the host keeps its previous set and the failure is logged);
  malformed notifications and skipped `replaceWorkspace` entries are
  logged; `onRequestConfiguration` failures are logged per cause.
- Versions realigned: crate and npm package both ship 0.3.0 (they had
  drifted to 0.2.0 / 0.2.1), so `serverInfo.version` matches npm.

### Compatibility

- LSP wire: capabilities unchanged; `Diagnostic.source` remains
  `"surreal-language-server"`; syntax code remains `"parse"`; the
  severity mapping is unchanged. New `code`/`data`/
  `relatedInformation` fields are standard optional LSP fields.
- Quick fixes now match on `Diagnostic.code` + `data`; the legacy
  message-text fallback is retained for **one release** and will be
  removed in 0.4.
- WASM JS API: constructor, method names, and JSON-RPC response
  strings are unchanged; `onShowMessage` is optional.
- Config: all existing shapes, aliases, defaults, and `SURREALDB_*`
  environment fallbacks are preserved (pinned by `tests/compat.rs`).
- Crate API: `LspNotifier` gained `show_message` with a default impl;
  `QueryFact` gained `#[serde(default)]` fields (`target_refs`,
  `field_refs`, `target_resolution`); `WorkspaceIndex` gained
  `scan_stats`. Exhaustive struct literals of these types need
  updating — hence the 0.3.0 bump.
