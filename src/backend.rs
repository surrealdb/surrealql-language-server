use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use tokio::sync::RwLock;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};
use walkdir::WalkDir;

use crate::config::ServerSettings;
use crate::grammar::{BuiltinFunction, builtin_function};
use crate::providers::surrealdb::SurrealDbProvider;
use crate::semantic::analyzer::analyze_document;
use crate::semantic::model::is_record_type_context;
use crate::semantic::text::{token_at, token_prefix, word_range};
use crate::semantic::types::{
    DocumentAnalysis, LiveMetadataSnapshot, MergedSemanticModel, QueryFact, SymbolOrigin,
    WorkspaceIndex,
};

#[derive(Debug)]
pub struct Backend {
    client: Client,
    state: RwLock<BackendState>,
}

#[derive(Debug, Default)]
struct BackendState {
    settings: ServerSettings,
    workspace_folders: Vec<PathBuf>,
    saved_workspace: WorkspaceIndex,
    open_documents: HashMap<Url, DocumentAnalysis>,
    live_metadata: LiveMetadataSnapshot,
    model: MergedSemanticModel,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            state: RwLock::new(BackendState::default()),
        }
    }

    async fn initialize_state(&self, params: &InitializeParams) {
        let settings = ServerSettings::from_sources(params.initialization_options.as_ref(), None);
        let workspace_folders = resolve_workspace_folders(params);
        let saved_workspace = load_workspace_documents(&workspace_folders);
        let live_metadata = SurrealDbProvider::fetch_snapshot(&settings).await;
        let model = MergedSemanticModel::build(&saved_workspace, &live_metadata);

        let mut state = self.state.write().await;
        state.settings = settings;
        state.workspace_folders = workspace_folders;
        state.saved_workspace = saved_workspace;
        state.live_metadata = live_metadata;
        state.model = model;
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
            state.settings.clone()
        };
        let settings = ServerSettings::from_sources(None, configuration.as_ref())
            .merge_with_env_if_missing(current_settings);

        self.apply_settings(settings).await;
    }

    async fn apply_settings(&self, settings: ServerSettings) {
        let workspace_folders = {
            let state = self.state.read().await;
            state.workspace_folders.clone()
        };
        let saved_workspace = load_workspace_documents(&workspace_folders);
        let live_metadata = SurrealDbProvider::fetch_snapshot(&settings).await;
        let open_documents = {
            let state = self.state.read().await;
            state.open_documents.clone()
        };
        let workspace = merged_workspace(&saved_workspace, &open_documents);
        let model = MergedSemanticModel::build(&workspace, &live_metadata);

        {
            let mut state = self.state.write().await;
            state.settings = settings;
            state.saved_workspace = saved_workspace;
            state.live_metadata = live_metadata;
            state.model = model;
        }

        self.publish_open_document_diagnostics().await;
    }

    async fn upsert_open_document(&self, uri: Url, text: String) {
        let Some(analysis) = analyze_document(uri.clone(), &text, SymbolOrigin::Local) else {
            return;
        };
        {
            let mut state = self.state.write().await;
            state.open_documents.insert(uri.clone(), analysis);
        }
        self.recompute_model().await;
        self.publish_diagnostics_for_uri(&uri).await;
    }

    async fn sync_saved_document_from_disk(&self, uri: &Url) {
        let Some(path) = uri.to_file_path().ok() else {
            return;
        };
        let Some(text) = fs::read_to_string(path).ok() else {
            return;
        };
        let Some(analysis) = analyze_document(uri.clone(), &text, SymbolOrigin::Local) else {
            return;
        };
        let mut state = self.state.write().await;
        state
            .saved_workspace
            .documents
            .insert(uri.clone(), analysis);
    }

    async fn recompute_model(&self) {
        let (workspace, live_metadata) = {
            let state = self.state.read().await;
            (
                merged_workspace(&state.saved_workspace, &state.open_documents),
                state.live_metadata.clone(),
            )
        };

        let model = MergedSemanticModel::build(&workspace, &live_metadata);
        let mut state = self.state.write().await;
        state.model = model;
    }

    async fn refresh_remote_metadata_if_needed(&self) {
        let settings = {
            let state = self.state.read().await;
            state.settings.clone()
        };
        let live_metadata = SurrealDbProvider::fetch_snapshot(&settings).await;
        {
            let mut state = self.state.write().await;
            state.live_metadata = live_metadata;
        }
        self.recompute_model().await;
    }

    async fn publish_diagnostics_for_uri(&self, uri: &Url) {
        let (analysis, model, settings) = {
            let state = self.state.read().await;
            let analysis = state
                .open_documents
                .get(uri)
                .cloned()
                .or_else(|| state.saved_workspace.documents.get(uri).cloned());
            (analysis, state.model.clone(), state.settings.clone())
        };

        if let Some(analysis) = analysis {
            let mut diagnostics = analysis.syntax_diagnostics.clone();
            diagnostics.extend(model.semantic_diagnostics(&analysis, &settings));
            self.client
                .publish_diagnostics(uri.clone(), diagnostics, None)
                .await;
        }
    }

    async fn publish_open_document_diagnostics(&self) {
        let uris = {
            let state = self.state.read().await;
            state.open_documents.keys().cloned().collect::<Vec<_>>()
        };
        for uri in uris {
            self.publish_diagnostics_for_uri(&uri).await;
        }
    }

    async fn clear_diagnostics(&self, uri: Url) {
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
    }

    async fn snapshot_for_uri(
        &self,
        uri: &Url,
    ) -> Option<(DocumentAnalysis, MergedSemanticModel, ServerSettings)> {
        let state = self.state.read().await;
        let analysis = state
            .open_documents
            .get(uri)
            .cloned()
            .or_else(|| state.saved_workspace.documents.get(uri).cloned())?;
        Some((analysis, state.model.clone(), state.settings.clone()))
    }
}

