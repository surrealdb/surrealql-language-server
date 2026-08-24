//! The transport-agnostic language server.
//!
//! [`LanguageServerCore`] owns every piece of mutable state plus the
//! complete LSP request/notification handling logic. The native
//! `Backend` and the WASM `WasmLanguageServer` are thin adapters: they
//! receive transport-specific input (tower-lsp invocations or
//! JSON-RPC strings handed in from JavaScript), call the equivalent
//! method here, and return whatever the core produces.
//!
//! The three trait-bound generics [`LspNotifier`], [`WorkspaceLoader`]
//! and [`MetadataProvider`] keep this module ignorant of how
//! diagnostics are actually shipped, where `.surql` files come from,
//! and how live SurrealDB metadata is fetched. See
//! [`crate::core::client`] for the trait definitions and
//! [`crate::native`] / [`crate::wasm`] for the per-target impls.

use std::path::PathBuf;
use std::sync::Arc;

use ls_types::*;

use crate::config::{AuthContext, ServerSettings};
use crate::core::client::{LspNotifier, MetadataProvider, WorkspaceLoader};
use crate::core::completion_context::{
    ColumnSlot, active_query_fact, column_completion_context, completion_prefix,
    completion_table_qualifier, graph_anchors, graph_edge_context, head_slot_at,
    is_table_name_context, statement_target_in_text,
};
use crate::core::state::{ServerState, merged_workspace, workspace_signature};
use crate::core::statement_shape::SlotYield;
use crate::grammar::{BuiltinFunction, BuiltinSignature, builtin_function, builtin_signature};
use crate::runtime;
use crate::semantic::analyzer::{analyze_document, analyze_document_with_limit};
use crate::semantic::model::{
    field_completion_tables, function_signature_with_return, is_record_type_context, param_label,
};
use crate::semantic::pipeline::diagnostics_for_document;
use crate::semantic::text::{token_at, word_range};
use crate::semantic::types::{
    DocumentAnalysis, FunctionDef, LiveMetadataSnapshot, MergedSemanticModel, SymbolOrigin,
    WorkspaceIndex,
};

/// The crate version plus the source and grammar revisions this binary was
/// compiled from.
///
/// The bare crate version is the *same string* on an unreleased branch and
/// on the published release, so on its own it cannot tell you which binary
/// an editor is actually talking to. Clients surface
/// `serverInfo.version`, which makes that answerable at a glance instead of
/// by deduction.
pub fn build_version() -> String {
    format!(
        "{} ({})",
        env!("CARGO_PKG_VERSION"),
        env!("SURREALQL_LS_BUILD")
    )
}

/// Hosting-agnostic language server. Generic over the three boundary
/// traits so that a native (tower-lsp + walkdir + surrealdb) and a
/// browser (wasm-bindgen) front-end can share every line of business
/// logic.
pub struct LanguageServerCore<N: LspNotifier, W: WorkspaceLoader, M: MetadataProvider> {
    notifier: Arc<N>,
    workspace_loader: Arc<W>,
    metadata_provider: Arc<M>,
    state: Arc<runtime::sync::RwLock<ServerState>>,
    /// Serializes every settings/metadata application (the native
    /// adapter spawns `didChangeConfiguration` / `didSave` handlers,
    /// so two read-merge-apply sequences would otherwise interleave
    /// and lose updates or invert the metadata-error status).
    config_lock: Arc<runtime::sync::Mutex<()>>,
}

