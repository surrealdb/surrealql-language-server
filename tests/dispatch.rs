//! JSON-RPC wire tests against the transport-agnostic dispatcher —
//! the exact behavior the WASM `handleMessage` shim returns to
//! Surrealist, exercised natively.

mod common;

use common::core_with;
use serde_json::{Value, json};
use surrealql_language_server::core::dispatch::{DispatchOutput, dispatch_json_rpc};

fn response_json(output: DispatchOutput) -> Value {
    match output {
        DispatchOutput::Response(text) => {
            serde_json::from_str(&text).expect("response must be valid JSON")
        }
        DispatchOutput::None => panic!("expected a response, got a notification outcome"),
    }
}

#[tokio::test]
async fn invalid_json_yields_invalid_request_with_null_id() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let response = response_json(dispatch_json_rpc(&core, "{not json").await);
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], Value::Null);
    assert_eq!(response["error"]["code"], -32600);
}

#[tokio::test]
async fn unknown_method_request_yields_method_not_found() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let message = json!({"jsonrpc": "2.0", "id": 7, "method": "foo/bar"}).to_string();
    let response = response_json(dispatch_json_rpc(&core, &message).await);
    assert_eq!(response["id"], 7);
    assert_eq!(response["error"]["code"], -32601);
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("foo/bar")
    );
}

#[tokio::test]
async fn unknown_method_notification_is_dropped_without_response() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let message =
        json!({"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"id": 1}}).to_string();
    assert_eq!(
        dispatch_json_rpc(&core, &message).await,
        DispatchOutput::None
    );
}

#[tokio::test]
async fn malformed_request_params_yield_invalid_params() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let message = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "textDocument/hover",
        "params": {"bogus": true},
    })
    .to_string();
    let response = response_json(dispatch_json_rpc(&core, &message).await);
    assert_eq!(response["id"], 3);
    assert_eq!(response["error"]["code"], -32602);
}

#[tokio::test]
async fn shutdown_returns_null_result_and_echoes_id() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let message = json!({"jsonrpc": "2.0", "id": "shutdown-1", "method": "shutdown"}).to_string();
    let response = response_json(dispatch_json_rpc(&core, &message).await);
    assert_eq!(response["id"], "shutdown-1");
    assert_eq!(response["result"], Value::Null);
    assert!(response.get("error").is_none());
}

#[tokio::test]
async fn initialize_reports_server_info() {
    let (core, _, _) = core_with(Default::default(), Default::default());
    let message = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {"capabilities": {}},
    })
    .to_string();
    let response = response_json(dispatch_json_rpc(&core, &message).await);
    assert_eq!(
        response["result"]["serverInfo"]["name"],
        "surreal-language-server"
    );
    assert!(response["result"]["capabilities"].is_object());
}

#[tokio::test]
async fn malformed_notification_params_are_logged_not_swallowed() {
    use tower_lsp_server::ls_types::MessageType;

    let (core, notifier, _) = core_with(Default::default(), Default::default());
    let message = json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": { "bogus": true },
    })
    .to_string();

    assert_eq!(
        dispatch_json_rpc(&core, &message).await,
        DispatchOutput::None,
        "notifications never get a response, even malformed ones"
    );
    assert!(
        notifier.logs().iter().any(|(level, log)| {
            *level == MessageType::WARNING
                && log.contains("textDocument/didChange")
                && log.contains("malformed")
        }),
        "the dropped notification must be logged: {:?}",
        notifier.logs()
    );
}

#[tokio::test]
async fn did_open_notification_returns_none_and_publishes() {
    let (core, notifier, _) = core_with(Default::default(), Default::default());
    let message = json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": "file:///workspace/a.surql",
                "languageId": "surrealql",
                "version": 1,
                "text": "DEFINE TABLE @@@bad@@@;",
            },
        },
    })
    .to_string();
    assert_eq!(
        dispatch_json_rpc(&core, &message).await,
        DispatchOutput::None
    );
    assert!(
        !notifier.published().is_empty(),
        "didOpen must publish diagnostics"
    );
}
