//! Headless `check` subcommand: one-shot diagnostics with no LSP lifecycle.
//!
//! `surrealql-language-server check [paths…]` runs the same pipeline the
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

use ls_types::{Diagnostic, DiagnosticSeverity, NumberOrString, Uri};
use serde::Serialize;
use walkdir::WalkDir;

use crate::config::ServerSettings;
use crate::core::client::WorkspaceLoader;
use crate::core::server::build_version;
use crate::native::workspace_fs::{
    FilesystemWorkspaceLoader, MAX_FILE_SIZE_BYTES, MAX_WORKSPACE_FILES, should_descend,
};
use crate::semantic::analyzer::analyze_document_with_limit;
use crate::semantic::text::LineIndex;
use crate::semantic::types::{
    DocumentAnalysis, LiveMetadataSnapshot, MergedSemanticModel, SymbolOrigin,
};

/// Usage text for `check`, shared with the `--help` paths in `main`.
pub const USAGE: &str = "\
Usage: surrealql-language-server check [OPTIONS] [PATHS...]

Check .surql files and report diagnostics.

Arguments:
  [PATHS...]                 Files or directories to check. Directories are
                             walked with the same skip rules as the LSP
                             workspace scan (.surql/.surrealql only).

Options:
      --stdin                Read the single target document from stdin.
                             Positional paths are not accepted with --stdin.
      --stdin-filename <p>   The path the stdin content represents. The
                             content shadows the on-disk file of that path.
      --workspace <dir>      Directory analyzed for schema context only;
                             never reported on. Repeatable.
      --format <text|json>   Output format (default: text).
      --config <file.json>   Settings file; same shape as the LSP settings
                             (a `surrealql` key or the keys at the root).
      --param <name>         Declare a variable your caller binds at runtime;
                             suppresses undefined-variable for it. Repeatable.
      --fail-on <severity>   Lowest severity that fails the run: error,
                             warning, info, or hint (default: error).
  -h, --help                 Print this help.

Subcommands:
  explain <code>             Print what one diagnostic code means, and how to
                             fix it: the same prose every diagnostic links to.

Exit codes:
  0  ran to completion, nothing at or above --fail-on
  1  ran to completion, diagnostics at or above --fail-on
  2  usage error, unreadable input, or a skipped target file";

/// How `check` renders the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Text,
    Json,
}

/// Parsed command line for `check`.
#[derive(Debug, PartialEq, Eq)]
pub struct CheckOptions {
    pub paths: Vec<PathBuf>,
    pub stdin: bool,
    pub stdin_filename: Option<PathBuf>,
    pub workspace_dirs: Vec<PathBuf>,
    pub format: OutputFormat,
    pub config: Option<PathBuf>,
    pub params: Vec<String>,
    /// Report only these codes. Empty means every code.
    pub only: Vec<String>,
    /// Never report these codes.
    ///
    /// Suppresses *reporting*, not analysis: an ignored code still runs, it
    /// just does not reach the output or the exit code. The report says which
    /// filters were in force, so a clean run cannot be mistaken for full
    /// coverage.
    pub ignore: Vec<String>,
    /// Codes to repair in place.
    ///
    /// Deliberately an allowlist rather than a switch. Most quick fixes here are
    /// string-distance guesses ("did you mean `person`?"), and applying one
    /// unattended can silently repoint a query at a *different real table*. Only
    /// `renamed-function` is mechanical enough: its replacement comes from
    /// SurrealDB's own rename table.
    pub fix: Vec<String>,
    /// Severity rank (1 = error … 4 = hint) at or above which the run
    /// exits 1. Stored as the rank so the comparison is a single `<=`.
    pub fail_on: u8,
}

/// Outcome of [`parse_args`]: a run, a help request, or an `explain`.
#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    Run(CheckOptions),
    Help,
    /// `check explain <code>`: print what one diagnostic code means.
    Explain(String),
}

/// The prose for every diagnostic code, compiled in.
///
/// The same file `Diagnostic.codeDescription` links to, so an agent offline or
/// behind a proxy reads exactly what a human clicking the link would, and the
/// two cannot drift. Native-only: the browser build has no `explain` and no
/// reason to carry the markdown.
const DIAGNOSTICS_DOC: &str = include_str!("../../docs/diagnostics.md");

