//! `format` — rewrite files in the canonical layout.

use std::path::PathBuf;
use std::process::ExitCode;

use crate::core::client::WorkspaceLoader;
use crate::native::workspace_fs::FilesystemWorkspaceLoader;

use super::{EXIT_FINDINGS, EXIT_OK, EXIT_USAGE};

pub async fn run(args: &[String]) -> ExitCode {
    let mut check_only = false;
    let mut paths: Vec<PathBuf> = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--check" => check_only = true,
            other if other.starts_with('-') => {
                eprintln!("surrealql-language-server: unknown option `{other}`.");
                return ExitCode::from(EXIT_USAGE);
            }
            path => paths.push(PathBuf::from(path)),
        }
    }
    if paths.is_empty() {
        paths.push(PathBuf::from("."));
    }

    // Reuse the workspace walk rather than writing a second one, so `format`
    // and `check` agree about which files belong to the project — including
    // the size and count caps.
    let loader = FilesystemWorkspaceLoader::new();
    let (directories, files): (Vec<PathBuf>, Vec<PathBuf>) =
        paths.into_iter().partition(|path| path.is_dir());
    let mut targets: Vec<PathBuf> = files;
    if !directories.is_empty() {
        let index = loader.load(&directories).await;
        for uri in index.documents.keys() {
            if let Some(path) = uri.to_file_path() {
                targets.push(path.into_owned());
            }
        }
    }
    targets.sort();
    targets.dedup();

    let mut changed = 0usize;
    let mut refused = 0usize;
    for path in &targets {
        let Ok(source) = std::fs::read_to_string(path) else {
            eprintln!(
                "surrealql-language-server: could not read {}",
                path.display()
            );
            return ExitCode::from(EXIT_USAGE);
        };
        let formatted = crate::format::format(&source);
        if formatted == source {
            // Either already canonical or refused. Only a document the grammar
            // cannot read is worth telling the user about.
            if source_has_error(&source) {
                refused += 1;
                eprintln!(
                    "surrealql-language-server: {} could not be parsed and was left alone",
                    path.display()
                );
            }
            continue;
        }
        changed += 1;
        if check_only {
            println!("would reformat {}", path.display());
        } else if let Err(error) = std::fs::write(path, &formatted) {
            eprintln!(
                "surrealql-language-server: could not write {}: {error}",
                path.display()
            );
            return ExitCode::from(EXIT_USAGE);
        } else {
            println!("reformatted {}", path.display());
        }
    }

    println!(
        "{} file(s): {changed} {}, {refused} unparseable.",
        targets.len(),
        if check_only {
            "would change"
        } else {
            "changed"
        },
    );

    if check_only && changed > 0 {
        ExitCode::from(EXIT_FINDINGS)
    } else {
        ExitCode::from(EXIT_OK)
    }
}

fn source_has_error(source: &str) -> bool {
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&crate::grammar::language()).is_err() {
        return true;
    }
    parser
        .parse(source, None)
        .map(|tree| tree.root_node().has_error())
        .unwrap_or(true)
}
