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
    /// What the client declared at `initialize`. All-false until then, which is
    /// also what a client that declares nothing gets.
    pub client: ClientProfile,
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

/// What the client told us it can do, reduced to the handful of answers this
/// server actually branches on.
///
/// `initialize` used to discard `params.capabilities` entirely, which is why
/// every optional protocol feature was either unavailable or unconditional.
/// Walking the nested `Option` soup per request would be both slow and easy to
/// get subtly wrong in one place and not another, so it is read once and the
/// answers are cached here.
///
/// Every field defaults to `false`, which is also what a capabilities-free
/// client gets: the wasm host sends one today. That is deliberate: an absent
/// capability must never turn a working behaviour off.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClientProfile {
    /// The client will ask for diagnostics rather than be told
    /// (`textDocument/diagnostic`). When true the server must **stop pushing**,
    /// or a client that does both shows every diagnostic twice.
    pub pull_diagnostics: bool,
    /// `textDocument/definition` may answer with `LocationLink`, which carries
    /// the origin range so the editor underlines the right extent.
    pub location_links: bool,
    /// `workspace/didChangeWatchedFiles` can be registered at run time. Without
    /// it there is no point asking.
    pub watched_file_registration: bool,
    /// `textDocument/documentSymbol` understands the nested form.
    pub hierarchical_symbols: bool,
    /// The client accepts `workspace/diagnostic/refresh`. Only meaningful
    /// alongside [`Self::pull_diagnostics`]: it is how a pulling client is told
    /// that an edit in another file changed what this one means.
    pub diagnostic_refresh: bool,
}

