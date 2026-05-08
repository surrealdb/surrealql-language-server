//! Filesystem-backed [`WorkspaceLoader`] for the native binary.
//!
//! Walks the configured workspace folders looking for `.surql` /
//! `.surrealql` files, parses them in parallel via tree-sitter, and
//! returns the resulting [`WorkspaceIndex`].

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use ls_types::Uri;
use walkdir::WalkDir;

use crate::core::client::WorkspaceLoader;
use crate::semantic::analyzer::analyze_document;
use crate::semantic::types::{DocumentAnalysis, SymbolOrigin, WorkspaceIndex};

/// Skip files larger than this — pathological generated SurrealQL dumps
/// would otherwise blow up parser memory at startup.
const MAX_FILE_SIZE_BYTES: u64 = 2 * 1024 * 1024;

/// Hard cap on the total number of `.surql` / `.surrealql` files we
/// ingest, to keep cold start bounded on huge monorepos.
const MAX_WORKSPACE_FILES: usize = 5000;

#[derive(Default)]
pub struct FilesystemWorkspaceLoader;

impl FilesystemWorkspaceLoader {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl WorkspaceLoader for FilesystemWorkspaceLoader {
    async fn load(&self, folders: &[PathBuf]) -> WorkspaceIndex {
        let folders = folders.to_vec();
        // Tree-sitter parsing is CPU-bound and can take seconds on a
        // large repo — keep it off the tokio reactor thread.
        tokio::task::spawn_blocking(move || load_workspace_documents(&folders))
            .await
            .unwrap_or_default()
    }

    async fn read_document(&self, uri: &Uri) -> Option<String> {
        let path = uri.to_file_path()?.into_owned();
        tokio::task::spawn_blocking(move || fs::read_to_string(path).ok())
            .await
            .ok()
            .flatten()
    }
}

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

    // Second pass: parse files in parallel — tree-sitter parsing is
    // CPU-bound and trivially parallelisable per-file. We're already
    // inside a `spawn_blocking`, so `std::thread::scope` is the cheapest
    // way to fan out.
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
        !matches!(
            name,
            ".git" | "target" | "node_modules" | ".idea" | ".gradle"
        )
    } else {
        true
    }
}
