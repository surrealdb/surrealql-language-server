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
use crate::semantic::types::{DocumentAnalysis, SymbolOrigin, WorkspaceIndex, WorkspaceScanStats};

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

    /// Analyze an explicit list of files, under the same size cap and the same
    /// parallel parse as [`WorkspaceLoader::load`].
    ///
    /// The server only ever has folders. The command-line mode accepts file
    /// arguments, so it needs a way in that skips the walk without
    /// re-implementing the parse.
    pub async fn load_paths(&self, paths: &[PathBuf]) -> WorkspaceIndex {
        let paths = paths.to_vec();
        tokio::task::spawn_blocking(move || {
            let mut stats = WorkspaceScanStats::default();
            let mut candidates = Vec::with_capacity(paths.len());
            for path in paths {
                match fs::metadata(&path) {
                    Ok(meta) if meta.len() > MAX_FILE_SIZE_BYTES => {
                        stats.skipped_oversize += 1;
                    }
                    Ok(_) => candidates.push(path),
                    Err(_) => stats.walk_errors += 1,
                }
            }
            parse_candidates(candidates, stats)
        })
        .await
        .unwrap_or_default()
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

    async fn load_project_config(
        &self,
        folders: &[PathBuf],
    ) -> (Option<serde_json::Value>, Vec<String>) {
        let folders = folders.to_vec();
        let found =
            tokio::task::spawn_blocking(move || crate::native::project_config::discover(&folders))
                .await
                .unwrap_or_default();
        (found.value, found.warnings)
    }
}

fn load_workspace_documents(workspace_folders: &[PathBuf]) -> WorkspaceIndex {
    let (candidates, stats) = collect_candidates(workspace_folders);
    parse_candidates(candidates, stats)
}

/// The `.surql` / `.surrealql` files under `workspace_folders`, in traversal
/// order, with the scan counters that traversal produced.
///
/// Split from [`parse_candidates`] so the command-line mode can supply an
/// explicit file list instead of a folder walk without duplicating the parse
/// half — see [`FilesystemWorkspaceLoader::load_paths`].
fn collect_candidates(workspace_folders: &[PathBuf]) -> (Vec<PathBuf>, WorkspaceScanStats) {
    let mut stats = WorkspaceScanStats::default();

    // First pass: gather candidate file paths sequentially (cheap,
    // IO-bound). The traversal order (and therefore which files win
    // under the cap) must stay identical to the pre-stats code.
    let mut candidates: Vec<PathBuf> = Vec::new();
    'outer: for folder in workspace_folders {
        for entry in WalkDir::new(folder)
            .into_iter()
            .filter_entry(|entry| should_descend(entry.path()))
        {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    stats.walk_errors += 1;
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }
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
                stats.skipped_oversize += 1;
                continue;
            }
            candidates.push(path.to_path_buf());
            if candidates.len() >= MAX_WORKSPACE_FILES {
                stats.file_cap_hit = true;
                break 'outer;
            }
        }
    }

    (candidates, stats)
}

