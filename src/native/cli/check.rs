//! `check` — analyze paths and report findings.

use std::path::PathBuf;
use std::process::ExitCode;

use ls_types::DiagnosticSeverity;

use crate::config::ServerSettings;
use crate::core::client::WorkspaceLoader;
use crate::native::project_config;
use crate::native::workspace_fs::FilesystemWorkspaceLoader;
use crate::semantic::pipeline::diagnostics_in;
use crate::semantic::rules::Requires;
use crate::semantic::types::{LiveMetadataSnapshot, MergedSemanticModel, WorkspaceIndex};

use super::report::{human_line, json_value};
use super::{EXIT_FINDINGS, EXIT_OK, EXIT_USAGE};

#[derive(Default)]
struct Options {
    paths: Vec<PathBuf>,
    json: bool,
    quiet: bool,
    no_config: bool,
    overrides: Vec<(String, String)>,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "--format" => {
                index += 1;
                match args.get(index).map(String::as_str) {
                    Some("json") => options.json = true,
                    Some("human") => options.json = false,
                    Some(other) => return Err(format!("unknown --format `{other}`.")),
                    None => return Err("--format needs a value.".to_string()),
                }
            }
            "--rule" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Err("--rule needs ID=SEVERITY.".to_string());
                };
                let Some((id, severity)) = value.split_once('=') else {
                    return Err(format!("--rule `{value}` must be ID=SEVERITY."));
                };
                options
                    .overrides
                    .push((id.to_string(), severity.to_string()));
            }
            "--no-config" => options.no_config = true,
            "--quiet" => options.quiet = true,
            other if other.starts_with('-') => {
                return Err(format!("unknown option `{other}`."));
            }
            path => options.paths.push(PathBuf::from(path)),
        }
        index += 1;
    }
    if options.paths.is_empty() {
        options.paths.push(PathBuf::from("."));
    }
    Ok(options)
}

