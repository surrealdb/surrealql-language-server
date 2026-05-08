//! [`WorkspaceLoader`] and [`MetadataProvider`] implementations that
//! return whatever the host (Surrealist) has previously pushed in.
//!
//! The browser sandbox can't walk a filesystem and shouldn't pull in
//! the SurrealDB Rust SDK just to re-issue an `INFO FOR DB` Surrealist
//! is already capable of running through its existing JS connection.
//! These types therefore behave as passive caches: the host calls
//! `push_*` setters on the [`WasmLanguageServer`][crate::wasm::WasmLanguageServer],
//! which mutates the shared snapshots stored here, and the core reads
//! them through the trait when it needs them.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use ls_types::Uri;
use parking_lot::RwLock;

use crate::config::ServerSettings;
use crate::core::client::{MetadataProvider, WorkspaceLoader};
use crate::semantic::types::{LiveMetadataSnapshot, WorkspaceIndex};

/// Snapshot of `.surql` documents the host has pushed in.
#[derive(Default)]
pub struct HostPushedWorkspaceStore {
    inner: RwLock<WorkspaceIndex>,
}

impl HostPushedWorkspaceStore {
    pub fn snapshot(&self) -> WorkspaceIndex {
        self.inner.read().clone()
    }

    pub fn replace(&self, workspace: WorkspaceIndex) {
        *self.inner.write() = workspace;
    }
}

/// Snapshot of the most recent `INFO FOR DB` / `INFO FOR TABLE`
/// payload the host has pushed in.
#[derive(Default)]
pub struct HostPushedMetadataStore {
    inner: RwLock<LiveMetadataSnapshot>,
}

impl HostPushedMetadataStore {
    pub fn snapshot(&self) -> LiveMetadataSnapshot {
        self.inner.read().clone()
    }

    pub fn replace(&self, snapshot: LiveMetadataSnapshot) {
        *self.inner.write() = snapshot;
    }
}

/// Trait adapter over [`HostPushedWorkspaceStore`].
///
/// The core's `apply_settings` calls [`WorkspaceLoader::load`] every
/// time the workspace fingerprint changes; we ignore the supplied
/// `folders` because the browser host enumerates documents itself and
/// pushes them in by URI.
pub struct HostPushedWorkspace {
    store: Arc<HostPushedWorkspaceStore>,
}

impl HostPushedWorkspace {
    pub fn new(store: Arc<HostPushedWorkspaceStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl WorkspaceLoader for HostPushedWorkspace {
    async fn load(&self, _folders: &[PathBuf]) -> WorkspaceIndex {
        self.store.snapshot()
    }

    async fn read_document(&self, uri: &Uri) -> Option<String> {
        // The browser can't re-read a file off disk; the host is
        // expected to push fresh document text via `did_change`
        // whenever a save happens.
        self.store
            .snapshot()
            .documents
            .get(uri)
            .map(|analysis| analysis.text.clone())
    }
}

/// Trait adapter over [`HostPushedMetadataStore`].
pub struct HostPushedMetadata {
    store: Arc<HostPushedMetadataStore>,
}

impl HostPushedMetadata {
    pub fn new(store: Arc<HostPushedMetadataStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl MetadataProvider for HostPushedMetadata {
    async fn fetch(&self, _settings: &ServerSettings) -> LiveMetadataSnapshot {
        self.store.snapshot()
    }
}
