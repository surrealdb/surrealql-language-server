//! Headless `check` subcommand: one-shot diagnostics with no LSP lifecycle.
//!
//! `surreal-lsp check [paths…]` runs the same pipeline the
//! editor path publishes from — analyze, merge the workspace model, then
//! [`MergedSemanticModel::document_diagnostics`] — and renders the result as
//! human-readable text or as one JSON object. Coding agents, CI jobs and
//! pre-commit hooks all consume it. The JSON diagnostics are the LSP wire
//! objects verbatim (0-based lines, UTF-16 columns, camelCase keys), so the
//! stable codes in [`crate::semantic::codes`] and the `data` hints survive
//! unchanged; the text format is 1-based in both line and column.
//!
//! `check` never connects to a database. The model is built against an empty
//! [`LiveMetadataSnapshot`], so a `SURREALDB_ENDPOINT` in the environment (or
//! connection details in `--config`) has no effect here.
//!
//! The exit code is the contract agents key on, and it must never claim
//! coverage the run did not have: a target file the directory walk skipped
//! (oversize, unreadable, or past the file cap) is exit code 2, not a silent
//! pass. Skips in `--workspace` context directories only cost schema context,
//! so they are stderr warnings instead.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::builder::{PossibleValuesParser, TypedValueParser};
use clap::{Args, ValueEnum};
use ls_types::{Diagnostic, DiagnosticSeverity, NumberOrString, Uri};
use serde::Serialize;
use walkdir::WalkDir;

use crate::config::ServerSettings;
use crate::core::client::WorkspaceLoader;
use crate::core::server::build_version;
use crate::native::error::{self, Envelope};
use crate::native::workspace_fs::{
    FilesystemWorkspaceLoader, MAX_FILE_SIZE_BYTES, MAX_WORKSPACE_FILES, should_descend,
};
use crate::semantic::analyzer::analyze_document_with_limit;
use crate::semantic::types::{
    DocumentAnalysis, LiveMetadataSnapshot, MergedSemanticModel, SymbolOrigin,
};

/// How `check` renders the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
}

/// The command line for `check`, parsed by clap as a subcommand of
/// [`crate::native::cli::Cli`].
///
/// Every flag here is part of the contract `tests/compat.rs` pins: rename
/// one and a caller breaks.
#[derive(Debug, PartialEq, Eq, Args)]
pub struct CheckOptions {
    /// Files or directories to check. Directories are walked with the same
    /// skip rules as the LSP workspace scan (.surql/.surrealql only).
    #[arg(value_name = "PATHS", required_unless_present = "stdin")]
    pub paths: Vec<PathBuf>,

    /// Read the single target document from stdin. Positional paths are not
    /// accepted with --stdin; pass schema context via --workspace.
    #[arg(long, conflicts_with = "paths")]
    pub stdin: bool,

    /// The path the stdin content represents. The content shadows the
    /// on-disk file of that path. Needs --stdin.
    #[arg(long, value_name = "PATH")]
    pub stdin_filename: Option<PathBuf>,

    /// Directory analyzed for schema context only; never reported on.
    /// Repeatable.
    #[arg(long = "workspace", value_name = "DIR")]
    pub workspace_dirs: Vec<PathBuf>,

    #[arg(long, short = 'j', help = crate::native::cli::JSON_HELP, conflicts_with = "format")]
    pub json: bool,

    /// Output format. Hidden: `--json` is the documented spelling of
    /// `--format json`, and the flag every component of the `surreal` CLI
    /// shares. Still accepted, because scripts already pass it.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text, hide = true)]
    pub format: OutputFormat,

    /// Settings file; same shape as the LSP settings (a `surrealql` key or
    /// the keys at the root).
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Declare a variable your caller binds at runtime; suppresses
    /// undefined-variable for it. Repeatable.
    #[arg(long = "param", value_name = "NAME")]
    pub params: Vec<String>,

    // Stored as the severity's rank (1 = error … 4 = hint), so the
    // comparison against a diagnostic is a single `<=`.
    /// Lowest severity that fails the run.
    #[arg(
        long,
        value_name = "SEVERITY",
        default_value = "error",
        value_parser = PossibleValuesParser::new(["error", "warning", "info", "hint"])
            .map(|severity| fail_on_rank(&severity)),
    )]
    pub fail_on: u8,
}