pub async fn run(args: &[String]) -> ExitCode {
    let options = match parse(args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("surrealql-language-server: {message}");
            return ExitCode::from(EXIT_USAGE);
        }
    };

    let (directories, files): (Vec<PathBuf>, Vec<PathBuf>) = options
        .paths
        .iter()
        .cloned()
        .partition(|path| path.is_dir());
    for path in options.paths.iter() {
        if !path.exists() {
            eprintln!(
                "surrealql-language-server: no such path `{}`.",
                path.display()
            );
            return ExitCode::from(EXIT_USAGE);
        }
    }

    // The project file is looked up from the first directory argument, or the
    // parent of the first file — the same root the editor would use.
    let config_root = directories.first().cloned().or_else(|| {
        files
            .first()
            .and_then(|file| file.parent().map(PathBuf::from))
    });
    let mut settings = match (options.no_config, config_root) {
        (false, Some(root)) => {
            let found = project_config::discover(&[root]);
            for warning in &found.warnings {
                eprintln!("surrealql-language-server: {warning}");
            }
            ServerSettings::from_sources_with_project(found.value.as_ref(), None, None).0
        }
        _ => ServerSettings::default(),
    };
    for (id, severity) in &options.overrides {
        settings
            .analysis
            .rule_severity
            .insert(id.clone(), severity.clone());
    }
    // `--rule` bypasses the file, so it has to be validated the same way.
    for warning in settings.validate_and_repair() {
        eprintln!("surrealql-language-server: {warning}");
    }

    let loader = FilesystemWorkspaceLoader::new();
    let mut index = WorkspaceIndex::default();
    if !directories.is_empty() {
        index = loader.load(&directories).await;
    }
    if !files.is_empty() {
        let extra = loader.load_paths(&files).await;
        for (uri, analysis) in extra.documents {
            index.documents.insert(uri, analysis);
        }
    }

    let model = MergedSemanticModel::build(&index, &LiveMetadataSnapshot::default());

    // The workspace and an auth context, but no live database.
    //
    // `LIVE_METADATA` is what a command-line run genuinely lacks, and
    // withholding it is what stands `unknown-table` down instead of reporting
    // every table in the workspace as undefined. `AUTH_CONTEXT` comes from the
    // settings, which a command-line run has as much as the editor does —
    // withholding it as well turned off the permission rules for no reason.
    let mut findings: Vec<(String, ls_types::Diagnostic)> = Vec::new();
    let available = Requires::MODEL.union(Requires::AUTH_CONTEXT);
    let mut documents: Vec<_> = index.documents.iter().collect();
    documents.sort_by_key(|(uri, _)| uri.as_str().to_string());
    for (uri, analysis) in documents {
        for diagnostic in diagnostics_in(analysis, &model, &settings, available) {
            findings.push((display_path(uri), diagnostic));
        }
    }

    let errors = findings
        .iter()
        .filter(|(_, diagnostic)| diagnostic.severity == Some(DiagnosticSeverity::ERROR))
        .count();

    if options.json {
        let payload = serde_json::json!({
            "version": 1,
            "diagnostics": findings
                .iter()
                .map(|(path, diagnostic)| json_value(path, diagnostic))
                .collect::<Vec<_>>(),
            "summary": {
                "files": index.documents.len(),
                "findings": findings.len(),
                "errors": errors,
            },
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
    } else {
        for (path, diagnostic) in &findings {
            println!("{}", human_line(path, diagnostic));
        }
        if !options.quiet {
            if findings.is_empty() {
                println!(
                    "Checked {} file(s). Nothing to report.",
                    index.documents.len()
                );
            } else {
                println!();
                println!(
                    "Checked {} file(s): {} finding(s), {} error(s).",
                    index.documents.len(),
                    findings.len(),
                    errors,
                );
            }
        }
    }

    if errors > 0 {
        ExitCode::from(EXIT_FINDINGS)
    } else {
        ExitCode::from(EXIT_OK)
    }
}

/// A path relative to the working directory where possible — an absolute path
/// in every line makes real output unreadable.
fn display_path(uri: &ls_types::Uri) -> String {
    let Some(path) = uri.to_file_path() else {
        return uri.as_str().to_string();
    };
    let path = path.into_owned();
    std::env::current_dir()
        .ok()
        .and_then(|cwd| path.strip_prefix(&cwd).ok().map(PathBuf::from))
        .unwrap_or(path)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn no_path_means_the_working_directory() {
        let options = parse(&args(&[])).expect("parsed");
        assert_eq!(options.paths, vec![PathBuf::from(".")]);
    }

    #[test]
    fn paths_accumulate_in_order() {
        let options = parse(&args(&["a.surql", "schema/"])).expect("parsed");
        assert_eq!(
            options.paths,
            vec![PathBuf::from("a.surql"), PathBuf::from("schema/")]
        );
    }

    #[test]
    fn the_format_option_selects_json() {
        assert!(parse(&args(&["--format", "json"])).expect("parsed").json);
        assert!(!parse(&args(&["--format", "human"])).expect("parsed").json);
    }

    #[test]
    fn an_unknown_format_is_a_usage_error() {
        assert!(parse(&args(&["--format", "xml"])).is_err());
        assert!(parse(&args(&["--format"])).is_err());
    }

    #[test]
    fn rule_overrides_are_repeatable() {
        let options = parse(&args(&[
            "--rule",
            "let-type=off",
            "--rule",
            "unknown-table=error",
        ]))
        .expect("parsed");
        assert_eq!(
            options.overrides,
            vec![
                ("let-type".to_string(), "off".to_string()),
                ("unknown-table".to_string(), "error".to_string()),
            ]
        );
    }

    #[test]
    fn a_rule_override_needs_an_equals_sign() {
        assert!(parse(&args(&["--rule", "let-type"])).is_err());
        assert!(parse(&args(&["--rule"])).is_err());
    }

    #[test]
    fn unknown_options_are_a_usage_error() {
        assert!(parse(&args(&["--nonsense"])).is_err());
    }

    /// A path that begins with a dash is an option, not a file. Treating it as
    /// a path would turn a typo into a confusing "no such path".
    #[test]
    fn the_flags_are_recognised() {
        let options = parse(&args(&["--quiet", "--no-config", "x.surql"])).expect("parsed");
        assert!(options.quiet);
        assert!(options.no_config);
        assert_eq!(options.paths, vec![PathBuf::from("x.surql")]);
    }
}
