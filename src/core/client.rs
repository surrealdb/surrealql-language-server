//! Transport-agnostic boundary between the language server core and
//! whatever environment is hosting it. The native binary plugs in
//! tower-lsp-server / walkdir / the SurrealDB Rust SDK; the WASM module
//! plugs in JavaScript callbacks supplied by Surrealist.
//!
//! Every trait in here intentionally trafficks in plain LSP types
//! (`ls_types`) and standard library types so the core never has to
//! know which target is running it.

use std::path::PathBuf;

use async_trait::async_trait;
use ls_types::{Diagnostic, MessageType, Uri};
use serde_json::Value;

use crate::config::ServerSettings;
use crate::semantic::types::{LiveMetadataSnapshot, WorkspaceIndex};

/// Outbound channel for everything the server needs to push toward the
/// client: diagnostics, log messages, and pull-style configuration
/// requests.
#[async_trait]
pub trait LspNotifier: Send + Sync + 'static {
    /// Equivalent to LSP `textDocument/publishDiagnostics`.
    async fn publish_diagnostics(&self, uri: Uri, diagnostics: Vec<Diagnostic>);

    /// Equivalent to LSP `window/logMessage`.
    async fn log_message(&self, level: MessageType, message: String);

    /// Equivalent to LSP `workspace/configuration` for the
    /// `surrealql` section. Returns `None` when the client either
    /// doesn't support configuration pulls or returns nothing.
    async fn request_configuration(&self) -> Option<Value>;
}

/// Source of `.surql` / `.surrealql` documents that already exist on
/// disk (or in the host's saved-document store) but aren't currently
/// open in an editor.
///
/// Native: walkdir-backed scan of the workspace folders.
/// WASM: snapshot of whatever Surrealist has pushed via
/// `push_workspace_document` — the browser sandbox can't walk a real
/// filesystem.
#[async_trait]
pub trait WorkspaceLoader: Send + Sync + 'static {
    /// Scan the given workspace folders and return every parsed
    /// `.surql` document found within them.
    async fn load(&self, folders: &[PathBuf]) -> WorkspaceIndex;

    /// Re-read a single document by URI. Used after `didSave` /
    /// `didClose` to refresh the saved snapshot from disk.
    async fn read_document(&self, uri: &Uri) -> Option<String>;
}

/// Source of "live" SurrealDB schema metadata (the result of running
/// `INFO FOR DB` / `INFO FOR TABLE` against an authenticated
/// connection). Native uses the surrealdb Rust SDK; WASM uses
/// metadata pushed in by the host.
#[async_trait]
pub trait MetadataProvider: Send + Sync + 'static {
    async fn fetch(&self, settings: &ServerSettings) -> LiveMetadataSnapshot;
}
