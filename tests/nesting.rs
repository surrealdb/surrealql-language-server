#![cfg(feature = "server")]

//! Schemas that are wide (thousands of statements) or deep (one statement
//! nested thousands of levels) load and answer every request.
//!
//! Each shape runs at ten times the size that faulted the wasm build: wide
//! schemas through tree-sitter's C allocator (`src/wasm/alloc.rs`), deep ones
//! through the recursive semantic walks overflowing the stack
//! ([`MAX_TREE_DEPTH`]). The wasm build is covered by `tests/wasm/stress.mjs`;
//! these keep the native pipeline honest at the same sizes.

mod common;

use std::sync::Arc;

use common::{core_with, uri};
use serde_json::{Value, json};
use surrealql_language_server::core::dispatch::{DispatchOutput, dispatch_json_rpc};
use surrealql_language_server::semantic::analyzer::{MAX_TREE_DEPTH, analyze_document};
use surrealql_language_server::semantic::codes;
use surrealql_language_server::semantic::types::{LiveMetadataSnapshot, SymbolOrigin};
use tower_lsp_server::ls_types::Diagnostic;

fn define_tables(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("DEFINE TABLE t{i}")).collect()
}

fn define_fields(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| format!("DEFINE FIELD f{i} ON t TYPE string"))
        .collect()
}

fn define_events(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| format!("DEFINE EVENT e{i} ON t WHEN true THEN {{ RETURN 1; }}"))
        .collect()
}

/// Nine statements per table, in the shapes `INFO FOR DB` returns.
fn realistic_schema(n: usize) -> Vec<String> {
    let tables: Vec<String> = (0..n.div_ceil(9)).map(|i| format!("table_{i}")).collect();
    let mut out = Vec::new();
    for (i, t) in tables.iter().enumerate() {
        let other = &tables[(i + 1) % tables.len()];
        out.extend([
            format!(
                "DEFINE TABLE {t} TYPE NORMAL SCHEMAFULL PERMISSIONS FOR select WHERE owner = $auth.id FOR create, update, delete NONE"
            ),
            format!("DEFINE FIELD owner ON {t} TYPE record<user> PERMISSIONS FULL"),
            format!(
                "DEFINE FIELD name ON {t} TYPE string ASSERT string::len($value) > 0 PERMISSIONS FULL"
            ),
            format!(
                "DEFINE FIELD created ON {t} TYPE datetime DEFAULT time::now() READONLY PERMISSIONS FULL"
            ),
            format!("DEFINE FIELD link ON {t} TYPE option<record<{other}>> PERMISSIONS FULL"),
            format!(
                "DEFINE FIELD items ON {t} TYPE array<record<{other}>> DEFAULT [] PERMISSIONS FULL"
            ),
            format!("DEFINE FIELD items.* ON {t} TYPE record<{other}> PERMISSIONS FULL"),
            format!("DEFINE INDEX {t}_name ON {t} FIELDS name UNIQUE"),
            format!(
                "DEFINE EVENT deleted ON {t} WHEN $event = \"DELETE\" THEN {{ DELETE {other} WHERE link = $before.id; RETURN fn::group_date($before.created); }}"
            ),
        ]);
    }
    out.push("DEFINE FUNCTION fn::group_date($date: datetime) { RETURN time::format(time::floor($date, 1d), \"%Y-%m-%d\"); } PERMISSIONS FULL".to_string());
    out.truncate(n);
    out
}

fn else_if(n: usize) -> String {
    let branches: String = (1..=n)
        .map(|i| format!(" ELSE IF $x = {i} {{ RETURN {i}; }}"))
        .collect();
    format!(
        "DEFINE FUNCTION fn::branch($x: int) {{ IF $x = 0 {{ RETURN 0; }}{branches} ELSE {{ RETURN -1; }}; }}"
    )
}

fn sum(n: usize) -> String {
    format!("DEFINE FIELD total ON t VALUE {}1", "1 + ".repeat(n))
}

