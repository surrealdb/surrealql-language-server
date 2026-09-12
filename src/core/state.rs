//! Shared mutable state for the language server core.
//!
//! Mirrors the layout of the previous `BackendState` (see commit
//! history of `src/backend.rs`) but lives in the portable core so the
//! native `Backend` adapter and the WASM dispatcher hold the *same*
//! struct behind a `tokio::sync::RwLock`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use ls_types::{Range, Uri};

use crate::config::ServerSettings;
use crate::semantic::text::LineIndex;
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
    /// Sorted signature of the settings warnings most recently logged,
    /// so a persistently bad configuration doesn't re-log on every
    /// pull. Same pattern as [`Self::last_metadata_errors`].
    pub last_settings_warnings: Option<Vec<String>>,
}

/// What the client last sent for one open document, before any analysis.
///
/// This (not `DocumentAnalysis.text`) is the authoritative buffer. The
/// distinction did not matter under full-document sync, where every
/// notification carries the whole text: the analysis *was* the buffer, one
/// debounce window behind. It matters completely under incremental sync, where
/// the next change's ranges are expressed against the text the previous change
/// produced. Resolving them against an analysis that is a debounce window stale
/// is not lag, it is corruption.
///
/// One consequence is worth stating so nobody "fixes" it later: during a
/// debounce window this text is *ahead* of the analysis a hover reads. That is
/// already true today. Pointing request handlers here instead would pair new
/// text with an old tree and old ranges, which is strictly worse.
#[derive(Debug, Clone)]
pub struct OpenBuffer {
    /// Exactly what the client last sent, with every change applied in order.
    pub text: Arc<String>,
    /// Line starts for [`Self::text`], used to convert the *next* change's
    /// ranges to byte offsets. Rebuilt after each applied change.
    pub line_index: LineIndex,
    /// The version of the last applied change.
    ///
    /// The debounce uses it to decide whether the edit it waited for is still
    /// the newest, and the publish step uses it to drop a result computed from
    /// text the client has already replaced: without it, running the analysis
    /// off the reactor would let an older version finish last and win.
    ///
    /// `didOpen` *replaces* it rather than comparing against it: the client is
    /// declaring where this buffer's versioning now starts. `didClose` drops the
    /// whole entry. Leaving a closed document's high-water mark behind froze
    /// diagnostics on reopen, because a client that restarts its counter then
    /// looked stale forever.
    pub version: i32,
    /// Set when a ranged change could not be applied: an out-of-bounds range,
    /// or no base text to apply it to.
    ///
    /// Further ranged changes are refused until a full replacement or a re-open
    /// re-establishes the baseline. There is no LSP request for "please resend
    /// the document", so refusing until the client sends a whole one is the
    /// recovery available, and it turns silent corruption into a visible,
    /// self-healing failure.
    pub desynced: bool,
    /// The tree of the newest completed analysis, with [`tree_sitter::Tree::edit`]
    /// applied for every change since: the old-tree argument for the next
    /// parse.
    ///
    /// Reparsing against it costs 0.77 ms on a 3,200-line document where a fresh
    /// parse costs 16.4 ms, and typing is exactly the case it is built for.
    ///
    /// `None` whenever the next parse must start clean: before the first
    /// analysis, after a whole-document replacement, and after any desync. A
    /// stale tree here would be worse than none, so every path that cannot
    /// maintain it clears it.
    pub pending_tree: Option<tree_sitter::Tree>,
}

impl OpenBuffer {
    /// A buffer holding `text` as its whole content.
    pub fn new(text: String, version: i32) -> Self {
        Self {
            line_index: LineIndex::new(&text),
            text: Arc::new(text),
            version,
            desynced: false,
            pending_tree: None,
        }
    }

    /// Replace the whole content, clearing any desync.
    ///
    /// Also drops the pending tree: there is no edit to describe a wholesale
    /// replacement, and reparsing against a tree of different text is worse than
    /// reparsing from nothing.
    pub fn replace(&mut self, text: String, version: i32) {
        self.line_index = LineIndex::new(&text);
        self.text = Arc::new(text);
        self.version = version;
        self.desynced = false;
        self.pending_tree = None;
    }

    /// Splice `replacement` into the region `range` covers.
    ///
    /// Returns `false` when the range describes a document this buffer is not:
    /// the caller marks the buffer desynced and refuses further ranged changes
    /// until a whole document arrives.
    ///
    /// The bounds check is not belt and braces. [`LineIndex::offset`] *clamps*:
    /// a line past the end of the document answers `source.len()` rather than
    /// failing, so a wrong range silently converts to a plausible offset and the
    /// splice lands somewhere real. Under full-document sync that self-corrects
    /// on the next keystroke; under incremental sync it compounds forever. The
    /// requested position has to be checked before the conversion is trusted.
    pub fn splice(&mut self, range: Range, replacement: &str, version: i32) -> bool {
        let line_count = self.line_index.line_count() as u32;
        if range.start.line >= line_count || range.end.line >= line_count {
            return false;
        }

        let start = self.line_index.offset(&self.text, range.start);
        let end = self.line_index.offset(&self.text, range.end);
        if start > end || end > self.text.len() {
            return false;
        }

        let mut text = String::with_capacity(self.text.len() - (end - start) + replacement.len());
        text.push_str(&self.text[..start]);
        text.push_str(replacement);
        text.push_str(&self.text[end..]);

        // Describe the splice to the pending tree before the text moves out from
        // under it, so the next parse can reuse everything the edit did not
        // touch. Positions are in *bytes* here: `tree_sitter::Point.column` is
        // not the protocol's UTF-16 character.
        let new_end = start + replacement.len();
        let edit = tree_sitter::InputEdit {
            start_byte: start,
            old_end_byte: end,
            new_end_byte: new_end,
            start_position: self.line_index.point(&self.text, start),
            old_end_position: self.line_index.point(&self.text, end),
            new_end_position: LineIndex::new(&text).point(&text, new_end),
        };
        let mut pending = self.pending_tree.take();
        if let Some(tree) = pending.as_mut() {
            tree.edit(&edit);
        }

        // Rebuilt per change, not once per batch: the next change in the same
        // notification is expressed against the text this one produced.
        self.replace(text, version);
        self.pending_tree = pending;
        true
    }

    /// Record the tree of a completed analysis, so the next parse can reuse it.
    pub fn set_pending_tree(&mut self, tree: tree_sitter::Tree) {
        self.pending_tree = Some(tree);
    }
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