/// The on-disk path for a target, when there is one.
///
/// `--stdin` has no file to rewrite, so a fix there is reported and not applied.
fn fixable_path(display: &str) -> Option<PathBuf> {
    let path = PathBuf::from(display);
    path.is_file().then_some(path)
}

/// Rewrite the renamed builtins in `text`, returning the new text and how many
/// were replaced.
///
/// Applied right-to-left so an earlier edit cannot shift the offsets of a later
/// one. Only `renamed-function` reaches here: see `CheckOptions::fix` for why
/// the allowlist is not a switch.
fn apply_renames(text: &str, lines: &LineIndex, diagnostics: &[Diagnostic]) -> (String, usize) {
    let mut edits: Vec<(usize, usize, &'static str)> = diagnostics
        .iter()
        .filter(|diagnostic| code_of(diagnostic) == crate::semantic::codes::RENAMED_FUNCTION)
        .filter_map(|diagnostic| {
            let start = lines.offset(text, diagnostic.range.start);
            let end = lines.offset(text, diagnostic.range.end);
            // The replacement comes from SurrealDB's own rename table, keyed on
            // the text in the diagnostic's own range, not from the message, and
            // not from a guess.
            let current = crate::grammar::renamed_builtin(text.get(start..end)?.trim())?;
            Some((start, end, current))
        })
        .collect();

    edits.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
    edits.dedup_by_key(|(start, _, _)| *start);

    let mut fixed = text.to_string();
    let applied = edits.len();
    for (start, end, replacement) in edits {
        fixed.replace_range(start..end, replacement);
    }
    (fixed, applied)
}

/// Accept a code only if this server can emit it.
///
/// A typo'd `--ignore parse-error` would otherwise filter nothing and look like
/// it worked, which is the failure mode a CI filter can least afford.
fn known_code(value: &str) -> Result<String, String> {
    if crate::semantic::codes::ALL.contains(&value) {
        return Ok(value.to_string());
    }
    Err(format!(
        "`{value}` is not a diagnostic code. Known codes: {}",
        known_codes().join(", ")
    ))
}

/// Every code `explain` will answer for.
pub fn known_codes() -> Vec<&'static str> {
    crate::semantic::codes::ALL.to_vec()
}

/// The section of [`DIAGNOSTICS_DOC`] describing `code`.
pub fn explain(code: &str) -> Option<String> {
    let heading = format!("## {code}\n");
    let start = DIAGNOSTICS_DOC.find(&heading)?;
    let body = &DIAGNOSTICS_DOC[start..];
    // Up to the next section, or the end of the file.
    let end = body[heading.len()..]
        .find("\n## ")
        .map(|offset| heading.len() + offset)
        .unwrap_or(body.len());
    Some(body[..end].trim_end().to_string())
}

