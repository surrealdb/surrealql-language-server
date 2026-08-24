//! Shared mock implementations of the three core boundary traits so
//! integration tests can drive [`LanguageServerCore`] end-to-end and
//! observe everything it pushes toward the client.
//!
//! Each test target compiles this module independently and uses a
//! different subset of the helpers, so dead-code lints are noise here.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tower_lsp_server::ls_types::{Diagnostic, MessageType, Uri};

use surrealql_language_server::config::ServerSettings;
use surrealql_language_server::core::{
    LanguageServerCore, LspNotifier, MetadataProvider, WorkspaceLoader,
};
use surrealql_language_server::semantic::types::{LiveMetadataSnapshot, WorkspaceIndex};

/// Everything the server pushed toward the client, in call order.
#[derive(Default)]
pub struct Recorded {
    pub published: Vec<(Uri, Vec<Diagnostic>)>,
    pub logs: Vec<(MessageType, String)>,
    pub shows: Vec<(MessageType, String)>,
}

/// [`LspNotifier`] that records every outbound call and answers
/// configuration pulls with a canned value.
#[derive(Clone, Default)]
pub struct RecordingNotifier {
    recorded: Arc<Mutex<Recorded>>,
    pub configuration: Arc<Mutex<Option<serde_json::Value>>>,
}

impl RecordingNotifier {
    pub fn recorded(&self) -> Arc<Mutex<Recorded>> {
        Arc::clone(&self.recorded)
    }

    pub fn published(&self) -> Vec<(Uri, Vec<Diagnostic>)> {
        self.recorded.lock().unwrap().published.clone()
    }

    pub fn logs(&self) -> Vec<(MessageType, String)> {
        self.recorded.lock().unwrap().logs.clone()
    }

    pub fn shows(&self) -> Vec<(MessageType, String)> {
        self.recorded.lock().unwrap().shows.clone()
    }

    pub fn last_published_for(&self, uri: &Uri) -> Option<Vec<Diagnostic>> {
        self.recorded
            .lock()
            .unwrap()
            .published
            .iter()
            .rev()
            .find(|(published_uri, _)| published_uri == uri)
            .map(|(_, diagnostics)| diagnostics.clone())
    }
}

#[async_trait]
impl LspNotifier for RecordingNotifier {
    async fn publish_diagnostics(&self, uri: Uri, diagnostics: Vec<Diagnostic>) {
        self.recorded
            .lock()
            .unwrap()
            .published
            .push((uri, diagnostics));
    }

    async fn log_message(&self, level: MessageType, message: String) {
        self.recorded.lock().unwrap().logs.push((level, message));
    }

    async fn show_message(&self, level: MessageType, message: String) {
        self.recorded.lock().unwrap().shows.push((level, message));
    }

    async fn request_configuration(&self) -> Option<serde_json::Value> {
        self.configuration.lock().unwrap().clone()
    }
}

/// [`WorkspaceLoader`] serving a fixed in-memory snapshot.
#[derive(Default)]
pub struct StaticWorkspace {
    pub index: WorkspaceIndex,
    /// Stands in for a `surrealql.toml`, already converted to JSON. `None`
    /// exercises the trait's default — which is what the browser host and
    /// most tests use.
    pub project_config: Option<serde_json::Value>,
}

#[async_trait]
impl WorkspaceLoader for StaticWorkspace {
    async fn load(&self, _folders: &[PathBuf]) -> WorkspaceIndex {
        self.index.clone()
    }

    async fn read_document(&self, _uri: &Uri) -> Option<String> {
        None
    }

    async fn load_project_config(
        &self,
        _folders: &[PathBuf],
    ) -> (Option<serde_json::Value>, Vec<String>) {
        (self.project_config.clone(), Vec::new())
    }
}

/// [`MetadataProvider`] returning a canned snapshot and recording the
/// settings each fetch received (so tests can observe which
/// connection details survived a configuration change).
#[derive(Clone, Default)]
pub struct RecordingMetadata {
    pub snapshot: Arc<Mutex<LiveMetadataSnapshot>>,
    pub last_settings: Arc<Mutex<Option<ServerSettings>>>,
}

#[async_trait]
impl MetadataProvider for RecordingMetadata {
    async fn fetch(&self, settings: &ServerSettings) -> LiveMetadataSnapshot {
        *self.last_settings.lock().unwrap() = Some(settings.clone());
        self.snapshot.lock().unwrap().clone()
    }
}

pub type TestCore = LanguageServerCore<RecordingNotifier, StaticWorkspace, RecordingMetadata>;

/// Build a core wired to fresh mocks, returning handles to observe them.
pub fn core_with(
    workspace: WorkspaceIndex,
    metadata: LiveMetadataSnapshot,
) -> (TestCore, RecordingNotifier, RecordingMetadata) {
    let notifier = RecordingNotifier::default();
    let provider = RecordingMetadata {
        snapshot: Arc::new(Mutex::new(metadata)),
        last_settings: Arc::new(Mutex::new(None)),
    };
    let core = LanguageServerCore::new(
        notifier.clone(),
        StaticWorkspace {
            index: workspace,
            project_config: None,
        },
        provider.clone(),
    );
    (core, notifier, provider)
}

/// [`core_with`] plus a stand-in `surrealql.toml`.
pub fn core_with_project_config(
    project_config: serde_json::Value,
) -> (TestCore, RecordingNotifier, RecordingMetadata) {
    let notifier = RecordingNotifier::default();
    let provider = RecordingMetadata::default();
    let core = LanguageServerCore::new(
        notifier.clone(),
        StaticWorkspace {
            index: WorkspaceIndex::default(),
            project_config: Some(project_config),
        },
        provider.clone(),
    );
    (core, notifier, provider)
}

/// The capabilities a modern editor advertises.
///
/// The server now reads them and stays quiet about anything a client did not
/// claim, so a test that expects a configuration pull or `relatedInformation`
/// has to say the client supports it — exactly as a real client does.
pub fn modern_client() -> tower_lsp_server::ls_types::ClientCapabilities {
    use tower_lsp_server::ls_types::{
        ClientCapabilities, PublishDiagnosticsClientCapabilities, TextDocumentClientCapabilities,
        WorkspaceClientCapabilities,
    };
    ClientCapabilities {
        workspace: Some(WorkspaceClientCapabilities {
            configuration: Some(true),
            ..Default::default()
        }),
        text_document: Some(TextDocumentClientCapabilities {
            publish_diagnostics: Some(PublishDiagnosticsClientCapabilities {
                related_information: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// `initialize` with [`modern_client`] capabilities.
pub async fn initialize_modern(core: &TestCore) {
    core.initialize(tower_lsp_server::ls_types::InitializeParams {
        capabilities: modern_client(),
        ..Default::default()
    })
    .await;
}

pub fn uri(path: &str) -> Uri {
    format!("file:///workspace/{path}")
        .parse()
        .expect("valid uri")
}