impl<N, W, M> LanguageServerCore<N, W, M>
where
    N: LspNotifier,
    W: WorkspaceLoader,
    M: MetadataProvider,
{
    pub fn new(notifier: N, workspace_loader: W, metadata_provider: M) -> Self {
        Self {
            notifier: Arc::new(notifier),
            workspace_loader: Arc::new(workspace_loader),
            metadata_provider: Arc::new(metadata_provider),
            state: Arc::new(runtime::sync::RwLock::new(ServerState::default())),
            config_lock: Arc::new(runtime::sync::Mutex::new(())),
        }
    }

    /// Borrow the outbound notifier (used by the native adapter to
    /// drive `did_save`/`did_change_configuration` background tasks).
    pub fn notifier(&self) -> &Arc<N> {
        &self.notifier
    }

    /// LSP capability advertisement, identical for both targets.
    pub fn server_capabilities() -> ServerCapabilities {
        ServerCapabilities {
            // Stated rather than left to the default. UTF-16 *is* the protocol
            // default, so this changes nothing on the wire — but every offset
            // this server produces is UTF-16 (`semantic::text`), and a client
            // reading the field learns that instead of assuming it. Negotiating
            // UTF-8 would mean changing every conversion; until someone needs
            // it, saying so beats implying it.
            position_encoding: Some(PositionEncodingKind::UTF16),
            // Options rather than the bare `Kind`: without a `save` entry a
            // spec-compliant client never sends `textDocument/didSave`, and
            // `did_save` is the only path that refreshes live database metadata
            // and re-reads a file from disk.
            text_document_sync: Some(TextDocumentSyncCapability::Options(
                TextDocumentSyncOptions {
                    open_close: Some(true),
                    // Incremental, now that `apply_document_change` runs in
                    // arrival order on the reactor. Full sync sent the whole
                    // document on every keystroke.
                    change: Some(TextDocumentSyncKind::INCREMENTAL),
                    save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                        include_text: Some(false),
                    })),
                    ..TextDocumentSyncOptions::default()
                },
            )),
            completion_provider: Some(CompletionOptions {
                // Table items ship without documentation and get it from
                // `completion_resolve`, so the dropdown does not pay to render
                // hover markdown for every table in the schema.
                resolve_provider: Some(true),
                // `>` closes a `->`, the way `<` opens a `<-`. Without it the
                // two arrow directions behaved differently: `<-` popped the
                // list and `->` did not, so the commoner spelling was the one
                // that looked broken.
                trigger_characters: Some(vec![
                    ".".into(),
                    ":".into(),
                    "<".into(),
                    ">".into(),
                    "$".into(),
                    "(".into(),
                ]),
                ..CompletionOptions::default()
            }),
            hover_provider: Some(HoverProviderCapability::Simple(true)),
            definition_provider: Some(OneOf::Left(true)),
            references_provider: Some(OneOf::Left(true)),
            rename_provider: Some(OneOf::Right(RenameOptions {
                prepare_provider: Some(true),
                work_done_progress_options: Default::default(),
            })),
            signature_help_provider: Some(SignatureHelpOptions {
                trigger_characters: Some(vec!["(".into(), ",".into()]),
                retrigger_characters: Some(vec![",".into()]),
                work_done_progress_options: Default::default(),
            }),
            // The kinds we actually produce. A client uses these to decide
            // which actions to request, and to bind "source.fixAll"-style
            // commands; `Simple(true)` told it nothing.
            code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
                code_action_kinds: Some(vec![
                    CodeActionKind::QUICKFIX,
                    CodeActionKind::REFACTOR_REWRITE,
                ]),
                resolve_provider: Some(false),
                work_done_progress_options: Default::default(),
            })),
            document_highlight_provider: Some(OneOf::Left(true)),
            inlay_hint_provider: Some(OneOf::Right(InlayHintServerCapabilities::Options(
                InlayHintOptions {
                    resolve_provider: Some(false),
                    ..InlayHintOptions::default()
                },
            ))),
            call_hierarchy_provider: Some(CallHierarchyServerCapability::Simple(true)),
            semantic_tokens_provider: Some(
                SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
                    legend: crate::semantic::highlight::legend(),
                    // Delta, so a client re-highlighting after a keystroke
                    // receives the edits rather than every token in the file.
                    full: Some(SemanticTokensFullOptions::Delta { delta: Some(true) }),
                    range: Some(true),
                    work_done_progress_options: Default::default(),
                }),
            ),
            document_symbol_provider: Some(OneOf::Left(true)),
            folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
            selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
            type_definition_provider: Some(TypeDefinitionProviderCapability::Simple(true)),
            // Pull diagnostics, alongside the push channel rather than instead
            // of it: a client that does not pull still gets
            // `publishDiagnostics`, and one that does gets a whole-workspace
            // view a push model cannot give it.
            diagnostic_provider: Some(DiagnosticServerCapabilities::Options(DiagnosticOptions {
                identifier: Some("surrealql".to_string()),
                // A schema in one file decides whether a query in another is
                // valid, so editing one document really does change another's
                // diagnostics.
                inter_file_dependencies: true,
                workspace_diagnostics: true,
                work_done_progress_options: Default::default(),
            })),
            document_formatting_provider: Some(OneOf::Left(true)),
            document_range_formatting_provider: Some(OneOf::Left(true)),
            document_link_provider: Some(DocumentLinkOptions {
                resolve_provider: Some(false),
                work_done_progress_options: Default::default(),
            }),
            workspace_symbol_provider: Some(OneOf::Left(true)),
            workspace: Some(WorkspaceServerCapabilities {
                workspace_folders: Some(WorkspaceFoldersServerCapabilities {
                    supported: Some(true),
                    change_notifications: Some(OneOf::Left(true)),
                }),
                file_operations: None,
            }),
            ..ServerCapabilities::default()
        }
    }

    // ──────────────────────────────────────────────────────────────────
    // Lifecycle
    // ──────────────────────────────────────────────────────────────────

    /// Keep only the actions that touch `range`.
    ///
    /// The handler used to return every action in the document, so the
    /// add-PERMISSIONS refactor appeared for every table in the file no matter
    /// where the cursor was.
    fn retain_actions_in_range(actions: &mut Vec<CodeActionOrCommand>, range: Range) {
        actions.retain(|action| match action {
            CodeActionOrCommand::CodeAction(action) => {
                // A quick fix is anchored by the diagnostic it fixes.
                if let Some(diagnostics) = &action.diagnostics
                    && !diagnostics.is_empty()
                {
                    return diagnostics
                        .iter()
                        .any(|diagnostic| Self::ranges_intersect(diagnostic.range, range));
                }
                // Everything else is anchored by what it would edit — by line, not
                // by character. Such an action is usually an insertion at a single
                // point (the end of a statement), and a user whose cursor is at the
                // start of that line still means that statement.
                match &action.edit {
                    Some(edit) => {
                        Self::edit_ranges(edit).any(|edited| Self::lines_intersect(edited, range))
                    }
                    // No diagnostic and no edit: nothing to anchor it to, so leave
                    // it rather than guess.
                    None => true,
                }
            }
            CodeActionOrCommand::Command(_) => true,
        });
    }

    /// Keep only the kinds the client asked for.
    ///
    /// Kinds are hierarchical: a request for `refactor` includes
    /// `refactor.rewrite`.
    fn retain_requested_kinds(
        actions: &mut Vec<CodeActionOrCommand>,
        only: Option<&[CodeActionKind]>,
    ) {
        let Some(only) = only.filter(|kinds| !kinds.is_empty()) else {
            return;
        };
        actions.retain(|action| match action {
            CodeActionOrCommand::CodeAction(action) => match &action.kind {
                Some(kind) => only.iter().any(|wanted| {
                    kind == wanted || kind.as_str().starts_with(&format!("{}.", wanted.as_str()))
                }),
                None => false,
            },
            CodeActionOrCommand::Command(_) => false,
        });
    }

    /// Whether two ranges share a line. The anchor for an action that edits at a
    /// point rather than over a span.
    fn lines_intersect(left: Range, right: Range) -> bool {
        left.start.line <= right.end.line && right.start.line <= left.end.line
    }

    fn ranges_intersect(left: Range, right: Range) -> bool {
        let start =
            (left.start.line, left.start.character).max((right.start.line, right.start.character));
        let end = (left.end.line, left.end.character).min((right.end.line, right.end.character));
        start <= end
    }

    fn edit_ranges(edit: &WorkspaceEdit) -> impl Iterator<Item = Range> + '_ {
        let from_changes = edit
            .changes
            .iter()
            .flat_map(|map| map.values())
            .flatten()
            .map(|text_edit| text_edit.range);
        let from_documents = edit.document_changes.iter().flat_map(|changes| {
            let edits: Vec<Range> = match changes {
                DocumentChanges::Edits(edits) => edits
                    .iter()
                    .flat_map(|document| document.edits.iter())
                    .map(|edit| match edit {
                        OneOf::Left(text_edit) => text_edit.range,
                        OneOf::Right(annotated) => annotated.text_edit.range,
                    })
                    .collect(),
                // An `Operations` list mixes edits with file operations. Only the
                // edits carry a range; a create or a rename has nothing to anchor
                // to and is skipped.
                DocumentChanges::Operations(operations) => operations
                    .iter()
                    .filter_map(|operation| match operation {
                        ls_types::DocumentChangeOperation::Edit(document) => Some(document),
                        ls_types::DocumentChangeOperation::Op(_) => None,
                    })
                    .flat_map(|document| document.edits.iter())
                    .map(|edit| match edit {
                        OneOf::Left(text_edit) => text_edit.range,
                        OneOf::Right(annotated) => annotated.text_edit.range,
                    })
                    .collect(),
            };
            edits
        });
        from_changes.chain(from_documents)
    }

    /// Read the two client capabilities this server changes its behaviour for.
    ///
    /// Absent means unsupported, per the protocol: a client that says nothing gets
    /// the conservative treatment rather than the optimistic one.
    fn summarize_client_capabilities(
        capabilities: &ls_types::ClientCapabilities,
    ) -> crate::core::state::ClientCapabilitySummary {
        crate::core::state::ClientCapabilitySummary {
            configuration: capabilities
                .workspace
                .as_ref()
                .and_then(|workspace| workspace.configuration)
                .unwrap_or(false),
            related_information: capabilities
                .text_document
                .as_ref()
                .and_then(|document| document.publish_diagnostics.as_ref())
                .and_then(|diagnostics| diagnostics.related_information)
                .unwrap_or(false),
        }
    }

    /// Stash the client-supplied initialization options + workspace
    /// folders. The heavy work (workspace walk, live metadata fetch) is
    /// deferred to [`Self::apply_settings`], usually triggered by
    /// `initialized → reload_from_client_configuration`.
    pub async fn initialize(&self, params: InitializeParams) -> InitializeResult {
        // Folders first: the project configuration file lives in one of them,
        // and it has to be read before the settings are built so it can sit
        // underneath `initializationOptions`.
        let workspace_folders = resolve_workspace_folders(&params);
        let (project, mut warnings) = self
            .workspace_loader
            .load_project_config(&workspace_folders)
            .await;
        let (settings, settings_warnings) = ServerSettings::from_sources_with_project(
            project.as_ref(),
            params.initialization_options.as_ref(),
            None,
        );
        warnings.extend(settings_warnings);

        let client_capabilities = Self::summarize_client_capabilities(&params.capabilities);

        {
            let mut state = self.state.write().await;
            state.settings = Arc::new(settings);
            state.workspace_folders = workspace_folders;
            state.client_capabilities = client_capabilities;
            // The client can't receive `window/logMessage` until the
            // initialize handshake completes; `initialized` drains these.
            state.pending_settings_warnings = warnings;
        }

        InitializeResult {
            server_info: Some(ServerInfo {
                name: "surreal-language-server".to_string(),
                version: Some(build_version()),
            }),
            capabilities: Self::server_capabilities(),
            ..Default::default()
        }
    }

    /// Pull the latest configuration from the client and re-run
    /// [`Self::apply_settings`]. Logs an info message on completion.
    pub async fn initialized(&self) {
        // Ask the client to watch `.surql` files. Without this a file created,
        // changed or deleted outside the editor stays invisible to the model
        // until the server restarts.
        self.notifier.register_file_watchers().await;

        let pending_warnings = {
            let mut state = self.state.write().await;
            std::mem::take(&mut state.pending_settings_warnings)
        };
        self.report_settings_warnings(&pending_warnings).await;
        self.reload_from_client_configuration().await;
        self.notifier
            .log_message(
                MessageType::INFO,
                "SurrealQL semantic language server ready".to_string(),
            )
            .await;
    }

    /// Asks the client for the `surrealql` configuration section
    /// (`workspace/configuration`), merges the response with whatever
    /// settings are already in flight, and applies the result.
    pub async fn reload_from_client_configuration(&self) {
        // Ask the client before taking the config lock — a slow
        // configuration pull must not stall other settings work.
        // Only ask a client that said it answers. The request used to go to
        // everybody, with the failure swallowed.
        let supports_configuration = {
            let state = self.state.read().await;
            state.client_capabilities.configuration
        };
        let configuration = if supports_configuration {
            self.notifier.request_configuration().await
        } else {
            None
        };
        let folders = {
            let state = self.state.read().await;
            state.workspace_folders.clone()
        };
        // Re-read rather than cache: the file is editable, and a configuration
        // pull is exactly when a user expects an edit to it to take effect.
        let (project, mut warnings) = self.workspace_loader.load_project_config(&folders).await;
        let (settings, settings_warnings) = ServerSettings::from_sources_with_project(
            project.as_ref(),
            None,
            configuration.as_ref(),
        );
        warnings.extend(settings_warnings);
        // A client without configuration support answers `None`, and
        // VS Code / Neovim answer the pull with JSON `null` when no
        // `surrealql` section is configured — both always yield zero
        // warnings. Reporting those would reset the dedup signature
        // and fire a spurious "resolved" line right after the
        // initialize-stashed warnings. (A *real* clean section does
        // report: it replaces the previous settings, so its empty
        // warning set genuinely resolves them.)
        if configuration.as_ref().is_some_and(|value| !value.is_null()) || project.is_some() {
            self.report_settings_warnings(&warnings).await;
        }

        let _guard = self.config_lock.lock().await;
        let current_settings = {
            let state = self.state.read().await;
            (*state.settings).clone()
        };
        let settings = settings.merge_with_env_if_missing(current_settings);
        self.apply_settings_inner(settings).await;
    }

    /// Log settings warnings, once per *distinct* warning set — a
    /// persistently misconfigured editor would otherwise re-log the
    /// same lines on every configuration pull and `didChange`. Same
    /// pattern as [`Self::report_metadata_errors`]: the state lock is
    /// released before any notifier await.
    async fn report_settings_warnings(&self, warnings: &[String]) {
        let mut signature = warnings.to_vec();
        signature.sort();

        let previous = {
            let mut state = self.state.write().await;
            state.last_settings_warnings.replace(signature.clone())
        };

        if previous.as_ref() == Some(&signature) {
            return;
        }

        if signature.is_empty() {
            if previous.is_some_and(|previous| !previous.is_empty()) {
                self.notifier
                    .log_message(
                        MessageType::INFO,
                        "SurrealQL settings: previous warnings resolved".to_string(),
                    )
                    .await;
            }
            return;
        }

        for warning in warnings {
            self.notifier
                .log_message(
                    MessageType::WARNING,
                    format!("SurrealQL settings: {warning}"),
                )
                .await;
        }
    }

    /// Persist new settings, re-load the saved workspace, refresh live
    /// metadata, rebuild the merged model, and republish diagnostics
    /// for every open document.
    ///
    /// Native callers wrap this in `tokio::spawn` so notification
    /// handlers return immediately. WASM callers `await` it directly
    /// — there is no advantage to background scheduling on a
    /// single-threaded event loop.
    pub async fn apply_settings(&self, settings: ServerSettings) {
        let _guard = self.config_lock.lock().await;
        self.apply_settings_inner(settings).await;
    }

    /// [`Self::apply_settings`] body; callers must hold `config_lock`.
    async fn apply_settings_inner(&self, settings: ServerSettings) {
        let (workspace_folders, last_walked, previous_syntax_limit) = {
            let mut state = self.state.write().await;
            let previous_syntax_limit = state.settings.analysis.max_syntax_diagnostics;
            state.settings = Arc::new(settings.clone());
            (
                state.workspace_folders.clone(),
                state.last_walked.clone(),
                previous_syntax_limit,
            )
        };

        // The syntax cap is applied while the tree is walked, so an already
        // analyzed document keeps the count it was parsed under. Re-analyze
        // the open ones when the cap moves, otherwise raising it appears to
        // do nothing until each buffer is edited.
        if previous_syntax_limit != settings.analysis.max_syntax_diagnostics {
            self.reanalyze_open_documents(settings.analysis.max_syntax_diagnostics)
                .await;
        }

        let folder_signature = workspace_signature(&workspace_folders);
        let need_walk = last_walked
            .as_ref()
            .map(|previous| previous != &folder_signature)
            .unwrap_or(true);

        let saved_workspace = if settings.metadata.filesystem_enabled() {
            if need_walk {
                Arc::new(self.workspace_loader.load(&workspace_folders).await)
            } else {
                let s = self.state.read().await;
                Arc::clone(&s.saved_workspace)
            }
        } else {
            self.state.write().await.last_walked = None;
            Arc::new(Default::default())
        };

        self.notifier
            .begin_progress("surrealql/metadata", "Reading SurrealDB schema")
            .await;
        let live_metadata = Arc::new(self.metadata_provider.fetch(&settings).await);
        self.notifier.end_progress("surrealql/metadata", None).await;
        self.report_metadata_errors(&live_metadata).await;
        if need_walk {
            self.report_scan_stats(&saved_workspace).await;
        }
        let (open_documents, uris_for_diag) = {
            let s = self.state.read().await;
            (
                s.open_documents.clone(),
                s.open_documents.keys().cloned().collect::<Vec<_>>(),
            )
        };
        let workspace = merged_workspace(&saved_workspace, &open_documents);
        let model = Arc::new(MergedSemanticModel::build(&workspace, &live_metadata));

        {
            let mut state = self.state.write().await;
            state.saved_workspace = Arc::clone(&saved_workspace);
            state.live_metadata = Arc::clone(&live_metadata);
            state.model = Arc::clone(&model);
            if need_walk {
                state.last_walked = Some(folder_signature);
            }
        }

        let saved_for_diag = saved_workspace;
        let model_for_diag = model;
        let settings_for_diag = Arc::new(settings);
        for uri in uris_for_diag {
            let analysis = open_documents
                .get(&uri)
                .cloned()
                .or_else(|| saved_for_diag.documents.get(&uri).cloned());
            if let Some(analysis) = analysis {
                let diagnostics =
                    diagnostics_for_document(&analysis, &model_for_diag, &settings_for_diag);
                self.notifier.publish_diagnostics(uri, diagnostics).await;
            }
        }
    }

    // ──────────────────────────────────────────────────────────────────
    // Document lifecycle
    // ──────────────────────────────────────────────────────────────────

    pub async fn did_open(&self, params: DidOpenTextDocumentParams) {
        {
            let mut state = self.state.write().await;
            state.buffers.insert(
                params.text_document.uri.clone(),
                params.text_document.text.clone(),
            );
            state.pending_edits.remove(&params.text_document.uri);
        }
        let document = params.text_document;
        self.upsert_open_document(document.uri, document.text, Edit::Opened)
            .await;
    }

    /// Apply an edit to the authoritative buffer, then analyse.
    ///
    /// Kept as one call for the browser host, which dispatches serially. The
    /// native adapter calls the two halves separately so the edit lands in
    /// arrival order while only the analysis is spawned.
    pub async fn did_change(&self, params: DidChangeTextDocumentParams) {
        if let Some((uri, text, version)) = self.apply_document_change(params).await {
            self.analyze_changed_document(uri, text, version).await;
        }
    }

    /// The ordering-critical half of `didChange`: update the buffer.
    ///
    /// Must run to completion in arrival order. Under incremental sync a change
    /// carries a range into the *previous* text, so applying two notifications
    /// out of order corrupts the document — which is why the analysis is a
    /// separate call the caller may spawn.
    pub async fn apply_document_change(
        &self,
        params: DidChangeTextDocumentParams,
    ) -> Option<(Uri, String, i32)> {
        let uri = params.text_document.uri;
        let version = params.text_document.version;
        if params.content_changes.is_empty() {
            return None;
        }

        let mut state = self.state.write().await;
        // Versions are monotonic per document, so a version we have already
        // seen means the notification arrived out of order. Under incremental
        // sync its range refers to text that no longer exists, and applying it
        // would corrupt the buffer — so it is dropped here rather than
        // filtered later. Applying edits on the reactor should make this
        // unreachable; it is the guard that says so.
        if state
            .document_versions
            .get(&uri)
            .is_some_and(|seen| *seen >= version)
        {
            return None;
        }
        // A full-text change replaces the buffer whatever came before, so a
        // client that never sent `didOpen` still works.
        // Taken out and put back rather than borrowed in place: the edit
        // accumulator lives in the same struct, and both are touched here.
        let mut text = state.buffers.remove(&uri).unwrap_or_default();
        let mut edits = Vec::with_capacity(params.content_changes.len());
        let mut replaced_whole = false;
        for change in &params.content_changes {
            // Computed against the text as it stands *before* this change,
            // which is what tree-sitter needs.
            match apply_content_change(&mut text, change) {
                Some(edit) => edits.push(edit),
                None => {
                    // A whole-document replacement invalidates every earlier
                    // edit; the next analysis parses from scratch.
                    edits.clear();
                    replaced_whole = true;
                }
            }
        }
        let updated = text.clone();
        state.buffers.insert(uri.clone(), text);
        if replaced_whole {
            state.pending_edits.remove(&uri);
        }
        if !edits.is_empty() {
            state
                .pending_edits
                .entry(uri.clone())
                .or_default()
                .extend(edits);
        }
        state.document_versions.insert(uri.clone(), version);
        Some((uri, updated, version))
    }

    /// The half that may be spawned: analyse the buffer and publish.
    pub async fn analyze_changed_document(&self, uri: Uri, text: String, version: i32) {
        self.upsert_open_document(uri, text, Edit::Changed(version))
            .await;
    }

    pub async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let (refresh_remote, uri) = {
            let state = self.state.read().await;
            (
                state.settings.metadata.refresh_on_save,
                params.text_document.uri.clone(),
            )
        };
        self.sync_saved_document_from_disk(&uri).await;
        self.recompute_model().await;
        if refresh_remote {
            self.refresh_remote_metadata_if_needed().await;
        }
        self.publish_diagnostics_for_uri(&uri).await;
    }

    pub async fn did_close(&self, params: DidCloseTextDocumentParams) {
        {
            let mut state = self.state.write().await;
            state.buffers.remove(&params.text_document.uri);
            state.pending_edits.remove(&params.text_document.uri);
        }
        let uri = params.text_document.uri;
        {
            let mut state = self.state.write().await;
            state.open_documents.remove(&uri);
        }
        self.sync_saved_document_from_disk(&uri).await;
        self.recompute_model().await;
        self.notifier.publish_diagnostics(uri, Vec::new()).await;
    }

    pub async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        // Clients that support configuration pulls send `null` here as
        // a "something changed, ask me" signal.
        if params.settings.is_null() {
            self.reload_from_client_configuration().await;
            return;
        }

        // Merge over the in-flight settings — a partial payload (e.g.
        // one that only carries `metadata.*`) must not wipe the
        // connection details that arrived via initializationOptions.
        // The read-merge-apply sequence runs under the config lock so
        // two spawned configuration changes can't lose each other's
        // updates.
        let folders = {
            let state = self.state.read().await;
            state.workspace_folders.clone()
        };
        // The project file is a layer under this payload, exactly as it is
        // under a configuration pull. Without it here, a `didChangeConfiguration`
        // carrying one key would discard every key the file supplied.
        let (project, mut warnings) = self.workspace_loader.load_project_config(&folders).await;
        let (settings, settings_warnings) = ServerSettings::from_sources_with_project(
            project.as_ref(),
            None,
            Some(&params.settings),
        );
        warnings.extend(settings_warnings);
        self.report_settings_warnings(&warnings).await;

        let _guard = self.config_lock.lock().await;
        let current_settings = {
            let state = self.state.read().await;
            (*state.settings).clone()
        };
        let settings = settings.merge_with_env_if_missing(current_settings);
        self.apply_settings_inner(settings).await;
    }

    /// A `.surql` file changed on disk outside the editor.
    ///
    /// A created or changed file is re-read into the saved workspace; a deleted
    /// one is dropped. An open buffer always wins, so a file the user is
    /// editing is left alone — the editor is the authority on that text.
    pub async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        let mut changed = false;
        for event in params.changes {
            let is_open = {
                let state = self.state.read().await;
                state.open_documents.contains_key(&event.uri)
            };
            if is_open {
                continue;
            }
            match event.typ {
                FileChangeType::DELETED => {
                    let mut state = self.state.write().await;
                    let mut workspace = (*state.saved_workspace).clone();
                    if workspace.documents.remove(&event.uri).is_some() {
                        state.saved_workspace = Arc::new(workspace);
                        changed = true;
                    }
                }
                _ => {
                    let Some(text) = self.workspace_loader.read_document(&event.uri).await else {
                        continue;
                    };
                    let limit = {
                        let state = self.state.read().await;
                        state.settings.analysis.max_syntax_diagnostics
                    };
                    let Some(analysis) =
                        analyze_off_reactor(event.uri.clone(), text, limit, None).await
                    else {
                        continue;
                    };
                    let mut state = self.state.write().await;
                    let mut workspace = (*state.saved_workspace).clone();
                    workspace
                        .documents
                        .insert(event.uri.clone(), Arc::new(analysis));
                    state.saved_workspace = Arc::new(workspace);
                    changed = true;
                }
            }
        }
        if changed {
            self.recompute_model().await;
            self.republish_open_diagnostics().await;
        }
    }

    pub async fn did_change_workspace_folders(&self, params: DidChangeWorkspaceFoldersParams) {
        {
            let mut state = self.state.write().await;
            for removed in params.event.removed {
                if let Some(path) = removed.uri.to_file_path() {
                    let path = path.into_owned();
                    state.workspace_folders.retain(|folder| folder != &path);
                }
            }
            for added in params.event.added {
                if let Some(path) = added.uri.to_file_path() {
                    let path = path.into_owned();
                    if !state.workspace_folders.contains(&path) {
                        state.workspace_folders.push(path);
                    }
                }
            }
        }

        let settings = {
            let state = self.state.read().await;
            (*state.settings).clone()
        };
        self.apply_settings(settings).await;
    }

    /// Re-run the workspace loader and rebuild the merged model
    /// without touching settings or live metadata. The WASM adapter
    /// uses this after the host pushes / drops a saved document so
    /// the change is reflected immediately.
    pub async fn reload_workspace(&self) {
        let folders = {
            let state = self.state.read().await;
            state.workspace_folders.clone()
        };
        self.notifier
            .begin_progress("surrealql/scan", "Scanning SurrealQL files")
            .await;
        let workspace = Arc::new(self.workspace_loader.load(&folders).await);
        self.notifier
            .end_progress(
                "surrealql/scan",
                Some(format!("{} file(s)", workspace.documents.len())),
            )
            .await;
        self.report_scan_stats(&workspace).await;
        {
            let mut state = self.state.write().await;
            state.saved_workspace = workspace;
        }
        self.recompute_model().await;
        self.republish_open_diagnostics().await;
    }

    /// Replace the current live metadata snapshot. Used by the WASM
    /// target so Surrealist can push DEFINE strings over from its
    /// already-open SurrealDB connection.
    pub async fn replace_live_metadata(
        &self,
        snapshot: crate::semantic::types::LiveMetadataSnapshot,
    ) {
        {
            let mut state = self.state.write().await;
            state.live_metadata = Arc::new(snapshot);
        }
        self.recompute_model().await;
        self.republish_open_diagnostics().await;
    }

    // ──────────────────────────────────────────────────────────────────
    // Per-request handlers
    // ──────────────────────────────────────────────────────────────────

    pub async fn completion(&self, params: CompletionParams) -> Option<CompletionResponse> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let (analysis, model, settings) = self.snapshot_for_uri(&uri).await?;

        let record_type_context =
            is_record_type_context(&analysis.text, &analysis.line_index, position);
        let prefix = completion_prefix(
            &analysis.text,
            &analysis.line_index,
            position,
            record_type_context,
        );

        // A cursor just after `->` / `<-` names the next hop of a graph
        // traversal, so only a table is legal there — and the ones actually
        // reachable from this statement's table belong at the top. Checked
        // before the plain table slot below because the two scans disagree
        // about the arrow: `is_table_name_context` walks back over identifier
        // characters, which `>` is not, so it would answer `false` and let the
        // whole catalogue through.
        if !record_type_context
            && let Some(slot) = graph_edge_context(&analysis.text, &analysis.line_index, position)
        {
            let anchors = graph_anchors(&slot, &analysis, position);
            let items = model.graph_completion_items(
                prefix.trim_matches(|ch: char| ch == ':'),
                &anchors,
                slot.direction,
                slot.from_edge,
                settings.active_auth_context(),
            );
            // Say exactly which characters an item replaces, instead of
            // leaving the client to work it out. Everywhere else in this
            // handler the cursor follows a space or a `.`, so any client's
            // word scan agrees with ours; after `->` it does not. A client
            // whose word pattern admits `-` or `>` reads the prefix as
            // `person->`, filters every item against it, matches none, and
            // shows an empty popup — the same defect this server had, arrived
            // at independently. An explicit range removes the guesswork.
            let replace = replaced_range(&analysis, position, prefix.len());
            return Some(CompletionResponse::Array(with_replace_range(
                items, replace,
            )));
        }

        // When the cursor sits in a slot that only accepts a table name
        // (e.g. `SELECT * FROM |`, `INSERT INTO |`, `UPDATE |`), restrict
        // suggestions to known tables — otherwise the dropdown is flooded
        // with keywords/functions/fields/params the user can't legally use
        // there.
        if !record_type_context
            && is_table_name_context(&analysis.text, &analysis.line_index, position)
        {
            let items = model.table_completion_items(
                prefix.trim_matches(|ch: char| ch == ':'),
                settings.active_auth_context(),
            );
            return Some(CompletionResponse::Array(items));
        }

        let trimmed_prefix = prefix.trim_matches(|ch: char| ch == ':');

        // A statement head whose legal continuations are a closed set —
        // `INFO FOR `, `USE `, `DEFINE `, `REMOVE `, `ALTER `, `SHOW `. Offering
        // the whole catalogue in those positions is the reported defect: an
        // empty prefix disables every filter below, so `INFO FOR ` returned ~375
        // items of which nine were legal.
        //
        // The table answers `Expression` for anything it does not model, which
        // falls through to the behaviour this handler had before, so no working
        // position can regress.
        if !record_type_context {
            let slot = head_slot_at(&analysis.text, &analysis.line_index, position);
            if slot != SlotYield::Expression {
                return Some(CompletionResponse::Array(head_slot_items(
                    slot,
                    trimmed_prefix,
                    &model,
                    settings.active_auth_context(),
                )));
            }
        }

        let statement_fact = active_query_fact(&analysis, position);
        let qualifier = completion_table_qualifier(&analysis.text, &analysis.line_index, position);

        // A `.` admits a field *and* a method, so these are added to whatever the
        // position already offers rather than replacing it.
        let method_items = model.method_completion_items(&analysis, position, trimmed_prefix);

        // A `.` on a value that has named members — a row a query built, or a
        // record pointing at a declared table. Only those members and the
        // methods are legal there, so this answers outright. `completion_table_
        // qualifier` cannot serve this case: it deliberately refuses a `$`
        // receiver, having no way to tell a variable from a table name.
        let property_items = model.property_completion_items(&analysis, position, trimmed_prefix);
        if !property_items.is_empty() {
            let mut items = property_items;
            items.extend(method_items);
            return Some(CompletionResponse::Array(items));
        }

        // Decide whether the cursor is in a column-name slot. A `tbl.`
        // qualifier is always treated as a strict slot (the only legal
        // continuations are field names of `tbl`).
        let column_slot = if qualifier.is_some() {
            Some(ColumnSlot::Strict { allow_star: false })
        } else if record_type_context {
            None
        } else {
            column_completion_context(&analysis.text, &analysis.line_index, position)
        };

        if let Some(ColumnSlot::Strict { allow_star }) = column_slot {
            let field_tables = field_tables_at(&analysis, position, qualifier.as_deref());
            if !field_tables.is_empty() {
                let multi_table_context = qualifier.is_none() && field_tables.len() > 1;
                let mut items = model.column_completion_items(
                    trimmed_prefix,
                    &field_tables,
                    multi_table_context,
                    settings.active_auth_context(),
                );
                if allow_star && (trimmed_prefix.is_empty() || "*".starts_with(trimmed_prefix)) {
                    items.insert(
                        0,
                        CompletionItem {
                            label: "*".to_string(),
                            kind: Some(CompletionItemKind::OPERATOR),
                            detail: Some("All columns".to_string()),
                            insert_text: Some("*".to_string()),
                            sort_text: Some("0-aaa-star".to_string()),
                            ..CompletionItem::default()
                        },
                    );
                }
                items.extend(method_items);
                return Some(CompletionResponse::Array(items));
            }
        }

        let mut items = model.completion_items(
            trimmed_prefix,
            record_type_context,
            settings.active_auth_context(),
            statement_fact,
            qualifier.as_deref(),
        );
        // In-scope `$variables` sort above everything else: when the user
        // has typed a `$`, a local binding is almost always what they mean.
        if !record_type_context {
            let mut variables =
                model.variable_completion_items(&analysis, position, trimmed_prefix);
            variables.append(&mut items);
            items = variables;
        }
        // Methods first: at a `.` position they are what the user is reaching
        // for, and their `sort_text` already ranks a type-matched method above
        // the untyped fallback.
        if !method_items.is_empty() {
            let mut merged = method_items;
            merged.append(&mut items);
            items = merged;
        }
        Some(CompletionResponse::Array(items))
    }

    /// Fill in the documentation for the completion item the client is
    /// showing. See [`MergedSemanticModel::resolve_completion_item`].
    ///
    /// The item is echoed back unchanged when nothing can be added, which is
    /// what the protocol expects — a resolve must never drop fields the client
    /// already has.
    pub async fn completion_resolve(&self, item: CompletionItem) -> CompletionItem {
        let (model, settings) = {
            let state = self.state.read().await;
            (Arc::clone(&state.model), Arc::clone(&state.settings))
        };
        model.resolve_completion_item(item, settings.active_auth_context())
    }

    pub async fn hover(&self, params: HoverParams) -> Option<Hover> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let (analysis, model, settings) = self.snapshot_for_uri(&uri).await?;

        let token = token_at(&analysis.text, &analysis.line_index, position)?;
        let range = word_range(&analysis.text, &analysis.line_index, position)?;
        // A column name resolves only against the table the statement reads, so
        // hover needs the same context completion does — but *not* the `tbl.`
        // qualifier. Completion needs it because the cursor sits after the dot
        // with nothing else to go on; hover can see the whole idiom, and
        // `address.street` would otherwise read `address` as a table and look
        // the column up on a table that does not exist. `field_hover` splits a
        // genuine `table.column` itself.
        let field_tables = field_tables_at(&analysis, position, None);

        let contents = model.hover_markdown_at(
            &analysis,
            position,
            token.trim_matches(|ch: char| {
                matches!(ch, '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';')
            }),
            settings.active_auth_context(),
            &field_tables,
        )?;

        Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: contents,
            }),
            range: Some(range),
        })
    }

    pub async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Option<DocumentSymbolResponse> {
        let uri = params.text_document.uri;
        let (analysis, _, _) = self.snapshot_for_uri(&uri).await?;
        Some(DocumentSymbolResponse::Nested(
            analysis.document_symbols.clone(),
        ))
    }

    /// Full-document semantic tokens. Re-parses the document and maps
    /// tree-sitter node kinds onto the standard LSP token legend (see
    /// [`crate::semantic::highlight`]).
    pub async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Option<SemanticTokensResult> {
        let uri = params.text_document.uri;
        let (analysis, _, _) = self.snapshot_for_uri(&uri).await?;
        let data = crate::semantic::highlight::collect_semantic_tokens(
            &analysis.tree,
            &analysis.text,
            &analysis.line_index,
        );
        let result_id = self.remember_semantic_tokens(&uri, &data).await;
        Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: Some(result_id),
            data,
        }))
    }

    /// Semantic tokens as edits against the set the client already has.
    ///
    /// A keystroke changes a few tokens in a long file; sending the whole set
    /// back was the difference between a small message and a large one on every
    /// edit. When the quoted result id is not the one we last handed out — a
    /// restart, a second client, a dropped response — the full set is returned,
    /// which the protocol allows and which is always correct.
    pub async fn semantic_tokens_full_delta(
        &self,
        params: SemanticTokensDeltaParams,
    ) -> Option<SemanticTokensFullDeltaResult> {
        let uri = params.text_document.uri;
        let (analysis, _, _) = self.snapshot_for_uri(&uri).await?;
        let data = crate::semantic::highlight::collect_semantic_tokens(
            &analysis.tree,
            &analysis.text,
            &analysis.line_index,
        );

        let previous = {
            let state = self.state.read().await;
            state
                .last_semantic_tokens
                .get(&uri)
                .filter(|(id, _)| *id == params.previous_result_id)
                .map(|(_, tokens)| tokens.clone())
        };
        let result_id = self.remember_semantic_tokens(&uri, &data).await;

        match previous {
            Some(previous) => Some(SemanticTokensFullDeltaResult::TokensDelta(
                SemanticTokensDelta {
                    result_id: Some(result_id),
                    edits: token_edits(&previous, &data),
                },
            )),
            None => Some(SemanticTokensFullDeltaResult::Tokens(SemanticTokens {
                result_id: Some(result_id),
                data,
            })),
        }
    }

    /// Store a token set and return the id the client should quote next time.
    ///
    /// The id is the document version where there is one, so it changes exactly
    /// when the tokens can have changed.
    async fn remember_semantic_tokens(
        &self,
        uri: &Uri,
        data: &[ls_types::SemanticToken],
    ) -> String {
        let mut state = self.state.write().await;
        let version = state.document_versions.get(uri).copied().unwrap_or(0);
        let id = format!("{version}-{}", data.len());
        state
            .last_semantic_tokens
            .insert(uri.clone(), (id.clone(), data.to_vec()));
        id
    }

    /// Semantic tokens for a viewport range — same mapping as
    /// [`Self::semantic_tokens_full`], restricted to nodes overlapping
    /// `params.range`.
    pub async fn semantic_tokens_range(
        &self,
        params: SemanticTokensRangeParams,
    ) -> Option<SemanticTokensRangeResult> {
        let uri = params.text_document.uri;
        let (analysis, _, _) = self.snapshot_for_uri(&uri).await?;
        let data = crate::semantic::highlight::collect_semantic_tokens_range(
            &analysis.tree,
            &analysis.text,
            &analysis.line_index,
            params.range,
        );
        Some(SemanticTokensRangeResult::Tokens(SemanticTokens {
            result_id: None,
            data,
        }))
    }

    pub async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Option<GotoDefinitionResponse> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let (analysis, model, _) = self.snapshot_for_uri(&uri).await?;
        let token = token_at(&analysis.text, &analysis.line_index, position)?;

        let token = token.trim().to_string();
        model
            .definition_for_token(&token)
            .map(GotoDefinitionResponse::Scalar)
    }

    pub async fn references(&self, params: ReferenceParams) -> Vec<Location> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let Some((analysis, model, _)) = self.snapshot_for_uri(&uri).await else {
            return Vec::new();
        };
        let Some(token) = token_at(&analysis.text, &analysis.line_index, position) else {
            return Vec::new();
        };
        let name = token.trim();
        let include_declaration = params.context.include_declaration;
        let workspace = self.merged_workspace_snapshot().await;
        model
            .references_for_symbol(name, &workspace)
            .into_iter()
            .filter(|reference| {
                // `includeDeclaration` was ignored, so a client that asked for
                // uses only still got the `DEFINE`.
                include_declaration || !model.is_declaration_of(name, reference)
            })
            .map(|reference| reference.location)
            .collect()
    }

    pub async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Option<PrepareRenameResponse> {
        let uri = params.text_document.uri;
        let position = params.position;
        let (analysis, model, _) = self.snapshot_for_uri(&uri).await?;
        let token = token_at(&analysis.text, &analysis.line_index, position)?;
        let name = token.trim();
        // Refuse early rather than accept the rename and then produce no edits.
        // A symbol defined in the live database cannot be renamed: the server
        // cannot edit a database definition, and doing half of it breaks the
        // schema.
        model.renameable_symbol(name)?;
        let range =
            crate::semantic::text::word_range(&analysis.text, &analysis.line_index, position)?;
        Some(PrepareRenameResponse::RangeWithPlaceholder {
            range,
            placeholder: name.to_string(),
        })
    }

    pub async fn rename(&self, params: RenameParams) -> Option<WorkspaceEdit> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let (analysis, model, _) = self.snapshot_for_uri(&uri).await?;
        let token = token_at(&analysis.text, &analysis.line_index, position)?;
        let workspace = self.merged_workspace_snapshot().await;
        let changes = model.rename_edits_for_symbol(token.trim(), &params.new_name, &workspace)?;
        // `documentChanges` rather than the legacy `changes` map: it carries the
        // document version, so a client can refuse an edit computed against
        // text the user has since changed.
        Some(WorkspaceEdit {
            document_changes: Some(DocumentChanges::Edits(
                changes
                    .into_iter()
                    .map(|(uri, edits)| TextDocumentEdit {
                        text_document: OptionalVersionedTextDocumentIdentifier {
                            uri,
                            version: None,
                        },
                        edits: edits.into_iter().map(OneOf::Left).collect(),
                    })
                    .collect(),
            )),
            ..WorkspaceEdit::default()
        })
    }

    /// Fold every statement, block and multi-line comment.
    ///
    /// A schema file is a long list of `DEFINE`s; without folding there is no
    /// way to collapse one table and look at the next.
    pub async fn folding_range(&self, params: FoldingRangeParams) -> Vec<FoldingRange> {
        let uri = params.text_document.uri;
        let Some((analysis, _, _)) = self.snapshot_for_uri(&uri).await else {
            return Vec::new();
        };
        let mut ranges = Vec::new();
        collect_folding_ranges(
            analysis.tree.root_node(),
            &analysis.text,
            &analysis.line_index,
            &mut ranges,
        );
        ranges
    }

    /// The chain of enclosing syntax nodes at each requested position.
    ///
    /// This is what "expand selection" binds to. Built straight from the tree,
    /// innermost first, which is the order the protocol wants.
    pub async fn selection_range(&self, params: SelectionRangeParams) -> Vec<SelectionRange> {
        let uri = params.text_document.uri;
        let Some((analysis, _, _)) = self.snapshot_for_uri(&uri).await else {
            return Vec::new();
        };
        params
            .positions
            .into_iter()
            .filter_map(|position| {
                let offset = analysis.line_index.offset(&analysis.text, position);
                let mut node = analysis
                    .tree
                    .root_node()
                    .named_descendant_for_byte_range(offset, offset)?;
                let mut chain = vec![node];
                while let Some(parent) = node.parent() {
                    chain.push(parent);
                    node = parent;
                }
                // Built outermost-in so each link can own the next.
                let mut built: Option<SelectionRange> = None;
                for node in chain.into_iter().rev() {
                    built = Some(SelectionRange {
                        range: analysis.line_index.range(
                            &analysis.text,
                            node.start_byte(),
                            node.end_byte(),
                        ),
                        parent: built.map(Box::new),
                    });
                }
                built
            })
            .collect()
    }

    /// The table a `record<T>` field points at.
    ///
    /// "Go to type definition" on a field is the graph-navigation move a schema
    /// author reaches for: from a link field to the table it links to.
    pub async fn goto_type_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Option<GotoDefinitionResponse> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let (analysis, model, _) = self.snapshot_for_uri(&uri).await?;
        let token = token_at(&analysis.text, &analysis.line_index, position)?;
        let name = token.trim();

        // The field's declared type, from whichever table declares a field with
        // this name.
        let field = model
            .fields
            .values()
            .flat_map(|table| table.values())
            .find(|field| field.name == name)?;
        let tables = field.type_expr.as_ref()?.record_tables();
        let locations: Vec<Location> = tables
            .into_iter()
            .filter_map(|table| model.tables.get(&table))
            .map(|table| table.location.clone())
            .collect();
        (!locations.is_empty()).then_some(GotoDefinitionResponse::Array(locations))
    }

    /// Link each `DEFINE INDEX … ANALYZER <name>` to the analyzer's definition.
    ///
    /// Nothing validates that the analyzer exists yet, but making the name
    /// clickable at least tells the reader whether it does.
    pub async fn document_link(&self, params: DocumentLinkParams) -> Vec<DocumentLink> {
        let uri = params.text_document.uri;
        let Some((analysis, model, _)) = self.snapshot_for_uri(&uri).await else {
            return Vec::new();
        };
        let mut links = Vec::new();
        for index in &analysis.indexes {
            let Some(name) = index.analyzer() else {
                continue;
            };
            let Some(definition) = model.analyzers.get(name) else {
                continue;
            };
            let Ok(target) = format!(
                "{}#L{}",
                definition.location.uri.as_str(),
                definition.location.range.start.line + 1
            )
            .parse() else {
                continue;
            };
            links.push(DocumentLink {
                range: index.location.range,
                target: Some(target),
                tooltip: Some(format!("Analyzer `{name}`")),
                data: None,
            });
        }
        links
    }

    /// Diagnostics for one document, on request.
    ///
    /// The same set `publishDiagnostics` pushes — the pipeline is shared, so a
    /// client cannot see one answer by pulling and a different one by
    /// listening.
    pub async fn document_diagnostic(
        &self,
        params: DocumentDiagnosticParams,
    ) -> DocumentDiagnosticReportResult {
        let uri = params.text_document.uri;
        let items = match self.snapshot_for_uri(&uri).await {
            Some((analysis, model, settings)) => {
                let mut diagnostics = diagnostics_for_document(&analysis, &model, &settings);
                self.strip_unsupported_fields(&mut diagnostics).await;
                diagnostics
            }
            None => Vec::new(),
        };
        DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(
            RelatedFullDocumentDiagnosticReport {
                related_documents: None,
                full_document_diagnostic_report: FullDocumentDiagnosticReport {
                    result_id: None,
                    items,
                },
            },
        ))
    }

    /// Diagnostics for every document in the workspace.
    ///
    /// This is the answer to the biggest gap in the push model: only open
    /// buffers were ever diagnosed, so a schema error in a file nobody had
    /// opened was invisible. A closed file's report carries no version, per the
    /// protocol.
    pub async fn workspace_diagnostic(
        &self,
        _params: WorkspaceDiagnosticParams,
    ) -> WorkspaceDiagnosticReportResult {
        let (documents, model, settings, versions) = {
            let state = self.state.read().await;
            let mut documents: Vec<(Uri, Arc<DocumentAnalysis>)> = state
                .saved_workspace
                .documents
                .iter()
                .map(|(uri, analysis)| (uri.clone(), Arc::clone(analysis)))
                .collect();
            // An open buffer wins: it is what the user is looking at.
            for (uri, analysis) in &state.open_documents {
                documents.retain(|(existing, _)| existing != uri);
                documents.push((uri.clone(), Arc::clone(analysis)));
            }
            (
                documents,
                Arc::clone(&state.model),
                Arc::clone(&state.settings),
                state.document_versions.clone(),
            )
        };

        let mut items = Vec::with_capacity(documents.len());
        for (uri, analysis) in documents {
            let mut diagnostics = diagnostics_for_document(&analysis, &model, &settings);
            self.strip_unsupported_fields(&mut diagnostics).await;
            items.push(WorkspaceDocumentDiagnosticReport::Full(
                WorkspaceFullDocumentDiagnosticReport {
                    uri: uri.clone(),
                    version: versions.get(&uri).map(|version| i64::from(*version)),
                    full_document_diagnostic_report: FullDocumentDiagnosticReport {
                        result_id: None,
                        items: diagnostics,
                    },
                },
            ));
        }
        WorkspaceDiagnosticReportResult::Report(WorkspaceDiagnosticReport { items })
    }

    /// Format the whole document.
    ///
    /// Returns nothing when the formatter refused the text — which it does for
    /// anything the grammar cannot parse. A no-op is the correct answer there:
    /// an editor that gets no edit leaves the buffer alone, which is exactly
    /// what should happen to text nobody can safely rewrite.
    pub async fn formatting(&self, params: DocumentFormattingParams) -> Option<Vec<TextEdit>> {
        let uri = params.text_document.uri;
        let (analysis, _, _) = self.snapshot_for_uri(&uri).await?;
        let formatted = crate::format::format(&analysis.text);
        if formatted == analysis.text {
            return Some(Vec::new());
        }
        Some(vec![TextEdit {
            range: analysis
                .line_index
                .range(&analysis.text, 0, analysis.text.len()),
            new_text: formatted,
        }])
    }

    /// Format the statements the requested range covers.
    ///
    /// The range is widened to whole statements first: this formatter works on
    /// a token stream, and half a statement is not something it can parse, let
    /// alone lay out. A range that covers no complete statement formats
    /// nothing.
    pub async fn range_formatting(
        &self,
        params: DocumentRangeFormattingParams,
    ) -> Option<Vec<TextEdit>> {
        let uri = params.text_document.uri;
        let (analysis, _, _) = self.snapshot_for_uri(&uri).await?;
        let start = analysis
            .line_index
            .offset(&analysis.text, params.range.start);
        let end = analysis.line_index.offset(&analysis.text, params.range.end);

        let mut covered: Option<(usize, usize)> = None;
        let root = analysis.tree.root_node();
        let mut cursor = root.walk();
        for statement in root.named_children(&mut cursor) {
            if statement.start_byte() >= start && statement.end_byte() <= end {
                let span = covered.get_or_insert((statement.start_byte(), statement.end_byte()));
                span.0 = span.0.min(statement.start_byte());
                span.1 = span.1.max(statement.end_byte());
            }
        }
        let (from, to) = covered?;

        let slice = &analysis.text[from..to];
        let formatted = crate::format::format(slice);
        if formatted.trim_end() == slice.trim_end() {
            return Some(Vec::new());
        }
        Some(vec![TextEdit {
            range: analysis.line_index.range(&analysis.text, from, to),
            new_text: formatted.trim_end().to_string(),
        }])
    }

    pub async fn signature_help(&self, params: SignatureHelpParams) -> Option<SignatureHelp> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let (analysis, model, _) = self.snapshot_for_uri(&uri).await?;
        let offset = analysis.line_index.offset(&analysis.text, position);
        let prefix = &analysis.text[..offset];
        let call = enclosing_call(prefix)?;
        let open_paren = call.open_paren;
        let function_name = callee_name_before(prefix, open_paren);
        let active_parameter = call.active_parameter;

        // A method: `'abc'.slice(` reads as one whitespace-delimited token, so the
        // tail after the last `.` is the method name. It resolves through the
        // receiver's table, and parameter zero is dropped because the receiver
        // already fills it.
        //
        // The receiver is found from the text rather than from an `IdiomFunction`
        // node, because on the `(` keystroke there is no such node yet: with an
        // empty argument list the grammar reads `.slice` as a *field access* and
        // leaves the `(` as an ERROR sibling. Signature help is most useful at
        // exactly that moment, so it cannot wait for the tree to agree.
        if let Some((_, method)) = function_name.rsplit_once('.')
            && !method.is_empty()
            && let Some(dot) = open_paren.checked_sub(method.len() + 1)
            && analysis.text.as_bytes().get(dot) == Some(&b'.')
        {
            let receiver = analysis
                .tree
                .root_node()
                .named_descendant_for_byte_range(dot.saturating_sub(1), dot);
            if let Some(receiver) = receiver {
                let bindings = crate::semantic::infer::resolve_bindings(&analysis, &model);
                let ctx = crate::semantic::infer::TypeCtx {
                    model: &model,
                    source: &analysis.text,
                    lines: &analysis.line_index,
                    bindings: &bindings,
                };
                let receiver_type = crate::semantic::infer::infer_expr_type(receiver, &ctx);
                if let Some(resolved) = crate::semantic::method::resolve(&receiver_type, method)
                    && let Some(signature) = crate::grammar::builtin_signature(resolved.function)
                {
                    let labels = signature.param_labels();
                    let written: Vec<String> = labels.iter().skip(1).cloned().collect();
                    return Some(SignatureHelp {
                        signatures: vec![SignatureInformation {
                            label: format!(".{method}({})", written.join(", ")),
                            documentation: crate::grammar::builtin_function(resolved.function).map(
                                |curated| {
                                    Documentation::MarkupContent(MarkupContent {
                                        kind: MarkupKind::Markdown,
                                        value: curated.summary.to_string(),
                                    })
                                },
                            ),
                            parameters: Some(
                                written
                                    .into_iter()
                                    .map(|label| ParameterInformation {
                                        label: ParameterLabel::Simple(label),
                                        documentation: None,
                                    })
                                    .collect(),
                            ),
                            active_parameter: Some(active_parameter),
                        }],
                        active_signature: Some(0),
                        active_parameter: Some(active_parameter),
                    });
                }
            }
        }

        if let Some(function) = model.functions.get(function_name) {
            return Some(SignatureHelp {
                signatures: vec![SignatureInformation {
                    label: function_signature_with_return(
                        function,
                        model.inferred_function_returns.get(function_name),
                    ),
                    documentation: function.comment.clone().map(|value| {
                        Documentation::MarkupContent(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value,
                        })
                    }),
                    parameters: Some(
                        function
                            .params
                            .iter()
                            .map(|param| ParameterInformation {
                                label: ParameterLabel::Simple(param_label(param)),
                                documentation: None,
                            })
                            .collect(),
                    ),
                    active_parameter: Some(active_parameter),
                }],
                active_signature: Some(0),
                active_parameter: Some(active_parameter),
            });
        }

        // The generated catalogue supplies parameters for every builtin; the
        // curated table adds prose for the 79 that have it. Before this, help
        // came only from the curated table, so `math::clamp(` offered nothing.
        let signature = builtin_signature(function_name)?;
        Some(SignatureHelp {
            signatures: vec![builtin_signature_information(
                signature,
                builtin_function(function_name),
            )],
            active_signature: Some(0),
            active_parameter: Some(active_parameter),
        })
    }

    pub async fn code_action(&self, params: CodeActionParams) -> Option<CodeActionResponse> {
        let uri = params.text_document.uri;
        let (analysis, model, settings) = self.snapshot_for_uri(&uri).await?;
        // The capability stays advertised either way. A client that has already
        // been told the server does code actions and then gets an error is
        // worse off than one that gets an empty list, and `tests/compat.rs`
        // pins the advertised set by exact equality.
        if !settings.analysis.enable_code_actions {
            return Some(Vec::new());
        }
        let mut actions = model.code_actions(&uri, &analysis, &params.context.diagnostics);
        Self::retain_actions_in_range(&mut actions, params.range);
        Self::retain_requested_kinds(&mut actions, params.context.only.as_deref());
        Some(actions)
    }

    pub async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Vec<DocumentHighlight> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let Some((analysis, model, _)) = self.snapshot_for_uri(&uri).await else {
            return Vec::new();
        };
        let Some(token) = token_at(&analysis.text, &analysis.line_index, position) else {
            return Vec::new();
        };
        let name = token.trim();
        let workspace = self.merged_workspace_snapshot().await;
        model
            .references_for_symbol(name, &workspace)
            .into_iter()
            .filter(|reference| reference.location.uri == uri)
            .map(|reference| DocumentHighlight {
                range: reference.location.range,
                // A definition is a write; everything else is a read. The
                // handler used to mark every occurrence READ, so an editor
                // could not tell where a symbol was introduced.
                kind: Some(if model.is_declaration_of(name, &reference) {
                    DocumentHighlightKind::WRITE
                } else {
                    DocumentHighlightKind::READ
                }),
            })
            .collect()
    }

    /// Emit `parameter_name:` hints next to each argument of every
    /// custom function call in the requested viewport range. Builtin
    /// functions don't expose structured parameter names (their
    /// signatures are free-form strings), so they're skipped.
    pub async fn inlay_hint(&self, params: InlayHintParams) -> Vec<InlayHint> {
        let uri = params.text_document.uri;
        let Some((analysis, model, _)) = self.snapshot_for_uri(&uri).await else {
            return Vec::new();
        };

        let range_start = analysis
            .line_index
            .offset(&analysis.text, params.range.start);
        let range_end = analysis.line_index.offset(&analysis.text, params.range.end);

        crate::semantic::analyzer::collect_inlay_hints(
            analysis.tree.root_node(),
            &analysis.text,
            range_start,
            range_end,
            &model,
        )
    }

    pub async fn prepare_call_hierarchy(
        &self,
        params: CallHierarchyPrepareParams,
    ) -> Option<Vec<CallHierarchyItem>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let (analysis, model, _) = self.snapshot_for_uri(&uri).await?;
        let token = token_at(&analysis.text, &analysis.line_index, position)?;
        let function = model.functions.get(token.trim())?;
        Some(vec![call_hierarchy_item(function)])
    }

    pub async fn incoming_calls(
        &self,
        params: CallHierarchyIncomingCallsParams,
    ) -> Vec<CallHierarchyIncomingCall> {
        let item = params.item;
        let state = self.state.read().await;
        let Some(target) = resolve_call_hierarchy_item(&state.model, &item) else {
            return Vec::new();
        };
        // Deduplicated: a caller that calls the target twice is still one
        // caller, with two call sites. Listing it twice made the tree show the
        // same function under itself.
        let mut callers = state
            .model
            .function_callers
            .get(&target.name)
            .cloned()
            .unwrap_or_default();
        callers.sort_unstable();
        callers.dedup();
        let mut calls = Vec::new();
        for caller_name in callers {
            if let Some(caller) = state.model.functions.get(&caller_name) {
                calls.push(CallHierarchyIncomingCall {
                    from: call_hierarchy_item(caller),
                    // Where the caller calls *this* function.
                    from_ranges: call_sites(&state.model, caller, &target.name),
                });
            }
        }
        calls
    }

    pub async fn outgoing_calls(
        &self,
        params: CallHierarchyOutgoingCallsParams,
    ) -> Vec<CallHierarchyOutgoingCall> {
        let item = params.item;
        let state = self.state.read().await;
        let Some(function) = resolve_call_hierarchy_item(&state.model, &item) else {
            return Vec::new();
        };
        // Same deduplication as the incoming direction, for the same reason.
        let mut callees = function.called_functions.clone();
        callees.sort_unstable();
        callees.dedup();
        let mut calls = Vec::new();
        for callee_name in &callees {
            if let Some(callee) = state.model.functions.get(callee_name) {
                calls.push(CallHierarchyOutgoingCall {
                    to: call_hierarchy_item(callee),
                    // Where this function calls the callee.
                    from_ranges: call_sites(&state.model, function, callee_name),
                });
            }
        }
        calls
    }

    pub async fn workspace_symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Option<WorkspaceSymbolResponse> {
        let state = self.state.read().await;
        Some(state.model.workspace_symbol_items(&params.query).into())
    }

    // ──────────────────────────────────────────────────────────────────
    // Internal helpers
    // ──────────────────────────────────────────────────────────────────

    async fn upsert_open_document(&self, uri: Uri, text: String, edit: Edit) {
        let (limit, debounce_ms) = {
            let state = self.state.read().await;
            (
                state.settings.analysis.max_syntax_diagnostics,
                state.settings.analysis.diagnostic_debounce_ms,
            )
        };

        // Record the version first, so a later edit can tell that this one is
        // superseded even while this call is still waiting or analysing.
        if let Edit::Changed(version) = edit {
            let mut state = self.state.write().await;
            if state
                .document_versions
                .get(&uri)
                .is_some_and(|newest| *newest > version)
            {
                // A newer edit already arrived. Its own call does the work.
                return;
            }
            state.document_versions.insert(uri.clone(), version);
        }

        // Let a burst of keystrokes settle. `didOpen` skips this entirely.
        if let Edit::Changed(version) = edit
            && debounce_ms > 0
        {
            runtime::time::sleep(std::time::Duration::from_millis(debounce_ms)).await;
            if self.superseded(&uri, version).await {
                return;
            }
        }

        // Parsing and extraction are CPU-bound and now the most frequent work
        // the server does, so they must not run on a thread that is also
        // serving requests.
        // The previous tree plus every edit since it was produced. Both come
        // out of one read so they cannot disagree.
        let reusable = {
            let state = self.state.read().await;
            match (
                state.open_documents.get(&uri),
                state.pending_edits.get(&uri),
            ) {
                (Some(previous), Some(edits)) if !edits.is_empty() => {
                    Some((previous.tree.clone(), edits.clone()))
                }
                _ => None,
            }
        };
        let replayed = reusable.as_ref().map(|(_, edits)| edits.len()).unwrap_or(0);
        let Some(analysis) = analyze_off_reactor(uri.clone(), text, limit, reusable).await else {
            return;
        };

        // The text may have moved on while the analysis ran.
        if let Edit::Changed(version) = edit
            && self.superseded(&uri, version).await
        {
            return;
        }

        {
            let mut state = self.state.write().await;
            state.open_documents.insert(uri.clone(), Arc::new(analysis));
            // Cleared in the same critical section that stores the tree those
            // edits were applied to. Dropping only what was replayed keeps an
            // edit that arrived while the analysis ran.
            if replayed > 0
                && let Some(pending) = state.pending_edits.get_mut(&uri)
            {
                pending.drain(..replayed.min(pending.len()));
            }
        }
        self.recompute_model().await;
        self.publish_diagnostics_for_uri(&uri).await;
    }

    /// The saved workspace with open buffers layered over it.
    ///
    /// Built per request rather than cached: the callers are all
    /// user-initiated, and a cache would have to be invalidated on every edit.
    async fn merged_workspace_snapshot(&self) -> WorkspaceIndex {
        let state = self.state.read().await;
        crate::core::state::merged_workspace(&state.saved_workspace, &state.open_documents)
    }

    /// The stored analysis for an open document.
    ///
    /// A seam for tests that need to compare the tree the incremental path
    /// produced against a fresh parse — nothing else can observe that
    /// difference, and a wrong `InputEdit` is otherwise silent.
    pub async fn open_document_analysis(&self, uri: &Uri) -> Option<Arc<DocumentAnalysis>> {
        let state = self.state.read().await;
        state.open_documents.get(uri).cloned()
    }

    /// True when a newer `didChange` for `uri` has arrived since `version`.
    async fn superseded(&self, uri: &Uri, version: i32) -> bool {
        self.state
            .read()
            .await
            .document_versions
            .get(uri)
            .is_some_and(|newest| *newest > version)
    }

    /// Re-run the analysis of every open document under a new syntax cap.
    ///
    /// The text is taken from the stored analysis rather than re-read from
    /// disk: an open buffer may be dirty, and its `DocumentAnalysis.text` is
    /// the exact content the client last sent.
    async fn reanalyze_open_documents(&self, limit: usize) {
        let open_documents = self.state.read().await.open_documents.clone();
        let reanalyzed: Vec<(Uri, Arc<DocumentAnalysis>)> = open_documents
            .iter()
            .filter_map(|(uri, analysis)| {
                analyze_document_with_limit(uri.clone(), &analysis.text, SymbolOrigin::Local, limit)
                    .map(|fresh| (uri.clone(), Arc::new(fresh)))
            })
            .collect();

        let mut state = self.state.write().await;
        for (uri, analysis) in reanalyzed {
            state.open_documents.insert(uri, analysis);
        }
    }

    async fn sync_saved_document_from_disk(&self, uri: &Uri) {
        let Some(text) = self.workspace_loader.read_document(uri).await else {
            return;
        };
        let Some(analysis) = analyze_document(uri.clone(), &text, SymbolOrigin::Local) else {
            return;
        };
        let mut state = self.state.write().await;
        let mut workspace = (*state.saved_workspace).clone();
        workspace.documents.insert(uri.clone(), Arc::new(analysis));
        state.saved_workspace = Arc::new(workspace);
    }

    async fn recompute_model(&self) {
        let (workspace, live_metadata) = {
            let state = self.state.read().await;
            (
                merged_workspace(&state.saved_workspace, &state.open_documents),
                Arc::clone(&state.live_metadata),
            )
        };

        let model = Arc::new(MergedSemanticModel::build(&workspace, &live_metadata));
        let mut state = self.state.write().await;
        state.model = model;
    }

    async fn refresh_remote_metadata_if_needed(&self) {
        // Under the config lock so a concurrent apply_settings can't
        // interleave its fetch/report/store with this one (which
        // would let a stale "clean" fetch overwrite a fresh failure
        // report, or vice versa).
        let _guard = self.config_lock.lock().await;
        let settings = {
            let state = self.state.read().await;
            Arc::clone(&state.settings)
        };
        self.notifier
            .begin_progress("surrealql/metadata", "Reading SurrealDB schema")
            .await;
        let live_metadata = Arc::new(self.metadata_provider.fetch(&settings).await);
        self.notifier.end_progress("surrealql/metadata", None).await;
        self.report_metadata_errors(&live_metadata).await;
        {
            let mut state = self.state.write().await;
            state.live_metadata = live_metadata;
        }
        self.recompute_model().await;
    }

    /// Forward live-metadata fetch failures (bad endpoint, auth
    /// failure, timeout, per-table query errors) to the IDE. Before
    /// this existed, `LiveMetadataSnapshot.errors` was collected and
    /// then dropped — a misconfigured connection looked identical to
    /// an empty schema.
    ///
    /// Each *distinct* failure set toasts once (`window/showMessage`)
    /// with full details in the log, and recovery back to a clean
    /// fetch logs an INFO line. The state lock is released before any
    /// notifier call — the wasm executor is single-threaded and a
    /// held lock across an await can deadlock it.
    async fn report_metadata_errors(&self, snapshot: &LiveMetadataSnapshot) {
        let mut signature = snapshot.errors.clone();
        signature.sort();

        let previous = {
            let mut state = self.state.write().await;
            state.last_metadata_errors.replace(signature.clone())
        };

        if previous.as_ref() == Some(&signature) {
            return;
        }

        if signature.is_empty() {
            if previous.is_some_and(|previous| !previous.is_empty()) {
                self.notifier
                    .log_message(
                        MessageType::INFO,
                        "SurrealQL: live schema metadata is available again".to_string(),
                    )
                    .await;
            }
            return;
        }

        for error in &snapshot.errors {
            self.notifier
                .log_message(MessageType::WARNING, format!("SurrealQL metadata: {error}"))
                .await;
        }

        let first = &snapshot.errors[0];
        let message = match snapshot.errors.len() {
            1 => format!("SurrealQL: live schema metadata unavailable: {first}"),
            more => format!(
                "SurrealQL: live schema metadata unavailable: {first} (+{} more — see the log)",
                more - 1
            ),
        };
        self.notifier
            .show_message(MessageType::WARNING, message)
            .await;
    }

    /// Summarize what a fresh workspace walk had to skip (unreadable
    /// entries, oversized files, the file-count cap) so silent
    /// truncation doesn't masquerade as full coverage.
    async fn report_scan_stats(&self, workspace: &WorkspaceIndex) {
        let stats = workspace.scan_stats;
        if stats == Default::default() {
            return;
        }

        let mut parts = Vec::new();
        if stats.walk_errors > 0 {
            parts.push(format!(
                "{} unreadable directory entries",
                stats.walk_errors
            ));
        }
        if stats.skipped_oversize > 0 {
            parts.push(format!("{} oversized files", stats.skipped_oversize));
        }
        if stats.skipped_unreadable > 0 {
            parts.push(format!("{} unreadable files", stats.skipped_unreadable));
        }
        if stats.file_cap_hit {
            parts.push("the workspace file limit was reached".to_string());
        }
        self.notifier
            .log_message(
                MessageType::WARNING,
                format!("SurrealQL: workspace scan skipped {}.", parts.join(", ")),
            )
            .await;

        if stats.file_cap_hit {
            self.notifier
                .show_message(
                    MessageType::WARNING,
                    "SurrealQL: the workspace contains more .surql files than the indexing \
                     limit; some files were not indexed."
                        .to_string(),
                )
                .await;
        }
    }

    /// Republish diagnostics for every open editor buffer after the
    /// merged model changes without a document edit (e.g. live
    /// metadata arriving from the host).
    async fn republish_open_diagnostics(&self) {
        let uris = {
            let state = self.state.read().await;
            state.open_documents.keys().cloned().collect::<Vec<_>>()
        };
        for uri in uris {
            self.publish_diagnostics_for_uri(&uri).await;
        }
    }

    async fn publish_diagnostics_for_uri(&self, uri: &Uri) {
        let (analysis, model, settings) = {
            let state = self.state.read().await;
            let analysis = state
                .open_documents
                .get(uri)
                .cloned()
                .or_else(|| state.saved_workspace.documents.get(uri).cloned());
            (
                analysis,
                Arc::clone(&state.model),
                Arc::clone(&state.settings),
            )
        };

        if let Some(analysis) = analysis {
            let mut diagnostics = diagnostics_for_document(&analysis, &model, &settings);
            self.strip_unsupported_fields(&mut diagnostics).await;
            self.notifier
                .publish_diagnostics(uri.clone(), diagnostics)
                .await;
        }
    }

    /// Drop diagnostic fields the client did not say it understands.
    ///
    /// `relatedInformation` is the one that matters: a client without it shows
    /// nothing for the extra locations, and the spec says not to send it.
    async fn strip_unsupported_fields(&self, diagnostics: &mut [Diagnostic]) {
        let supported = {
            let state = self.state.read().await;
            state.client_capabilities.related_information
        };
        if supported {
            return;
        }
        for diagnostic in diagnostics.iter_mut() {
            diagnostic.related_information = None;
        }
    }

    async fn snapshot_for_uri(
        &self,
        uri: &Uri,
    ) -> Option<(
        Arc<DocumentAnalysis>,
        Arc<MergedSemanticModel>,
        Arc<ServerSettings>,
    )> {
        let state = self.state.read().await;
        let analysis = state
            .open_documents
            .get(uri)
            .cloned()
            .or_else(|| state.saved_workspace.documents.get(uri).cloned())?;
        Some((
            analysis,
            Arc::clone(&state.model),
            Arc::clone(&state.settings),
        ))
    }
}

