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

use crate::config::ServerSettings;
use crate::core::client::{LspNotifier, MetadataProvider, WorkspaceLoader};
use crate::core::completion_context::{
    ColumnSlot, active_query_fact, column_completion_context, completion_prefix,
    completion_table_qualifier, is_table_name_context,
};
use crate::core::state::{ServerState, merged_workspace, workspace_signature};
use crate::grammar::{BuiltinFunction, builtin_function};
use crate::runtime;
use crate::semantic::analyzer::analyze_document;
use crate::semantic::model::{field_completion_tables, is_record_type_context};
use crate::semantic::text::{position_to_offset, token_at, word_range};
use crate::semantic::types::{DocumentAnalysis, FunctionDef, MergedSemanticModel, SymbolOrigin};

/// Hosting-agnostic language server. Generic over the three boundary
/// traits so that a native (tower-lsp + walkdir + surrealdb) and a
/// browser (wasm-bindgen) front-end can share every line of business
/// logic.
pub struct LanguageServerCore<N: LspNotifier, W: WorkspaceLoader, M: MetadataProvider> {
    notifier: Arc<N>,
    workspace_loader: Arc<W>,
    metadata_provider: Arc<M>,
    state: Arc<runtime::sync::RwLock<ServerState>>,
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
            text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
            completion_provider: Some(CompletionOptions {
                resolve_provider: Some(false),
                trigger_characters: Some(vec![
                    ".".into(),
                    ":".into(),
                    "<".into(),
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
            code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
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
        let settings = Arc::new(ServerSettings::from_sources(
            params.initialization_options.as_ref(),
            None,
        ));
        let workspace_folders = resolve_workspace_folders(&params);

        {
            let mut state = self.state.write().await;
            state.settings = settings;
            state.workspace_folders = workspace_folders;
        }

        InitializeResult {
            server_info: Some(ServerInfo {
                name: "surreal-language-server".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            capabilities: Self::server_capabilities(),
            ..Default::default()
        }
    }

    /// Pull the latest configuration from the client and re-run
    /// [`Self::apply_settings`]. Logs an info message on completion.
    pub async fn initialized(&self) {
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
        let configuration = self.notifier.request_configuration().await;
        let current_settings = {
            let state = self.state.read().await;
            (*state.settings).clone()
        };
        let settings = ServerSettings::from_sources(None, configuration.as_ref())
            .merge_with_env_if_missing(current_settings);
        self.apply_settings(settings).await;
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
        let (workspace_folders, last_walked) = {
            let mut state = self.state.write().await;
            state.settings = Arc::new(settings.clone());
            (state.workspace_folders.clone(), state.last_walked.clone())
        };

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
                let mut diagnostics = analysis.syntax_diagnostics.clone();
                diagnostics
                    .extend(model_for_diag.semantic_diagnostics(&analysis, &settings_for_diag));
                self.notifier.publish_diagnostics(uri, diagnostics).await;
            }
        }
    }

    // ──────────────────────────────────────────────────────────────────
    // Document lifecycle
    // ──────────────────────────────────────────────────────────────────

    pub async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let document = params.text_document;
        self.upsert_open_document(document.uri, document.text).await;
    }

    pub async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let Some(change) = params.content_changes.into_iter().last() else {
            return;
        };
        self.upsert_open_document(params.text_document.uri, change.text)
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
        let settings = ServerSettings::from_sources(None, Some(&params.settings));
        self.apply_settings(settings).await;
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

        let record_type_context = is_record_type_context(&analysis.text, position);
        let prefix = completion_prefix(&analysis.text, position, record_type_context);

        // When the cursor sits in a slot that only accepts a table name
        // (e.g. `SELECT * FROM |`, `INSERT INTO |`, `UPDATE |`), restrict
        // suggestions to known tables — otherwise the dropdown is flooded
        // with keywords/functions/fields/params the user can't legally use
        // there.
        if !record_type_context && is_table_name_context(&analysis.text, position) {
            let items = model.table_completion_items(
                prefix.trim_matches(|ch: char| ch == ':'),
                settings.active_auth_context(),
            );
            return Some(CompletionResponse::Array(items));
        }

        let statement_fact = active_query_fact(&analysis, position);
        let qualifier = completion_table_qualifier(&analysis.text, position);
        let trimmed_prefix = prefix.trim_matches(|ch: char| ch == ':');

        // Decide whether the cursor is in a column-name slot. A `tbl.`
        // qualifier is always treated as a strict slot (the only legal
        // continuations are field names of `tbl`).
        let column_slot = if qualifier.is_some() {
            Some(ColumnSlot::Strict { allow_star: false })
        } else if record_type_context {
            None
        } else {
            column_completion_context(&analysis.text, position)
        };

        if let Some(ColumnSlot::Strict { allow_star }) = column_slot {
            let field_tables = field_completion_tables(statement_fact, qualifier.as_deref());
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
                return Some(CompletionResponse::Array(items));
            }
        }

        let items = model.completion_items(
            trimmed_prefix,
            record_type_context,
            settings.active_auth_context(),
            statement_fact,
            qualifier.as_deref(),
        );
        Some(CompletionResponse::Array(items))
    }

