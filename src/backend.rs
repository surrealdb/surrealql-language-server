use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::RwLock;
use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;
use tower_lsp_server::{Client, LanguageServer};
use walkdir::WalkDir;

use crate::config::ServerSettings;
use crate::grammar::{BuiltinFunction, builtin_function};
use crate::providers::surrealdb::SurrealDbProvider;
use crate::semantic::analyzer::analyze_document;
use crate::semantic::model::{field_completion_tables, is_record_type_context};
use crate::semantic::text::{token_at, token_prefix, word_range};
use crate::semantic::types::{
    DocumentAnalysis, LiveMetadataSnapshot, MergedSemanticModel, QueryFact, SymbolOrigin,
    WorkspaceIndex,
};

#[derive(Debug)]
pub struct Backend {
    client: Client,
    state: Arc<RwLock<BackendState>>,
}

/// All shared state lives behind [`Arc`]: cloning a snapshot for an LSP
/// request handler is now a handful of pointer-bumps instead of a deep clone
/// of the entire workspace model (which previously copied every table /
/// field / function definition for every hover / completion).
#[derive(Debug, Default)]
struct BackendState {
    settings: Arc<ServerSettings>,
    workspace_folders: Vec<PathBuf>,
    saved_workspace: Arc<WorkspaceIndex>,
    open_documents: HashMap<Uri, Arc<DocumentAnalysis>>,
    live_metadata: Arc<LiveMetadataSnapshot>,
    model: Arc<MergedSemanticModel>,
    /// Fingerprint of the last successful workspace walk. When the new
    /// fingerprint matches, `apply_settings` skips the walk entirely — this
    /// is the common path for `didChangeConfiguration` events that don't
    /// touch the folder set.
    last_walked: Option<Vec<PathBuf>>,
}

