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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tree_sitter::Tree;

use ls_types::*;

use crate::config::{AuthContext, ServerSettings, merge_absent};
use crate::core::client::{LspNotifier, MetadataProvider, WorkspaceLoader};
use crate::core::completion_context::{
    ColumnSlot, active_query_fact, column_completion_context, completion_prefix,
    completion_table_qualifier, graph_anchors, graph_edge_context, head_slot_at,
    is_table_name_context, statement_target_in_text,
};
use crate::core::state::{OpenBuffer, ServerState, merged_workspace, workspace_signature};
use crate::core::statement_shape::SlotYield;
use crate::grammar::{BuiltinFunction, BuiltinSignature, builtin_function, builtin_signature};
use crate::runtime;
use crate::semantic::analyzer::{
    analyze_document, analyze_document_incremental, analyze_document_with_limit,
};
use crate::semantic::model::{
    field_completion_tables, function_signature_with_return, is_record_type_context, param_label,
};
use crate::semantic::text::{enclosing_call, token_at, word_range};
use crate::semantic::types::{
    DocumentAnalysis, FunctionDef, LiveMetadataSnapshot, MergedSemanticModel, QueryAction,
    SymbolOrigin, WorkspaceIndex,
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
    /// What the client last sent for each open document, before any analysis.
    ///
    /// Behind a **`std::sync::Mutex`**, not the async `RwLock` that holds
    /// everything else, and that is the load-bearing detail. `lock()` has no
    /// await point, so a handler that only touches this map runs from its first
    /// poll to completion without yielding, which is what makes the order two
    /// edits are applied in the order they arrived, rather than whatever order
    /// the executor gets round to polling them.
    ///
    /// Poisoning is unreachable: `panic = 'abort'` means a panic never unwinds
    /// past the guard. The one hazard is holding it across an await, so every
    /// critical section is a block that returns owned data.
    buffers: Arc<std::sync::Mutex<HashMap<Uri, OpenBuffer>>>,
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
            buffers: Arc::new(std::sync::Mutex::new(HashMap::new())),
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
            // Incremental since 0.7. A 166 KB document used to cross the wire,
            // and get JSON-unescaped into a fresh `String` on the reactor: on
            // *every keystroke*: at ten characters a second that is 1.6 MB/s of
            // decoding before the debounce even sees the message, and in the
            // browser a full JS-to-wasm string copy each time. No benchmark here
            // measures that, because it is paid before any code this repository
            // owns runs.
            //
            // Safe only because the edit is applied on the ordered path: see
            // `apply_document_change`. A client that ignores this and keeps
            // sending whole documents still works: that is the `range: None`
            // branch.
            text_document_sync: Some(TextDocumentSyncCapability::Kind(
                TextDocumentSyncKind::INCREMENTAL,
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
            // Declaring the kinds is what lets a client ask for a subset:
            // VS Code's Quick Fix menu requests `quickfix`, and a
            // "fix all on save" request asks for `source.fixAll`. Advertising a
            // bare `true` meant every request got every action back, including
            // refactors in a quick-fix menu. The handler honours
            // `context.only`; this tells the client it is worth sending.
            code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
                code_action_kinds: Some(vec![
                    CodeActionKind::QUICKFIX,
                    CodeActionKind::REFACTOR_REWRITE,
                ]),
                work_done_progress_options: Default::default(),
                resolve_provider: None,
            })),
            document_highlight_provider: Some(OneOf::Left(true)),
            folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
            selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
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
                    full: Some(SemanticTokensFullOptions::Bool(true)),
                    range: Some(true),
                    work_done_progress_options: Default::default(),
                }),
            ),
            document_symbol_provider: Some(OneOf::Left(true)),
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

    /// Stash the client-supplied initialization options + workspace
    /// folders. The heavy work (workspace walk, live metadata fetch) is
    /// deferred to [`Self::apply_settings`], usually triggered by
    /// `initialized → reload_from_client_configuration`.
    pub async fn initialize(&self, params: InitializeParams) -> InitializeResult {
        let (settings, warnings) = ServerSettings::from_sources_with_warnings(
            params.initialization_options.as_ref(),
            None,
        );
        let workspace_folders = resolve_workspace_folders(&params);

        {
            let mut state = self.state.write().await;
            state.settings = Arc::new(settings);
            state.workspace_folders = workspace_folders;
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
        let configuration = self.notifier.request_configuration().await;
        let (settings, warnings, present) =
            ServerSettings::from_sources_with_presence(None, configuration.as_ref());
        // A client without configuration support answers `None`, and
        // VS Code / Neovim answer the pull with JSON `null` when no
        // `surrealql` section is configured — both always yield zero
        // warnings. Reporting those would reset the dedup signature
        // and fire a spurious "resolved" line right after the
        // initialize-stashed warnings. (A *real* clean section does
        // report: it replaces the previous settings, so its empty
        // warning set genuinely resolves them.)
        if configuration.as_ref().is_some_and(|value| !value.is_null()) {
            self.report_settings_warnings(&warnings).await;
        }

        let _guard = self.config_lock.lock().await;
        let current_settings = {
            let state = self.state.read().await;
            (*state.settings).clone()
        };
        let settings = merge_absent(settings, &current_settings, &present);
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

        let live_metadata = Arc::new(self.metadata_provider.fetch(&settings).await);
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
                    model_for_diag.document_diagnostics(&analysis, &settings_for_diag);
                self.notifier.publish_diagnostics(uri, diagnostics).await;
            }
        }
    }

    // ──────────────────────────────────────────────────────────────────
    // Document lifecycle
    // ──────────────────────────────────────────────────────────────────

    pub async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let document = params.text_document;
        self.upsert_open_document(document.uri, document.text, Edit::Opened(document.version))
            .await;
    }

    /// Apply a `didChange` to the authoritative buffer, returning the edit to
    /// analyse.
    ///
    /// Split out so the native adapter can apply on the ordered path and spawn
    /// only the analysis. See [`Self::apply_document_change`] for why that
    /// matters.
    pub fn apply_did_change(&self, params: &DidChangeTextDocumentParams) -> Option<Edit> {
        if params.content_changes.is_empty() {
            return None;
        }
        let edit = Edit::Changed(params.text_document.version);
        self.apply_document_change(&params.text_document.uri, &params.content_changes, edit)
            .map(|_| edit)
    }

    /// Apply and analyse, for callers with no reason to separate them.
    ///
    /// The native adapter does separate them (see
    /// [`Self::apply_did_change`]), so that the apply stays on the ordered
    /// path. This is the wasm dispatcher's entry point, where ordering comes
    /// free from `handleMessage` processing one message at a time.
    ///
    /// Note it no longer takes only the *last* change. Under incremental sync a
    /// notification carries a batch, and every one of them has to be applied, in
    /// order, against the text the previous one produced.
    pub async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let Some(edit) = self.apply_did_change(&params) else {
            return;
        };
        self.analyze_buffer(params.text_document.uri, edit).await;
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
        let uri = params.text_document.uri;
        {
            let mut state = self.state.write().await;
            state.open_documents.remove(&uri);
        }
        // The buffer goes with the document, version high-water mark and all. A
        // client that reopens a file starts counting from 1 again (VS Code
        // does), and a remembered 57 would make every later edit look stale:
        // diagnostics frozen at whatever the file looked like when it opened.
        self.buffers
            .lock()
            .expect("panic = 'abort' makes poisoning unreachable")
            .remove(&uri);
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
        let (settings, warnings, present) =
            ServerSettings::from_sources_with_presence(None, Some(&params.settings));
        self.report_settings_warnings(&warnings).await;

        let _guard = self.config_lock.lock().await;
        let current_settings = {
            let state = self.state.read().await;
            (*state.settings).clone()
        };
        let settings = merge_absent(settings, &current_settings, &present);
        self.apply_settings_inner(settings).await;
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
        let workspace = Arc::new(self.workspace_loader.load(&folders).await);
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
        Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data,
        }))
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
        model.references_for_function(token.trim())
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
        let location = model.definition_for_function(name)?;
        Some(PrepareRenameResponse::RangeWithPlaceholder {
            range: location.range,
            placeholder: name.to_string(),
        })
    }

    pub async fn rename(&self, params: RenameParams) -> Option<WorkspaceEdit> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let (analysis, model, _) = self.snapshot_for_uri(&uri).await?;
        let token = token_at(&analysis.text, &analysis.line_index, position)?;
        let changes = model.rename_edits(token.trim(), &params.new_name)?;
        Some(WorkspaceEdit {
            changes: Some(changes),
            ..WorkspaceEdit::default()
        })
    }

    pub async fn signature_help(&self, params: SignatureHelpParams) -> Option<SignatureHelp> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let (analysis, model, _) = self.snapshot_for_uri(&uri).await?;
        let offset = analysis.line_index.offset(&analysis.text, position);
        let prefix = &analysis.text[..offset];
        // Depth- and string-aware: `math::max([1, 2], fn::f(a, b` is the second
        // argument of `fn::f`, not the fifth of `math::max`, and a comma inside
        // a string literal is not an argument separator. See `enclosing_call`.
        let (open_paren, active_parameter) = enclosing_call(prefix)?;
        let function_name = prefix[..open_paren]
            .split_whitespace()
            .last()
            .map(str::trim)
            .unwrap_or_default();

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

        // A call through a variable: `$double(`. The binding's type carries the
        // closure's parameters and result, so it answers as a `DEFINE FUNCTION`
        // does. The binding is the one in scope at the `(`, which is where the
        // call sits.
        if function_name.starts_with('$') {
            let bindings = crate::semantic::infer::resolve_bindings(&analysis, &model);
            if let Some(binding) = bindings.resolve(function_name, open_paren)
                && let crate::semantic::type_expr::TypeExpr::Function { params, returns } =
                    &binding.ty
            {
                let labels: Vec<String> = params
                    .iter()
                    .map(|(name, ty)| match ty {
                        Some(ty) => format!("{name}: {ty}"),
                        None => name.clone(),
                    })
                    .collect();
                let mut label = format!("{function_name}({})", labels.join(", "));
                if **returns != crate::semantic::type_expr::TypeExpr::Unknown {
                    label.push_str(&format!(" -> {returns}"));
                }
                return Some(SignatureHelp {
                    signatures: vec![SignatureInformation {
                        label,
                        documentation: None,
                        parameters: Some(
                            labels
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

        // `analysis.enableCodeActions` parsed, validated and did nothing for
        // three releases. A settings surface that lies is worse than a smaller
        // one, so it is read here.
        if !settings.analysis.enable_code_actions {
            return Some(Vec::new());
        }

        Some(model.code_actions(
            &uri,
            &analysis,
            &params.context.diagnostics,
            params.range,
            params.context.only.as_deref(),
        ))
    }

    /// Foldable regions: statements, blocks, object and array literals, and
    /// runs of comments.
    ///
    /// Reads the cached tree, so it costs one walk and no re-parse, and it
    /// answers for a document the analyzer declined too: folding is a fact
    /// about the shape of the text, and a file that will not analyse is exactly
    /// when someone is folding their way through it.
    pub async fn folding_range(&self, params: FoldingRangeParams) -> Option<Vec<FoldingRange>> {
        let uri = params.text_document.uri;
        let (analysis, _, _) = self.snapshot_for_uri(&uri).await?;
        Some(crate::semantic::folding::folding_ranges(&analysis.tree))
    }

    /// The expand-selection chain at each requested position.
    pub async fn selection_range(
        &self,
        params: SelectionRangeParams,
    ) -> Option<Vec<SelectionRange>> {
        let uri = params.text_document.uri;
        let (analysis, _, _) = self.snapshot_for_uri(&uri).await?;
        Some(
            params
                .positions
                .into_iter()
                .filter_map(|position| {
                    crate::semantic::folding::selection_range(
                        &analysis.tree,
                        &analysis.text,
                        &analysis.line_index,
                        position,
                    )
                })
                .collect(),
        )
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
        let token = token.trim();

        // A custom function. Every occurrence is a call, so every one reads.
        let mut highlights: Vec<DocumentHighlight> = model
            .references_for_function(token)
            .into_iter()
            .filter(|location| location.uri == uri)
            .map(|location| DocumentHighlight {
                range: location.range,
                kind: Some(DocumentHighlightKind::READ),
            })
            .collect();

        // A table or a field, which is what people actually put the cursor on.
        // This used to return nothing for either, and marked everything it did
        // return as READ, so an editor could not tell a `SELECT` from the
        // `DELETE` three lines below it.
        //
        // `QueryFact` already carries token-tight ranges per name and the action
        // that produced them, which is exactly the read/write distinction the
        // protocol wants.
        for fact in &analysis.query_facts {
            let kind = highlight_kind(fact.action);
            let named = fact.target_refs.iter().chain(fact.field_refs.iter());
            highlights.extend(
                named
                    .filter(|reference| reference.name == token)
                    .map(|reference| DocumentHighlight {
                        range: reference.range,
                        kind: Some(kind),
                    }),
            );
        }

        // The declaration itself is a write: it is where the name is introduced.
        let declarations = analysis
            .tables
            .iter()
            .filter(|table| table.explicit && table.name == token)
            .map(|table| table.location.range)
            .chain(
                analysis
                    .fields
                    .iter()
                    .filter(|field| field.name == token)
                    .map(|field| field.location.range),
            );
        highlights.extend(declarations.map(|range| DocumentHighlight {
            range,
            kind: Some(DocumentHighlightKind::WRITE),
        }));

        // Two facts can name the same token in the same place: a field read and
        // written by one statement, say. Keep the first, which is the stronger
        // claim in source order.
        highlights.sort_by_key(|highlight| {
            (
                highlight.range.start.line,
                highlight.range.start.character,
                highlight.range.end.line,
                highlight.range.end.character,
            )
        });
        highlights.dedup_by_key(|highlight| highlight.range);
        highlights
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
        let callers = state
            .model
            .function_callers
            .get(&item.name)
            .cloned()
            .unwrap_or_default();
        let mut calls = Vec::new();
        for caller_name in callers {
            if let Some(function) = state.model.functions.get(&caller_name) {
                calls.push(CallHierarchyIncomingCall {
                    from: call_hierarchy_item(function),
                    from_ranges: vec![function.selection_range],
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
        let Some(function) = state.model.functions.get(&item.name) else {
            return Vec::new();
        };
        let mut calls = Vec::new();
        for callee_name in &function.called_functions {
            if let Some(callee) = state.model.functions.get(callee_name) {
                calls.push(CallHierarchyOutgoingCall {
                    to: call_hierarchy_item(callee),
                    from_ranges: vec![function.selection_range],
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

    /// Apply what the client sent to the authoritative buffer.
    ///
    /// **Contains no `.await`, and must not gain one.** Handler futures are
    /// first-polled in arrival order, so a handler that completes inside its
    /// first poll applies edits in the order they arrived, which is the whole
    /// ordering guarantee. Add an await here and two edits in flight can be
    /// applied out of order, which is harmless under full-document sync and
    /// corrupting under incremental sync.
    ///
    /// Returns the version applied, or `None` when the change was dropped as
    /// stale or refused.
    ///
    /// The escape hatch, if a desync is ever observed in the field:
    /// `Server::new(…).concurrency_level(1)` in `main.rs` makes the ordering
    /// unconditional, at the cost of serialising requests behind each other and
    /// disabling `$/cancelRequest`.
    pub fn apply_document_change(
        &self,
        uri: &Uri,
        changes: &[TextDocumentContentChangeEvent],
        edit: Edit,
    ) -> Option<i32> {
        let mut buffers = self
            .buffers
            .lock()
            .expect("panic = 'abort' makes poisoning unreachable");

        let version = match edit {
            Edit::Opened(version) => version,
            Edit::Changed(version) => {
                // LSP does not require contiguous versions, so a gap is not
                // evidence of reordering: only a version below the one already
                // applied is stale.
                if buffers
                    .get(uri)
                    .is_some_and(|buffer| version < buffer.version)
                {
                    return None;
                }
                version
            }
        };

        let mut desynced = None;
        for change in changes {
            match change.range {
                // A whole-document replacement. Always accepted, and it clears a
                // desync: this is the client telling us what the buffer is.
                None => match buffers.get_mut(uri) {
                    Some(buffer) => buffer.replace(change.text.clone(), version),
                    None => {
                        buffers.insert(uri.clone(), OpenBuffer::new(change.text.clone(), version));
                    }
                },
                Some(range) => {
                    let Some(buffer) = buffers.get_mut(uri) else {
                        // A ranged change against a document we have no text
                        // for. There is nothing to splice into.
                        desynced = Some("a ranged change arrived for a document with no buffer");
                        continue;
                    };
                    if buffer.desynced {
                        continue;
                    }
                    if !buffer.splice(range, &change.text, version) {
                        buffer.desynced = true;
                        desynced = Some("a ranged change described text this buffer does not have");
                    }
                }
            }
        }

        if let Some(reason) = desynced {
            // Visible and self-healing rather than silently wrong. There is no
            // LSP request for "please resend the document", so refusing further
            // ranged changes until a whole one arrives is the recovery
            // available, and the next full replacement clears it.
            self.log_desync(uri, version, reason);
        }

        buffers.get(uri).map(|buffer| buffer.version)
    }

    /// Report a buffer that has fallen out of step with its client.
    ///
    /// Deliberately not `async`: [`Self::apply_document_change`] must not gain
    /// an await. The message is queued through the notifier's own spawn rather
    /// than awaited here.
    fn log_desync(&self, uri: &Uri, version: i32, reason: &str) {
        let notifier = Arc::clone(&self.notifier);
        let message = format!(
            "SurrealQL: {} ({} at version {}). Ranged edits are ignored until the \
             editor sends the whole document again.",
            reason,
            uri.as_str(),
            version,
        );
        runtime::spawn(async move {
            notifier.log_message(MessageType::WARNING, message).await;
        });
    }

    /// Analyse the buffer at `uri` and publish what it says.
    ///
    /// The other half of what used to be one function. Everything slow lives
    /// here (the debounce, the parse, the model rebuild), so the caller can
    /// spawn it and leave [`Self::apply_document_change`] on the ordered path.
    pub async fn analyze_buffer(&self, uri: Uri, edit: Edit) {
        let (limit, max_bytes, debounce_ms) = {
            let state = self.state.read().await;
            (
                state.settings.analysis.max_syntax_diagnostics,
                state.settings.analysis.max_document_bytes,
                state.settings.analysis.diagnostic_debounce_ms,
            )
        };

        // Let a burst of keystrokes settle. `didOpen` skips this entirely: the
        // file just appeared and the user is waiting to see what is wrong.
        if let Edit::Changed(version) = edit
            && debounce_ms > 0
        {
            runtime::time::sleep(std::time::Duration::from_millis(debounce_ms)).await;
            if self.superseded(&uri, version) {
                return;
            }
        }

        // Text and pending tree are taken together, under one lock, so the tree
        // always describes the text it is paired with.
        let Some((text, pending_tree)) = self.buffer_for_analysis(&uri) else {
            // Closed while the debounce ran.
            return;
        };

        // Parsing and extraction are CPU-bound and the most frequent work the
        // server does, so they must not run on a thread that is also serving
        // requests.
        let Some(analysis) = analyze_off_reactor(
            uri.clone(),
            text.to_string(),
            limit,
            max_bytes,
            pending_tree,
        )
        .await
        else {
            // The previous analysis stays in `open_documents`, so the editor
            // keeps showing diagnostics for text the user has already changed.
            // That is the worst kind of wrong (stale and silent), so say it
            // happened. `analyze_document` only answers `None` when the grammar
            // fails to load, which is a total outage rather than a bad document.
            self.notifier
                .log_message(
                    MessageType::ERROR,
                    format!(
                        "SurrealQL: could not analyze {}; its diagnostics are now stale.",
                        uri.as_str()
                    ),
                )
                .await;
            return;
        };

        // The text may have moved on while the analysis ran.
        if let Edit::Changed(version) = edit
            && self.superseded(&uri, version)
        {
            return;
        }

        // Record the tree for the next parse to build on, in the same critical
        // section as the desync check so the two cannot disagree. A superseded
        // analysis leaves it alone: the buffer's tree has already absorbed the
        // newer edits and is still the right thing to reparse from.
        {
            let mut buffers = self
                .buffers
                .lock()
                .expect("panic = 'abort' makes poisoning unreachable");
            if let Some(buffer) = buffers.get_mut(&uri)
                && !buffer.desynced
                // A refused document (past the size or nesting cap) carries an
                // *empty* tree, since parsing it is what was declined. Keeping
                // that as the base for the next parse would apply the
                // intervening edits, which are byte offsets into a large
                // document, to a tree describing nothing.
                && analysis.parsed_whole_document()
                && buffer.text.len() == analysis.text.len()
            {
                buffer.set_pending_tree(analysis.tree.clone());
            } else if let Some(buffer) = buffers.get_mut(&uri) {
                buffer.pending_tree = None;
            }
        }

        {
            let mut state = self.state.write().await;
            state.open_documents.insert(uri.clone(), Arc::new(analysis));
        }
        self.recompute_model().await;
        self.publish_diagnostics_for_uri(&uri).await;
    }

    /// Whether a reusable tree is held for `uri`. For tests: a stale pending
    /// tree is worse than none, so the paths that clear it need pinning.
    pub fn has_pending_tree(&self, uri: &Uri) -> bool {
        self.buffers
            .lock()
            .expect("panic = 'abort' makes poisoning unreachable")
            .get(uri)
            .is_some_and(|buffer| buffer.pending_tree.is_some())
    }

    /// The text and pending tree for `uri`, taken together so they agree.
    fn buffer_for_analysis(&self, uri: &Uri) -> Option<(Arc<String>, Option<Tree>)> {
        self.buffers
            .lock()
            .expect("panic = 'abort' makes poisoning unreachable")
            .get(uri)
            .map(|buffer| (Arc::clone(&buffer.text), buffer.pending_tree.clone()))
    }

    /// Apply and analyse in one call, for callers with no reason to separate
    /// them: the wasm dispatcher, which processes one message at a time, and
    /// the tests.
    async fn upsert_open_document(&self, uri: Uri, text: String, edit: Edit) {
        let changes = [TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text,
        }];
        if self.apply_document_change(&uri, &changes, edit).is_some() {
            self.analyze_buffer(uri, edit).await;
        }
    }

    /// The s-expression of the analysed tree for `uri`. For tests comparing a
    /// document reached by editing against the same text opened whole.
    pub async fn tree_sexp(&self, uri: &Uri) -> Option<String> {
        let state = self.state.read().await;
        state
            .open_documents
            .get(uri)
            .map(|analysis| analysis.tree.root_node().to_sexp())
    }

    /// The authoritative text for `uri`, as a `String`.
    ///
    /// Exists for tests: asserting on what the server believes a buffer contains
    /// is the only way to test incremental sync directly, and going through the
    /// analysis would only show what survived it.
    pub fn buffer_snapshot(&self, uri: &Uri) -> Option<String> {
        self.buffer_text(uri).map(|text| text.to_string())
    }

    /// The authoritative text for `uri`, if the client has it open.
    fn buffer_text(&self, uri: &Uri) -> Option<Arc<String>> {
        self.buffers
            .lock()
            .expect("panic = 'abort' makes poisoning unreachable")
            .get(uri)
            .map(|buffer| Arc::clone(&buffer.text))
    }

    /// True when a newer `didChange` for `uri` has arrived since `version`.
    fn superseded(&self, uri: &Uri, version: i32) -> bool {
        self.buffers
            .lock()
            .expect("panic = 'abort' makes poisoning unreachable")
            .get(uri)
            .is_some_and(|buffer| buffer.version > version)
    }

    /// Re-run the analysis of every open document under a new syntax cap.
    ///
    /// The text comes from the authoritative buffer rather than from disk or
    /// from the stored analysis: an open buffer may be dirty, and the analysis
    /// may be a debounce window behind what the client last sent.
    async fn reanalyze_open_documents(&self, limit: usize) {
        // Copied out before the analysis so the expensive part (one parse per
        // open buffer) runs off the reactor. At 45 ms for a 3,200-line file,
        // twenty open buffers was a near-second stall on the thread serving
        // hover and completion.
        let sources: Vec<(Uri, String)> = {
            let buffers = self
                .buffers
                .lock()
                .expect("panic = 'abort' makes poisoning unreachable");
            buffers
                .iter()
                .map(|(uri, buffer)| (uri.clone(), buffer.text.to_string()))
                .collect()
        };

        let Some(reanalyzed) = off_reactor(move || {
            sources
                .into_iter()
                .filter_map(|(uri, text)| {
                    analyze_document_with_limit(uri.clone(), text, SymbolOrigin::Local, limit)
                        .map(|fresh| (uri, Arc::new(fresh)))
                })
                .collect::<Vec<(Uri, Arc<DocumentAnalysis>)>>()
        })
        .await
        else {
            return;
        };

        let still_open: std::collections::HashSet<Uri> = {
            let buffers = self
                .buffers
                .lock()
                .expect("panic = 'abort' makes poisoning unreachable");
            buffers.keys().cloned().collect()
        };

        let mut state = self.state.write().await;
        for (uri, analysis) in reanalyzed {
            // Only if the document is still open: an edit or a close may have
            // landed while this ran, and neither should be undone by a
            // re-analysis of the text as it was.
            if still_open.contains(&uri) {
                state.open_documents.insert(uri, analysis);
            }
        }
    }

    async fn sync_saved_document_from_disk(&self, uri: &Uri) {
        let Some(text) = self.workspace_loader.read_document(uri).await else {
            return;
        };
        // Runs on every `didSave` *and* every `didClose`, so it is the most
        // frequent of the analyses that used to sit on the reactor.
        let owned = uri.clone();
        let Some(Some(analysis)) =
            off_reactor(move || analyze_document(owned, &text, SymbolOrigin::Local)).await
        else {
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

        // Rebuilt on every keystroke, over the whole workspace. Cheap today
        // (1.3 ms at 200 documents), but it scales with workspace size rather
        // than edit size, and `infer_function_return_types` is 89% of it on a
        // function-heavy corpus (`docs/pain-points.md` H14). Moving it costs a
        // thread hand-off and removes a stall that grows with the repository.
        let Some(model) =
            off_reactor(move || Arc::new(MergedSemanticModel::build(&workspace, &live_metadata)))
                .await
        else {
            return;
        };
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
        let live_metadata = Arc::new(self.metadata_provider.fetch(&settings).await);
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
    /// Recompute and publish diagnostics for every open buffer.
    ///
    /// One hop off the reactor for all of them rather than one per document:
    /// the per-document cost is small, so at twenty open buffers the thread
    /// hand-offs would be a meaningful fraction of the work.
    async fn republish_open_diagnostics(&self) {
        let (documents, model, settings) = {
            let state = self.state.read().await;
            (
                state
                    .open_documents
                    .iter()
                    .map(|(uri, analysis)| (uri.clone(), Arc::clone(analysis)))
                    .collect::<Vec<_>>(),
                Arc::clone(&state.model),
                Arc::clone(&state.settings),
            )
        };

        let Some(published) = off_reactor(move || {
            documents
                .into_iter()
                .map(|(uri, analysis)| {
                    let diagnostics = model.document_diagnostics(&analysis, &settings);
                    (uri, diagnostics)
                })
                .collect::<Vec<_>>()
        })
        .await
        else {
            return;
        };

        for (uri, diagnostics) in published {
            self.notifier.publish_diagnostics(uri, diagnostics).await;
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

        let Some(analysis) = analysis else {
            return;
        };

        // `document_diagnostics` runs `type_diagnostics`, which is six full-tree
        // walks, plus the query-fact loop. Under a millisecond on a typical
        // document: this is moved for uniformity with the paths above rather
        // than for a measured win, and the thread hand-off is a real fraction of
        // it, but it is also where the stack overflow landed before the depth
        // guard, which is reason enough not to run it on a request thread.
        let Some(diagnostics) =
            off_reactor(move || model.document_diagnostics(&analysis, &settings)).await
        else {
            return;
        };
        self.notifier
            .publish_diagnostics(uri.clone(), diagnostics)
            .await;
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

/// The folders to index, from whichever of the three `initialize` fields the
/// client filled in.
///
/// `workspaceFolders` is the modern one and what most clients send. The other
/// two are deprecated but still in wide use (eglot and a number of minimal
/// clients send `rootUri` alone), and reading only the first meant such a client
/// got **no workspace schema at all**, silently: every cross-file table came
/// back undefined and nothing said why.
#[allow(deprecated)] // root_uri and root_path are how some clients still speak.
fn resolve_workspace_folders(params: &InitializeParams) -> Vec<PathBuf> {
    if let Some(folders) = params.workspace_folders.as_ref()
        && !folders.is_empty()
    {
        let resolved: Vec<PathBuf> = folders
            .iter()
            .filter_map(|folder| folder.uri.to_file_path().map(|path| path.into_owned()))
            .collect();
        if !resolved.is_empty() {
            return resolved;
        }
    }

    if let Some(root) = params
        .root_uri
        .as_ref()
        .and_then(|uri| uri.to_file_path())
        .map(|path| path.into_owned())
    {
        return vec![root];
    }

    // The oldest spelling, a plain path rather than a URI.
    params
        .root_path
        .as_ref()
        .map(|path| vec![PathBuf::from(path)])
        .unwrap_or_default()
}

/// Whether a statement reads or writes the names it touches.
///
/// `Execute` is a function call, which reads its arguments. `Relate` writes the
/// edge and both endpoints.
fn highlight_kind(action: QueryAction) -> DocumentHighlightKind {
    match action {
        QueryAction::Select | QueryAction::Execute => DocumentHighlightKind::READ,
        QueryAction::Create | QueryAction::Update | QueryAction::Delete | QueryAction::Relate => {
            DocumentHighlightKind::WRITE
        }
    }
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
        data: None,
    }
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
pub enum Edit {
    /// `didOpen`, carrying the version the client says the buffer is at.
    ///
    /// Per LSP that version is authoritative for the newly-opened document, so
    /// it *replaces* whatever high-water mark the URI had. A client that
    /// reopens without closing first (and any client whose counter restarts)
    /// is covered by this as well as by the removal in `did_close`.
    Opened(i32),
    Changed(i32),
}

/// Run CPU-bound work without occupying a thread that serves requests.
///
/// On native this hands the closure to tokio's blocking pool, so a hover or
/// completion arriving mid-keystroke is not queued behind it. The workspace walk
/// already did this (see
/// [`crate::native::workspace_fs::FilesystemWorkspaceLoader::load`]); the
/// per-edit path did not, and it is the far more frequent one.
///
/// **The guard cannot cross this call.** `RwLockReadGuard` is not `Send`, so
/// every caller has to snapshot what it needs under the guard, drop it, compute
/// here, and re-acquire to store. That shape is not incidental: it is what
/// keeps a request handler from waiting on a model rebuild.
///
/// On `wasm32` it runs inline. `tokio_with_wasm` would move it to a web worker,
/// which means shipping the module to that worker and a serialisation hop for
/// every edit — a change to how the browser build works that nothing here can
/// test, since CI does not exercise the wasm JS surface. Inline keeps the
/// browser behaviour exactly as it was, on a runtime that has one thread anyway.
#[cfg(not(target_arch = "wasm32"))]
async fn off_reactor<T, F>(work: F) -> Option<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    runtime::task::spawn_blocking(work).await.ok()
}

#[cfg(target_arch = "wasm32")]
async fn off_reactor<T, F>(work: F) -> Option<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    Some(work())
}

async fn analyze_off_reactor(
    uri: Uri,
    text: String,
    limit: usize,
    max_bytes: usize,
    old_tree: Option<Tree>,
) -> Option<DocumentAnalysis> {
    off_reactor(move || {
        analyze_document_incremental(
            uri,
            text,
            SymbolOrigin::Local,
            limit,
            max_bytes,
            old_tree.as_ref(),
        )
    })
    .await
    .flatten()
}