impl CheckOptions {
    /// The format to render: `--json` is `--format json`.
    pub fn output_format(&self) -> OutputFormat {
        if self.json {
            OutputFormat::Json
        } else {
            self.format
        }
    }
}

/// The rank of a `--fail-on` severity. Only reached with one of the four
/// values the parser accepts.
fn fail_on_rank(severity: &str) -> u8 {
    match severity {
        "warning" => 2,
        "info" => 3,
        "hint" => 4,
        _ => 1,
    }
}

/// One checked file in the JSON report. `diagnostics` is the LSP wire
/// shape verbatim; every target appears, including clean ones, so an agent
/// can confirm coverage.
#[derive(Debug, Serialize)]
pub struct FileReport {
    pub path: String,
    pub diagnostics: Vec<Diagnostic>,
}

/// Severity totals over every reported diagnostic.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub files_checked: usize,
    pub errors: usize,
    pub warnings: usize,
    pub information: usize,
    pub hints: usize,
}

/// What the run failed to read, combined over the target walk and the
/// `--workspace` context walk. Only target skips drive the exit code, but
/// both kinds cost correctness, so both are visible.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub walk_errors: usize,
    pub skipped_oversize: usize,
    pub skipped_unreadable: usize,
    pub file_cap_hit: bool,
}

/// The complete machine-readable result. Field names and shape are a
/// compatibility surface pinned by `tests/compat.rs` — additive changes only.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckReport {
    pub version: String,
    pub files: Vec<FileReport>,
    pub summary: Summary,
    pub scan: ScanReport,
    pub config_warnings: Vec<String>,
    pub exit_code: u8,
}

/// Render the report as the single JSON object `--json` prints.
pub fn render_json(report: &CheckReport) -> String {
    let mut out = serde_json::to_string_pretty(report).unwrap_or_else(|_| "{}".to_string());
    out.push('\n');
    out
}

/// Render the report as the human-readable text `--format text` prints.
/// Lines and columns are 1-based; columns count UTF-16 code units, like the
/// LSP positions they come from.
pub fn render_text(report: &CheckReport) -> String {
    let mut out = String::new();
    for file in &report.files {
        for diagnostic in &file.diagnostics {
            let line = diagnostic.range.start.line + 1;
            let column = diagnostic.range.start.character + 1;
            let severity = severity_word(severity_rank(diagnostic));
            let code = match &diagnostic.code {
                Some(NumberOrString::String(code)) => code.clone(),
                Some(NumberOrString::Number(code)) => code.to_string(),
                None => String::new(),
            };
            out.push_str(&format!(
                "{}:{line}:{column} {severity}[{code}]: {}\n",
                file.path, diagnostic.message
            ));
        }
    }
    let mut tallies = vec![
        plural(report.summary.errors, "error"),
        plural(report.summary.warnings, "warning"),
    ];
    if report.summary.information > 0 {
        tallies.push(format!("{} information", report.summary.information));
    }
    if report.summary.hints > 0 {
        tallies.push(plural(report.summary.hints, "hint"));
    }
    out.push_str(&format!(
        "{} in {}\n",
        tallies.join(", "),
        plural(report.summary.files_checked, "file")
    ));
    out
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// Severity as a rank: 1 error … 4 hint. A diagnostic without a severity is
/// counted as an error — under-reporting would let a failure exit 0.
fn severity_rank(diagnostic: &Diagnostic) -> u8 {
    match diagnostic.severity {
        Some(DiagnosticSeverity::WARNING) => 2,
        Some(DiagnosticSeverity::INFORMATION) => 3,
        Some(DiagnosticSeverity::HINT) => 4,
        _ => 1,
    }
}

fn severity_word(rank: u8) -> &'static str {
    match rank {
        1 => "error",
        2 => "warning",
        3 => "information",
        _ => "hint",
    }
}

/// One target document: where it came from and what to call it in output.
struct Target {
    uri: Uri,
    display: String,
    text: String,
}

/// Everything `run` needs before analysis: the targets, plus the counters
/// from walking target directories.
struct Targets {
    targets: Vec<Target>,
    scan: ScanReport,
    /// True when any *target* was skipped — drives exit code 2.
    target_skipped: bool,
}