fn workspace_signature(folders: &[PathBuf]) -> Vec<PathBuf> {
    let mut signature = folders.to_vec();
    signature.sort();
    signature
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            state: Arc::new(RwLock::new(BackendState::default())),
        }
    }

    async fn initialize_state(&self, params: &InitializeParams) {
        // Stash settings + workspace folders synchronously so subsequent requests
        // see the configured connection / folders straight away. The heavy
        // workspace walk + SurrealDB fetch is deferred to `initialized →
        // apply_settings`, where it runs exactly once with the
        // client-supplied configuration merged in.
        let settings = Arc::new(ServerSettings::from_sources(
            params.initialization_options.as_ref(),
            None,
        ));
        let workspace_folders = resolve_workspace_folders(params);

        let mut state = self.state.write().await;
        state.settings = settings;
        state.workspace_folders = workspace_folders;
    }

    async fn reload_from_client_configuration(&self) {
        let configuration = self
            .client
            .configuration(vec![ConfigurationItem {
                scope_uri: None,
                section: Some("surrealql".to_string()),
            }])
            .await
            .ok()
            .and_then(|mut values| values.pop());

        let current_settings = {
            let state = self.state.read().await;
            (*state.settings).clone()
        };
        let settings = ServerSettings::from_sources(None, configuration.as_ref())
            .merge_with_env_if_missing(current_settings);

        self.apply_settings(settings).await;
    }

    async fn apply_settings(&self, settings: ServerSettings) {
        // Persist settings synchronously so subsequent requests see them immediately.
        let (workspace_folders, last_walked) = {
            let mut state = self.state.write().await;
            state.settings = Arc::new(settings.clone());
            (state.workspace_folders.clone(), state.last_walked.clone())
        };

        // Defer the heavy `load_workspace_documents` walk and the SurrealDB
        // metadata fetch to a background task so notification handlers
        // (initialized, didChangeConfiguration, didChangeWorkspaceFolders)
        // return immediately. Otherwise tower-lsp serialises subsequent
        // requests behind the blocking walk and LSP4IJ kills the server.
        let state = Arc::clone(&self.state);
        let client = self.client.clone();
        let folders = workspace_folders;
        let settings_for_bg = settings;
        tokio::spawn(async move {
            // Skip the walk entirely if every workspace folder is unchanged
            // since the last successful walk — this is the common case for
            // `didChangeConfiguration` and avoids redundant tree-sitter
            // parsing of every .surql file in large repos.
            let folder_signature = workspace_signature(&folders);
            let need_walk = last_walked
                .as_ref()
                .map(|previous| previous != &folder_signature)
                .unwrap_or(true);

            let saved_workspace = if need_walk {
                let folders_for_walk = folders.clone();
                let walked = tokio::task::spawn_blocking(move || {
                    load_workspace_documents(&folders_for_walk)
                })
                .await
                .unwrap_or_default();
                Arc::new(walked)
            } else {
                let s = state.read().await;
                Arc::clone(&s.saved_workspace)
            };

            let live_metadata = Arc::new(SurrealDbProvider::fetch_snapshot(&settings_for_bg).await);
            let open_documents = {
                let s = state.read().await;
                s.open_documents.clone()
            };
            let workspace = merged_workspace(&saved_workspace, &open_documents);
            let model = Arc::new(MergedSemanticModel::build(&workspace, &live_metadata));

            let (uris, model_for_diag, settings_for_diag, open_for_diag, saved_for_diag) = {
                let mut s = state.write().await;
                s.saved_workspace = Arc::clone(&saved_workspace);
                s.live_metadata = Arc::clone(&live_metadata);
                s.model = Arc::clone(&model);
                if need_walk {
                    s.last_walked = Some(folder_signature);
                }
                (
                    s.open_documents.keys().cloned().collect::<Vec<_>>(),
                    Arc::clone(&s.model),
                    Arc::clone(&s.settings),
                    s.open_documents.clone(),
                    Arc::clone(&s.saved_workspace),
                )
            };

            // Inline the equivalent of publish_open_document_diagnostics
            // (we don't have `&self` here, but `client` + `state` snapshots
            // are sufficient).
            for uri in uris {
                let analysis = open_for_diag
                    .get(&uri)
                    .cloned()
                    .or_else(|| saved_for_diag.documents.get(&uri).cloned());
                if let Some(analysis) = analysis {
                    let mut diagnostics = analysis.syntax_diagnostics.clone();
                    diagnostics
                        .extend(model_for_diag.semantic_diagnostics(&analysis, &settings_for_diag));
                    client.publish_diagnostics(uri, diagnostics, None).await;
                }
            }
        });
    }

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
        let Some(path) = uri.to_file_path() else {
            return;
        };
        let Some(text) = fs::read_to_string(path).ok() else {
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
        let live_metadata = Arc::new(SurrealDbProvider::fetch_snapshot(&settings).await);
        {
            let mut state = self.state.write().await;
            state.live_metadata = live_metadata;
        }
        self.recompute_model().await;
    }

    async fn publish_diagnostics_for_uri(&self, uri: &Uri) {
        let (analysis, model, settings) = {
            let state = self.state.read().await;
            let analysis = state
                .open_documents
                .get(uri)
                .cloned()
                .or_else(|| state.saved_workspace.documents.get(uri).cloned());
            (analysis, Arc::clone(&state.model), Arc::clone(&state.settings))
        };

        if let Some(analysis) = analysis {
            let mut diagnostics = analysis.syntax_diagnostics.clone();
            diagnostics.extend(model.semantic_diagnostics(&analysis, &settings));
            self.client
                .publish_diagnostics(uri.clone(), diagnostics, None)
                .await;
        }
    }

    async fn clear_diagnostics(&self, uri: Uri) {
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
    }

    async fn snapshot_for_uri(
        &self,
        uri: &Uri,
    ) -> Option<(Arc<DocumentAnalysis>, Arc<MergedSemanticModel>, Arc<ServerSettings>)> {
        let state = self.state.read().await;
        let analysis = state
            .open_documents
            .get(uri)
            .cloned()
            .or_else(|| state.saved_workspace.documents.get(uri).cloned())?;
        Some((analysis, Arc::clone(&state.model), Arc::clone(&state.settings)))
    }
}

impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        self.initialize_state(&params).await;

        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "surreal-language-server".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
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
                call_hierarchy_provider: Some(CallHierarchyServerCapability::Simple(true)),
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
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.reload_from_client_configuration().await;
        self.client
            .log_message(
                MessageType::INFO,
                "SurrealQL semantic language server ready",
            )
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let document = params.text_document;
        self.upsert_open_document(document.uri, document.text).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let Some(change) = params.content_changes.into_iter().last() else {
            return;
        };
        self.upsert_open_document(params.text_document.uri, change.text)
            .await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
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

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        {
            let mut state = self.state.write().await;
            state.open_documents.remove(&uri);
        }
        self.sync_saved_document_from_disk(&uri).await;
        self.recompute_model().await;
        self.clear_diagnostics(uri).await;
    }

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        let settings = ServerSettings::from_sources(None, Some(&params.settings));
        self.apply_settings(settings).await;
    }

    async fn did_change_workspace_folders(&self, params: DidChangeWorkspaceFoldersParams) {
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

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let snap = self.snapshot_for_uri(&uri).await;
        let Some((analysis, model, settings)) = snap else {
            return Ok(None);
        };

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
            return Ok(Some(CompletionResponse::Array(items)));
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
                return Ok(Some(CompletionResponse::Array(items)));
            }
            // No identifiable target table — fall through to the generic
            // completion below so the dropdown isn't empty.
        }

        let items = model.completion_items(
            trimmed_prefix,
            record_type_context,
            settings.active_auth_context(),
            statement_fact,
            qualifier.as_deref(),
        );
        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let Some((analysis, model, settings)) = self.snapshot_for_uri(&uri).await else {
            return Ok(None);
        };

        let Some(token) = token_at(&analysis.text, position) else {
            return Ok(None);
        };
        let Some(range) = word_range(&analysis.text, position) else {
            return Ok(None);
        };
        let Some(contents) = model.hover_markdown_for_token(
            token.trim_matches(|ch: char| {
                matches!(ch, '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';')
            }),
            settings.active_auth_context(),
        ) else {
            return Ok(None);
        };

        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: contents,
            }),
            range: Some(range),
        }))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri;
        let Some((analysis, _, _)) = self.snapshot_for_uri(&uri).await else {
            return Ok(None);
        };
        Ok(Some(DocumentSymbolResponse::Nested(
            analysis.document_symbols.clone(),
        )))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let Some((analysis, model, _)) = self.snapshot_for_uri(&uri).await else {
            return Ok(None);
        };
        let Some(token) = token_at(&analysis.text, position) else {
            return Ok(None);
        };

        let token = token.trim().to_string();
        let location = model.definition_for_token(&token);

        Ok(location.map(GotoDefinitionResponse::Scalar))
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let Some((analysis, model, _)) = self.snapshot_for_uri(&uri).await else {
            return Ok(None);
        };
        let Some(token) = token_at(&analysis.text, position) else {
            return Ok(None);
        };
        let locations = model.references_for_function(token.trim());
        Ok(Some(locations))
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        let uri = params.text_document.uri;
        let position = params.position;
        let Some((analysis, model, _)) = self.snapshot_for_uri(&uri).await else {
            return Ok(None);
        };
        let Some(token) = token_at(&analysis.text, position) else {
            return Ok(None);
        };
        let name = token.trim();
        let Some(location) = model.definition_for_function(name) else {
            return Ok(None);
        };
        Ok(Some(PrepareRenameResponse::RangeWithPlaceholder {
            range: location.range,
            placeholder: name.to_string(),
        }))
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let Some((analysis, model, _)) = self.snapshot_for_uri(&uri).await else {
            return Ok(None);
        };
        let Some(token) = token_at(&analysis.text, position) else {
            return Ok(None);
        };
        let Some(changes) = model.rename_edits(token.trim(), &params.new_name) else {
            return Ok(None);
        };
        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            ..WorkspaceEdit::default()
        }))
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let Some((analysis, model, _)) = self.snapshot_for_uri(&uri).await else {
            return Ok(None);
        };
        let offset = crate::semantic::text::position_to_offset(&analysis.text, position);
        let prefix = &analysis.text[..offset];
        let open_paren = prefix.rfind('(');
        let Some(open_paren) = open_paren else {
            return Ok(None);
        };
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
            return Ok(Some(SignatureHelp {
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
            }));
        }

        let Some(function) = builtin_function(function_name) else {
            return Ok(None);
        };

        Ok(Some(SignatureHelp {
            signatures: vec![builtin_signature_information(function)],
            active_signature: Some(0),
            active_parameter: Some(active_parameter),
        }))
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri = params.text_document.uri;
        let Some((analysis, model, _)) = self.snapshot_for_uri(&uri).await else {
            return Ok(None);
        };
        Ok(Some(model.code_actions(
            &uri,
            &analysis,
            &params.context.diagnostics,
        )))
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let Some((analysis, model, _)) = self.snapshot_for_uri(&uri).await else {
            return Ok(None);
        };
        let Some(token) = token_at(&analysis.text, position) else {
            return Ok(None);
        };
        let highlights = model
            .references_for_function(token.trim())
            .into_iter()
            .filter(|location| location.uri == uri)
            .map(|location| DocumentHighlight {
                range: location.range,
                kind: Some(DocumentHighlightKind::READ),
            })
            .collect::<Vec<_>>();
        Ok(Some(highlights))
    }

    async fn prepare_call_hierarchy(
        &self,
        params: CallHierarchyPrepareParams,
    ) -> Result<Option<Vec<CallHierarchyItem>>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let Some((analysis, model, _)) = self.snapshot_for_uri(&uri).await else {
            return Ok(None);
        };
        let Some(token) = token_at(&analysis.text, position) else {
            return Ok(None);
        };
        let Some(function) = model.functions.get(token.trim()) else {
            return Ok(None);
        };
        Ok(Some(vec![call_hierarchy_item(function)]))
    }

    async fn incoming_calls(
        &self,
        params: CallHierarchyIncomingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyIncomingCall>>> {
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
        Ok(Some(calls))
    }

    async fn outgoing_calls(
        &self,
        params: CallHierarchyOutgoingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyOutgoingCall>>> {
        let item = params.item;
        let state = self.state.read().await;
        let Some(function) = state.model.functions.get(&item.name) else {
            return Ok(None);
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
        Ok(Some(calls))
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<WorkspaceSymbolResponse>> {
        let state = self.state.read().await;
        Ok(Some(state.model.workspace_symbol_items(&params.query).into()))
    }
}

fn resolve_workspace_folders(params: &InitializeParams) -> Vec<PathBuf> {
    if let Some(folders) = &params.workspace_folders {
        let resolved = folders
            .iter()
            .filter_map(|folder| folder.uri.to_file_path().map(|p| p.into_owned()))
            .collect::<Vec<_>>();
        if !resolved.is_empty() {
            return resolved;
        }
    }

    params
        .root_uri
        .as_ref()
        .and_then(|uri| uri.to_file_path().map(|p| p.into_owned()))
        .into_iter()
        .collect()
}

/// Skip files larger than this — pathological generated SurrealQL dumps would
/// otherwise blow up parser memory at startup.
const MAX_FILE_SIZE_BYTES: u64 = 2 * 1024 * 1024;
/// Hard cap on the total number of `.surql` / `.surrealql` files we ingest.
const MAX_WORKSPACE_FILES: usize = 5000;

fn load_workspace_documents(workspace_folders: &[PathBuf]) -> WorkspaceIndex {
    // First pass: gather candidate file paths sequentially (cheap, IO-bound).
    let mut candidates: Vec<PathBuf> = Vec::new();
    'outer: for folder in workspace_folders {
        for entry in WalkDir::new(folder)
            .into_iter()
            .filter_entry(|entry| should_descend(entry.path()))
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_file())
        {
            let path = entry.path();
            if !matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("surql" | "surrealql")
            ) {
                continue;
            }
            if entry
                .metadata()
                .map(|meta| meta.len() > MAX_FILE_SIZE_BYTES)
                .unwrap_or(false)
            {
                continue;
            }
            candidates.push(path.to_path_buf());
            if candidates.len() >= MAX_WORKSPACE_FILES {
                break 'outer;
            }
        }
    }

    // Second pass: parse files in parallel — tree-sitter parsing is CPU-bound
    // and trivially parallelisable per-file. We're already inside a
    // spawn_blocking, so std::thread::scope is the cheapest way to fan out.
    let worker_count = std::thread::available_parallelism()
        .map(|n| n.get().min(8))
        .unwrap_or(2)
        .max(1);
    let chunk_size = candidates.len().div_ceil(worker_count).max(1);
    let mut index = WorkspaceIndex::default();
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(worker_count);
        for chunk in candidates.chunks(chunk_size) {
            let chunk = chunk.to_vec();
            handles.push(scope.spawn(move || -> Vec<(Uri, Arc<DocumentAnalysis>)> {
                let mut local = Vec::with_capacity(chunk.len());
                for path in chunk {
                    let Some(uri) = Uri::from_file_path(&path) else {
                        continue;
                    };
                    let Some(text) = fs::read_to_string(&path).ok() else {
                        continue;
                    };
                    if let Some(analysis) =
                        analyze_document(uri.clone(), &text, SymbolOrigin::Local)
                    {
                        local.push((uri, Arc::new(analysis)));
                    }
                }
                local
            }));
        }
        for handle in handles {
            if let Ok(results) = handle.join() {
                for (uri, analysis) in results {
                    index.documents.insert(uri, analysis);
                }
            }
        }
    });

    index
}