// ──────────────────────────────────────────────────────────────────────
// Free helpers
// ──────────────────────────────────────────────────────────────────────

/// The completion items for one closed-vocabulary statement-head slot.
///
/// Keyword order follows the engine's own parser arms rather than the
/// alphabet, because `INFO FOR ` reads better as `ROOT NAMESPACE NS …` than as
/// `DATABASE DB INDEX …`. The `sort_text` preserves that order and keeps every
/// The tables whose columns are in scope at `position`.
///
/// A `tbl.` qualifier names its own table. Otherwise it is the statement's
/// target: from the query fact when the document parses, and from the raw text
/// when it does not. That second half matters more than it looks — the query
/// facts describe the document *as it stands*, and a projection the author has
/// not filled in yet does not parse. `SELECT  FROM person` yields an `ERROR`
/// node and no fact at all, so the one position where the column list is most
/// wanted was the one position with no table to draw it from.
///
/// Shared by completion and hover so the two cannot disagree about which table
/// a bare column name belongs to.
fn field_tables_at(
    analysis: &DocumentAnalysis,
    position: Position,
    qualifier: Option<&str>,
) -> Vec<String> {
    let tables = field_completion_tables(active_query_fact(analysis, position), qualifier);
    if !tables.is_empty() || qualifier.is_some() {
        return tables;
    }
    statement_target_in_text(&analysis.text, &analysis.line_index, position)
        .into_iter()
        .collect()
}