impl ClientProfile {
    /// Read the handful of answers that matter out of an `initialize` payload.
    pub fn from_capabilities(capabilities: &ls_types::ClientCapabilities) -> Self {
        let text_document = capabilities.text_document.as_ref();
        Self {
            pull_diagnostics: text_document.is_some_and(|caps| caps.diagnostic.is_some()),
            location_links: text_document
                .and_then(|caps| caps.definition.as_ref())
                .and_then(|definition| definition.link_support)
                .unwrap_or(false),
            watched_file_registration: capabilities
                .workspace
                .as_ref()
                .and_then(|workspace| workspace.did_change_watched_files.as_ref())
                .and_then(|watched| watched.dynamic_registration)
                .unwrap_or(false),
            hierarchical_symbols: text_document
                .and_then(|caps| caps.document_symbol.as_ref())
                .and_then(|symbol| symbol.hierarchical_document_symbol_support)
                .unwrap_or(false),
            diagnostic_refresh: capabilities
                .workspace
                .as_ref()
                .and_then(|workspace| workspace.diagnostics.as_ref())
                .and_then(|diagnostics| diagnostics.refresh_support)
                .unwrap_or(false),
        }
    }
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
    /// Set when a ranged change could not be applied: a range whose end precedes
    /// its start, or no base text to apply it to.
    ///
    /// Further ranged changes are refused until a full replacement or a re-open
    /// re-establishes the baseline, because there is no LSP request for "please
    /// resend the document". A conformant client will not volunteer one under
    /// `TextDocumentSyncKind::INCREMENTAL`, so the honest description of the
    /// recovery is *reopen the file*: that is what the diagnostic this raises
    /// tells the user, in the document, rather than in a log nobody reads when
    /// their squiggles stop moving.
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
    /// Out-of-range positions **clamp**, because the protocol says they do: a
    /// line past the end of the document is the number of lines in it, and a
    /// character past the end of its line is that line's length. Clients rely on
    /// this. `{line: 999999, character: 0}` is a normal way to say "the end",
    /// and refusing it used to wedge the buffer for the rest of the session,
    /// which is a far worse failure than the one the strictness was guarding
    /// against. [`LineIndex::offset`] already clamps both halves.
    ///
    /// Returns `false` only for a change that cannot be interpreted at all: a
    /// range whose end precedes its start. The caller then marks the buffer
    /// desynced, since applying later ranged changes to text that no longer
    /// matches the editor's is the corruption this whole path exists to avoid.
    pub fn splice(&mut self, range: Range, replacement: &str, version: i32) -> bool {
        let start = self.line_index.offset(&self.text, range.start);
        let end = self.line_index.offset(&self.text, range.end);
        if start > end {
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
        let start_position = self.line_index.point(&self.text, start);
        let edit = tree_sitter::InputEdit {
            start_byte: start,
            old_end_byte: end,
            new_end_byte: new_end,
            start_position,
            old_end_position: self.line_index.point(&self.text, end),
            new_end_position: end_point_after(start_position, replacement),
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

    /// Record the tree of a completed analysis, so the next parse can reuse it,
    /// but only while this buffer is still the text that analysis described.
    ///
    /// `analyzed_version` is the version read when the analysis started. An
    /// edit landing while it ran leaves the buffer ahead of the tree, and the
    /// tree carries no [`tree_sitter::Tree::edit`] for that edit, since there
    /// was no tree here to apply one to. Reparsing against it then hands
    /// tree-sitter byte ranges into text that no longer exists and gets back a
    /// tree that is quietly, structurally wrong.
    ///
    /// The version is what decides it. A length comparison would call an
    /// overtype or a same-size selection replacement "unchanged", which is a
    /// common way to type and the exact case this has to catch. Returns whether
    /// the tree was kept; a rejected one clears what is there rather than
    /// leaving an older tree behind.
    pub fn set_pending_tree_if_current(
        &mut self,
        analyzed_version: i32,
        tree: tree_sitter::Tree,
    ) -> bool {
        if self.desynced || self.version != analyzed_version {
            self.pending_tree = None;
            return false;
        }
        self.pending_tree = Some(tree);
        true
    }
}

/// Where a splice ends, given where it started and what was written there.
///
/// Derived from the replacement rather than by indexing the text it produced.
/// This runs per keystroke, and building a second [`LineIndex`] to read the
/// answer back out meant two full scans of the buffer inside the method that
/// exists to make keystrokes cheap.
///
/// `Point.column` counts **bytes**, not the protocol's UTF-16 characters, which
/// is what makes the arithmetic a matter of counting the replacement's own
/// bytes and newlines.
fn end_point_after(start: tree_sitter::Point, replacement: &str) -> tree_sitter::Point {
    match replacement.rfind('\n') {
        // The replacement ended a line, so the edit ends on a later row, as
        // many bytes in as follow that last newline.
        Some(last) => tree_sitter::Point {
            row: start.row + replacement.matches('\n').count(),
            column: replacement.len() - last - 1,
        },
        // Still on the row it started on, shifted by the replacement's length.
        None => tree_sitter::Point {
            row: start.row,
            column: start.column + replacement.len(),
        },
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

#[cfg(test)]
mod tests {
    use ls_types::Position;

    use super::*;

    fn tree_of(source: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&crate::grammar::language())
            .expect("grammar loads");
        parser.parse(source, None).expect("parses")
    }

    /// The guard is the version, not the length of the text.
    ///
    /// An overtype, or replacing a selection with the same number of
    /// characters, leaves the buffer a different document of exactly the same
    /// size. A length comparison calls that "unchanged" and keeps a tree
    /// describing the text before the edit, with no `Tree::edit` recorded for
    /// it, because there was no tree here to apply one to while the analysis
    /// ran. The next parse then reuses it and answers with a tree that is
    /// structurally wrong and stays wrong.
    #[test]
    fn a_same_length_edit_still_invalidates_the_analysed_tree() {
        let mut buffer = OpenBuffer::new("RETURN 1;\n".to_string(), 1);
        let analyzed_version = buffer.version;
        let tree = tree_of(&buffer.text);

        // An overtype: same byte count, different document.
        assert!(buffer.splice(Range::new(Position::new(0, 7), Position::new(0, 8)), "2", 2,));
        assert_eq!(buffer.text.len(), "RETURN 1;\n".len());

        assert!(
            !buffer.set_pending_tree_if_current(analyzed_version, tree),
            "a tree analysed before this edit must not become the base for the next parse"
        );
        assert!(buffer.pending_tree.is_none());
    }

    /// The case it must not refuse: nothing happened while the analysis ran.
    #[test]
    fn an_untouched_buffer_keeps_the_analysed_tree() {
        let mut buffer = OpenBuffer::new("RETURN 1;\n".to_string(), 4);
        let tree = tree_of(&buffer.text);

        assert!(buffer.set_pending_tree_if_current(4, tree));
        assert!(buffer.pending_tree.is_some());
    }

    /// The derived end point has to agree, in every case, with what building a
    /// fresh `LineIndex` over the spliced text would have said. That reindex is
    /// exactly what this replaced, so it is the thing to check against.
    #[test]
    fn the_derived_edit_end_matches_a_full_reindex() {
        let cases = [
            // No newline: same row, column shifted by the replacement.
            ("RETURN 1;\nRETURN 2;\n", (1, 7), (1, 8), "9"),
            // One newline: the row advances and the column restarts.
            ("RETURN 1;\nRETURN 2;\n", (0, 9), (0, 9), "\nRETURN 3;"),
            // Several at once, a paste.
            ("RETURN 1;\n", (0, 0), (0, 0), "a\nbb\nccc\n"),
            // A trailing newline, so the edit ends at column 0 of a new row.
            ("RETURN 1;\n", (0, 9), (0, 9), ";\n"),
            // An empty replacement, which is a deletion.
            ("RETURN 1;\nRETURN 2;\n", (0, 0), (1, 0), ""),
            // Multi-byte text, where bytes and UTF-16 units disagree.
            ("RETURN '₹';\n", (0, 8), (0, 9), "€\n€"),
            // An edit that starts partway along a line holding multi-byte text.
            ("-- ₹₹₹\nRETURN 1;\n", (0, 5), (0, 6), "€"),
        ];

        for (source, start, end, replacement) in cases {
            let index = LineIndex::new(source);
            let range = Range::new(Position::new(start.0, start.1), Position::new(end.0, end.1));
            let start_byte = index.offset(source, range.start);
            let end_byte = index.offset(source, range.end);
            let spliced = format!(
                "{}{}{}",
                &source[..start_byte],
                replacement,
                &source[end_byte..]
            );
            let new_end_byte = start_byte + replacement.len();

            let derived = end_point_after(index.point(source, start_byte), replacement);
            let reindexed = LineIndex::new(&spliced).point(&spliced, new_end_byte);
            assert_eq!(
                derived, reindexed,
                "splicing {replacement:?} into {source:?} at {start:?}"
            );
        }
    }

    /// And the whole splice still holds together end to end: the text is right
    /// and the tree it carries has absorbed the edit.
    #[test]
    fn a_splice_moves_the_text_and_the_tree_together() {
        let mut buffer = OpenBuffer::new("RETURN 1;\nRETURN 2;\n".to_string(), 1);
        buffer.pending_tree = Some(tree_of(&buffer.text));

        assert!(buffer.splice(
            Range::new(Position::new(0, 9), Position::new(0, 9)),
            "\nRETURN 3;",
            2,
        ));
        assert_eq!(*buffer.text, "RETURN 1;\nRETURN 3;\nRETURN 2;\n");

        let tree = buffer.pending_tree.as_ref().expect("kept through the edit");
        assert_eq!(
            tree.root_node().end_byte(),
            buffer.text.len(),
            "the edited tree must span the text it now describes"
        );
    }

    /// A desynced buffer takes no tree at all: its text is not what the editor
    /// holds, so nothing derived from it is a sound base for a reparse.
    #[test]
    fn a_desynced_buffer_refuses_the_analysed_tree() {
        let mut buffer = OpenBuffer::new("RETURN 1;\n".to_string(), 1);
        let tree = tree_of(&buffer.text);
        buffer.desynced = true;

        assert!(!buffer.set_pending_tree_if_current(1, tree));
        assert!(buffer.pending_tree.is_none());
    }
}