/// Run `check` to completion. Every early return is exit code 2 with the
/// reason on stderr, as the JSON error envelope under `--json`, and nothing
/// on stdout; a completed run prints its report and derives the exit code
/// from the diagnostics.
pub async fn run(options: CheckOptions) -> ExitCode {
    // Settings come from the same parser the LSP path uses, so a config file
    // an editor accepts is accepted here, warnings included.
    let config_value = match &options.config {
        Some(path) => match fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(value) => Some(value),
                Err(error) => {
                    let message = format!("`{}` is not valid JSON: {error}", path.display());
                    return fail(&options, failure(error::INVALID, message));
                }
            },
            Err(error) => {
                let message = format!("cannot read `{}`: {error}", path.display());
                return fail(&options, failure(io_kind(&error), message));
            }
        },
        None => None,
    };
    let (mut settings, config_warnings) =
        ServerSettings::from_sources_with_warnings(config_value.as_ref(), None);
    settings
        .analysis
        .external_params
        .extend(options.params.iter().cloned());

    // Context first: `--workspace` directories contribute schema and are
    // never reported on. Their scan losses cost context, not coverage.
    let mut index = FilesystemWorkspaceLoader::new()
        .load(&options.workspace_dirs)
        .await;
    let context_scan = index.scan_stats;

    let collected = match collect_targets(&options) {
        Ok(collected) => collected,
        Err(failure) => return fail(&options, failure),
    };

    // Analyze targets at the configured syntax cap (the context walk uses the
    // default cap, which is fine: context syntax diagnostics are never
    // reported). A target the analyzer cannot produce a tree for is a failed
    // read, not a clean file.
    let mut analyses: Vec<(String, Arc<DocumentAnalysis>)> = Vec::new();
    for target in collected.targets {
        let Some(analysis) = analyze_document_with_limit(
            target.uri.clone(),
            target.text,
            SymbolOrigin::Local,
            settings.analysis.max_syntax_diagnostics,
        ) else {
            let message = format!("cannot analyze `{}`", target.display);
            return fail(&options, failure(error::UNKNOWN, message));
        };
        let analysis = Arc::new(analysis);
        // A target shadows the same-uri context entry, exactly like an open
        // editor buffer shadows the saved file.
        index.documents.insert(target.uri, Arc::clone(&analysis));
        analyses.push((target.display, analysis));
    }

    // No database, ever: an empty snapshot instead of a metadata provider.
    let model = MergedSemanticModel::build(&index, &LiveMetadataSnapshot::default());

    analyses.sort_by(|a, b| a.0.cmp(&b.0));
    let mut summary = Summary {
        files_checked: analyses.len(),
        ..Summary::default()
    };
    let mut files = Vec::with_capacity(analyses.len());
    let mut worst_rank: u8 = u8::MAX;
    for (display, analysis) in analyses {
        let mut diagnostics = model.document_diagnostics(&analysis, &settings);
        diagnostics.sort_by(|a, b| {
            let key = |d: &Diagnostic| {
                (
                    d.range.start.line,
                    d.range.start.character,
                    code_of(d),
                    d.message.clone(),
                )
            };
            key(a).cmp(&key(b))
        });
        for diagnostic in &diagnostics {
            let rank = severity_rank(diagnostic);
            worst_rank = worst_rank.min(rank);
            match rank {
                1 => summary.errors += 1,
                2 => summary.warnings += 1,
                3 => summary.information += 1,
                _ => summary.hints += 1,
            }
        }
        files.push(FileReport {
            path: display,
            diagnostics,
        });
    }

    let exit_code = if collected.target_skipped {
        2
    } else if worst_rank <= options.fail_on {
        1
    } else {
        0
    };

    let report = CheckReport {
        version: build_version(),
        files,
        summary,
        scan: ScanReport {
            walk_errors: collected.scan.walk_errors + context_scan.walk_errors,
            skipped_oversize: collected.scan.skipped_oversize + context_scan.skipped_oversize,
            skipped_unreadable: collected.scan.skipped_unreadable + context_scan.skipped_unreadable,
            file_cap_hit: collected.scan.file_cap_hit || context_scan.file_cap_hit,
        },
        config_warnings,
        exit_code,
    };

    for warning in &report.config_warnings {
        eprintln!("warning: {warning}");
    }
    report_scan_losses(&report.scan, collected.target_skipped);

    match options.output_format() {
        OutputFormat::Text => print!("{}", render_text(&report)),
        OutputFormat::Json => print!("{}", render_json(&report)),
    }
    ExitCode::from(exit_code)
}

/// The exit code of a run that could not do what was asked: a missing or
/// unreadable input. 2, as the `check` contract has always said, under
/// `--json` too; the envelope's `kind` says which failure it was.
const CANNOT_CHECK: u8 = 2;