/// Parse each candidate in parallel and collect them into an index.
fn parse_candidates(candidates: Vec<PathBuf>, mut stats: WorkspaceScanStats) -> WorkspaceIndex {
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
            handles.push(
                scope.spawn(move || -> (Vec<(Uri, Arc<DocumentAnalysis>)>, usize) {
                    let mut local = Vec::with_capacity(chunk.len());
                    let mut unreadable = 0usize;
                    for path in chunk {
                        let Some(uri) = Uri::from_file_path(&path) else {
                            continue;
                        };
                        let Some(text) = fs::read_to_string(&path).ok() else {
                            unreadable += 1;
                            continue;
                        };
                        if let Some(analysis) =
                            analyze_document(uri.clone(), &text, SymbolOrigin::Local)
                        {
                            local.push((uri, Arc::new(analysis)));
                        }
                    }
                    (local, unreadable)
                }),
            );
        }
        for handle in handles {
            if let Ok((results, unreadable)) = handle.join() {
                stats.skipped_unreadable += unreadable;
                for (uri, analysis) in results {
                    index.documents.insert(uri, analysis);
                }
            }
        }
    });

    index.scan_stats = stats;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("surrealql-workspace-{tag}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create");
        dir
    }

    fn write(dir: &Path, name: &str, body: &str) {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, body).expect("write");
    }

    #[test]
    fn only_surql_extensions_are_collected() {
        let dir = temp_dir("extensions");
        write(&dir, "a.surql", "DEFINE TABLE a SCHEMAFULL;");
        write(&dir, "b.surrealql", "DEFINE TABLE b SCHEMAFULL;");
        write(&dir, "c.sql", "DEFINE TABLE c SCHEMAFULL;");
        write(&dir, "d.txt", "not surrealql");

        let (candidates, stats) = collect_candidates(std::slice::from_ref(&dir));
        let mut names: Vec<String> = candidates
            .iter()
            .filter_map(|path| path.file_name()?.to_str().map(str::to_string))
            .collect();
        names.sort();
        assert_eq!(names, vec!["a.surql", "b.surrealql"]);
        assert_eq!(stats.walk_errors, 0);
    }

    /// A vendored dependency tree is not the project's schema, and walking one
    /// on a real repository dominates cold start.
    #[test]
    fn noisy_directories_are_skipped() {
        let dir = temp_dir("skips");
        write(&dir, "keep.surql", "DEFINE TABLE keep SCHEMAFULL;");
        for noisy in ["node_modules", "target", ".git", ".idea", ".gradle"] {
            write(
                &dir,
                &format!("{noisy}/skip.surql"),
                "DEFINE TABLE s SCHEMAFULL;",
            );
        }

        let (candidates, _) = collect_candidates(std::slice::from_ref(&dir));
        assert_eq!(candidates.len(), 1, "{candidates:?}");
        assert!(candidates[0].ends_with("keep.surql"));
    }

    #[test]
    fn an_oversize_file_is_skipped_and_counted() {
        let dir = temp_dir("oversize");
        write(&dir, "small.surql", "DEFINE TABLE a SCHEMAFULL;");
        let big = "-- padding\n".repeat((MAX_FILE_SIZE_BYTES as usize / 11) + 16);
        write(&dir, "big.surql", &big);

        let (candidates, stats) = collect_candidates(std::slice::from_ref(&dir));
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].ends_with("small.surql"));
        assert_eq!(
            stats.skipped_oversize, 1,
            "a skipped file must be counted, not silently dropped"
        );
    }

    #[test]
    fn nested_directories_are_walked() {
        let dir = temp_dir("nested");
        write(&dir, "a.surql", "DEFINE TABLE a SCHEMAFULL;");
        write(&dir, "one/b.surql", "DEFINE TABLE b SCHEMAFULL;");
        write(&dir, "one/two/c.surql", "DEFINE TABLE c SCHEMAFULL;");

        let (candidates, _) = collect_candidates(std::slice::from_ref(&dir));
        assert_eq!(candidates.len(), 3, "{candidates:?}");
    }

    #[test]
    fn parsing_produces_an_index_keyed_by_uri() {
        let dir = temp_dir("parse");
        write(&dir, "schema.surql", "DEFINE TABLE person SCHEMAFULL;");

        let (candidates, stats) = collect_candidates(std::slice::from_ref(&dir));
        let index = parse_candidates(candidates, stats);
        assert_eq!(index.documents.len(), 1);
        let analysis = index.documents.values().next().expect("one document");
        assert_eq!(analysis.tables.len(), 1);
        assert_eq!(analysis.tables[0].name, "person");
    }

    /// A file that is not valid UTF-8 is counted rather than failing the walk.
    #[test]
    fn an_unreadable_file_is_counted() {
        let dir = temp_dir("unreadable");
        write(&dir, "good.surql", "DEFINE TABLE a SCHEMAFULL;");
        fs::write(dir.join("bad.surql"), [0xff, 0xfe, 0xfd]).expect("write");

        let (candidates, stats) = collect_candidates(std::slice::from_ref(&dir));
        assert_eq!(candidates.len(), 2, "both are candidates");
        let index = parse_candidates(candidates, stats);
        assert_eq!(index.documents.len(), 1, "only the readable one parses");
        assert_eq!(index.scan_stats.skipped_unreadable, 1);
    }

    #[test]
    fn a_missing_folder_is_not_a_failure() {
        let (candidates, stats) =
            collect_candidates(&[PathBuf::from("/definitely/not/a/real/path")]);
        assert!(candidates.is_empty());
        assert!(stats.walk_errors > 0, "the error must be counted");
    }
}