/// Hand-rolled argument parser, following the `xtask` precedent — no
/// dependency enters the graph for a fixed flag set this small.
pub fn parse_args(args: impl Iterator<Item = String>) -> Result<Parsed, String> {
    let mut options = CheckOptions {
        paths: Vec::new(),
        stdin: false,
        stdin_filename: None,
        workspace_dirs: Vec::new(),
        format: OutputFormat::Text,
        config: None,
        params: Vec::new(),
        fail_on: 1,
        only: Vec::new(),
        ignore: Vec::new(),
        fix: Vec::new(),
    };
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        let value_for = |flag: &str, args: &mut std::iter::Peekable<_>| match args.next() {
            Some(value) => Ok(value),
            None => Err(format!("`{flag}` needs a value")),
        };
        match arg.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            // `check explain <code>`. A subcommand rather than a flag because it
            // does not check anything: it takes no paths and produces no report.
            "explain" if options.paths.is_empty() && !options.stdin => {
                let code = value_for(&arg, &mut args)?;
                if let Some(extra) = args.next() {
                    return Err(format!("`explain` takes one code, not `{extra}` as well"));
                }
                return Ok(Parsed::Explain(code));
            }
            "--stdin" => options.stdin = true,
            "--stdin-filename" => {
                options.stdin_filename = Some(PathBuf::from(value_for(&arg, &mut args)?));
            }
            "--workspace" => {
                options
                    .workspace_dirs
                    .push(PathBuf::from(value_for(&arg, &mut args)?));
            }
            "--format" => {
                options.format = match value_for(&arg, &mut args)?.as_str() {
                    "text" => OutputFormat::Text,
                    "json" => OutputFormat::Json,
                    other => return Err(format!("unknown format `{other}` (text or json)")),
                };
            }
            "--config" => {
                options.config = Some(PathBuf::from(value_for(&arg, &mut args)?));
            }
            "--param" => options.params.push(value_for(&arg, &mut args)?),
            "--only" => options.only.push(known_code(&value_for(&arg, &mut args)?)?),
            "--ignore" => options
                .ignore
                .push(known_code(&value_for(&arg, &mut args)?)?),
            "--fix" => {
                let code = known_code(&value_for(&arg, &mut args)?)?;
                if code != crate::semantic::codes::RENAMED_FUNCTION {
                    return Err(format!(
                        "`--fix {code}` is not supported. Only `renamed-function` can be \
                         applied unattended; every other fix is a suggestion whose \
                         replacement is inferred, and applying one blindly can change what \
                         a query means"
                    ));
                }
                options.fix.push(code);
            }
            "--fail-on" => {
                options.fail_on = match value_for(&arg, &mut args)?.as_str() {
                    "error" => 1,
                    "warning" => 2,
                    "info" => 3,
                    "hint" => 4,
                    other => {
                        return Err(format!(
                            "unknown severity `{other}` (error, warning, info or hint)"
                        ));
                    }
                };
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown argument `{other}`"));
            }
            path => options.paths.push(PathBuf::from(path)),
        }
    }
    if options.stdin && !options.paths.is_empty() {
        return Err("positional paths are not accepted with --stdin; \
                    pass schema context via --workspace"
            .to_string());
    }
    if options.stdin_filename.is_some() && !options.stdin {
        return Err("`--stdin-filename` needs `--stdin`".to_string());
    }
    if !options.stdin && options.paths.is_empty() {
        return Err("no input files (pass paths, or --stdin)".to_string());
    }
    Ok(Parsed::Run(options))
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

/// Why a run could not produce diagnostics.
///
/// `kind` is the stable machine field; `message` is prose that may be reworded.
/// Present only on an exit-2 report, and omitted entirely otherwise, so the
/// golden for a successful run is unchanged.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckError {
    pub kind: &'static str,
    pub message: String,
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
    /// Set only when the run could not complete. Skipped when absent so a
    /// clean report serialises exactly as it always has.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<CheckError>,
    /// Set only when `--only` or `--ignore` was used.
    ///
    /// Coverage has to be honest: without this, filtering every code that would
    /// have failed produces a report indistinguishable from a clean one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filters: Option<Filters>,
    /// How many diagnostics `--fix` repaired in place. Absent when none were.
    ///
    /// Rewriting a file is the only side effect `check` has; a run that did it
    /// has to say so.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixed: Option<usize>,
}

/// Which codes a run reported on, when it did not report on all of them.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Filters {
    pub only: Vec<String>,
    pub ignore: Vec<String>,
    /// How many diagnostics were produced and then not reported.
    pub suppressed: usize,
}

impl CheckReport {
    /// The report for a run that could not produce diagnostics.
    ///
    /// `--format json` prints exactly one JSON object on stdout for every exit
    /// code, including this one. Four paths used to write a plain sentence to
    /// stderr and exit 2 with stdout empty, which made every JSON consumer
    /// special-case "no output": the exact ambiguity the exit-code contract
    /// exists to remove.
    pub fn failed(kind: &'static str, message: String) -> Self {
        Self {
            version: build_version(),
            files: Vec::new(),
            summary: Summary::default(),
            scan: ScanReport::default(),
            config_warnings: Vec::new(),
            exit_code: 2,
            error: Some(CheckError { kind, message }),
            filters: None,
            fixed: None,
        }
    }
}