/// Why a run could not start: the error envelope's `kind`, and the message.
struct Failure {
    kind: &'static str,
    message: String,
}

fn failure(kind: &'static str, message: impl Into<String>) -> Failure {
    Failure {
        kind,
        message: message.into(),
    }
}

/// The envelope `kind` of a failed read: a missing path is `not_found`.
fn io_kind(error: &std::io::Error) -> &'static str {
    match error.kind() {
        std::io::ErrorKind::NotFound => error::NOT_FOUND,
        _ => error::UNKNOWN,
    }
}

/// Report a failure before the report and leave with its exit code.
///
/// Nothing has been written to stdout yet, and nothing will be. Under
/// `--json` the reason is the JSON error envelope on stderr; otherwise it is
/// one `error:` line there.
fn fail(options: &CheckOptions, failure: Failure) -> ExitCode {
    match options.output_format() {
        OutputFormat::Json => Envelope::new(failure.kind, failure.message, CANNOT_CHECK).eprint(),
        OutputFormat::Text => eprintln!("error: {}", failure.message),
    }
    ExitCode::from(CANNOT_CHECK)
}

fn code_of(diagnostic: &Diagnostic) -> String {
    match &diagnostic.code {
        Some(NumberOrString::String(code)) => code.clone(),
        Some(NumberOrString::Number(code)) => code.to_string(),
        None => String::new(),
    }
}

fn report_scan_losses(scan: &ScanReport, target_skipped: bool) {
    if scan.walk_errors == 0
        && scan.skipped_oversize == 0
        && scan.skipped_unreadable == 0
        && !scan.file_cap_hit
    {
        return;
    }
    let detail = format!(
        "{} walk error(s), {} oversize, {} unreadable, file cap hit: {}",
        scan.walk_errors, scan.skipped_oversize, scan.skipped_unreadable, scan.file_cap_hit
    );
    if target_skipped {
        eprintln!("error: the scan skipped target files ({detail})");
    } else {
        eprintln!("warning: the scan skipped context files ({detail})");
    }
}

/// Gather the target documents: explicit files verbatim (an unreadable one is
/// an immediate error), directories through the same walk rules as the LSP
/// scan, or stdin. Duplicate URIs keep the first occurrence.
fn collect_targets(options: &CheckOptions) -> Result<Targets, Failure> {
    let mut collected = Targets {
        targets: Vec::new(),
        scan: ScanReport::default(),
        target_skipped: false,
    };
    let mut seen = std::collections::HashSet::new();

    if options.stdin {
        let mut text = String::new();
        use std::io::Read;
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|error| failure(io_kind(&error), format!("cannot read stdin: {error}")))?;
        let (uri, display) = match &options.stdin_filename {
            Some(path) => {
                let absolute = std::path::absolute(path).map_err(|error| {
                    let message = format!("cannot resolve `{}`: {error}", path.display());
                    failure(io_kind(&error), message)
                })?;
                let uri = Uri::from_file_path(&absolute).ok_or_else(|| {
                    let message = format!("cannot make a URI for `{}`", absolute.display());
                    failure(error::INVALID, message)
                })?;
                (uri, display_path(path))
            }
            None => (
                "untitled:stdin"
                    .parse::<Uri>()
                    .map_err(|_| failure(error::UNKNOWN, "cannot make the stdin URI"))?,
                "<stdin>".to_string(),
            ),
        };
        collected.targets.push(Target { uri, display, text });
        return Ok(collected);
    }

    for path in &options.paths {
        let metadata = fs::metadata(path).map_err(|error| {
            let message = format!("cannot read `{}`: {error}", path.display());
            failure(io_kind(&error), message)
        })?;
        if metadata.is_dir() {
            walk_target_dir(path, &mut collected, &mut seen);
        } else {
            // A file named explicitly is checked no matter its extension,
            // but the size ceiling still holds: past it the parse is the
            // risk, and silently skipping a named file would be a lie.
            if metadata.len() > MAX_FILE_SIZE_BYTES {
                let message = format!(
                    "`{}` is over the {} MB limit",
                    path.display(),
                    MAX_FILE_SIZE_BYTES / (1024 * 1024)
                );
                return Err(failure(error::INVALID, message));
            }
            let text = fs::read_to_string(path).map_err(|error| {
                let message = format!("cannot read `{}`: {error}", path.display());
                failure(io_kind(&error), message)
            })?;
            push_target(path, text, &mut collected, &mut seen).map_err(|()| {
                let message = format!("cannot make a URI for `{}`", path.display());
                failure(error::INVALID, message)
            })?;
        }
    }
    Ok(collected)
}

