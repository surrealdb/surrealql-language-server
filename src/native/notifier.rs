//! [`LspNotifier`] implementation backed by tower-lsp-server's
//! [`Client`]. Wraps the three outbound calls the core needs into a
//! single trait object so the core itself never sees the tower types.

use async_trait::async_trait;
use ls_types::{ConfigurationItem, Diagnostic, MessageType, Uri};
use serde_json::Value;
use tower_lsp_server::Client;

use crate::core::client::LspNotifier;

pub struct TowerNotifier {
    client: Client,
}

impl TowerNotifier {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl LspNotifier for TowerNotifier {
    async fn publish_diagnostics(&self, uri: Uri, diagnostics: Vec<Diagnostic>) {
        self.client
            .publish_diagnostics(uri, diagnostics, None)
            .await;
    }

    async fn log_message(&self, level: MessageType, message: String) {
        self.client.log_message(level, message).await;
    }

    async fn show_message(&self, level: MessageType, message: String) {
        self.client.show_message(level, message).await;
    }

    async fn request_configuration(&self) -> Option<Value> {
        self.client
            .configuration(vec![ConfigurationItem {
                scope_uri: None,
                section: Some("surrealql".to_string()),
            }])
            .await
            .ok()
            .and_then(|mut values| values.pop())
    }

    async fn register_file_watchers(&self) {
        use tower_lsp_server::ls_types::{
            DidChangeWatchedFilesRegistrationOptions, FileSystemWatcher, GlobPattern, Registration,
        };

        let watchers = ["**/*.surql", "**/*.surrealql"]
            .into_iter()
            .map(|pattern| FileSystemWatcher {
                glob_pattern: GlobPattern::String(pattern.to_string()),
                // All three kinds: a created file must join the model, a
                // changed one must be re-read, and a deleted one must leave.
                kind: None,
            })
            .collect();

        let registration = Registration {
            id: "surrealql-watched-files".to_string(),
            method: "workspace/didChangeWatchedFiles".to_string(),
            register_options: serde_json::to_value(DidChangeWatchedFilesRegistrationOptions {
                watchers,
            })
            .ok(),
        };

        // A client that does not support dynamic registration answers with an
        // error. That is not a failure worth a toast — the server simply keeps
        // the behaviour it had before, where a file is picked up on save.
        if self
            .client
            .register_capability(vec![registration])
            .await
            .is_err()
        {
            self.log_message(
                MessageType::LOG,
                "the client declined to watch `.surql` files; \
                 changes made outside the editor will be picked up on save"
                    .to_string(),
            )
            .await;
        }
    }

    async fn begin_progress(&self, token: &str, title: &str) {
        use tower_lsp_server::ls_types::{
            NumberOrString, ProgressParams, ProgressParamsValue, WorkDoneProgress,
            WorkDoneProgressBegin, WorkDoneProgressCreateParams,
        };

        let token = NumberOrString::String(token.to_string());
        // A client that does not support server-initiated progress answers with
        // an error. Nothing is lost by continuing without it.
        if self
            .client
            .send_request::<tower_lsp_server::ls_types::request::WorkDoneProgressCreate>(
                WorkDoneProgressCreateParams {
                    token: token.clone(),
                },
            )
            .await
            .is_err()
        {
            return;
        }
        self.client
            .send_notification::<tower_lsp_server::ls_types::notification::Progress>(
                ProgressParams {
                    token,
                    value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(
                        WorkDoneProgressBegin {
                            title: title.to_string(),
                            cancellable: Some(false),
                            message: None,
                            percentage: None,
                        },
                    )),
                },
            )
            .await;
    }

    async fn end_progress(&self, token: &str, message: Option<String>) {
        use tower_lsp_server::ls_types::{
            NumberOrString, ProgressParams, ProgressParamsValue, WorkDoneProgress,
            WorkDoneProgressEnd,
        };

        self.client
            .send_notification::<tower_lsp_server::ls_types::notification::Progress>(
                ProgressParams {
                    token: NumberOrString::String(token.to_string()),
                    value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(
                        WorkDoneProgressEnd { message },
                    )),
                },
            )
            .await;
    }
}