/// The span of the `prefix_len` bytes immediately before the cursor.
///
/// That run is the partial name the author has typed, so it is what a chosen
/// completion replaces.
fn replaced_range(analysis: &DocumentAnalysis, position: Position, prefix_len: usize) -> Range {
    let cursor = analysis.line_index.offset(&analysis.text, position);
    let start = cursor.saturating_sub(prefix_len);
    analysis.line_index.range(&analysis.text, start, cursor)
}

/// Pin every item to replace exactly `range`.
///
/// Without a `textEdit` the client picks the range itself, from its own idea of
/// where the current word starts — which is only safe while the character
/// before the cursor is one every client agrees ends a word.
fn with_replace_range(items: Vec<CompletionItem>, range: Range) -> Vec<CompletionItem> {
    items
        .into_iter()
        .map(|mut item| {
            let new_text = item
                .insert_text
                .clone()
                .unwrap_or_else(|| item.label.clone());
            item.text_edit = Some(CompletionTextEdit::Edit(TextEdit { range, new_text }));
            // With an explicit range the client filters the range's text
            // against this rather than against a word it scanned for itself.
            item.filter_text = Some(item.label.clone());
            item
        })
        .collect()
}

/// Rank the head-slot keywords in the order the engine documents them, each
/// keyword above the table names, which sort under `0-{priority}-{name}` with
/// a priority of at least 1 (`crate::semantic::model`).
fn head_slot_items(
    slot: SlotYield,
    prefix: &str,
    model: &MergedSemanticModel,
    active_context: Option<&AuthContext>,
) -> Vec<CompletionItem> {
    // Analyzer names come from the model rather than from a keyword list.
    if slot == SlotYield::Analyzers {
        return model.analyzer_completion_items(prefix);
    }

    let (keywords, with_tables) = match slot {
        SlotYield::Keywords(list) => (list, false),
        SlotYield::KeywordsAndTables(list) => (list, true),
        SlotYield::Tables => (&[] as &[&str], true),
        SlotYield::Analyzers => unreachable!("handled above"),
        // The caller must not reach this arm: `Expression` means "keep the
        // list you already build".
        SlotYield::Expression => return Vec::new(),
    };

    let upper = prefix.to_ascii_uppercase();
    let mut items: Vec<CompletionItem> = keywords
        .iter()
        .enumerate()
        .filter(|(_, keyword)| upper.is_empty() || keyword.starts_with(&upper))
        .map(|(index, keyword)| CompletionItem {
            label: (*keyword).to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some("SurrealQL keyword".to_string()),
            insert_text: Some((*keyword).to_string()),
            sort_text: Some(format!("0-0-{index:02}-{keyword}")),
            ..CompletionItem::default()
        })
        .collect();

    if with_tables {
        items.extend(model.table_completion_items(prefix, active_context));
    }
    items
}

