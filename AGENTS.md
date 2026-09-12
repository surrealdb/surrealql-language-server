# Agent Guide

How an AI coding agent works *on* this repository and *with* the shipped
binary. This is not user documentation — that is the [README](README.md) —
and it states rules, not aspirations: every claim below is pinned by a test
or a generated file.

## What this is

A Language Server Protocol implementation for
[SurrealQL](https://surrealdb.com/docs/surrealql), with two headless
surfaces built for machines:

- **`surrealql-language-server check`** — one-shot diagnostics for files,
  directories, or stdin. The generate → check → repair loop.
- **[`builtins.json`](builtins.json)** — every builtin function SurrealDB
  accepts (signatures, return types, arity, renames, method receivers),
  generated from the engine's own source. Use it instead of guessing a
  function's shape.
- **`surrealql-language-server schema`**: the tables, fields, types,
  permissions, indexes and functions a workspace defines. Read it *before*
  writing a query rather than discovering the schema from `check` afterwards.

The opportunity map behind both is [`docs/ai-plan.md`](docs/ai-plan.md).

## Build and test

The build needs a sibling checkout of the tree-sitter grammar — this is the
first thing that bites every fresh clone:

```bash
bash scripts/setup-grammar.sh   # or: TREE_SITTER_SURREALQL_DIR=/path cargo build
cargo test
```

The grammar revision lives in [`grammar.pin`](grammar.pin) and nowhere else.
`build.rs` fails the build when the checkout has drifted off it, naming the one
command that fixes it: a checkout that silently predates the pin used to fail
about twenty tests with `parse` errors on valid SurrealQL, which reads as a
language-server bug.

The catalogue tests and the conformance sweep additionally want a SurrealDB
checkout, at the revision [`surrealdb.pin`](surrealdb.pin) names:

```bash
bash scripts/setup-surrealdb.sh   # or: SURREALDB_DIR=/path cargo test
```

Without one they skip and say so. With one at a *different* revision they run
and report version skew as though it were a defect, so both suites print which
revision they read when the two disagree.

`make help` lists every maintenance target.

## Checking SurrealQL

Run `check` after every `.surql` edit, exactly as you would run `cargo check`
after a Rust edit:

```bash
surrealql-language-server check queries/            # a directory
surrealql-language-server check --stdin --stdin-filename src/feed.surql \
    --workspace schema/                             # an unsaved buffer
surrealql-language-server check queries/ --format json --fail-on warning
```

| Exit | Meaning |
| --- | --- |
| 0 | Ran to completion; nothing at or above `--fail-on` (default `error`). |
| 1 | Ran to completion; diagnostics at or above the threshold. |
| 2 | Usage error, unreadable input, or a skipped target file. Exit 0 never claims coverage the run did not have. |

Facts a machine consumer must know:

- **JSON diagnostics are LSP wire objects verbatim**: 0-based lines, UTF-16
  columns, camelCase keys, integer severity (1 error, 2 warning), string
  `code`. Text output is 1-based in both line and column.
- **`--workspace <dir>` supplies schema context** (definitions in other
  files) and is never reported on. Without it, cross-file table references
  look undefined.
- **`--stdin-filename` shadows the on-disk file** of the same path, so an
  edited-but-unsaved buffer checks against the rest of the workspace.
- **`--param <name>` declares a variable your caller binds at runtime**
  (`db.query(sql).bind(("id", id))`), suppressing `undefined-variable` for
  it. `--config file.json` accepts the same JSON an editor sends the LSP.
- **`--format json` prints exactly one JSON object on stdout, for every exit
  code.** A run that could not complete (exit 2) prints a report whose `files`
  is empty and whose `error` names the kind (`usage`, `invalid-config`,
  `unreadable-input` or `analysis-failed`) alongside prose in `message`. Key
  repairs on `error.kind`; it is stable, the message is not. A clean run carries
  no `error` key at all. Never treat empty stdout as a result.
- **`check` never connects to a database.** `SURREALDB_ENDPOINT` has no
  effect on it.

## As an MCP server

For a harness that calls tools rather than shelling out:

```bash
surrealql-language-server mcp --workspace schema/
```

Five tools over stdio, each a thin adapter over the same analysis everything
else here uses: `validate_surrealql`, `get_schema`, `lookup_function`,
`search_functions`, `explain_diagnostic`. `tools/list` describes them.

Two conventions worth knowing:

- A tool that cannot answer returns a **result** carrying `isError`, not a
  JSON-RPC error, and the text says why. A JSON-RPC error means the protocol
  broke, not that the question had no answer.
- `validate_surrealql` checks against the schema in `--workspace`, so it catches
  a misspelled table as well as a syntax error. Without `--workspace` it still
  checks syntax and types; `get_schema` says so rather than returning an empty
  schema that reads as "this database has no tables".

Tool names and their input schemas are a permanent surface, pinned by
[`tests/mcp.rs`](tests/mcp.rs).

## Reading the schema

```bash
surrealql-language-server schema schema/            # SurrealQL-shaped, for a prompt
surrealql-language-server schema schema/ --format json
```

The default format is DDL-shaped prose, which is both denser than JSON and the
form a model has seen most of. It marks two things worth noticing:

- a table or field with `-- inferred` was **not defined anywhere**: it is what
  the queries imply, not a promise about what exists;
- a table's `PERMISSIONS` clause is printed, because it is the thing most likely
  to make a syntactically perfect query fail at run time.

`--format json` is a compatibility surface: `schemaVersion` is `1`, the field
shape is pinned by [`tests/compat.rs`](tests/compat.rs), and changes to it are
additive. Exit 2 means nothing readable was found: never an empty schema that
looks like an answer.

Like `check`, `schema` never connects to a database.

## Diagnostic codes

Stable, wire-visible strings from
[`src/semantic/codes.rs`](src/semantic/codes.rs) — never renamed, only
added. Key repairs on the code, not the message text.

| Code | Severity | Meaning |
| --- | --- | --- |
| `parse` | error | Tree-sitter could not parse the source. Every one is a real syntax error: see *Known false positives* below. |
| `unknown-type` | error | A type position holds a word SurrealQL's kind grammar does not have. |
| `unknown-table` | warning | A queried table reads as a typo of an explicitly defined one. |
| `unknown-field` | warning | A field not defined on an explicit (closed-schema) table. |
| `permission-denied` | error | Static permission evaluation proved the active auth context is denied. |
| `permission-unknown` | warning | Permission evaluation could not decide (row-level rules). |
| `dynamic-target` | warning | A statement target only resolvable at runtime. |
| `argument-type` | error | An argument cannot satisfy the declared parameter type. |
| `argument-count` | error | Too many or too few arguments. |
| `let-type` | error | A `LET $x: T = …` value cannot satisfy `T`. |
| `return-type` | error | A function body returns a value its declared return type rejects. |
| `operator-type` | error | Operand types SurrealDB's operator tables reject (`"a" + 1`). |
| `unknown-method` | error | A method the receiver's type does not have. |
| `undefined-variable` | error | A `$variable` nothing in scope binds (see `--param`). |
| `renamed-function` | warning | A builtin called by a name SurrealDB has renamed; `data` carries the new name. |
| `not-callable` | warning | A name the parser accepts but no implementation backs in call form. |
| `field-type` | error | A `DEFINE FIELD` whose `DEFAULT`/`VALUE`/`COMPUTED` cannot satisfy the declared type. |

Several diagnostics carry structured hints in `data` — for example
`unknown-table` includes `{"table": …, "suggestion": …}`. Prefer the hint
over re-deriving the fix.

Every diagnostic also carries `codeDescription.href`, pointing at the section of
[`docs/diagnostics.md`](docs/diagnostics.md) that explains it. Offline, the same
prose is one command away:

```bash
surrealql-language-server check explain unknown-table
```

### Narrowing and repairing

```bash
check q.surql --only argument-type --only argument-count   # report these codes
check q.surql --ignore dynamic-target                      # report all but these
check q.surql --fix renamed-function                       # repair in place
```

- `--only` / `--ignore` filter **reporting**, not analysis, and the JSON report
  carries a `filters` object saying what was hidden: a filtered clean run is
  not a clean run, and the report must not let it look like one. An unknown code
  is a usage error rather than a filter that silently matches nothing.
- `--fix` takes an explicit code and **only `renamed-function` is accepted**.
  That is not a temporary limitation: its replacement comes from SurrealDB's own
  rename table, while every other fix here is inferred: `unknown-table`'s is a
  string-distance guess, and applying it unattended can repoint a query at a
  *different real table*. A run that rewrote files reports `fixed` and
  re-analyses, so it never reports the errors it just repaired.

## Known false positives

**There are currently none.** Every `parse` error the server reports is a real
syntax error, and a query that trips one needs fixing.

This section used to list six shapes of valid SurrealQL that the pinned grammar
rejected: a union type in a `LET` annotation, a sized collection type, a nested
`SET` target, `%`, unary minus, and mock syntax. All of them were fixed upstream
in `surrealql-tree-sitter` and the pin now names a revision that parses them.

The category is not closed, only empty. The grammar is pinned in
[`grammar.pin`](grammar.pin), and when a gap is found again it is recorded in
[`docs/grammar-gaps.md`](docs/grammar-gaps.md) and listed here before it can
reach an agent. Until then, treat `parse` as trustworthy.

## Contributing rules

- **Compat discipline.** Wire shapes, config keys, diagnostic codes, the
  `check` JSON report, and the `builtins.json` entry shape are pinned by
  [`tests/compat.rs`](tests/compat.rs). Never update a golden to make a
  failure go away; a compat failure is a client-visible change that needs a
  reviewed decision.
- **No new shared dependencies.** The `[dependencies]` section of
  `Cargo.toml` is also the WASM dependency graph. Native-only crates go
  under `cfg(not(target_arch = "wasm32"))`; prefer zero new dependencies
  (the arg parsers here are hand-rolled on purpose).
- **Never hand-edit a generated file.** `src/grammar_generated.rs` and
  `builtins.json` come from `make builtins`; CI fails if they drift.
- **Small, focused pull requests**, one concern each, with tests in the
  same PR.