fn should_descend(path: &Path) -> bool {
    if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
        !matches!(name, ".git" | "target" | "node_modules" | ".idea" | ".gradle")
    } else {
        true
    }
}

fn merged_workspace(
    saved_workspace: &WorkspaceIndex,
    open_documents: &HashMap<Uri, Arc<DocumentAnalysis>>,
) -> WorkspaceIndex {
    let mut workspace = saved_workspace.clone();
    for (uri, analysis) in open_documents {
        workspace.documents.insert(uri.clone(), Arc::clone(analysis));
    }
    workspace
}

fn call_hierarchy_item(function: &crate::semantic::types::FunctionDef) -> CallHierarchyItem {
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

/// Returns true when the cursor is positioned in a SurrealQL slot that
/// only syntactically accepts a table name. Currently detects:
///
///   * `SELECT ... FROM |`               (single or comma-separated tables)
///   * `INSERT INTO |`
///   * `UPDATE |`
///   * `DELETE FROM |`
///
/// The check walks backwards from the cursor over (a) the partial
/// identifier being typed, then (b) any sequence of comma-separated
/// identifiers (so `FROM a, b, |` still resolves to `FROM`), and inspects
/// the keyword token immediately preceding that span.
fn is_table_name_context(source: &str, position: Position) -> bool {
    let offset = crate::semantic::text::position_to_offset(source, position);
    let Some(before) = source.get(..offset) else {
        return false;
    };
    let chars: Vec<char> = before.chars().collect();
    let mut i = chars.len();

    // Strip the partial identifier currently being typed at the cursor.
    while i > 0 && is_table_ident_char(chars[i - 1]) {
        i -= 1;
    }
    // Walk backwards over `<ws>* (, <ws>* <ident> <ws>*)*` so multi-table
    // forms like `FROM tbl1, tbl2, |` still detect FROM.
    loop {
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        if i == 0 || chars[i - 1] != ',' {
            break;
        }
        i -= 1;
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && is_table_ident_char(chars[i - 1]) {
            i -= 1;
        }
    }
    // Read the previous identifier (the candidate keyword).
    let keyword_end = i;
    while i > 0 && is_table_ident_char(chars[i - 1]) {
        i -= 1;
    }
    if i == keyword_end {
        return false;
    }
    let keyword: String = chars[i..keyword_end].iter().collect();
    matches!(
        keyword.to_ascii_uppercase().as_str(),
        "FROM" | "INTO" | "UPDATE"
    )
}

fn is_table_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '`'
}

