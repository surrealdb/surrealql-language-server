//! [`LspNotifier`] implementation backed by tower-lsp-server's
//! [`Client`]. Wraps the three outbound calls the core needs into a
//! single trait object so the core itself never sees the tower types.

use async_trait::async_trait;
use ls_types::{ConfigurationItem, Diagnostic, MessageType, Registration, Uri};
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

    async fn refresh_diagnostics(&self) {
        if let Err(error) = self.client.workspace_diagnostic_refresh().await {
            // Not fatal, but it does mean this client is now looking at
            // diagnostics computed before the edit that prompted the refresh.
            self.client
                .log_message(
                    MessageType::WARNING,
                    format!("SurrealQL: could not refresh diagnostics: {error}"),
                )
                .await;
        }
    }

    async fn register_capability(&self, registrations: Vec<Registration>) {
        if let Err(error) = self.client.register_capability(registrations).await {
            // Not fatal: the server keeps working, it just will not hear about
            // files changed outside the editor. Say so rather than leaving the
            // staleness to be discovered as a wrong diagnostic.
            self.client
                .log_message(
                    MessageType::WARNING,
                    format!("SurrealQL: could not register a file watcher: {error}"),
                )
                .await;
        }
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
}
