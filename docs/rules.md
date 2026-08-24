# SurrealQL diagnostic rules

<!-- Generated from `src/semantic/rules.rs`. Do not edit by hand -- `rule_catalogue_is_in_sync` prints the replacement when it drifts. -->

Every diagnostic carries a stable `code`. Set its severity with `analysis.ruleSeverity`, or silence it in place with `-- surql-ignore: <code>`. Both are described in `README.md`.

| Rule | Category | Default | Fix |
| --- | --- | --- | --- |
| [`argument-count`](#argument-count) | types | error | — |
| [`argument-type`](#argument-type) | types | error | — |
| [`duplicate-definition`](#duplicate-definition) | schema | warning | — |
| [`dynamic-target`](#dynamic-target) | schema | warning | — |
| [`field-type`](#field-type) | types | error | — |
| [`let-type`](#let-type) | types | error | — |
| [`not-callable`](#not-callable) | types | warning | — |
| [`operator-type`](#operator-type) | types | error | — |
| [`permission-denied`](#permission-denied) | permissions | error | — |
| [`permission-unknown`](#permission-unknown) | permissions | warning | — |
| [`renamed-function`](#renamed-function) | types | warning | automatic |
| [`return-type`](#return-type) | types | error | — |
| [`undefined-variable`](#undefined-variable) | types | error | — |
| [`unknown-field`](#unknown-field) | schema | warning | — |
| [`unknown-table`](#unknown-table) | schema | warning | suggested |
| [`unknown-index-field`](#unknown-index-field) | schema | warning | — |
| [`unknown-analyzer`](#unknown-analyzer) | schema | warning | — |
| [`unknown-function`](#unknown-function) | types | error | suggested |
| [`relation-endpoint`](#relation-endpoint) | schema | error | — |
| [`unused-binding`](#unused-binding) | types | hint | — |
| [`unused-suppression`](#unused-suppression) | syntax | hint | — |
| [`unknown-method`](#unknown-method) | types | error | — |
| [`unknown-type`](#unknown-type) | syntax | error | suggested |
| [`parse`](#parse) | syntax | error | — |

## argument-count

**A call passes too many or too few arguments.**

- Category: types
- Default severity: error
- Fix: none

The call site's argument count falls outside the arity the function declares. Arity comes from the generated catalogue for a builtin, and from the `DEFINE FUNCTION` signature for a user function. A call with no arguments at all is never reported, so that typing `f(` does not squiggle.

## argument-type

**An argument's type cannot satisfy the declared parameter type.**

- Category: types
- Default severity: error
- Fix: none

Reported only when the coercion relation answers `Incompatible`. An argument whose type cannot be determined answers `Unknown` and is passed over in silence.

## duplicate-definition

**The same name is defined more than once in the workspace.**

- Category: schema
- Default severity: warning
- Fix: none

Two `DEFINE` statements for the same table, field or function. The merge keeps one and drops the other silently, so the file that loses can look as though it were never read. A warning rather than an error: redefining is legal SurrealQL, and a migration script may do it deliberately.

## dynamic-target

**The statement target could not be resolved to a static table name.**

- Category: schema
- Default severity: warning
- Fix: none

The analyzer could not name the table a statement acts on, and the target is neither a parameter nor an expression — those two are legitimately dynamic and are classified out before this fires.

## field-type

**A DEFINE FIELD DEFAULT, VALUE or COMPUTED expression cannot satisfy the declared TYPE.**

- Category: types
- Default severity: error
- Fix: none

The engine coerces all three clauses to the declared type and fails with `Couldn't coerce value for field …`. `ASSERT` is deliberately excluded: it is a predicate over `$value`, not a value coerced to the declared type, so nothing may compare it against that type.

## let-type

**A LET value cannot satisfy its declared type.**

- Category: types
- Default severity: error
- Fix: none

`LET $x: T = …` where the value's type cannot be coerced to `T`. When the whole-value verdict is silent, each element of an array or object literal is judged on its own, so a single bad member is still reported.

## not-callable

**A builtin the parser accepts that no implementation backs in call form.**

- Category: types
- Default severity: warning
- Fix: none

The query parses and then fails at run time. A warning rather than an error, because the claim rests on reading the engine's dispatch tables rather than on the engine refusing the text. Deliberately not applied to the method form.

## operator-type

**An arithmetic operator whose operand types SurrealDB rejects.**

- Category: types
- Default severity: error
- Fix: none

Such as `"a" + 1`. The operand tables are transcribed arm-by-arm from the engine, and both operand types must be certain before this fires. Division never fails, so it is never reported.

## permission-denied

**Static permission evaluation proved the active auth context is denied.**

- Category: permissions
- Default severity: error
- Fix: none

The table or field declares `PERMISSIONS NONE`, or a role check that the active auth context cannot satisfy. `SELECT` and `RELATE` are exempt: their rules are row-level and cannot be decided without data.

## permission-unknown

**Static permission evaluation could not decide.**

- Category: permissions
- Default severity: warning
- Fix: none

The permission expression reads something only the running query knows — a record identity, a session value — or no explicit rule was found at all.

## renamed-function

**A builtin called by a name SurrealDB has renamed.**

- Category: types
- Default severity: warning
- Fix: offered, and safe to apply automatically

The engine still accepts the old name and records the replacement itself, so this is a warning rather than an error. The fix is machine-applicable: the new name comes out of the engine's own rename table, not out of an edit-distance guess.

## return-type

**A RETURN yields a value the function's declared return type cannot accept.**

- Category: types
- Default severity: error
- Fix: none

The engine coerces a function's result to its declared type and fails with `Couldn't coerce return value from function …`. Both an explicit `RETURN` and a block's trailing expression are checked, including inside an `IF` branch or a `FOR` body, which really do return from the enclosing function.

## undefined-variable

**A variable reference that nothing in scope binds.**

- Category: types
- Default severity: error
- Fix: none

Bindings come from `LET`, function parameters, `FOR` and closure parameters, plus `DEFINE PARAM` and the special variables the engine supplies. Names listed in `analysis.externalParams` are treated as bound.

## unknown-field

**A query touches a field that is not defined on an explicit table.**

- Category: schema
- Default severity: warning
- Fix: none

Only reported where the schema is closed — `SCHEMAFULL`, or `SCHEMALESS` under `analysis.schemalessDiagnostics: "strict"`. `id`, `in` and `out` are always allowed, and `RELATE` is skipped entirely.

## unknown-table

**A query targets a table with no known definition.**

- Category: schema
- Default severity: warning
- Fix: offered; review it before applying

Needs undegraded metadata: a half-loaded schema makes every table look undefined. A table the analyzer inferred from the very statement that names it is reported only when it is used once and a near-miss explicit table exists, which is what turns this from dead code into typo detection.

## unknown-index-field

**A DEFINE INDEX names a field the table does not declare.**

- Category: schema
- Default severity: warning
- Fix: none

The index is built over a field that has no `DEFINE FIELD` on the table. On a `SCHEMAFULL` table that field can never hold a value, so the index can never match anything.

## unknown-analyzer

**A DEFINE INDEX names an analyzer nothing defines.**

- Category: schema
- Default severity: warning
- Fix: none

A full-text index refers to an analyzer by name. Nothing validated that the name existed, so a typo produced an index that could not be built.

## unknown-function

**A call to a function that does not exist.**

- Category: types
- Default severity: error
- Fix: offered; review it before applying

The name is neither in the builtin catalogue nor defined anywhere in the workspace. Needs the merged model: a real function defined in a file the server has not read is not an unknown function, so this rule stands down when the workspace is unavailable.

## relation-endpoint

**A RELATE points at a table the edge does not declare.**

- Category: schema
- Default severity: error
- Fix: none

The edge table declares `TYPE RELATION IN … OUT …` and marks it `ENFORCED`, which means the engine itself refuses a row outside those lists. Only reported for an enforced relation: without `ENFORCED` the declaration is documentation, not a constraint.

## unused-binding

**A binding nothing reads.**

- Category: types
- Default severity: hint
- Fix: none

A `LET` whose value is never used, or a `DEFINE PARAM` or `DEFINE FUNCTION` nothing in the workspace calls. Reported as a hint and tagged `Unnecessary`, so an editor greys it out rather than adding a warning to the problems panel. Needs the merged model: a function called from a file the server has not read is not unused.

## unused-suppression

**A suppression directive that silenced nothing.**

- Category: syntax
- Default severity: hint
- Fix: none

The rule the directive names did not report where the directive sits. Either the fault was fixed and the comment outlived it, or the directive is on the wrong line. Reported as a hint and tagged `Unnecessary`, so an editor greys it out — a stale suppression is worth removing, not worth interrupting for. Silence it for a file that keeps directives deliberately with `-- surql-ignore-file: unused-suppression`; a line directive cannot cover it, because a line directive's scope is the next line of code rather than the line it sits on.

## unknown-method

**A method the receiver's type does not have.**

- Category: types
- Default severity: error
- Fix: none

Such as `"abc".nonsense()`. Reported only when the receiver's type is certain. An object receiver is exempt, because the engine falls back to a closure-valued field there.

## unknown-type

**A type position holds a word SurrealDB's kind grammar does not have.**

- Category: syntax
- Default severity: error
- Fix: offered; review it before applying

Such as `LET $x: xxx = 2`. Unlike the other judgements this is a *syntax* fault: the engine refuses to parse it at all, so the query never runs. It is therefore reported from the syntax pass and is not gated by `analysis.enableTypeChecking`.

## parse

**The grammar could not parse this text.**

- Category: syntax
- Default severity: error
- Fix: none

Covers both a missing token and an unparseable region. A multi-line error region is clamped to its first line, with the full extent attached as related information, so one typo does not smear a squiggle over the rest of the file.