/// Classification of a column-name slot near the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColumnSlot {
    /// The cursor is in a position that *only* accepts column names —
    /// `SELECT ... |, FROM tbl`, `UPDATE tbl SET |`, or after a `tbl.`
    /// qualifier. Suggestions should be column-only. The contained flag
    /// is true when emitting a leading `*` is appropriate (SELECT only).
    Strict { allow_star: bool },
    /// The cursor is in an expression position where columns are useful
    /// but other things (functions, $vars, literals) are also legal —
    /// `WHERE | / AND | / OR |`, `ORDER BY |`, `GROUP BY |`. Columns
    /// should be surfaced at the top of the dropdown but the rest of the
    /// generic completions should still appear.
    Loose,
}

/// Returns the column-completion classification for the cursor. Returns
/// `None` when the cursor is not in any column-name slot we recognise.
///
/// Strategy: walk backwards from the cursor over the partial identifier,
/// then over any `<ident> <ws>* (= <expr>)? <ws>* ,` runs (so multi-column
/// SELECT/SET lists still detect the leading `SELECT`/`SET`), and then
/// inspect the previous keyword token.
///
/// The algorithm intentionally avoids a full SurrealQL parse — it covers
/// the common, syntactically-unambiguous cases listed below and degrades
/// to `None` for anything unfamiliar (sub-queries, parenthesised
/// expressions, ON clauses, etc.).
fn column_completion_context(source: &str, position: Position) -> Option<ColumnSlot> {
    let offset = crate::semantic::text::position_to_offset(source, position);
    let before = source.get(..offset)?;
    let chars: Vec<char> = before.chars().collect();
    let mut i = chars.len();

    // Strip the partial identifier currently being typed at the cursor.
    while i > 0 && is_table_ident_char(chars[i - 1]) {
        i -= 1;
    }
    // Walk backwards over repeated `<ws>* (= ... ,)? <ws>* <ident-or-expr>`
    // segments so that:
    //   `SELECT a, b, |`            still resolves to SELECT
    //   `UPDATE t SET a = 1, b = 2, |`  still resolves to SET
    // For SET we can't safely parse the RHS expression — instead we just
    // skip backwards across non-comma chars until we hit a comma or the
    // closest column-context keyword.
    loop {
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        if i == 0 || chars[i - 1] != ',' {
            break;
        }
        i -= 1;
        // Skip the segment between the previous comma and this comma:
        // walk back over identifier characters, '=' assignment, simple
        // value tokens, and whitespace until we hit either a comma or a
        // keyword boundary. Stop at quotes / parens / braces to stay safe.
        while i > 0 {
            let c = chars[i - 1];
            if c == ',' {
                break;
            }
            if matches!(c, '\'' | '"' | '(' | ')' | '{' | '}' | '[' | ']' | ';') {
                return None;
            }
            i -= 1;
        }
    }
    // Skip whitespace, then read the previous identifier token.
    while i > 0 && chars[i - 1].is_whitespace() {
        i -= 1;
    }
    let keyword_end = i;
    while i > 0 && is_table_ident_char(chars[i - 1]) {
        i -= 1;
    }
    if i == keyword_end {
        return None;
    }
    let keyword: String = chars[i..keyword_end].iter().collect::<String>().to_ascii_uppercase();
    match keyword.as_str() {
        "SELECT" => Some(ColumnSlot::Strict { allow_star: true }),
        "SET" => Some(ColumnSlot::Strict { allow_star: false }),
        "WHERE" | "AND" | "OR" | "BY" => Some(ColumnSlot::Loose),
        _ => None,
    }
}

