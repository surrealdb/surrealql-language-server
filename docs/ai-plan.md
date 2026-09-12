# AI relevance plan

What the SurrealQL language server can become in a world where most SurrealQL
is written by models, not people. This is an opportunity map, not a committed
roadmap — each item names the asset it builds on, the work it needs, and the
order that makes sense.

## Why now

Three things changed around us:

1. **Agents write SurrealQL.** Copilot, Cursor, Claude Code and Surreal's own
   Sidekick generate queries all day. SurrealQL is exactly the kind of language
   models get wrong: niche (thin training data), fast-moving (2.x → 3.x renamed
   functions — we ship a `renamed-function` diagnostic for precisely this), and
   superficially SQL-like, so models confidently produce plausible non-SurrealQL.
2. **SurrealDB shipped [SurrealMCP](https://github.com/surrealdb/surrealmcp)** —
   the official MCP server that hands agents a *live database*: connect, query,
   cloud auth. It deliberately does not do language intelligence.
3. **Generic LSP↔MCP bridges became a category**
   ([mcpls](https://github.com/bug-ops/mcpls),
   [lsp-mcp](https://mcpservers.org/servers/Tritlo/lsp-mcp),
   [agent-lsp](https://blog.blackwell-systems.com/posts/agent-lsp/)) — proof
   that agents want LSP-grade truth, and that generic wrappers are the clumsy
   way to get it: they speak positions-in-open-documents, not
   "here is a query string, tell me what's wrong with it".

That leaves a niche only this project can fill: **the ground-truth engine for
SurrealQL as a language** — no database required, runs native and in the
browser, knows the user's actual schema from their `.surql` files, and knows
the real builtin surface because the catalogue is generated from the engine
source itself. SurrealMCP runs your query; we're the thing that makes sure
it's worth running.

## Assets we already have

Every opportunity below reuses something that exists today:

| Asset | Where | Why AI toolchains want it |
| --- | --- | --- |
| Transport-agnostic JSON-RPC core | `src/core/dispatch.rs` | A third front-end (CLI, MCP) beside native/WASM is a thin adapter, not a rewrite |
| Merged workspace schema model | `src/semantic/model.rs` | Schema truth from DDL files + live metadata, without a running database |
| Builtin catalogue, 434 functions, engine-generated | `src/grammar_generated.rs`, `xtask/` | Signatures, return types, arity, method receivers, renames — ground truth no model has memorised correctly |
| Stable diagnostic codes | `src/semantic/codes.rs`, pinned by `tests/compat.rs` | Machine-actionable errors an agent can key repairs on |
| Browser WASM package on npm | `src/wasm/`, `pkg/` | Client-side validation anywhere JS runs — Surrealist, playgrounds, docs |
| Conformance sweep over SurrealDB's corpus | `tests/conformance.rs` | Seed material for a generation eval set |
| Query result-type resolution (#31) | semantic layer | The bridge from queries to typed application code |

## Opportunities

### 1. Headless `check` mode — the cheapest way in

`surrealql-language-server check [paths|--stdin] [--format json]`: parse +
semantic diagnostics, exit code, no LSP lifecycle. `src/main.rs` takes no
arguments today, so this is purely additive.

One feature, three audiences: coding agents shell out to it from any harness
(no MCP setup needed), CI lints `.surql` files on every PR, and pre-commit
hooks catch drift locally. Agents iterate against it exactly the way they
iterate against `cargo check` — generate, check, repair — which is the single
highest-leverage loop we can offer. JSON output must carry the stable codes
plus spans, and ideally a `hint` field (the did-you-mean machinery exists).

Effort: small. The analyzer and model are callable without a client; this is
argument parsing plus an output serializer.

### 2. First-party MCP server mode

`surrealql-language-server mcp` (stdio), exposing semantic tools no generic
LSP bridge can synthesize:

| Tool | Backed by |
| --- | --- |
| `validate_surrealql(text, options)` | analyzer + merged model — one-shot string in, diagnostics out |
| `get_schema(format)` | merged workspace model (tables, fields, types, edges, permissions) |
| `lookup_function(name)` / `search_functions(query)` | the generated catalogue, including rename redirects |
| `infer_result_type(query)` | result-type resolution from #31 |
| `explain_diagnostic(code)` | codes registry + curated prose |

Positioning: complementary to SurrealMCP, never overlapping — no query
execution, no connections, no auth. A `.mcp.json` recipe in the README makes
it one paste away in Claude Code, Cursor, Zed, Copilot.

Implementation note: MCP is JSON-RPC over stdio, the same shape
`core/dispatch.rs` already speaks — this is a third front-end on the existing
seam. The MCP SDK dependency (`rmcp`) must stay behind
`cfg(not(target_arch = "wasm32"))` like tokio does; the shared dependency
graph is also the WASM graph and `Cargo.toml` enforces that discipline.

Effort: medium. The tools are thin wrappers; the work is schema/format design
and docs.

### 3. Schema context export — stop hallucinations at the source

`surrealql-language-server schema --format llm|json` emitting a compact,
token-efficient summary of the merged model: tables, fields with types,
record links and graph edges, permission posture. Users paste it into
prompts; tools inject it automatically; Sidekick could ground its
generations in the user's actual schema instead of documentation alone.

The merged model already computes all of this for hover and completions —
this is a serializer, not new analysis. The `llm` format is worth real design
effort (dense, unambiguous, stable ordering for prompt caching).

Effort: small once (1) exists; the CLI plumbing is shared.

### 4. Publish the builtin catalogue as data

The catalogue is our most defensible artifact — 434 functions with
signatures, return types and renames, generated from SurrealDB's own source,
freshness-checked in CI. Today it's locked inside a `.rs` file.

Add an xtask emit target producing `builtins.json`, attach it to every GitHub
release and ship it in the npm package. Anyone building SurrealQL AI tooling
— including SurrealDB's own docs and Sidekick teams — gets ground truth
instead of scraping documentation. This is also the natural seed for an
`llms.txt` for SurrealQL functions.

Effort: small. `xtask/src/emit.rs` already joins all the data; add a second
renderer.

### 5. Diagnostics designed for machine repair

An agent repairs from structured errors far better than from prose. Three
additive steps:

- `codeDescription.href` on every diagnostic, pointing at a public
  per-code documentation page (what it means, why SurrealDB rejects it, the
  canonical fix).
- Structured fix hints in the diagnostic `data` payload where we already
  know the answer (`renamed-function` knows the new name; `unknown-field`
  has did-you-mean candidates).
- A published registry page generated from `codes.rs` so the codes are
  discoverable outside the editor.

**The flip side: false positives become poison.** A human shrugs off a wrong
squiggle; an agent obediently "fixes" valid code until it's invalid. The known
grammar gaps (`docs/grammar-gaps.md`: `int | float` in LET, `array<T, N>`,
nested `SET name.first = …`) move from cosmetic to blocking the moment agents
consume our diagnostics programmatically. Fixing them upstream — or tagging
known-gap patterns as suppressed in machine output — is a hard prerequisite
for (1) and (2) being trustworthy.

### 6. Close the loop in Surrealist — the WASM payoff

Surrealist already embeds our WASM package, and Sidekick already generates
queries. The missing piece is a one-shot convenience API on
`WasmLanguageServer` — `validateQuery(text) → diagnostics` — that skips LSP
lifecycle ceremony, so Surrealist can:

- badge AI-generated queries before the user runs them ("references unknown
  field `usernme`"),
- run a client-side repair loop: Sidekick generates → WASM validates →
  errors feed back → regenerate, all before the query ever reaches a
  database, at zero server cost.

This is the strongest "why the WASM investment mattered" story available:
schema-aware validation of AI output, in the browser, offline. Cross-repo
work with the Surrealist team; our side is small.

### 7. Agent-facing packaging and docs

Cheap, compounding visibility work:

- `AGENTS.md` in the repo (how an agent should build, test, and use the
  check mode) and an `llms.txt`.
- A docs page "Using the SurrealQL language server with AI agents" with
  copy-paste recipes: Claude Code (skill + `.mcp.json`), Cursor rules,
  Zed, Copilot.
- Ship a small Claude Code skill / plugin that teaches the agent to run
  `check` after every `.surql` edit.

Effort: small, and it's the discoverability layer for everything above.

### 8. Longer horizon: an eval set for SurrealQL generation

We sweep SurrealDB's own corpus in `tests/conformance.rs` and maintain
labelled fixtures for every diagnostic. Curated into a public eval set
(task → generated query → judged by the language server), this lets model
vendors and SurrealDB measure and improve SurrealQL quality in models — and
makes the language server the *judge* in that loop, which is a durable
moat. Better models trained/evaluated against it means fewer hallucinations,
which is the root fix.

### 9. Longer horizon: query → type codegen

Result-type resolution (#31) can emit TypeScript types for queries
(`typegen`). Agents writing application code then get a compile-time check
against the actual query shape — extending our feedback loop from the query
into the app. Bigger lift, likely its own tool; worth a design doc before
committing.

## Non-goals

- **No query execution, connections, or cloud auth.** That is SurrealMCP's
  job; overlap would confuse the story and drag in exactly the dependencies
  the WASM graph can't carry.
- **No LLM inside the server.** The server stays deterministic — it is the
  tool models call, not a model host. AI-powered quick fixes belong in the
  clients (Surrealist, editors) that already have model access.
- **No parallel "AI diagnostics" surface.** Same codes, same analysis
  everywhere; only the output format differs. `tests/compat.rs` discipline
  applies to every new surface from day one.

## Status

Every opportunity below except the two long-horizon ones has shipped:

| # | Item | State |
| --- | --- | --- |
| 1 | Headless `check` mode | ✅ 0.6, hardened in 0.7 (`--only`/`--ignore`, `--fix renamed-function`, `explain`, JSON on every exit code) |
| 2 | MCP server mode | ✅ `surrealql-language-server mcp`, five tools, no new dependency |
| 3 | Schema context export | ✅ `surrealql-language-server schema --format llm\|json` |
| 4 | Catalogue as data | ✅ `builtins.json` |
| 5 | Diagnostics for machine repair | ✅ stable codes, `data` hints, and `codeDescription` linking to `docs/diagnostics.md` |
| 6 | Surrealist loop | ✅ `validateQuery(text, params?)` on the browser build |
| 7 | Agent packaging and docs | ✅ `AGENTS.md`, `llms.txt` |
| 8 | Eval set | ⏳ own project |
| 9 | Query → type codegen | ⏳ own project |

**The cross-cutting gate is satisfied**: by its first branch rather than its
second. It asked that the grammar gaps be *fixed upstream, or* fenced in
machine-readable output, before phase 1 was promoted to agents. They were fixed:
the pin moved to a revision that parses all seven shapes, and `AGENTS.md` now
says there are no known false positives. Building the fencing machinery as well
would be machinery for a category with no members; when a gap reappears, it is
recorded in `docs/grammar-gaps.md` and listed in `AGENTS.md` before it can reach
an agent.

## Suggested sequencing

| Phase | Items | Why this order |
| --- | --- | --- |
| 1 — ship a tool agents can hold | (1) check mode, (4) catalogue JSON, (7) AGENTS.md + recipes | Small, additive, one release; makes every existing agent harness immediately better at SurrealQL via Bash alone |
| 2 — first-class agent integration | (2) MCP mode, (3) schema export, (5) code docs + hrefs | Builds on phase 1 plumbing; this is the strategic differentiator vs generic bridges |
| 3 — ecosystem loops | (6) Surrealist/Sidekick loop, (8) eval set, (9) typegen | Cross-repo and cross-team; phases 1–2 give these teams something concrete to integrate |

Cross-cutting gate: the grammar gaps in `docs/grammar-gaps.md` (false `parse`
errors on valid syntax) should be fixed upstream — or explicitly fenced in
machine-readable output — before phase 1 is promoted to agents. False errors
that a human ignores are instructions an agent follows.

## Constraints to respect throughout

- **WASM dependency graph**: anything MCP/CLI-only stays in the
  `cfg(not(target_arch = "wasm32"))` section, per the existing `Cargo.toml`
  layout.
- **Compat surfaces**: wire format, config keys and diagnostic codes are
  pinned by `tests/compat.rs`; every new surface is additive and gets its own
  tripwire.
- **Latency**: one-shot check/MCP calls are less keystroke-sensitive than the
  editor path, and the former `offset_to_position` hotspot no longer exists —
  `LineIndex` fixed it (`docs/pain-points.md`, H13), so large files are not a
  per-call risk.