#[tower_lsp::async_trait]
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
                if let Ok(path) = removed.uri.to_file_path() {
                    state.workspace_folders.retain(|folder| folder != &path);
                }
            }
            for added in params.event.added {
                if let Ok(path) = added.uri.to_file_path() {
                    if !state.workspace_folders.contains(&path) {
                        state.workspace_folders.push(path);
                    }
                }
            }
        }

        let settings = {
            let state = self.state.read().await;
            state.settings.clone()
        };
        self.apply_settings(settings).await;
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let Some((analysis, model, settings)) = self.snapshot_for_uri(&uri).await else {
            return Ok(None);
        };

        let record_type_context = is_record_type_context(&analysis.text, position);
        let prefix = completion_prefix(&analysis.text, position, record_type_context);
        let statement_fact = active_query_fact(&analysis, position);
        let qualifier = completion_table_qualifier(&analysis.text, position);
        let items = model.completion_items(
            prefix.trim_matches(|ch: char| ch == ':'),
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
    ) -> Result<Option<Vec<SymbolInformation>>> {
        let state = self.state.read().await;
        Ok(Some(state.model.workspace_symbol_items(&params.query)))
    }
}

fn resolve_workspace_folders(params: &InitializeParams) -> Vec<PathBuf> {
    if let Some(folders) = &params.workspace_folders {
        let resolved = folders
            .iter()
            .filter_map(|folder| folder.uri.to_file_path().ok())
            .collect::<Vec<_>>();
        if !resolved.is_empty() {
            return resolved;
        }
    }

    params
        .root_uri
        .as_ref()
        .and_then(|uri| uri.to_file_path().ok())
        .into_iter()
        .collect()
}

fn load_workspace_documents(workspace_folders: &[PathBuf]) -> WorkspaceIndex {
    let mut index = WorkspaceIndex::default();
    for folder in workspace_folders {
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
            let Some(uri) = Url::from_file_path(path).ok() else {
                continue;
            };
            let Some(text) = fs::read_to_string(path).ok() else {
                continue;
            };
            if let Some(analysis) = analyze_document(uri.clone(), &text, SymbolOrigin::Local) {
                index.documents.insert(uri, analysis);
            }
        }
    }
    index
}

fn should_descend(path: &Path) -> bool {
    if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
        !matches!(name, ".git" | "target" | "node_modules")
    } else {
        true
    }
}

fn merged_workspace(
    saved_workspace: &WorkspaceIndex,
    open_documents: &HashMap<Url, DocumentAnalysis>,
) -> WorkspaceIndex {
    let mut workspace = saved_workspace.clone();
    for (uri, analysis) in open_documents {
        workspace.documents.insert(uri.clone(), analysis.clone());
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