fn parens(n: usize) -> String {
    format!(
        "DEFINE FUNCTION fn::deep() {{ RETURN {}1{}; }}",
        "(".repeat(n),
        ")".repeat(n)
    )
}

fn objects(n: usize) -> String {
    format!(
        "DEFINE FIELD nested ON t VALUE {}1{}",
        "{ a: ".repeat(n),
        " }".repeat(n)
    )
}

fn if_blocks(n: usize) -> String {
    format!(
        "DEFINE EVENT e ON t WHEN true THEN {{ {} RETURN 1; {} }}",
        "IF true { ".repeat(n),
        "} ".repeat(n)
    )
}

fn array_type(n: usize) -> String {
    format!(
        "DEFINE FIELD grid ON t TYPE {}string{}",
        "array<".repeat(n),
        ">".repeat(n)
    )
}

type Shape = fn(usize) -> String;

/// The deep shapes, each with ten times the size that overflowed the wasm
/// stack (`else_if` and `sum` at ten times the sizes first reported).
const DEEP: &[(&str, Shape, usize)] = &[
    ("else_if", else_if, 4_000),
    ("sum", sum, 27_000),
    ("parens", parens, 11_000),
    ("objects", objects, 9_000),
    ("if_blocks", if_blocks, 9_000),
    ("array_type", array_type, 27_000),
];

fn snapshot(statements: &[String]) -> LiveMetadataSnapshot {
    let mut snapshot = LiveMetadataSnapshot::default();
    for (index, define) in statements.iter().enumerate() {
        let uri: tower_lsp_server::ls_types::Uri = format!("surrealdb:///metadata/{index}.surql")
            .parse()
            .unwrap();
        if let Some(analysis) = analyze_document(uri.clone(), define, SymbolOrigin::Remote) {
            snapshot.documents.insert(uri, Arc::new(analysis));
        }
    }
    snapshot
}

fn too_deep(diagnostics: &[Diagnostic]) -> usize {
    diagnostics
        .iter()
        .filter(|diagnostic| codes::has_code(diagnostic, codes::TOO_DEEP))
        .count()
}

/// The diagnostics a lone statement analyses to.
fn statement_diagnostics(statement: &str) -> Vec<Diagnostic> {
    analyze_document(uri("probe.surql"), statement, SymbolOrigin::Local)
        .expect("analyses")
        .syntax_diagnostics
}

async fn send(core: &common::TestCore, message: Value) -> Value {
    match dispatch_json_rpc(core, &message.to_string()).await {
        DispatchOutput::Response(text) => serde_json::from_str(&text).unwrap(),
        DispatchOutput::None => Value::Null,
    }
}

/// Open `text` and send every position and document request the server
/// routes, at each of `columns` on line 0. Every request must succeed.
async fn exercise(core: &common::TestCore, text: &str, columns: &[usize]) {
    let uri = uri("deep.surql");
    send(
        core,
        json!({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": {"uri": uri, "languageId": "surrealql", "version": 1, "text": text},
        }}),
    )
    .await;
    let document = json!({"uri": uri});
    let whole = json!({"start": {"line": 0, "character": 0}, "end": {"line": 2, "character": 0}});
    let mut requests = vec![
        (
            "textDocument/documentSymbol",
            json!({"textDocument": document}),
        ),
        (
            "textDocument/semanticTokens/full",
            json!({"textDocument": document}),
        ),
        (
            "textDocument/semanticTokens/range",
            json!({"textDocument": document, "range": whole}),
        ),
        (
            "textDocument/inlayHint",
            json!({"textDocument": document, "range": whole}),
        ),
        (
            "textDocument/codeAction",
            json!({"textDocument": document, "range": whole, "context": {"diagnostics": []}}),
        ),
        ("workspace/symbol", json!({"query": ""})),
    ];
    for &character in columns {
        let at = json!({"textDocument": document, "position": {"line": 0, "character": character}});
        for method in [
            "textDocument/hover",
            "textDocument/completion",
            "textDocument/signatureHelp",
            "textDocument/definition",
            "textDocument/prepareRename",
            "textDocument/documentHighlight",
            "textDocument/prepareCallHierarchy",
        ] {
            requests.push((method, at.clone()));
        }
        let mut references = at.clone();
        references["context"] = json!({"includeDeclaration": true});
        requests.push(("textDocument/references", references));
        let mut rename = at.clone();
        rename["newName"] = json!("renamed");
        requests.push(("textDocument/rename", rename));
    }
    for (id, (method, params)) in requests.into_iter().enumerate() {
        let response = send(
            core,
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        )
        .await;
        assert!(
            response.get("error").is_none(),
            "{method} failed: {response}"
        );
    }
}