/// The folders this server treats as the workspace.
///
/// `workspaceFolders` first, then the deprecated `rootUri`, then the equally
/// deprecated `rootPath`. Only the first was read, and a client that sends just
/// a root — which several still do, and which is what the protocol says to fall
/// back to — got an empty list. Nothing was walked and no `surrealql.toml` was
/// ever found, which looked like both features being broken rather than the
/// folder list being empty.
pub fn resolve_workspace_folders_for_test(params: &InitializeParams) -> Vec<PathBuf> {
    resolve_workspace_folders(params)
}

fn resolve_workspace_folders(params: &InitializeParams) -> Vec<PathBuf> {
    let folders: Vec<PathBuf> = params
        .workspace_folders
        .as_ref()
        .map(|folders| {
            folders
                .iter()
                .filter_map(|folder| folder.uri.to_file_path().map(|path| path.into_owned()))
                .collect()
        })
        .unwrap_or_default();
    if !folders.is_empty() {
        return folders;
    }

    #[allow(deprecated)]
    if let Some(root) = params
        .root_uri
        .as_ref()
        .and_then(|uri| uri.to_file_path())
        .map(|path| path.into_owned())
    {
        return vec![root];
    }
    // `rootPath` predates `rootUri` and is a plain path rather than a URI. Both
    // are deprecated; reading them is what makes an older client work at all.
    #[allow(deprecated)]
    params
        .root_path
        .as_ref()
        .map(|path| vec![PathBuf::from(path)])
        .unwrap_or_default()
}

