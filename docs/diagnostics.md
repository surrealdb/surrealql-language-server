# Diagnostic codes

Every diagnostic this server emits carries a stable `code`. The codes are
wire-visible and **never renamed** (only added), so a repair, a CI filter or a
client setting can key on one and keep working. This page is what
`Diagnostic.codeDescription` links to, and what `check explain <code>` prints.

Each section says the same three things: what the code means, why SurrealDB
refuses the query, and what fixing it looks like.

Two facts worth knowing before the list:

- **Severity is not uniform.** An `error` means the engine rejects the query; a
  `warning` means this server believes something is wrong but the engine will
  run it anyway. Key a build failure on `--fail-on error` unless you have a
  reason not to.
- **Several carry structured hints** in `Diagnostic.data`. Where a section says
  so, prefer the hint over re-deriving the fix from the message text.

---

## parse

**Severity:** error.

Tree-sitter could not parse the source. The range is clamped to the first line
of the failure, with `relatedInformation` pointing at the full extent when the
error spans more than one line.

SurrealDB will not parse it either, so the query cannot run. Fix the syntax.

A `parse` error is trustworthy: there is currently no shape of valid SurrealQL
that the pinned grammar rejects. When that stops being true it is recorded in
[grammar-gaps.md](grammar-gaps.md) and listed in [AGENTS.md](../AGENTS.md)
before it can reach an agent.

Two shapes report `parse` for a reason other than syntax, and say so in the
message: a document nesting deeper than 1,024 levels, and one larger than
`analysis.maxDocumentBytes`. Neither is analysed at all.

## unknown-type

**Severity:** error.

A type position holds a word SurrealQL's kind grammar does not have:
`LET $x: strng = 1`, or `DEFINE FIELD n ON t TYPE numbr`.

This is a *syntax* fault rather than a judgement about a value: the engine
refuses to parse an unknown kind name at all (`expected a kind name`), so the
query never runs. It is therefore reported even when
`analysis.enableTypeChecking` is off.

A quick fix offers the nearest real type name.

## unknown-table

**Severity:** warning. **Carries `data`:** `{ "table": …, "suggestion": … }`.

A queried table reads as a typo of a table the workspace explicitly defines.
Only explicit definitions count: a table this server merely inferred from usage
is not evidence that a similar name is wrong.

SurrealDB will run the query (it creates tables on demand), which is exactly
why this is worth reporting. The failure is silent: rows go to a table nobody
meant to have.

Prefer `data.suggestion` over parsing the message. If the name is deliberate,
define the table, or pass `--workspace` so the definition is in scope.

## unknown-field

**Severity:** warning.

A field that the table's definition does not declare, on a table whose schema is
closed (`SCHEMAFULL`).

On a `SCHEMALESS` table ad-hoc fields are legal SurrealQL, so this is silenced
there by default: see `analysis.schemalessDiagnostics` in the
[README](../README.md#analysisschemalessdiagnostics).

## permission-denied

**Severity:** error.

Static evaluation of the table's `PERMISSIONS` clause proved the **active auth
context** is denied this operation. The context comes from
`authContexts` / `activeAuthContext`; the default is a single `viewer`.

The engine will refuse the query for the same reason. Either the query is wrong
or the auth context configured here does not match the one it runs under.

## permission-unknown

**Severity:** warning.

The `PERMISSIONS` clause could not be decided statically: typically a row-level
rule such as `WHERE user = $auth.id`, whose answer depends on data.

Not a defect. It records that this server *cannot* tell you whether the query is
permitted, which is different from telling you it is.

## dynamic-target

**Severity:** warning.

A statement whose target is only knowable at run time: `DELETE $table`, or a
target computed by an expression.

Nothing is wrong with the query. It means no schema-aware check could run on
that statement, so a clean result does not mean it was examined.

## argument-type

**Severity:** error.

An argument whose type cannot satisfy the parameter the function declares. The
signatures come from SurrealDB's own source, generated into
[builtins.json](../builtins.json), not from documentation.

Only definite mismatches are reported: anything the inference is unsure about
stays silent. So this firing means the engine will raise
`Incorrect arguments for function …`.

## argument-count

**Severity:** error.

Too many or too few arguments. Arity comes from the same generated catalogue.

In `builtins.json`, `params: null` means the signature is **unknown**, not
zero-arity: a function with `null` params is never reported here.

## let-type

**Severity:** error.

`LET $x: T = …` where the value cannot satisfy `T`. SurrealDB coerces the value
to the declared type and fails with `Tried to set '$x', but couldn't coerce
value`.

## return-type

**Severity:** error.

A `DEFINE FUNCTION … -> T` whose body returns a value `T` rejects. The engine
coerces a function's result to its declared type and fails with
`Couldn't coerce return value from function`.

## field-type

**Severity:** error.

A `DEFINE FIELD … TYPE T` whose `DEFAULT`, `VALUE` or `COMPUTED` expression
cannot satisfy `T`. All three are coerced by the engine.

`ASSERT` is deliberately not checked: it is a predicate over `$value`, not a
value coerced to `T`, so comparing it against the declared type would be wrong.

Note this fires on a `SCHEMALESS` table too under
`analysis.schemalessDiagnostics: errors`, because the engine coerces there as
well: a file that always fails can look clean under the default `quiet`.

## operator-type

**Severity:** error.

An operator whose operand types SurrealDB rejects: `"a" + 1`, `"8" % "3"`,
`-[1, 2, 3]`. The operand tables are transcribed from the engine's own
`TryAdd`/`TrySub`/`TryMul`/`TryDiv`/`TryRem` implementations, and the message
uses the engine's wording.

Division is never reported: `a / b` with no matching arm yields `NaN` rather
than failing, so there is no error to surface.

## unknown-method

**Severity:** error.

A method the receiver's type does not have: `"abc".nonsense()`. The engine
refuses it with `no such method found for the string type`.

Reported only when the receiver's type is *certain*. An inferred or unknown
receiver stays silent.

## undefined-variable

**Severity:** error.

A `$variable` that nothing in scope binds.

If your caller binds it at run time (`db.query(sql).bind(("id", id))`, or
Surrealist's variables panel) declare it rather than changing the query:

```bash
surrealql-language-server check q.surql --param id --param limit
```

or `analysis.externalParams` in the editor settings.

## renamed-function

**Severity:** warning. **Carries `data`:** the new name.

A builtin called by a name SurrealDB has renamed. The engine still accepts the
old name and records the replacement itself, which is why this is a warning.

This is the one code whose fix is mechanical enough to apply unattended: the
replacement comes from the engine's rename table, not from a guess. See
`check --fix renamed-function`.

## not-callable

**Severity:** warning.

A name the parser accepts that no implementation backs in call form, so the
query parses and then fails at run time.

A warning rather than an error because the claim rests on reading the engine's
dispatch tables rather than on running the query.