/// Walk one target directory with the LSP scan's own rules (extension
/// filter, skip list, size ceiling, file cap). Anything skipped is recorded
/// and marks the run as incomplete.
fn walk_target_dir(dir: &Path, collected: &mut Targets, seen: &mut std::collections::HashSet<Uri>) {
    for entry in WalkDir::new(dir)
        .into_iter()
        .filter_entry(|entry| should_descend(entry.path()))
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                collected.scan.walk_errors += 1;
                collected.target_skipped = true;
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
        if collected.targets.len() >= MAX_WORKSPACE_FILES {
            collected.scan.file_cap_hit = true;
            collected.target_skipped = true;
            return;
        }
        if entry
            .metadata()
            .map(|meta| meta.len() > MAX_FILE_SIZE_BYTES)
            .unwrap_or(false)
        {
            collected.scan.skipped_oversize += 1;
            collected.target_skipped = true;
            continue;
        }
        let Ok(text) = fs::read_to_string(path) else {
            collected.scan.skipped_unreadable += 1;
            collected.target_skipped = true;
            continue;
        };
        // A URI failure inside a walk is an unreadable file for exit
        // purposes: the file exists and was not checked.
        if push_target(path, text, collected, seen).is_err() {
            collected.scan.skipped_unreadable += 1;
            collected.target_skipped = true;
        }
    }
}

fn push_target(
    path: &Path,
    text: String,
    collected: &mut Targets,
    seen: &mut std::collections::HashSet<Uri>,
) -> Result<(), ()> {
    let absolute = std::path::absolute(path).map_err(|_| ())?;
    let uri = Uri::from_file_path(&absolute).ok_or(())?;
    if seen.insert(uri.clone()) {
        collected.targets.push(Target {
            uri,
            display: display_path(path),
            text,
        });
    }
    Ok(())
}

/// Paths print relative to the current directory when they are under it,
/// absolute otherwise — stable input for editors and agents that jump to
/// `path:line:column`.
fn display_path(path: &Path) -> String {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    match std::env::current_dir()
        .ok()
        .and_then(|cwd| absolute.strip_prefix(cwd).ok().map(Path::to_path_buf))
    {
        Some(relative) => relative.display().to_string(),
        None => absolute.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ls_types::{Position, Range};

    fn sample_report() -> CheckReport {
        let diagnostic = Diagnostic {
            range: Range {
                start: Position {
                    line: 2,
                    character: 14,
                },
                end: Position {
                    line: 2,
                    character: 17,
                },
            },
            severity: Some(DiagnosticSeverity::ERROR),
            code: Some(NumberOrString::String("unknown-table".to_string())),
            source: Some("surreal-language-server".to_string()),
            message: "Table `usr` is not defined.".to_string(),
            ..Diagnostic::default()
        };
        CheckReport {
            version: "test".to_string(),
            files: vec![FileReport {
                path: "schema/users.surql".to_string(),
                diagnostics: vec![diagnostic],
            }],
            summary: Summary {
                files_checked: 1,
                errors: 1,
                ..Summary::default()
            },
            scan: ScanReport {
                skipped_oversize: 1,
                ..ScanReport::default()
            },
            config_warnings: vec![],
            exit_code: 1,
        }
    }

    #[test]
    fn text_output_is_one_based_and_carries_the_code() {
        let text = render_text(&sample_report());
        assert!(
            text.contains(
                "schema/users.surql:3:15 error[unknown-table]: Table `usr` is not defined."
            ),
            "got: {text}"
        );
        assert!(
            text.contains("1 error, 0 warnings in 1 file"),
            "got: {text}"
        );
    }

    #[test]
    fn json_output_surfaces_the_scan_losses() {
        let json = render_json(&sample_report());
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(value["scan"]["skippedOversize"], 1);
        assert_eq!(value["exitCode"], 1);
        // The wire diagnostic survives verbatim: 0-based range, string code.
        assert_eq!(
            value["files"][0]["diagnostics"][0]["range"]["start"]["line"],
            2
        );
        assert_eq!(value["files"][0]["diagnostics"][0]["code"], "unknown-table");
    }
}