/// Render the report as the single JSON object `--format json` prints.
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

    // Both of these exist so a clean-looking run cannot be mistaken for a
    // complete one, or for one that changed nothing.
    if let Some(fixed) = report.fixed {
        out.push_str(&format!(
            "{} repaired in place\n",
            plural(fixed, "diagnostic")
        ));
    }
    if let Some(filters) = &report.filters
        && filters.suppressed > 0
    {
        out.push_str(&format!(
            "{} not reported because of --only/--ignore\n",
            plural(filters.suppressed, "diagnostic"),
        ));
    }
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
/// reason on stderr; a completed run prints its report and derives the exit
/// code from the diagnostics.
/// Report a run that could not complete, in whatever format the caller asked
/// for, and exit 2.
///
/// The message goes to stderr either way (a human running `--format json` in a
/// terminal should still see it), and, under `--format json`, stdout carries the
/// one object the contract promises: exactly one JSON object for every exit
/// code, so a consumer never has to special-case empty output.
fn fail(options: &CheckOptions, kind: &'static str, message: String) -> ExitCode {
    eprintln!("error: {message}");
    let report = CheckReport::failed(kind, message);
    if matches!(options.format, OutputFormat::Json) {
        print!("{}", render_json(&report));
    }
    ExitCode::from(report.exit_code)
}

