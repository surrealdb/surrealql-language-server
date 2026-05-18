//! tower-lsp-server [`LanguageServer`] adapter.
//!
//! Almost every method on this `Backend` is a one-liner that delegates
//! to [`LanguageServerCore`]. The only "logic" left here is deciding
//! which long-running operations should be spawned onto the tokio
//! runtime (so the LSP notification handler returns immediately and
//! tower-lsp doesn't serialise subsequent requests behind them).

use std::sync::Arc;

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;
use tower_lsp_server::{Client, LanguageServer};

use crate::core::LanguageServerCore;
use crate::native::metadata_db::SurrealDbMetadataProvider;
use crate::native::notifier::TowerNotifier;
use crate::native::workspace_fs::FilesystemWorkspaceLoader;

/// Concrete instantiation of the core for the native binary.
type NativeCore =
    LanguageServerCore<TowerNotifier, FilesystemWorkspaceLoader, SurrealDbMetadataProvider>;

pub struct Backend {
    core: Arc<NativeCore>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        let core = LanguageServerCore::new(
            TowerNotifier::new(client),
            FilesystemWorkspaceLoader::new(),
            SurrealDbMetadataProvider::new(),
        );
        Self {
            core: Arc::new(core),
        }
    }
}

impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        Ok(self.core.initialize(params).await)
    }

    async fn initialized(&self, _: InitializedParams) {
        // `reload_from_client_configuration → apply_settings` walks the
        // workspace and pings SurrealDB; let it run in the background so
        // the notification handler returns immediately. tower-lsp would
        // otherwise serialise subsequent requests behind it.
        let core = Arc::clone(&self.core);
        tokio::spawn(async move {
            core.initialized().await;
        });
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        self.core.did_open(params).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        self.core.did_change(params).await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let core = Arc::clone(&self.core);
        tokio::spawn(async move {
            core.did_save(params).await;
        });
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.core.did_close(params).await;
    }

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        let core = Arc::clone(&self.core);
        tokio::spawn(async move {
            core.did_change_configuration(params).await;
        });
    }

    async fn did_change_workspace_folders(&self, params: DidChangeWorkspaceFoldersParams) {
        let core = Arc::clone(&self.core);
        tokio::spawn(async move {
            core.did_change_workspace_folders(params).await;
        });
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        Ok(self.core.completion(params).await)
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        Ok(self.core.hover(params).await)
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        Ok(self.core.document_symbol(params).await)
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        Ok(self.core.goto_definition(params).await)
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        Ok(Some(self.core.references(params).await))
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        Ok(self.core.prepare_rename(params).await)
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        Ok(self.core.rename(params).await)
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        Ok(self.core.signature_help(params).await)
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        Ok(self.core.code_action(params).await)
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        Ok(Some(self.core.document_highlight(params).await))
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        Ok(Some(self.core.inlay_hint(params).await))
    }

    async fn prepare_call_hierarchy(
        &self,
        params: CallHierarchyPrepareParams,
    ) -> Result<Option<Vec<CallHierarchyItem>>> {
        Ok(self.core.prepare_call_hierarchy(params).await)
    }

    async fn incoming_calls(
        &self,
        params: CallHierarchyIncomingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyIncomingCall>>> {
        Ok(Some(self.core.incoming_calls(params).await))
    }

    async fn outgoing_calls(
        &self,
        params: CallHierarchyOutgoingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyOutgoingCall>>> {
        Ok(Some(self.core.outgoing_calls(params).await))
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<WorkspaceSymbolResponse>> {
        Ok(self.core.workspace_symbol(params).await)
    }
}
