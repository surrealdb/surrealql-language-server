//! The Model Context Protocol surface, driven as an agent would drive it.
//!
//! Tool names and input schemas are a **permanent** compatibility surface: an
//! agent keys on them, and a rename breaks it silently rather than loudly. The
//! shape assertions here are the tripwire for that, in the same spirit as
//! `tests/compat.rs`.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_surrealql-language-server")
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("surql-mcp-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn write_schema(dir: &Path) {
    std::fs::write(
        dir.join("schema.surql"),
        "DEFINE TABLE person SCHEMAFULL;\n\
         DEFINE FIELD email ON person TYPE option<string>;\n",
    )
    .expect("write schema");
}

/// Send each message on its own line and collect the responses in order.
fn converse(workspace: Option<&Path>, messages: &[Value]) -> Vec<Value> {
    let mut command = Command::new(binary());
    command.arg("mcp");
    if let Some(dir) = workspace {
        command.arg("--workspace").arg(dir);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mcp");

    {
        let stdin = child.stdin.as_mut().expect("stdin");
        for message in messages {
            writeln!(stdin, "{message}").expect("write");
        }
    }

    let Output { stdout, .. } = child.wait_with_output().expect("wait");
    String::from_utf8_lossy(&stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("each response is one JSON object"))
        .collect()
}

fn call(name: &str, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments },
    })
}

fn text_of(response: &Value) -> String {
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn initialize_reports_the_protocol_and_the_server() {
    let responses = converse(
        None,
        &[json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} })],
    );
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0]["jsonrpc"], "2.0");
    assert_eq!(responses[0]["id"], 1);
    assert_eq!(responses[0]["result"]["protocolVersion"], "2024-11-05");
    assert!(responses[0]["result"]["capabilities"]["tools"].is_object());
    assert_eq!(
        responses[0]["result"]["serverInfo"]["name"],
        "surrealql-language-server"
    );
}

/// A notification has no id and must draw no reply. Sending one is a protocol
/// error, and a client that sees an unexpected response can wedge.
#[test]
fn a_notification_draws_no_response() {
    let responses = converse(
        None,
        &[
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            json!({ "jsonrpc": "2.0", "id": 7, "method": "ping" }),
        ],
    );
    assert_eq!(responses.len(), 1, "only the ping may be answered");
    assert_eq!(responses[0]["id"], 7);
}

/// The tool names and their required arguments are the contract. A rename here
/// silently breaks every agent configured against it.
#[test]
fn the_tool_surface_is_stable() {
    let responses = converse(
        None,
        &[json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })],
    );
    let tools = responses[0]["result"]["tools"]
        .as_array()
        .expect("an array of tools");

    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().expect("named"))
        .collect();
    assert_eq!(
        names,
        vec![
            "validate_surrealql",
            "get_schema",
            "lookup_function",
            "search_functions",
            "explain_diagnostic",
        ],
        "tool names are a permanent surface: additive changes only"
    );

    for tool in tools {
        assert!(
            tool["description"].as_str().is_some_and(|d| d.len() > 40),
            "{} needs a description a model can choose on",
            tool["name"]
        );
        assert_eq!(
            tool["inputSchema"]["type"], "object",
            "{} must declare an object schema",
            tool["name"]
        );
    }

    let required = |name: &str| -> Vec<String> {
        tools
            .iter()
            .find(|tool| tool["name"] == name)
            .and_then(|tool| tool["inputSchema"]["required"].as_array())
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    assert_eq!(required("validate_surrealql"), vec!["query"]);
    assert_eq!(required("lookup_function"), vec!["name"]);
    assert_eq!(required("search_functions"), vec!["query"]);
    assert_eq!(required("explain_diagnostic"), vec!["code"]);
    assert!(
        required("get_schema").is_empty(),
        "get_schema takes no required argument"
    );
}

/// Validation sees the workspace, which is the difference between catching a
/// typo'd table and shrugging at it.
#[test]
fn validate_checks_against_the_workspace_schema() {
    let dir = scratch("validate");
    write_schema(&dir);

    let responses = converse(
        Some(&dir),
        &[call(
            "validate_surrealql",
            json!({ "query": "SELECT * FROM persn;" }),
        )],
    );
    let text = text_of(&responses[0]);
    let report: Value = serde_json::from_str(&text).expect("diagnostics as JSON");

    assert_eq!(report["count"], 1);
    let diagnostic = &report["diagnostics"][0];
    assert_eq!(diagnostic["code"], "unknown-table");
    assert_eq!(
        diagnostic["data"]["suggestion"], "person",
        "the structured hint is what makes the repair mechanical"
    );
    assert!(
        diagnostic["codeDescription"]["href"]
            .as_str()
            .is_some_and(|href| href.ends_with("#unknown-table")),
        "a diagnostic must carry its explanation link here too"
    );
}