fn completion_prefix(source: &str, position: Position, record_type_context: bool) -> String {
    let prefix = token_prefix(source, position).unwrap_or_default();
    if record_type_context {
        prefix
            .rsplit_once('<')
            .map(|(_, suffix)| suffix.to_string())
            .unwrap_or(prefix)
    } else {
        prefix
    }
}

fn active_query_fact<'a>(
    analysis: &'a DocumentAnalysis,
    position: Position,
) -> Option<&'a QueryFact> {
    analysis
        .query_facts
        .iter()
        .find(|fact| range_contains_position(fact.location.range, position))
}

fn range_contains_position(range: Range, position: Position) -> bool {
    position_gte(position, range.start) && position_lte(position, range.end)
}

fn position_lte(left: Position, right: Position) -> bool {
    left.line < right.line || (left.line == right.line && left.character <= right.character)
}

fn position_gte(left: Position, right: Position) -> bool {
    left.line > right.line || (left.line == right.line && left.character >= right.character)
}

fn completion_table_qualifier(source: &str, position: Position) -> Option<String> {
    let offset = crate::semantic::text::position_to_offset(source, position);
    let before_cursor = source.get(..offset)?;
    let (left, right) = before_cursor.rsplit_once('.')?;
    if !right.chars().all(is_field_prefix_char) {
        return None;
    }

    let qualifier = left
        .chars()
        .rev()
        .take_while(|ch| is_table_qualifier_char(*ch))
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    let qualifier = qualifier.trim_matches('`');
    if qualifier.is_empty() {
        return None;
    }
    if qualifier
        .chars()
        .next()
        .map(|ch| ch.is_ascii_digit())
        .unwrap_or(false)
    {
        return None;
    }

    let table = qualifier.split(':').next().unwrap_or(qualifier).trim();
    if table.is_empty() {
        None
    } else {
        Some(table.to_string())
    }
}

fn is_table_qualifier_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | ':' | '-' | '`')
}

fn is_field_prefix_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | ':' | '-')
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
        self
    }
}