pub async fn run(options: CheckOptions) -> ExitCode {
    // Settings come from the same parser the LSP path uses, so a config file
    // an editor accepts is accepted here, warnings included.
    let config_value = match &options.config {
        Some(path) => match fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(value) => Some(value),
                Err(error) => {
                    return fail(
                        &options,
                        "invalid-config",
                        format!("`{}` is not valid JSON: {error}", path.display()),
                    );
                }
            },
            Err(error) => {
                return fail(
                    &options,
                    "unreadable-input",
                    format!("cannot read `{}`: {error}", path.display()),
                );
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
        Err(message) => {
            return fail(&options, "unreadable-input", message);
        }
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
            return fail(
                &options,
                "analysis-failed",
                format!("cannot analyze `{}`", target.display),
            );
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
    let mut suppressed = 0usize;
    let mut fixed_count = 0usize;
    for (display, analysis) in analyses {
        let mut diagnostics = model.document_diagnostics(&analysis, &settings);

        // Repair before filtering, so `--ignore` cannot hide something that was
        // then silently rewritten. Writing the file is the only side effect
        // `check` has, so it happens only for codes explicitly named.
        if !options.fix.is_empty()
            && let Some(path) = fixable_path(&display)
        {
            let (fixed, applied) =
                apply_renames(&analysis.text, &analysis.line_index, &diagnostics);
            if applied > 0 {
                match fs::write(&path, &fixed) {
                    Ok(()) => {
                        fixed_count += applied;
                        // Re-analyse so the report describes the file as it now
                        // is, not as it was. Reporting the errors we just fixed
                        // would be actively misleading.
                        if let Some(reanalyzed) = analyze_document_with_limit(
                            analysis.uri.clone(),
                            fixed,
                            SymbolOrigin::Local,
                            settings.analysis.max_syntax_diagnostics,
                        ) {
                            diagnostics = model.document_diagnostics(&reanalyzed, &settings);
                        }
                    }
                    Err(error) => {
                        eprintln!("warning: could not write `{}`: {error}", path.display());
                    }
                }
            }
        }

        // Filter *reporting*, not analysis. An ignored check still runs; it
        // simply does not reach the output or the exit code, and the report
        // records that it happened, so a clean run cannot be read as full
        // coverage.
        let before = diagnostics.len();
        diagnostics.retain(|diagnostic| {
            let code = code_of(diagnostic);
            let wanted = options.only.is_empty() || options.only.contains(&code);
            let barred = options.ignore.contains(&code);
            wanted && !barred
        });
        suppressed += before - diagnostics.len();

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
        error: None,
        fixed: (fixed_count > 0).then_some(fixed_count),
        filters: (!options.only.is_empty() || !options.ignore.is_empty()).then(|| Filters {
            only: options.only.clone(),
            ignore: options.ignore.clone(),
            suppressed,
        }),
    };

    for warning in &report.config_warnings {
        eprintln!("warning: {warning}");
    }
    report_scan_losses(&report.scan, collected.target_skipped);

    match options.format {
        OutputFormat::Text => print!("{}", render_text(&report)),
        OutputFormat::Json => print!("{}", render_json(&report)),
    }
    ExitCode::from(exit_code)
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
fn collect_targets(options: &CheckOptions) -> Result<Targets, String> {
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
            .map_err(|error| format!("cannot read stdin: {error}"))?;
        let (uri, display) = match &options.stdin_filename {
            Some(path) => {
                let absolute = std::path::absolute(path)
                    .map_err(|error| format!("cannot resolve `{}`: {error}", path.display()))?;
                let uri = Uri::from_file_path(&absolute)
                    .ok_or_else(|| format!("cannot make a URI for `{}`", absolute.display()))?;
                (uri, display_path(path))
            }
            None => (
                "untitled:stdin"
                    .parse::<Uri>()
                    .map_err(|_| "cannot make the stdin URI".to_string())?,
                "<stdin>".to_string(),
            ),
        };
        collected.targets.push(Target { uri, display, text });
        return Ok(collected);
    }

    for path in &options.paths {
        let metadata = fs::metadata(path)
            .map_err(|error| format!("cannot read `{}`: {error}", path.display()))?;
        if metadata.is_dir() {
            walk_target_dir(path, &mut collected, &mut seen);
        } else {
            // A file named explicitly is checked no matter its extension,
            // but the size ceiling still holds: past it the parse is the
            // risk, and silently skipping a named file would be a lie.
            if metadata.len() > MAX_FILE_SIZE_BYTES {
                return Err(format!(
                    "`{}` is over the {} MB limit",
                    path.display(),
                    MAX_FILE_SIZE_BYTES / (1024 * 1024)
                ));
            }
            let text = fs::read_to_string(path)
                .map_err(|error| format!("cannot read `{}`: {error}", path.display()))?;
            push_target(path, text, &mut collected, &mut seen)
                .map_err(|()| format!("cannot make a URI for `{}`", path.display()))?;
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

    fn parse(args: &[&str]) -> Result<Parsed, String> {
        parse_args(args.iter().map(|arg| arg.to_string()))
    }

    fn parsed_options(args: &[&str]) -> CheckOptions {
        match parse(args).expect("parse") {
            Parsed::Run(options) => options,
            other => panic!("expected a run, got {other:?}"),
        }
    }

    #[test]
    fn defaults_are_text_format_and_fail_on_error() {
        let options = parsed_options(&["queries.surql"]);
        assert_eq!(options.format, OutputFormat::Text);
        assert_eq!(options.fail_on, 1);
        assert_eq!(options.paths, vec![PathBuf::from("queries.surql")]);
    }

    #[test]
    fn every_flag_parses() {
        let options = parsed_options(&[
            "--stdin",
            "--stdin-filename",
            "buffer.surql",
            "--workspace",
            "schema",
            "--workspace",
            "more",
            "--format",
            "json",
            "--config",
            "surql.json",
            "--param",
            "id",
            "--param",
            "limit",
            "--fail-on",
            "warning",
        ]);
        assert!(options.stdin);
        assert_eq!(options.stdin_filename, Some(PathBuf::from("buffer.surql")));
        assert_eq!(
            options.workspace_dirs,
            vec![PathBuf::from("schema"), PathBuf::from("more")]
        );
        assert_eq!(options.format, OutputFormat::Json);
        assert_eq!(options.config, Some(PathBuf::from("surql.json")));
        assert_eq!(options.params, vec!["id", "limit"]);
        assert_eq!(options.fail_on, 2);
    }

    #[test]
    fn help_is_not_a_usage_error() {
        assert_eq!(parse(&["--help"]), Ok(Parsed::Help));
        assert_eq!(parse(&["-h"]), Ok(Parsed::Help));
    }

    #[test]
    fn usage_errors_name_the_fault() {
        assert!(parse(&[]).unwrap_err().contains("no input files"));
        assert!(
            parse(&["--stdin", "a.surql"])
                .unwrap_err()
                .contains("--stdin")
        );
        assert!(
            parse(&["--stdin-filename", "a.surql", "a.surql"])
                .unwrap_err()
                .contains("--stdin")
        );
        assert!(parse(&["--nope"]).unwrap_err().contains("--nope"));
        assert!(parse(&["--format"]).unwrap_err().contains("needs a value"));
        assert!(
            parse(&["--format", "yaml", "a.surql"])
                .unwrap_err()
                .contains("yaml")
        );
        assert!(
            parse(&["--fail-on", "fatal", "a.surql"])
                .unwrap_err()
                .contains("fatal")
        );
    }

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
            error: None,
            filters: None,
            fixed: None,
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
