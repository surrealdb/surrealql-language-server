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
    /// What the client said it can do, at `initialize`.
    ///
    /// Read before sending anything optional. Requesting a capability the
    /// client never advertised is at best a wasted round trip and at worst an
    /// error the user sees; the server used to send `workspace/configuration`
    /// to everybody and swallow the failure.
    pub client_capabilities: ClientCapabilitySummary,
    pub workspace_folders: Vec<PathBuf>,
    pub saved_workspace: Arc<WorkspaceIndex>,
    pub open_documents: HashMap<Uri, Arc<DocumentAnalysis>>,
    /// The authoritative text of each open document.
    ///
    /// Held apart from [`Self::open_documents`] because the two now move at
    /// different times: an edit updates this synchronously and in arrival
    /// order, while the analysis that produces a `DocumentAnalysis` is
    /// debounced and may be dropped when a newer edit supersedes it. Under
    /// incremental sync a range edit applies to *this* text, so it cannot wait
    /// behind an analysis.
    pub buffers: HashMap<Uri, String>,
    /// Edits made since the tree in [`Self::open_documents`] was produced.
    ///
    /// tree-sitter can reuse a tree only if it is told exactly how the text
    /// changed. The list is *not* cleared when an analysis is dropped as
    /// superseded — the stored tree did not change either, so the next
    /// analysis replays the whole list against it and is still correct. It is
    /// cleared only in the same critical section that stores a new tree.
    pub pending_edits: HashMap<Uri, Vec<tree_sitter::InputEdit>>,
    /// The last semantic token set handed to the client, by result id.
    ///
    /// A delta request quotes the id it last received; without the tokens that
    /// id stood for there is nothing to diff against, so the server answers in
    /// full instead. One entry per document — a client only ever asks about the
    /// most recent id.
    pub last_semantic_tokens: HashMap<Uri, (String, Vec<ls_types::SemanticToken>)>,
    /// The newest `didChange` version seen for each open document.
    ///
    /// Two things read it. The debounce uses it to decide whether the edit it
    /// waited for is still the newest one, and the publish step uses it to drop
    /// a result computed from text the client has already replaced. Without it,
    /// running the analysis off the reactor would let an older version finish
    /// last and overwrite a newer one.
    ///
    /// `didOpen` does not record a version: it is never delayed, so there is
    /// nothing to supersede.
    pub document_versions: HashMap<Uri, i32>,
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
    /// Sorted signature of the settings warnings most recently logged,
    /// so a persistently bad configuration doesn't re-log on every
    /// pull. Same pattern as [`Self::last_metadata_errors`].
    pub last_settings_warnings: Option<Vec<String>>,
}

/// The parts of the client's advertised capabilities this server acts on.
///
/// A summary rather than the whole `ClientCapabilities` struct: only these two
/// change what the server sends, and storing the rest would invite reading it
/// somewhere that has not thought about the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClientCapabilitySummary {
    /// `workspace.configuration` — the client answers configuration pulls.
    pub configuration: bool,
    /// `textDocument.publishDiagnostics.relatedInformation`.
    pub related_information: bool,
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
