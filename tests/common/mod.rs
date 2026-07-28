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
}

#[async_trait]
impl WorkspaceLoader for StaticWorkspace {
    async fn load(&self, _folders: &[PathBuf]) -> WorkspaceIndex {
        self.index.clone()
    }

    async fn read_document(&self, _uri: &Uri) -> Option<String> {
        None
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
        StaticWorkspace { index: workspace },
        provider.clone(),
    );
    (core, notifier, provider)
}

pub fn uri(path: &str) -> Uri {
    format!("file:///workspace/{path}")
        .parse()
        .expect("valid uri")
}
