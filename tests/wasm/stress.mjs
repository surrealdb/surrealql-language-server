// Load wide and deep schemas into the built wasm package and drive every
// request against them. The native suite (`tests/nesting.rs`) cannot see
// either fault this guards: tree-sitter's C allocator corrupting the wasm
// heap (`src/wasm/alloc.rs`), and the 1 MiB wasm stack overflowing in a
// recursive walk (`MAX_TREE_DEPTH`). Both trap and kill the process, so
// reaching the end is the pass.
//
//   node tests/wasm/stress.mjs [pkg-dir]     (after `make wasm`)

import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const dir = resolve(process.argv[2] ?? new URL("../../pkg", import.meta.url).pathname);
const mod = await import(`${dir}/surrealql_language_server.js`);
mod.initSync({ module: readFileSync(`${dir}/surrealql_language_server_bg.wasm`) });

const published = new Map();
const server = new mod.WasmLanguageServer({
  onPublishDiagnostics: (uri, diagnostics) => published.set(uri, diagnostics),
  onLogMessage() {},
  onRequestConfiguration: async () => null,
});

const repeat = (n, f) => Array.from({ length: n }, (_, i) => f(i));
const realistic = (n) => {
  const tables = repeat(Math.ceil(n / 9), (i) => `table_${i}`);
  return tables
    .flatMap((t, i) => {
      const other = tables[(i + 1) % tables.length];
      return [
        `DEFINE TABLE ${t} TYPE NORMAL SCHEMAFULL PERMISSIONS FOR select WHERE owner = $auth.id FOR create, update, delete NONE`,
        `DEFINE FIELD owner ON ${t} TYPE record<user> PERMISSIONS FULL`,
        `DEFINE FIELD name ON ${t} TYPE string ASSERT string::len($value) > 0 PERMISSIONS FULL`,
        `DEFINE FIELD created ON ${t} TYPE datetime DEFAULT time::now() READONLY PERMISSIONS FULL`,
        `DEFINE FIELD link ON ${t} TYPE option<record<${other}>> PERMISSIONS FULL`,
        `DEFINE FIELD items ON ${t} TYPE array<record<${other}>> DEFAULT [] PERMISSIONS FULL`,
        `DEFINE FIELD items.* ON ${t} TYPE record<${other}> PERMISSIONS FULL`,
        `DEFINE INDEX ${t}_name ON ${t} FIELDS name UNIQUE`,
        `DEFINE EVENT deleted ON ${t} WHEN $event = "DELETE" THEN { DELETE ${other} WHERE link = $before.id; RETURN fn::group_date($before.created); }`,
      ];
    })
    .slice(0, n);
};

// Ten times the size that first faulted.
const WIDE = {
  tables: repeat(20_000, (i) => `DEFINE TABLE t${i}`),
  fields: repeat(10_000, (i) => `DEFINE FIELD f${i} ON t TYPE string`),
  events: repeat(5_000, (i) => `DEFINE EVENT e${i} ON t WHEN true THEN { RETURN 1; }`),
  realistic: realistic(4_000),
};

const DEEP = {
  else_if: [4_000, (n) => `DEFINE FUNCTION fn::branch($x: int) { IF $x = 0 { RETURN 0; }${repeat(n, (i) => ` ELSE IF $x = ${i + 1} { RETURN ${i + 1}; }`).join("")} ELSE { RETURN -1; }; }`],
  sum: [27_000, (n) => `DEFINE FIELD total ON t VALUE ${"1 + ".repeat(n)}1`],
  parens: [11_000, (n) => `DEFINE FUNCTION fn::deep() { RETURN ${"(".repeat(n)}1${")".repeat(n)}; }`],
  objects: [9_000, (n) => `DEFINE FIELD nested ON t VALUE ${"{ a: ".repeat(n)}1${" }".repeat(n)}`],
  if_blocks: [9_000, (n) => `DEFINE EVENT e ON t WHEN true THEN { ${"IF true { ".repeat(n)} RETURN 1; ${"} ".repeat(n)} }`],
  array_type: [27_000, (n) => `DEFINE FIELD grid ON t TYPE ${"array<".repeat(n)}string${">".repeat(n)}`],
};