    pub async fn hover(&self, params: HoverParams) -> Option<Hover> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let (analysis, model, settings) = self.snapshot_for_uri(&uri).await?;

        let token = token_at(&analysis.text, position)?;
        let range = word_range(&analysis.text, position)?;
        let contents = model.hover_markdown_for_token(
            token.trim_matches(|ch: char| {
                matches!(ch, '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';')
            }),
            settings.active_auth_context(),
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
        let data =
            crate::semantic::highlight::collect_semantic_tokens(&analysis.tree, &analysis.text);
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
        let token = token_at(&analysis.text, position)?;

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
        let Some(token) = token_at(&analysis.text, position) else {
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
        let token = token_at(&analysis.text, position)?;
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
        let token = token_at(&analysis.text, position)?;
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
        let offset = position_to_offset(&analysis.text, position);
        let prefix = &analysis.text[..offset];
        let open_paren = prefix.rfind('(')?;
        let function_name = prefix[..open_paren]
            .trim_end()
            .split_whitespace()
            .last()
            .map(str::trim)
            .unwrap_or_default();
        let active_parameter = prefix[open_paren + 1..]
            .chars()
            .filter(|ch| *ch == ',')
            .count() as u32;

        if let Some(function) = model.functions.get(function_name) {
            return Some(SignatureHelp {
                signatures: vec![SignatureInformation {
                    label: format!(
                        "{}({})",
                        function.name,
                        function
                            .params
                            .iter()
                            .map(|param| match &param.type_expr {
                                Some(type_expr) => format!("{}: {}", param.name, type_expr),
                                None => param.name.clone(),
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
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
                                label: ParameterLabel::Simple(match &param.type_expr {
                                    Some(type_expr) => format!("{}: {}", param.name, type_expr),
                                    None => param.name.clone(),
                                }),
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

        let function = builtin_function(function_name)?;
        Some(SignatureHelp {
            signatures: vec![builtin_signature_information(function)],
            active_signature: Some(0),
            active_parameter: Some(active_parameter),
        })
    }

    pub async fn code_action(&self, params: CodeActionParams) -> Option<CodeActionResponse> {
        let uri = params.text_document.uri;
        let (analysis, model, _) = self.snapshot_for_uri(&uri).await?;
        Some(model.code_actions(&uri, &analysis, &params.context.diagnostics))
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
        let Some(token) = token_at(&analysis.text, position) else {
            return Vec::new();
        };
        model
            .references_for_function(token.trim())
            .into_iter()
            .filter(|location| location.uri == uri)
            .map(|location| DocumentHighlight {
                range: location.range,
                kind: Some(DocumentHighlightKind::READ),
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

        let range_start = position_to_offset(&analysis.text, params.range.start);
        let range_end = position_to_offset(&analysis.text, params.range.end);

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
        let token = token_at(&analysis.text, position)?;
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

    async fn upsert_open_document(&self, uri: Uri, text: String) {
        let Some(analysis) = analyze_document(uri.clone(), &text, SymbolOrigin::Local) else {
            return;
        };
        {
            let mut state = self.state.write().await;
            state.open_documents.insert(uri.clone(), Arc::new(analysis));
        }
        self.recompute_model().await;
        self.publish_diagnostics_for_uri(&uri).await;
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
        let settings = {
            let state = self.state.read().await;
            Arc::clone(&state.settings)
        };
        let live_metadata = Arc::new(self.metadata_provider.fetch(&settings).await);
        {
            let mut state = self.state.write().await;
            state.live_metadata = live_metadata;
        }
        self.recompute_model().await;
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
            let mut diagnostics = analysis.syntax_diagnostics.clone();
            diagnostics.extend(model.semantic_diagnostics(&analysis, &settings));
            self.notifier
                .publish_diagnostics(uri.clone(), diagnostics)
                .await;
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

fn resolve_workspace_folders(params: &InitializeParams) -> Vec<PathBuf> {
    params
        .workspace_folders
        .as_ref()
        .map(|folders| {
            folders
                .iter()
                .filter_map(|folder| folder.uri.to_file_path().map(|p| p.into_owned()))
                .collect()
        })
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
        data: None,
    }
}

fn builtin_signature_information(function: &BuiltinFunction) -> SignatureInformation {
    let parameters = function
        .signature
        .split_once('(')
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(params, _)| {
            params
                .split(',')
                .map(str::trim)
                .filter(|param| !param.is_empty())
                .map(|param| ParameterInformation {
                    label: ParameterLabel::Simple(param.to_string()),
                    documentation: None,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    SignatureInformation {
        label: function.signature.to_string(),
        documentation: Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: format!(
                "{}\n\n[Docs]({})",
                function.summary, function.documentation_url
            ),
        })),
        parameters: Some(parameters),
        active_parameter: None,
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
