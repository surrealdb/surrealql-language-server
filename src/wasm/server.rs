//! `wasm-bindgen` surface exposed to JavaScript.
//!
//! Every method on [`WasmLanguageServer`] is `async` from the JS side
//! (returning a `Promise`) so the host can chain `await
//! server.handle_message(...)` exactly as if the server were a tower-lsp
//! style stdio process. Mutators that don't return data
//! (`push_workspace_document`, `set_live_metadata`) are also awaitable —
//! they internally drive the [`LanguageServerCore`] async API.

use std::sync::Arc;

use ls_types::Uri;
use wasm_bindgen::prelude::*;

use crate::core::LanguageServerCore;
use crate::semantic::analyzer::analyze_document;
use crate::semantic::types::{LiveMetadataSnapshot, SymbolOrigin, WorkspaceIndex};
use crate::wasm::dispatch;
use crate::wasm::host_data::{
    HostPushedMetadata, HostPushedMetadataStore, HostPushedWorkspace, HostPushedWorkspaceStore,
};
use crate::wasm::notifier::{JsCallbackNotifier, JsCallbacks};

/// Concrete instantiation of the core for the browser target.
pub(crate) type WasmCore =
    LanguageServerCore<JsCallbackNotifier, HostPushedWorkspace, HostPushedMetadata>;

#[wasm_bindgen]
pub struct WasmLanguageServer {
    core: Arc<WasmCore>,
    workspace: Arc<HostPushedWorkspaceStore>,
    metadata: Arc<HostPushedMetadataStore>,
}

#[wasm_bindgen]
impl WasmLanguageServer {
    /// Build a new server instance.
    ///
    /// `callbacks` must be a JS object literal of shape:
    ///
    /// ```js
    /// {
    ///   onPublishDiagnostics: (uri, diagnostics) => { ... },
    ///   onLogMessage: (level, message) => { ... },
    ///   onRequestConfiguration: async () => ({ ... }) | null,
    /// }
    /// ```
    #[wasm_bindgen(constructor)]
    pub fn new(callbacks: JsValue) -> Result<WasmLanguageServer, JsValue> {
        // Pretty Rust panics in the JS console rather than the
        // useless `unreachable executed` default.
        console_error_panic_hook::set_once();

        let workspace = Arc::new(HostPushedWorkspaceStore::default());
        let metadata = Arc::new(HostPushedMetadataStore::default());
        let core = LanguageServerCore::new(
            JsCallbackNotifier::new(JsCallbacks::from_object(&callbacks)?),
            HostPushedWorkspace::new(Arc::clone(&workspace)),
            HostPushedMetadata::new(Arc::clone(&metadata)),
        );

        Ok(WasmLanguageServer {
            core: Arc::new(core),
            workspace,
            metadata,
        })
    }

    /// Process one JSON-RPC message (request *or* notification) using
    /// the standard LSP wire format.
    ///
    /// Returns the JSON-RPC response string for requests, or
    /// `undefined` for notifications.
    #[wasm_bindgen(js_name = handleMessage)]
    pub async fn handle_message(&self, json: String) -> Result<JsValue, JsValue> {
        dispatch::handle_message(self.core.as_ref(), &json).await
    }

    /// Push (or replace) a `.surql` document the host considers part
    /// of the saved workspace. Used by Surrealist to seed the server
    /// with files that aren't currently open in any editor.
    #[wasm_bindgen(js_name = pushWorkspaceDocument)]
    pub async fn push_workspace_document(
        &self,
        uri: String,
        text: String,
    ) -> Result<(), JsValue> {
        let parsed = parse_uri(&uri)?;
        let mut workspace = self.workspace.snapshot();
        if let Some(analysis) = analyze_document(parsed.clone(), &text, SymbolOrigin::Local) {
            workspace.documents.insert(parsed, Arc::new(analysis));
        }
        self.workspace.replace(workspace);
        self.core.reload_workspace().await;
        Ok(())
    }

    /// Drop a previously-pushed saved document.
    #[wasm_bindgen(js_name = dropWorkspaceDocument)]
    pub async fn drop_workspace_document(&self, uri: String) -> Result<(), JsValue> {
        let parsed = parse_uri(&uri)?;
        let mut workspace = self.workspace.snapshot();
        workspace.documents.remove(&parsed);
        self.workspace.replace(workspace);
        self.core.reload_workspace().await;
        Ok(())
    }

    /// Replace the entire saved-workspace snapshot. Useful when
    /// Surrealist wants to bulk-reset the server (e.g. when switching
    /// connections clears all the previously-pushed docs).
    #[wasm_bindgen(js_name = replaceWorkspace)]
    pub async fn replace_workspace(&self, documents: JsValue) -> Result<(), JsValue> {
        let entries: Vec<DocumentEntry> = serde_wasm_bindgen::from_value(documents)
            .map_err(|error| JsValue::from_str(&format!("invalid workspace payload: {error}")))?;

        let mut workspace = WorkspaceIndex::default();
        for DocumentEntry { uri, text } in entries {
            let Ok(parsed) = uri.parse::<Uri>() else {
                continue;
            };
            if let Some(analysis) = analyze_document(parsed.clone(), &text, SymbolOrigin::Local) {
                workspace.documents.insert(parsed, Arc::new(analysis));
            }
        }
        self.workspace.replace(workspace);
        self.core.reload_workspace().await;
        Ok(())
    }

    /// Replace the live SurrealDB metadata snapshot from a JS-supplied
    /// list of `DEFINE …` strings (typically the result of running
    /// `INFO FOR DB` / `INFO FOR TABLE` from the host's existing
    /// SurrealDB connection).
    #[wasm_bindgen(js_name = setLiveMetadata)]
    pub async fn set_live_metadata(&self, define_strings: JsValue) -> Result<(), JsValue> {
        let strings: Vec<String> = serde_wasm_bindgen::from_value(define_strings)
            .map_err(|error| JsValue::from_str(&format!("invalid metadata payload: {error}")))?;

        let mut snapshot = LiveMetadataSnapshot::default();
        for (index, define) in strings.into_iter().enumerate() {
            let uri_string = format!("surrealdb:///metadata/{}.surql", index);
            let Ok(uri) = uri_string.parse::<Uri>() else {
                continue;
            };
            if let Some(analysis) = analyze_document(uri.clone(), &define, SymbolOrigin::Remote) {
                snapshot.documents.insert(uri, Arc::new(analysis));
            }
        }
        self.metadata.replace(snapshot.clone());
        self.core.replace_live_metadata(snapshot).await;
        Ok(())
    }
}

#[derive(serde::Deserialize)]
struct DocumentEntry {
    uri: String,
    text: String,
}

fn parse_uri(uri: &str) -> Result<Uri, JsValue> {
    uri.parse::<Uri>()
        .map_err(|error| JsValue::from_str(&format!("invalid uri `{uri}`: {error}")))
}