fn call_hierarchy_item(function: &FunctionDef) -> CallHierarchyItem {
    CallHierarchyItem {
        name: function.name.clone(),
        kind: SymbolKind::FUNCTION,
        tags: None,
        detail: function.comment.clone(),
        uri: function.location.uri.clone(),
        range: function.location.range,
        selection_range: function.selection_range,
        // Identity, so a follow-up request resolves the item the user actually
        // clicked. Resolving by name alone made two functions with the same
        // name in two files the same node.
        data: Some(serde_json::json!({
            "uri": function.location.uri.as_str(),
            "line": function.selection_range.start.line,
            "character": function.selection_range.start.character,
        })),
    }
}

/// The function an item refers to, by identity where the client returned our
/// `data`, and by name otherwise.
///
/// A client is allowed to drop `data`, and older clients do, so the name
/// lookup stays as a fallback rather than becoming an error.
fn resolve_call_hierarchy_item<'a>(
    model: &'a MergedSemanticModel,
    item: &CallHierarchyItem,
) -> Option<&'a FunctionDef> {
    if let Some(data) = &item.data
        && let Some(uri) = data.get("uri").and_then(|value| value.as_str())
        && let Some(line) = data.get("line").and_then(|value| value.as_u64())
    {
        let found = model.functions.values().find(|function| {
            function.location.uri.as_str() == uri
                && u64::from(function.selection_range.start.line) == line
        });
        if found.is_some() {
            return found;
        }
    }
    model.functions.get(&item.name)
}