#[tokio::test]
async fn wide_live_metadata_loads() {
    for (name, statements) in [
        ("tables", define_tables(20_000)),
        ("fields", define_fields(10_000)),
        ("events", define_events(5_000)),
        ("realistic", realistic_schema(4_000)),
    ] {
        let (core, notifier, _) = core_with(Default::default(), Default::default());
        let snapshot = snapshot(&statements);
        assert_eq!(snapshot.documents.len(), statements.len(), "{name}");
        core.replace_live_metadata(snapshot).await;
        exercise(&core, "SELECT * FROM t0;\n", &[14]).await;
        let published = notifier
            .last_published_for(&uri("deep.surql"))
            .expect("diagnostics published");
        assert_eq!(too_deep(&published), 0, "{name}");
    }
}

#[tokio::test]
async fn deep_statements_are_skipped_whole() {
    for &(name, shape, size) in DEEP {
        let statement = shape(size);
        let diagnostics = statement_diagnostics(&statement);
        // `else_if` is wide, not deep: its branches are siblings.
        let expected = usize::from(name != "else_if");
        assert_eq!(too_deep(&diagnostics), expected, "{name}");

        let (core, notifier, _) = core_with(Default::default(), Default::default());
        core.replace_live_metadata(snapshot(std::slice::from_ref(&statement)))
            .await;
        // The statement after the deep one is still analysed.
        let text = format!("{statement};\nDEFINE TABLE kept;\n");
        exercise(&core, &text, &[0, statement.len() / 2, statement.len()]).await;
        let published = notifier
            .last_published_for(&uri("deep.surql"))
            .expect("diagnostics published");
        assert_eq!(too_deep(&published), expected, "{name}");
        let symbols = send(
            &core,
            json!({"jsonrpc": "2.0", "id": 0, "method": "textDocument/documentSymbol",
                "params": {"textDocument": {"uri": uri("deep.surql")}}}),
        )
        .await;
        assert!(symbols["result"].to_string().contains("kept"), "{name}");
    }
}

/// The deepest statement of each shape that is still analysed answers every
/// request on a default 2 MiB test thread, in a debug build. Frames are far
/// larger there than in the size-optimised wasm build, so this is the margin
/// that keeps [`MAX_TREE_DEPTH`] safe.
#[tokio::test]
async fn deepest_analysed_statement_answers_every_request() {
    for &(name, shape, _) in DEEP.iter().filter(|(name, ..)| *name != "else_if") {
        let mut size = 1;
        while too_deep(&statement_diagnostics(&shape(size + 1))) == 0 {
            size += 1;
            assert!(size < MAX_TREE_DEPTH, "{name} never reached the limit");
        }
        let statement = shape(size);
        let (core, notifier, _) = core_with(Default::default(), Default::default());
        core.replace_live_metadata(snapshot(std::slice::from_ref(&statement)))
            .await;
        exercise(
            &core,
            &statement,
            &[0, statement.len() / 2, statement.len()],
        )
        .await;
        let published = notifier
            .last_published_for(&uri("deep.surql"))
            .expect("diagnostics published");
        assert_eq!(too_deep(&published), 0, "{name} at {size}");
    }
}