/// Clean SurrealQL says so in words, not an empty JSON envelope a model has to
/// interpret.
#[test]
fn validate_says_so_plainly_when_there_is_nothing_wrong() {
    let dir = scratch("validate-clean");
    write_schema(&dir);
    let responses = converse(
        Some(&dir),
        &[call(
            "validate_surrealql",
            json!({ "query": "SELECT * FROM person;" }),
        )],
    );
    assert_eq!(text_of(&responses[0]), "No problems found.");
}

/// A variable the caller binds is declared, not reported.
#[test]
fn validate_accepts_caller_bound_variables() {
    let dir = scratch("validate-params");
    write_schema(&dir);
    let responses = converse(
        Some(&dir),
        &[call(
            "validate_surrealql",
            json!({
                "query": "SELECT * FROM person WHERE email = $email;",
                "params": ["email"],
            }),
        )],
    );
    assert_eq!(text_of(&responses[0]), "No problems found.");
}

#[test]
fn get_schema_returns_the_workspace_definitions() {
    let dir = scratch("schema");
    write_schema(&dir);

    let responses = converse(Some(&dir), &[call("get_schema", json!({}))]);
    let text = text_of(&responses[0]);
    assert!(text.contains("DEFINE TABLE person SCHEMAFULL;"), "{text}");
    assert!(text.contains("DEFINE FIELD email ON person TYPE option<string>;"));

    let responses = converse(
        Some(&dir),
        &[call("get_schema", json!({ "format": "json" }))],
    );
    let report: Value = serde_json::from_str(&text_of(&responses[0])).expect("json");
    assert_eq!(report["schemaVersion"], 1);
}

/// Started without `--workspace`, `get_schema` says what to do rather than
/// returning an empty schema that reads as "this database has no tables".
#[test]
fn get_schema_explains_itself_when_no_workspace_was_given() {
    let responses = converse(None, &[call("get_schema", json!({}))]);
    assert_eq!(responses[0]["result"]["isError"], true);
    assert!(text_of(&responses[0]).contains("--workspace"));
}

#[test]
fn lookup_function_reports_a_signature_and_a_rename() {
    let responses = converse(
        None,
        &[
            call("lookup_function", json!({ "name": "string::concat" })),
            call("lookup_function", json!({ "name": "type::thing" })),
            call("lookup_function", json!({ "name": "string::nonsense" })),
        ],
    );

    assert!(text_of(&responses[0]).starts_with("string::concat("));
    assert!(
        text_of(&responses[1]).contains("renamed to `type::record`"),
        "a renamed builtin must say so before anything else"
    );
    assert_eq!(responses[2]["result"]["isError"], true);
    assert!(text_of(&responses[2]).contains("search_functions"));
}

#[test]
fn search_functions_finds_by_substring_and_respects_the_limit() {
    let responses = converse(
        None,
        &[
            call("search_functions", json!({ "query": "concat" })),
            call("search_functions", json!({ "query": "time::", "limit": 2 })),
            call("search_functions", json!({ "query": "zzzznope" })),
        ],
    );

    assert!(text_of(&responses[0]).contains("string::concat("));

    let limited = text_of(&responses[1]);
    assert_eq!(
        limited
            .lines()
            .filter(|line| line.contains("time::"))
            .count(),
        2,
        "the limit must be honoured: {limited}"
    );
    assert!(limited.contains("more."), "and the rest acknowledged");

    assert!(text_of(&responses[2]).contains("No builtin matches"));
}

#[test]
fn explain_diagnostic_returns_the_documented_prose() {
    let responses = converse(
        None,
        &[
            call("explain_diagnostic", json!({ "code": "unknown-table" })),
            call("explain_diagnostic", json!({ "code": "not-a-code" })),
        ],
    );

    assert!(text_of(&responses[0]).starts_with("## unknown-table"));
    assert_eq!(responses[1]["result"]["isError"], true);
    assert!(text_of(&responses[1]).contains("Known codes:"));
}

/// A tool that cannot answer returns a *result* carrying `isError`, not a
/// JSON-RPC error: the call was well-formed and the connection is fine, and the
/// model needs to read why. A JSON-RPC error is for a broken protocol.
#[test]
fn a_failing_tool_is_a_result_not_a_transport_error() {
    let responses = converse(None, &[call("no_such_tool", json!({}))]);
    assert!(
        responses[0]["error"].is_null(),
        "a tool failure is not a protocol failure"
    );
    assert_eq!(responses[0]["result"]["isError"], true);
}

#[test]
fn malformed_input_and_unknown_methods_are_protocol_errors() {
    let mut child = Command::new(binary())
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        writeln!(stdin, "{{not json").expect("write");
        writeln!(
            stdin,
            "{}",
            json!({ "jsonrpc": "2.0", "id": 2, "method": "nope/nope" })
        )
        .expect("write");
    }
    let output = child.wait_with_output().expect("wait");
    let responses: Vec<Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("json"))
        .collect();

    assert_eq!(responses[0]["error"]["code"], -32700, "parse error");
    assert!(responses[0]["id"].is_null(), "an unparseable id is null");
    assert_eq!(responses[1]["error"]["code"], -32601, "method not found");
    assert_eq!(responses[1]["id"], 2);
}
