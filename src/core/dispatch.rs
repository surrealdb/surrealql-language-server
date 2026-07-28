//! Transport-agnostic JSON-RPC dispatch table.
//!
//! Every supported LSP method is listed exactly once here, mapped to
//! the matching [`LanguageServerCore`] async method. The caller hands
//! us the raw JSON-RPC payload string and we return either a JSON-RPC
//! response string (for requests) or nothing (for notifications). All
//! other LSP framing (Content-Length headers, transport, batching) is
//! the caller's concern — this layer is pure data plane.
//!
//! The native binary doesn't use this module (tower-lsp-server does
//! its own framing); the WASM front-end wraps it in
//! [`crate::wasm::dispatch`]. Keeping it here means the wire behavior
//! is exercised by native `cargo test`.

use ls_types::*;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::core::client::{LspNotifier, MetadataProvider, WorkspaceLoader};
use crate::core::server::LanguageServerCore;

/// JSON-RPC error code reserved for "method not found", per the
/// JSON-RPC 2.0 spec.
pub const METHOD_NOT_FOUND: i64 = -32601;

/// JSON-RPC error code for malformed payloads.
pub const INVALID_REQUEST: i64 = -32600;

/// JSON-RPC error code for invalid params.
pub const INVALID_PARAMS: i64 = -32602;

/// JSON-RPC error code for internal errors (response serialization).
pub const INTERNAL_ERROR: i64 = -32603;

#[derive(Deserialize)]
struct Incoming {
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

/// What the transport should do with the processed message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchOutput {
    /// Notification (or notification-shaped error): send nothing.
    None,
    /// Request: send this JSON-RPC response string verbatim.
    Response(String),
}

pub async fn dispatch_json_rpc<N, W, M>(
    core: &LanguageServerCore<N, W, M>,
    json_text: &str,
) -> DispatchOutput
where
    N: LspNotifier,
    W: WorkspaceLoader,
    M: MetadataProvider,
{
    let message: Incoming = match serde_json::from_str(json_text) {
        Ok(value) => value,
        Err(error) => {
            return DispatchOutput::Response(error_response_string(
                None,
                INVALID_REQUEST,
                &format!("invalid JSON-RPC payload: {error}"),
            ));
        }
    };

    let is_request = message.id.is_some();

    match dispatch(core, &message.method, message.params).await {
        Outcome::Notification => DispatchOutput::None,
        Outcome::Response(value) => {
            if !is_request {
                // Server returned data for a notification — drop it.
                return DispatchOutput::None;
            }
            DispatchOutput::Response(success_response_string(
                message.id.unwrap_or(Value::Null),
                value,
            ))
        }
        Outcome::Error { code, message: msg } => {
            if !is_request {
                // A notification we recognised but couldn't decode is
                // silently lost state (e.g. a didChange that never
                // applies) — tell the log. Unknown-method notifications
                // ($/cancelRequest, $/setTrace, …) are legitimately
                // ignored per the LSP spec, so stay quiet about those.
                if code == INVALID_PARAMS {
                    core.notifier()
                        .log_message(
                            MessageType::WARNING,
                            format!(
                                "SurrealQL: dropped malformed `{}` notification: {msg}",
                                message.method
                            ),
                        )
                        .await;
                }
                return DispatchOutput::None;
            }
            DispatchOutput::Response(error_response_string(
                Some(message.id.unwrap_or(Value::Null)),
                code,
                &msg,
            ))
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
                code: INTERNAL_ERROR,
                message: format!("failed to serialise response: {error}"),
            },
        }
    }
}

async fn dispatch<N, W, M>(
    core: &LanguageServerCore<N, W, M>,
    method: &str,
    params: Value,
) -> Outcome
where
    N: LspNotifier,
    W: WorkspaceLoader,
    M: MetadataProvider,
{
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
        "textDocument/semanticTokens/range" => match decode::<SemanticTokensRangeParams>(params) {
            Ok(params) => Outcome::from_value(core.semantic_tokens_range(params).await),
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