/// Where `caller` calls `callee`, as ranges inside the caller's body.
///
/// This is what `fromRanges` means in both directions: the call sites, not the
/// callee's definition and not the caller's own name. Both handlers used to
/// send the caller's `selection_range`, so an editor highlighted the wrong
/// span — the function's own name rather than the call.
fn call_sites(model: &MergedSemanticModel, caller: &FunctionDef, callee: &str) -> Vec<Range> {
    let Some(body) = caller.body_range else {
        return Vec::new();
    };
    let ranges: Vec<Range> = model
        .function_references
        .get(callee)
        .map(|locations| {
            locations
                .iter()
                .filter(|location| {
                    location.uri == caller.location.uri && within(body, location.range)
                })
                .map(|location| location.range)
                .collect()
        })
        .unwrap_or_default();
    // A caller with a known body but no recorded site still has to answer
    // something the client can jump to, or the entry becomes unclickable.
    if ranges.is_empty() {
        vec![caller.selection_range]
    } else {
        ranges
    }
}

fn within(outer: Range, inner: Range) -> bool {
    (inner.start.line, inner.start.character) >= (outer.start.line, outer.start.character)
        && (inner.end.line, inner.end.character) <= (outer.end.line, outer.end.character)
}

/// Signature help for a builtin.
///
/// Parameters come from the generated catalogue, so every function has them.
/// They used to be recovered by splitting the curated signature string on `(`
/// and `,`, which broke on a parameter type containing a comma
/// (`array<string, 5>`) and covered only the 79 curated entries.
///
/// `curated` supplies the summary and documentation link when the function is
/// one of those 79.
fn builtin_signature_information(
    signature: &BuiltinSignature,
    curated: Option<&'static BuiltinFunction>,
) -> SignatureInformation {
    let parameters = signature
        .param_labels()
        .into_iter()
        .map(|label| ParameterInformation {
            label: ParameterLabel::Simple(label),
            documentation: None,
        })
        .collect();

    SignatureInformation {
        // Prefer the curated signature string: it is written for a reader, with
        // parameter names chosen for the documentation rather than taken from
        // the implementation's bindings. The generated fallback now carries a
        // return type too, read from the engine's registry.
        label: curated
            .map(|function| function.signature.to_string())
            .or_else(|| signature.display_signature())
            .unwrap_or_else(|| signature.generated.name.to_string()),
        documentation: curated.map(|function| {
            Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: format!(
                    "{}\n\n[Docs]({})",
                    function.summary, function.documentation_url
                ),
            })
        }),
        parameters: Some(parameters),
        active_parameter: None,
    }
}

