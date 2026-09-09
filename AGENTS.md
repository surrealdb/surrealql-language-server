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

The opportunity map behind both is [`docs/ai-plan.md`](docs/ai-plan.md).

## Build and test

The build needs a sibling checkout of the tree-sitter grammar — this is the
first thing that bites every fresh clone:

```bash
bash scripts/setup-grammar.sh   # or: TREE_SITTER_SURREALQL_DIR=/path cargo build
cargo test
```

`make help` lists every maintenance target. The catalogue tests additionally
want a SurrealDB checkout (`SURREALDB_DIR`, or `../surrealdb`); without one
they skip and say so.

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
- **`check` never connects to a database.** `SURREALDB_ENDPOINT` has no
  effect on it.

## Diagnostic codes

Stable, wire-visible strings from
[`src/semantic/codes.rs`](src/semantic/codes.rs) — never renamed, only
added. Key repairs on the code, not the message text.

| Code | Severity | Meaning |
| --- | --- | --- |
| `parse` | error | Tree-sitter could not parse the source (see the false-positive list below). |
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

## Known false positives — do not "fix" these

The pinned tree-sitter grammar rejects a few shapes that are **valid
SurrealQL**. They surface as `parse` errors. A `parse` error on one of these
shapes is a known grammar gap: **do not change the query**, and do not
"repair" it into something else. The full list with evidence is
[`docs/grammar-gaps.md`](docs/grammar-gaps.md); the shapes:

- A union type in a `LET` annotation: `LET $a: int | float = 2;`
- A sized collection type: `LET $b: array<float, 10> = 2;`
- A nested `SET` target: `CREATE person SET name.first = 'John';`
- The `%` operator: `8 % 3`
- Unary minus on a non-number: `-[1, 2, 3]`
- Mock syntax: `|test:1..4|`

These are grammar fixes in the `surrealql-tree-sitter` repository, not
query bugs. Everything else `parse` reports is a real syntax error.

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