let id = 0;
const send = async (method, params) => {
  const reply = await server.handleMessage(JSON.stringify({ jsonrpc: "2.0", id: ++id, method, params }));
  const response = JSON.parse(reply);
  if (response.error) throw new Error(`${method}: ${JSON.stringify(response.error)}`);
  return response.result;
};
const notify = (method, params) => server.handleMessage(JSON.stringify({ jsonrpc: "2.0", method, params }));

const uri = "file:///workspace/deep.surql";
let version = 0;
const open = async (text) => {
  await notify(version === 0 ? "textDocument/didOpen" : "textDocument/didChange", version === 0
    ? { textDocument: { uri, languageId: "surrealql", version: ++version, text } }
    : { textDocument: { uri, version: ++version }, contentChanges: [{ text }] });
  return (published.get(uri) ?? []).filter((d) => d.code === "too-deep").length;
};

// Every request the dispatcher routes, at each column of line 0.
const exercise = async (text, columns) => {
  await open(text);
  const textDocument = { uri };
  const range = { start: { line: 0, character: 0 }, end: { line: 2, character: 0 } };
  await send("textDocument/documentSymbol", { textDocument });
  await send("textDocument/semanticTokens/full", { textDocument });
  await send("textDocument/semanticTokens/range", { textDocument, range });
  await send("textDocument/inlayHint", { textDocument, range });
  await send("textDocument/codeAction", { textDocument, range, context: { diagnostics: [] } });
  await send("workspace/symbol", { query: "" });
  for (const character of columns) {
    const at = { textDocument, position: { line: 0, character } };
    for (const method of ["hover", "completion", "signatureHelp", "definition", "prepareRename", "documentHighlight", "prepareCallHierarchy"]) {
      await send(`textDocument/${method}`, at);
    }
    await send("textDocument/references", { ...at, context: { includeDeclaration: true } });
    await send("textDocument/rename", { ...at, newName: "renamed" });
  }
};

const check = (ok, message) => {
  if (!ok) {
    console.error(`FAIL ${message}`);
    process.exit(1);
  }
};

await send("initialize", { processId: null, rootUri: null, capabilities: {} });
await notify("initialized", {});

for (const [name, statements] of Object.entries(WIDE)) {
  const started = Date.now();
  await server.setLiveMetadata(statements);
  await exercise("SELECT * FROM t0;\n", [14]);
  console.log(`wide ${name} × ${statements.length}: ${Date.now() - started} ms`);
}

for (const [name, [size, shape]] of Object.entries(DEEP)) {
  const statement = shape(size);
  const started = Date.now();
  await server.setLiveMetadata([statement]);
  await exercise(`${statement};\nDEFINE TABLE kept;\n`, [0, statement.length >> 1, statement.length]);
  const expected = name === "else_if" ? 0 : 1;
  check((await open(`${statement};\nDEFINE TABLE kept;\n`)) === expected, `${name}: expected ${expected} too-deep`);
  console.log(`deep ${name} × ${size}: ${Date.now() - started} ms`);
}

// The deepest statement of each shape that is still analysed.
for (const [name, [size, shape]] of Object.entries(DEEP)) {
  if (name === "else_if") continue;
  let [low, high] = [1, size];
  while (high - low > 1) {
    const mid = (low + high) >> 1;
    if ((await open(shape(mid))) === 0) low = mid;
    else high = mid;
  }
  const statement = shape(low);
  await server.setLiveMetadata([statement]);
  await exercise(statement, [0, statement.length >> 1, statement.length]);
  check((await open(statement)) === 0, `${name} at ${low} should be analysed`);
  console.log(`limit ${name} at ${low}: ok`);
}

console.log("ok");
process.exit(0);
