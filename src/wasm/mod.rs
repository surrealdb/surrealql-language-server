//! Browser front-end for the language server core.
//!
//! Compiled only for `wasm32-unknown-unknown`. Surrealist drives the
//! server by sending standard LSP JSON-RPC payloads to
//! [`server::WasmLanguageServer::handle_message`]; outbound
//! notifications (diagnostics, log messages) are pushed back via JS
//! callbacks supplied at construction time.
//!
//! The host is responsible for two things the browser sandbox can't
//! provide on its own:
//!
//! 1. The set of "saved" workspace `.surql` documents — pushed via
//!    [`server::WasmLanguageServer::push_workspace_document`].
//! 2. The latest live SurrealDB schema (`DEFINE …` strings produced
//!    by `INFO FOR DB` / `INFO FOR TABLE`) — pushed via
//!    [`server::WasmLanguageServer::set_live_metadata`].

pub mod dispatch;
pub mod host_data;
pub mod notifier;
pub mod server;

pub use server::WasmLanguageServer;