/// Why a buffer is being (re)analysed.
///
/// `didOpen` and `didChange` differ in two ways that both matter here: an open
/// is never delayed, and it cannot be superseded because there is no earlier
/// version of the same document in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edit {
    Opened,
    Changed(i32),
}

/// Run [`analyze_document_with_limit`] without occupying a thread that serves
/// requests.
///
/// On native this hands the work to tokio's blocking pool, so a hover or
/// completion arriving mid-keystroke is not queued behind a reparse. The
/// workspace walk already did this (see
/// [`crate::native::workspace_fs::FilesystemWorkspaceLoader::load`]); the
/// per-edit path did not, and it is the far more frequent one.
///
/// On `wasm32` it runs inline. `tokio_with_wasm` would move it to a web worker,
/// which means shipping the module to that worker and a serialisation hop for
/// every edit — a change to how the browser build works that nothing here can
/// test, since CI does not exercise the wasm JS surface. Inline keeps the
/// browser behaviour exactly as it was.
#[cfg(not(target_arch = "wasm32"))]
async fn analyze_off_reactor(
    uri: Uri,
    text: String,
    limit: usize,
    reusable: Option<(tree_sitter::Tree, Vec<tree_sitter::InputEdit>)>,
) -> Option<DocumentAnalysis> {
    runtime::task::spawn_blocking(move || match reusable {
        Some((mut tree, edits)) => {
            for edit in &edits {
                tree.edit(edit);
            }
            crate::semantic::analyzer::analyze_document_reusing(
                uri,
                text,
                SymbolOrigin::Local,
                limit,
                Some(&tree),
            )
        }
        None => analyze_document_with_limit(uri, text, SymbolOrigin::Local, limit),
    })
    .await
    .ok()
    .flatten()
}

#[cfg(target_arch = "wasm32")]
async fn analyze_off_reactor(
    uri: Uri,
    text: String,
    limit: usize,
    reusable: Option<(tree_sitter::Tree, Vec<tree_sitter::InputEdit>)>,
) -> Option<DocumentAnalysis> {
    match reusable {
        Some((mut tree, edits)) => {
            for edit in &edits {
                tree.edit(edit);
            }
            crate::semantic::analyzer::analyze_document_reusing(
                uri,
                text,
                SymbolOrigin::Local,
                limit,
                Some(&tree),
            )
        }
        None => analyze_document_with_limit(uri, text, SymbolOrigin::Local, limit),
    }
}

/// Extension methods used by [`LanguageServerCore::reload_from_client_configuration`]
/// to merge incoming `workspace/configuration` snapshots with the
/// initial settings (which usually carry the connection details from
/// `initializationOptions`).
trait SettingsMergeExt {
    fn merge_with_env_if_missing(self, fallback: ServerSettings) -> ServerSettings;
}

impl SettingsMergeExt for ServerSettings {
    fn merge_with_env_if_missing(mut self, fallback: ServerSettings) -> ServerSettings {
        if self.connection.endpoint.is_none() {
            self.connection.endpoint = fallback.connection.endpoint;
        }
        if self.connection.namespace.is_none() {
            self.connection.namespace = fallback.connection.namespace;
        }
        if self.connection.database.is_none() {
            self.connection.database = fallback.connection.database;
        }
        if self.connection.username.is_none() {
            self.connection.username = fallback.connection.username;
        }
        if self.connection.password.is_none() {
            self.connection.password = fallback.connection.password;
        }
        if self.connection.token.is_none() {
            self.connection.token = fallback.connection.token;
        }
        if self.active_auth_context.is_none() {
            self.active_auth_context = fallback.active_auth_context;
        }
        if self.auth_contexts.is_empty() {
            self.auth_contexts = fallback.auth_contexts;
        }
        let default_mode = crate::config::MetadataSettings::default().mode;
        if self.metadata.mode == default_mode && fallback.metadata.mode != default_mode {
            self.metadata.mode = fallback.metadata.mode;
        }
        self
    }
}

/// The name immediately before an opening parenthesis.
///
/// Scans backwards over the characters a SurrealQL callable name can hold:
/// letters, digits, `_`, the `::` namespace separator and the `.` of a method
/// call. Splitting on whitespace instead — which is what this used to do —
/// picked up the enclosing call's text on a nested call, so
/// `string::concat(string::len(` looked up a function named
/// `string::concat(string::len`.
fn callee_name_before(prefix: &str, open_paren: usize) -> &str {
    let head = prefix[..open_paren].trim_end();
    let start = head
        .char_indices()
        .rev()
        .find(|(_, character)| {
            !(character.is_alphanumeric() || matches!(character, '_' | ':' | '.'))
        })
        .map(|(index, character)| index + character.len_utf8())
        .unwrap_or(0);
    &head[start..]
}

/// The innermost unclosed call before the cursor, and which argument the
/// cursor is in.
struct EnclosingCall {
    /// Byte index of the `(` that opens the call.
    open_paren: usize,
    /// Zero-based index of the argument the cursor sits in.
    active_parameter: u32,
}

/// Find the call the cursor is inside by scanning the text, tracking bracket
/// depth and string state.
///
/// Not a tree walk, deliberately. Signature help is most useful on the `(`
/// keystroke, and at that moment there is no call node: with an empty argument
/// list the grammar reads `.slice` as a field access and leaves the `(` as an
/// `ERROR` sibling (see `docs/grammar-gaps.md`). So the scan has to work on
/// text the parser cannot yet make sense of.
///
/// What it does not do any more is confuse itself. The previous version took
/// the last `(` before the cursor and counted every comma after it, so a nested
/// call reported the outer function with the inner call's argument index, and a
/// parenthesis or comma inside a string literal moved the cursor to a parameter
/// that does not exist.
fn enclosing_call(prefix: &str) -> Option<EnclosingCall> {
    // (open_paren_index, commas_seen_at_this_depth)
    let mut calls: Vec<(usize, u32)> = Vec::new();
    let mut depth_without_call: usize = 0;
    let mut quote: Option<char> = None;
    let mut escaped = false;

    for (index, character) in prefix.char_indices() {
        if let Some(open) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == open {
                quote = None;
            }
            continue;
        }
        match character {
            '\'' | '"' | '`' => quote = Some(character),
            '(' => calls.push((index, 0)),
            ')' => {
                calls.pop();
            }
            '[' | '{' => depth_without_call += 1,
            ']' | '}' => depth_without_call = depth_without_call.saturating_sub(1),
            // A comma only advances the parameter when it belongs to the call
            // itself, not to an array or object literal written inside it.
            ',' if depth_without_call == 0 => {
                if let Some(frame) = calls.last_mut() {
                    frame.1 += 1;
                }
            }
            _ => {}
        }
    }

    calls.last().map(|(open_paren, commas)| EnclosingCall {
        open_paren: *open_paren,
        active_parameter: *commas,
    })
}

/// Every foldable region: a statement, a block, or a multi-line comment.
///
/// A region that starts and ends on the same line is skipped — the protocol
/// wants regions worth collapsing, and a one-line fold is only clutter.
fn collect_folding_ranges(
    node: tree_sitter::Node<'_>,
    source: &str,
    lines: &crate::semantic::text::LineIndex,
    out: &mut Vec<FoldingRange>,
) {
    let kind = node.kind();
    let foldable = kind.ends_with("Statement")
        || kind == crate::semantic::node_kind::BLOCK
        || kind == crate::semantic::node_kind::BLOCK_COMMENT;
    if foldable {
        let range = lines.range(source, node.start_byte(), node.end_byte());
        if range.end.line > range.start.line {
            out.push(FoldingRange {
                start_line: range.start.line,
                end_line: range.end.line,
                kind: Some(if kind == crate::semantic::node_kind::BLOCK_COMMENT {
                    FoldingRangeKind::Comment
                } else {
                    FoldingRangeKind::Region
                }),
                ..FoldingRange::default()
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_folding_ranges(child, source, lines, out);
    }
}

/// Apply one `didChange` content change to the buffer.
///
/// A change with no range is the whole document. A change with one replaces
/// that range — and the range refers to the text *as it is now*, so the index
/// is rebuilt per change rather than once per notification. A notification
/// rarely carries more than a handful, and getting this wrong corrupts the
/// document rather than merely slowing it down.
fn apply_content_change(
    text: &mut String,
    change: &ls_types::TextDocumentContentChangeEvent,
) -> Option<tree_sitter::InputEdit> {
    let Some(range) = change.range else {
        text.clear();
        text.push_str(&change.text);
        // A whole-document replacement is not an edit tree-sitter can reuse a
        // tree across.
        return None;
    };

    let index = crate::semantic::text::LineIndex::new(text);
    let start_byte = index.offset(text, range.start);
    let old_end_byte = index.offset(text, range.end).max(start_byte);
    // The two "old" points have to be read from the text before the splice.
    let before = text.clone();
    text.replace_range(start_byte..old_end_byte, &change.text);
    let new_end_byte = start_byte + change.text.len();

    // tree-sitter wants byte offsets *and* points, and its points are
    // (row, byte column) — not the UTF-16 columns the protocol uses.
    Some(tree_sitter::InputEdit {
        start_byte,
        old_end_byte,
        new_end_byte,
        start_position: byte_point(&before, start_byte),
        old_end_position: byte_point(&before, old_end_byte),
        new_end_position: byte_point(text, new_end_byte),
    })
}

/// A byte offset as a tree-sitter point: the row, and the *byte* offset within
/// that row.
///
/// Not `LineIndex::position`, which answers in UTF-16 code units — that is what
/// the protocol wants and what tree-sitter does not. Written as a direct scan
/// rather than an index because it runs a handful of times per edit, against a
/// parse that costs orders of magnitude more.
fn byte_point(text: &str, offset: usize) -> tree_sitter::Point {
    let offset = offset.min(text.len());
    let before = &text[..offset];
    tree_sitter::Point {
        row: before.matches('\n').count(),
        column: offset - before.rfind('\n').map(|index| index + 1).unwrap_or(0),
    }
}

/// One replacement covering everything between the common prefix and the common
/// suffix of two token sets.
///
/// Not a minimal edit script. A keystroke changes a run of tokens in one place,
/// so trimming both ends captures nearly all of the saving for none of the
/// complexity of a real diff — and a wrong edit script corrupts highlighting in
/// a way that only looks like a rendering bug.
fn token_edits(
    previous: &[ls_types::SemanticToken],
    current: &[ls_types::SemanticToken],
) -> Vec<ls_types::SemanticTokensEdit> {
    let prefix = previous
        .iter()
        .zip(current.iter())
        .take_while(|(left, right)| left == right)
        .count();
    let remaining = previous.len().min(current.len()) - prefix;
    let suffix = previous
        .iter()
        .rev()
        .zip(current.iter().rev())
        .take(remaining)
        .take_while(|(left, right)| left == right)
        .count();

    if prefix == previous.len() && previous.len() == current.len() {
        return Vec::new();
    }

    // Tokens are five `u32`s each on the wire, so the protocol counts in
    // integers rather than tokens.
    let start = (prefix * 5) as u32;
    let delete_count = ((previous.len() - prefix - suffix) * 5) as u32;
    let data: Vec<ls_types::SemanticToken> = current[prefix..current.len() - suffix].to_vec();
    vec![ls_types::SemanticTokensEdit {
        start,
        delete_count,
        data: Some(data),
    }]
}
