//! Shared mutable state for the language server core.
//!
//! Mirrors the layout of the previous `BackendState` (see commit
//! history of `src/backend.rs`) but lives in the portable core so the
//! native `Backend` adapter and the WASM dispatcher hold the *same*
//! struct behind a `tokio::sync::RwLock`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use ls_types::Uri;

use crate::config::ServerSettings;
use crate::semantic::types::{
    DocumentAnalysis, LiveMetadataSnapshot, MergedSemanticModel, WorkspaceIndex,
};

/// All shared state lives behind [`Arc`] so cloning a snapshot for an
/// LSP request handler is a handful of pointer-bumps instead of a deep
/// clone of the entire workspace model. This was load-bearing for
/// hover/completion latency on large repos before the refactor and is
/// preserved verbatim.
#[derive(Debug, Default)]
pub struct ServerState {
    pub settings: Arc<ServerSettings>,
    pub workspace_folders: Vec<PathBuf>,
    pub saved_workspace: Arc<WorkspaceIndex>,
    pub open_documents: HashMap<Uri, Arc<DocumentAnalysis>>,
    pub live_metadata: Arc<LiveMetadataSnapshot>,
    pub model: Arc<MergedSemanticModel>,
    /// Fingerprint of the last successful workspace walk. When the new
    /// fingerprint matches, [`crate::core::server::LanguageServerCore::apply_settings`]
    /// skips the walk entirely — the common path for
    /// `didChangeConfiguration` events that don't touch the folder set.
    pub last_walked: Option<Vec<PathBuf>>,
    /// Sorted signature of the metadata errors most recently surfaced
    /// to the user. `Some(vec![])` means "last fetch was clean";
    /// `None` means nothing has been reported yet. Used to toast each
    /// distinct failure set once instead of on every save.
    pub last_metadata_errors: Option<Vec<String>>,
    /// Settings warnings gathered during `initialize`, before the
    /// client is ready to receive `window/logMessage`. Drained by
    /// `initialized`.
    pub pending_settings_warnings: Vec<String>,
}

/// Stable signature of a workspace-folder set, used to short-circuit
/// redundant walks.
pub fn workspace_signature(folders: &[PathBuf]) -> Vec<PathBuf> {
    let mut signature = folders.to_vec();
    signature.sort();
    signature
}

/// Combine the on-disk (or host-pushed) saved workspace with the
/// currently-open editor buffers. Open buffers always win — they
/// reflect the user's in-flight edits.
pub fn merged_workspace(
    saved_workspace: &WorkspaceIndex,
    open_documents: &HashMap<Uri, Arc<DocumentAnalysis>>,
) -> WorkspaceIndex {
    let mut workspace = saved_workspace.clone();
    for (uri, analysis) in open_documents {
        workspace
            .documents
            .insert(uri.clone(), Arc::clone(analysis));
    }
    workspace
}
