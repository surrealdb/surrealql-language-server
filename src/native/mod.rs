//! Native (non-wasm) front-end for the language server core.
//!
//! Hosts the tower-lsp-server adapter ([`backend::Backend`]) plus the
//! three target-specific trait impls:
//!
//! * [`notifier::TowerNotifier`] — outbound diagnostics / log /
//!   configuration via tower-lsp's [`tower_lsp_server::Client`].
//! * [`workspace_fs::FilesystemWorkspaceLoader`] — walkdir-based
//!   ingestion of `.surql` files in the configured workspace folders.
//! * [`metadata_db::SurrealDbMetadataProvider`] — `INFO FOR DB` /
//!   `INFO FOR TABLE` over an authenticated SurrealDB connection.

pub mod backend;
pub mod metadata_db;
pub mod notifier;
pub mod workspace_fs;

pub use backend::Backend;
