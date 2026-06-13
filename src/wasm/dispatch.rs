//! JSON-RPC dispatch table for the WASM server.
//!
//! Every supported LSP method is listed exactly once here, mapped to
//! the matching [`LanguageServerCore`] async method. The host hands
//! us the raw JSON-RPC payload string and we return either a JSON-RPC
//! response string (for requests) or `JsValue::UNDEFINED` (for
//! notifications). All other LSP framing (Content-Length headers,
//! transport, batching) is handled by the JS side — this layer is
//! pure data plane.

use ls_types::*;
use serde::Deserialize;
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;

use crate::wasm::server::WasmCore;

/// JSON-RPC error code reserved for "method not found", per the
/// JSON-RPC 2.0 spec.
const METHOD_NOT_FOUND: i64 = -32601;

/// JSON-RPC error code for malformed payloads.
const INVALID_REQUEST: i64 = -32600;

/// JSON-RPC error code for invalid params.
const INVALID_PARAMS: i64 = -32602;

#[derive(Deserialize)]
struct Incoming {
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

pub async fn handle_message(core: &WasmCore, json_text: &str) -> Result<JsValue, JsValue> {
    let message: Incoming = match serde_json::from_str(json_text) {
        Ok(value) => value,
        Err(error) => {
            return Ok(JsValue::from_str(&error_response_string(
                None,
                INVALID_REQUEST,
                &format!("invalid JSON-RPC payload: {error}"),
            )));
        }
    };

    let is_request = message.id.is_some();

    match dispatch(core, &message.method, message.params).await {
        Outcome::Notification => Ok(JsValue::UNDEFINED),
        Outcome::Response(value) => {
            if !is_request {
                // Server returned data for a notification — drop it.
                return Ok(JsValue::UNDEFINED);
            }
            Ok(JsValue::from_str(&success_response_string(
                message.id.unwrap_or(Value::Null),
                value,
            )))
        }
        Outcome::Error { code, message: msg } => {
            if !is_request {
                return Ok(JsValue::UNDEFINED);
            }
            Ok(JsValue::from_str(&error_response_string(
                Some(message.id.unwrap_or(Value::Null)),
                code,
                &msg,
            )))
        }
    }
}

enum Outcome {
    Notification,
    Response(Value),
    Error { code: i64, message: String },
}

impl Outcome {
    fn from_value<T: serde::Serialize>(value: T) -> Self {
        match serde_json::to_value(value) {
            Ok(value) => Outcome::Response(value),
            Err(error) => Outcome::Error {
                code: -32603,
                message: format!("failed to serialise response: {error}"),
            },
        }
    }
}

async fn dispatch(core: &WasmCore, method: &str, params: Value) -> Outcome {
    match method {
        // ── Lifecycle ──────────────────────────────────────────────
        "initialize" => match decode::<InitializeParams>(params) {
            Ok(params) => Outcome::from_value(core.initialize(params).await),
            Err(error) => error,
        },
        "initialized" => {
            core.initialized().await;
            Outcome::Notification
        }
        "shutdown" => Outcome::Response(Value::Null),
        "exit" => Outcome::Notification,

        // ── Text document sync ─────────────────────────────────────
        "textDocument/didOpen" => match decode::<DidOpenTextDocumentParams>(params) {
            Ok(params) => {
                core.did_open(params).await;
                Outcome::Notification
            }
            Err(error) => error,
        },
        "textDocument/didChange" => match decode::<DidChangeTextDocumentParams>(params) {
            Ok(params) => {
                core.did_change(params).await;
                Outcome::Notification
            }
            Err(error) => error,
        },
        "textDocument/didSave" => match decode::<DidSaveTextDocumentParams>(params) {
            Ok(params) => {
                core.did_save(params).await;
                Outcome::Notification
            }
            Err(error) => error,
        },
        "textDocument/didClose" => match decode::<DidCloseTextDocumentParams>(params) {
            Ok(params) => {
                core.did_close(params).await;
                Outcome::Notification
            }
            Err(error) => error,
        },

        // ── Workspace ──────────────────────────────────────────────
        "workspace/didChangeConfiguration" => {
            match decode::<DidChangeConfigurationParams>(params) {
                Ok(params) => {
                    core.did_change_configuration(params).await;
                    Outcome::Notification
                }
                Err(error) => error,
            }
        }
        "workspace/didChangeWorkspaceFolders" => {
            match decode::<DidChangeWorkspaceFoldersParams>(params) {
                Ok(params) => {
                    core.did_change_workspace_folders(params).await;
                    Outcome::Notification
                }
                Err(error) => error,
            }
        }
        "workspace/symbol" => match decode::<WorkspaceSymbolParams>(params) {
            Ok(params) => Outcome::from_value(core.workspace_symbol(params).await),
            Err(error) => error,
        },

        // ── Completion / hover / navigation ────────────────────────
        "textDocument/completion" => match decode::<CompletionParams>(params) {
            Ok(params) => Outcome::from_value(core.completion(params).await),
            Err(error) => error,
        },
        "textDocument/hover" => match decode::<HoverParams>(params) {
            Ok(params) => Outcome::from_value(core.hover(params).await),
            Err(error) => error,
        },
        "textDocument/documentSymbol" => match decode::<DocumentSymbolParams>(params) {
            Ok(params) => Outcome::from_value(core.document_symbol(params).await),
            Err(error) => error,
        },
        "textDocument/semanticTokens/full" => match decode::<SemanticTokensParams>(params) {
            Ok(params) => Outcome::from_value(core.semantic_tokens_full(params).await),
            Err(error) => error,
        },
        "textDocument/definition" => match decode::<GotoDefinitionParams>(params) {
            Ok(params) => Outcome::from_value(core.goto_definition(params).await),
            Err(error) => error,
        },
        "textDocument/references" => match decode::<ReferenceParams>(params) {
            Ok(params) => Outcome::from_value(core.references(params).await),
            Err(error) => error,
        },
        "textDocument/prepareRename" => match decode::<TextDocumentPositionParams>(params) {
            Ok(params) => Outcome::from_value(core.prepare_rename(params).await),
            Err(error) => error,
        },
        "textDocument/rename" => match decode::<RenameParams>(params) {
            Ok(params) => Outcome::from_value(core.rename(params).await),
            Err(error) => error,
        },
        "textDocument/signatureHelp" => match decode::<SignatureHelpParams>(params) {
            Ok(params) => Outcome::from_value(core.signature_help(params).await),
            Err(error) => error,
        },
        "textDocument/codeAction" => match decode::<CodeActionParams>(params) {
            Ok(params) => Outcome::from_value(core.code_action(params).await),
            Err(error) => error,
        },
        "textDocument/documentHighlight" => match decode::<DocumentHighlightParams>(params) {
            Ok(params) => Outcome::from_value(core.document_highlight(params).await),
            Err(error) => error,
        },
        "textDocument/inlayHint" => match decode::<InlayHintParams>(params) {
            Ok(params) => Outcome::from_value(core.inlay_hint(params).await),
            Err(error) => error,
        },

        // ── Call hierarchy ─────────────────────────────────────────
        "textDocument/prepareCallHierarchy" => match decode::<CallHierarchyPrepareParams>(params) {
            Ok(params) => Outcome::from_value(core.prepare_call_hierarchy(params).await),
            Err(error) => error,
        },
        "callHierarchy/incomingCalls" => match decode::<CallHierarchyIncomingCallsParams>(params) {
            Ok(params) => Outcome::from_value(core.incoming_calls(params).await),
            Err(error) => error,
        },
        "callHierarchy/outgoingCalls" => match decode::<CallHierarchyOutgoingCallsParams>(params) {
            Ok(params) => Outcome::from_value(core.outgoing_calls(params).await),
            Err(error) => error,
        },

        _ => Outcome::Error {
            code: METHOD_NOT_FOUND,
            message: format!("unknown LSP method `{method}`"),
        },
    }
}

fn decode<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, Outcome> {
    serde_json::from_value(params).map_err(|error| Outcome::Error {
        code: INVALID_PARAMS,
        message: format!("invalid params: {error}"),
    })
}

fn success_response_string(id: Value, result: Value) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
    .to_string()
}

fn error_response_string(id: Option<Value>, code: i64, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": {
            "code": code,
            "message": message,
        },
    })
    .to_string()
}
