//! The command line: `surreal-lsp`, which the `surreal` launcher runs as
//! `surreal lsp`.
//!
//! `stdio` serves the Language Server Protocol over stdio, `check` runs the
//! one-shot diagnostics mode from [`crate::native::check`], and `version`
//! says which release and build this is. Anything else
//! is a usage error on stderr with exit code 2, so a typo cannot silently
//! start a server that eats the terminal's stdin.
//!
//! What no command at all does depends on who started the program:
//!
//! - **Through the `surreal` launcher** ([`LAUNCHER_VAR`] set and not empty),
//!   it prints help, as every other component of the `surreal` CLI does.
//!   `surreal lsp stdio` is how an editor starts the server.
//! - **Otherwise**, the legacy flow: no command, or `--stdio`, serves over
//!   stdio. The editor extensions start the bare executable and depend on
//!   it, so it stays for as long as anything does.
//!
//! Serving writes nothing of its own on either stream, under either name:
//! stdout is the JSON-RPC channel, and editors show every stderr line as a
//! server message or treat it as a failure. No banner, no notice, no
//! deprecation line. `tests/stdio.rs` holds the server to that byte for byte.
//!
//! Parsed with clap, the same way as the other components of the `surreal`
//! CLI: `--version` and `--help` are clap's, and help names the command the
//! way the launcher says it was typed ([`INVOKED_AS_VAR`]). A usage error
//! from a caller that asked for `--json` is the JSON error envelope from
//! [`crate::native::error`] instead of clap's text.

use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

use crate::native::check::{self, CheckOptions, OutputFormat};
use crate::native::error::{self, Envelope};

const ABOUT: &str = "The SurrealQL language server, for editors";

/// `X.Y.Z`, what `-V` prints and the launcher checks an install against.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// `X.Y.Z (master-vX.Y.Z-3-gabc1234, grammar cb2e6b5)`, what `--version`
/// prints: the release, and which source and grammar it was built from.
///
/// The same string as [`crate::core::server::build_version`], built with
/// `concat!` so it is a `&'static str` clap can take directly.
pub const LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("SURREALQL_LS_BUILD"),
    ")"
);

/// The help text of `--json`, the same in every component of the `surreal`
/// CLI.
pub const JSON_HELP: &str = "Emit machine-readable JSON on stdout";

/// What `check --help` adds below the flags: the exit codes agents key on.
const CHECK_EXIT_CODES: &str = "\
Exit codes:
  0  ran to completion, nothing at or above --fail-on
  1  ran to completion, diagnostics at or above --fail-on
  2  usage error, unreadable input, or a skipped target file";

#[derive(Parser, Debug)]
#[command(name = "surreal-lsp", bin_name = "surreal-lsp")]
#[command(about = ABOUT, long_about = None)]
#[command(version = VERSION, long_version = LONG_VERSION)]
#[command(args_conflicts_with_subcommands = true)]
pub struct Cli {
    /// Legacy: serve over stdio, as no command does outside the launcher.
    /// Hidden, because `stdio` is the command to document; accepted because
    /// LSP clients append it to whatever they start.
    #[arg(long, hide = true)]
    pub stdio: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Serve the Language Server Protocol over stdio, for an editor.
    Stdio(StdioOptions),

    /// Check .surql files and report diagnostics.
    #[command(after_help = CHECK_EXIT_CODES)]
    Check(CheckOptions),

    /// Print the version, and the source and grammar it was built from.
    Version(VersionOptions),
}

/// `stdio` takes no options of its own.
#[derive(clap::Args, Debug)]
pub struct StdioOptions {
    /// Accepted and ignored: LSP clients append `--stdio` to whatever command
    /// they are configured with, which here is already `stdio`.
    #[arg(long = "stdio", hide = true)]
    pub stdio: bool,
}

/// The options of `version`.
#[derive(clap::Args, Debug)]
pub struct VersionOptions {
    #[arg(long, short = 'j', help = JSON_HELP)]
    pub json: bool,
}

/// What `version --json` prints: the release, then what the build recorded
/// about itself. A part the build could not find out is `null`.
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub struct VersionReport {
    /// The release, `X.Y.Z`: what `-V` prints.
    pub version: &'static str,
    /// The short revision of this repository the binary was built from.
    pub commit: Option<&'static str>,
    /// The Rust target triple, such as `aarch64-apple-darwin`.
    pub target: Option<&'static str>,
    /// `git describe` of the source, with the branch: what `--version`
    /// shows in brackets before the grammar.
    pub build: Option<&'static str>,
    /// The short revision of the tree-sitter grammar compiled in.
    pub grammar: Option<&'static str>,
}

impl VersionReport {
    /// This binary's report, from what `build.rs` recorded.
    pub fn current() -> Self {
        let known = |value: &'static str| (value != "unknown").then_some(value);
        Self {
            version: VERSION,
            commit: known(env!("SURREALQL_LS_COMMIT")),
            target: known(env!("SURREALQL_LS_TARGET")),
            build: known(env!("SURREALQL_LS_SOURCE")),
            grammar: known(env!("SURREALQL_LS_GRAMMAR")),
        }
    }
}

/// Print the version: as `--version` prints it, `name` first, or as one JSON
/// object.
fn version(name: &str, options: &VersionOptions) {
    if options.json {
        let report = serde_json::to_string_pretty(&VersionReport::current())
            .expect("the version report serializes");
        println!("{report}");
    } else {
        println!("{name} {LONG_VERSION}");
    }
}

impl Cli {
    /// Parse the process's arguments, naming the program `name` in help and
    /// `--version` unless the launcher says how it was reached.
    ///
    /// Not `Cli::parse`: the name differs between the two executables, and
    /// the launcher's name has to be applied to the built command before it
    /// parses anything.
    pub fn from_env(name: &'static str) -> Self {
        let mut command = Self::command().name(name).bin_name(name);
        if let Some(invoked) = invoked_as(std::env::var(INVOKED_AS_VAR).ok().as_deref()) {
            command = command.bin_name(invoked);
        }
        if launched(std::env::var_os(LAUNCHER_VAR).as_deref()) {
            command = launcher_flow(command);
        }
        let program = command
            .get_bin_name()
            .unwrap_or_else(|| command.get_name())
            .to_owned();
        let argv: Vec<std::ffi::OsString> = std::env::args_os().collect();
        Self::parse_with(&mut command, argv.iter().cloned())
            .unwrap_or_else(|err| parse_failure(&err, &command, &argv, &program))
    }

    /// Parse `args` against `command`, with the rules clap cannot state.
    ///
    /// - `--stdin-filename` needs `--stdin`, but clap counts a flag's
    ///   implicit `false` as present, so `requires = "stdin"` never fires.
    /// - `--json` conflicts with `--format text` only: `--format json` says
    ///   the same thing, and clap's `conflicts_with` cannot tell the values
    ///   apart.
    ///
    /// Both are reported as clap reports any other usage error: on stderr,
    /// with the usage line, exit code 2.
    fn parse_with(
        command: &mut clap::Command,
        args: impl IntoIterator<Item = impl Into<std::ffi::OsString> + Clone>,
    ) -> Result<Self, clap::Error> {
        use clap::error::ErrorKind;

        let cli = Self::from_arg_matches(&command.try_get_matches_from_mut(args)?)?;
        if let Some(Command::Check(options)) = &cli.command {
            let refusal = if options.stdin_filename.is_some() && !options.stdin {
                Some((
                    ErrorKind::MissingRequiredArgument,
                    "`--stdin-filename` needs `--stdin`",
                ))
            } else if options.json && options.format == Some(OutputFormat::Text) {
                Some((
                    ErrorKind::ArgumentConflict,
                    "the argument '--json' cannot be used with '--format text'",
                ))
            } else {
                None
            };
            if let Some((kind, message)) = refusal {
                let check = command
                    .find_subcommand_mut("check")
                    .expect("`check` is a subcommand");
                return Err(check.error(kind, message));
            }
        }
        Ok(cli)
    }
}

/// Leave after a parse that did not produce a [`Cli`].
///
/// Help and `--version` are clap's, on stdout with exit code 0, whatever else
/// was passed. A usage error is exit code 2. Under `--json`, `-j` or
/// `--format json` (up to a `--`; see [`error::asks_for_json`]) it is the
/// JSON error envelope on stderr instead of clap's text, as in every other
/// component of the `surreal` CLI: a script that asked for JSON reads stderr
/// as JSON, and clap's prose there is a second failure.
fn parse_failure(
    err: &clap::Error,
    command: &clap::Command,
    argv: &[std::ffi::OsString],
    program: &str,
) -> ! {
    if err.use_stderr() && error::asks_for_json(command, argv) {
        let envelope = Envelope::usage(err, program);
        envelope.eprint();
        std::process::exit(i32::from(envelope.exit_code));
    }
    err.exit()
}

/// The variable the `surreal` launcher sets on the component it runs, naming
/// how the user reached it: `surreal lsp`.
///
/// Read so `--help` says `Usage: surreal lsp check` to somebody who typed
/// exactly that.
pub const INVOKED_AS_VAR: &str = "SURREAL_INVOKED_AS";

/// The variable the `surreal` launcher sets on every component it runs: the
/// launcher's own path.
///
/// Its presence switches no command from serving to printing help. Only the
/// launcher sets it, so an editor that starts the executable directly keeps
/// the legacy flow it depends on.
pub const LAUNCHER_VAR: &str = "SURREAL_CLI_LAUNCHER";

/// Whether the `surreal` launcher started this process: [`LAUNCHER_VAR`] set
/// and not empty.
fn launched(raw: Option<&std::ffi::OsStr>) -> bool {
    raw.is_some_and(|value| !value.is_empty())
}

/// The standard flow, as the launcher runs it: no command prints help.
///
/// Through clap's own rule, so it reads like every other component of the
/// `surreal` CLI: the full help on stderr, exit code 2. The usage line is
/// spelled out because clap's would also offer the bare command, which here
/// only prints this help.
fn launcher_flow(command: clap::Command) -> clap::Command {
    let bin = command
        .get_bin_name()
        .unwrap_or_else(|| command.get_name())
        .to_owned();
    command
        .arg_required_else_help(true)
        .override_usage(format!("{bin} <COMMAND>"))
}

/// The name to show in help, when the launcher supplied a usable one.
///
/// Absent, empty or odd values keep the executable's own name. The value
/// lands in help text on stdout, so it is held to what a command name can
/// plausibly contain rather than trusted with control characters.
fn invoked_as(raw: Option<&str>) -> Option<String> {
    let name = raw?.trim();
    let plausible = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'));
    plausible.then(|| name.to_owned())
}

/// Run the program under `name`: the executable's own name, which help and
/// the panic line use when the launcher does not supply one.
pub async fn run(name: &'static str) -> ExitCode {
    // The release profile aborts on panic, so this hook is the only trace a
    // crash leaves. It must write to stderr: stdout carries the LSP wire (or
    // the check report) and editors surface stderr in their output panel.
    std::panic::set_hook(Box::new(move |info| {
        eprintln!("{name} panicked: {info}");
    }));

    match Cli::from_env(name).command {
        // `None` is the legacy flow; under the launcher it never gets here,
        // because no command prints help.
        None | Some(Command::Stdio(_)) => {
            serve().await;
            ExitCode::SUCCESS
        }
        Some(Command::Check(options)) => check::run(options).await,
        Some(Command::Version(options)) => {
            version(name, &options);
            ExitCode::SUCCESS
        }
    }
}

/// Serve the Language Server Protocol over stdio until the client exits.
async fn serve() {
    use tower_lsp_server::{LspService, Server};

    use crate::native::Backend;

    let (service, socket) = LspService::new(Backend::new);
    Server::new(tokio::io::stdin(), tokio::io::stdout(), socket)
        .serve(service)
        .await;
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        let argv = std::iter::once("surreal-lsp").chain(args.iter().copied());
        Cli::parse_with(&mut Cli::command(), argv)
    }

    /// Parse as the `surreal` launcher runs the program.
    fn parse_launched(args: &[&str]) -> Result<Cli, clap::Error> {
        let argv = std::iter::once("surreal-lsp").chain(args.iter().copied());
        Cli::parse_with(&mut launcher_flow(Cli::command()), argv)
    }

    fn check_options(args: &[&str]) -> CheckOptions {
        let mut argv = vec!["check"];
        argv.extend_from_slice(args);
        match parse(&argv).expect("parse").command {
            Some(Command::Check(options)) => options,
            other => panic!("expected check, got {other:?}"),
        }
    }

    /// Catches duplicate argument ids, conflicting shorts and bad
    /// relationships between arguments in one assertion.
    #[test]
    fn the_command_tree_is_well_formed() {
        Cli::command().debug_assert();
    }

    /// The legacy flow: editors start the bare executable, or append
    /// `--stdio`. Both serve.
    #[test]
    fn no_command_and_stdio_both_serve() {
        assert!(parse(&[]).expect("bare").command.is_none());
        let cli = parse(&["--stdio"]).expect("--stdio");
        assert!(cli.stdio);
        assert!(cli.command.is_none());
    }

    /// The standard flow: `stdio` serves, with or without the `--stdio` an
    /// LSP client appends, under the launcher and outside it.
    #[test]
    fn the_stdio_command_serves() {
        for parse in [parse, parse_launched] {
            for args in [&["stdio"][..], &["stdio", "--stdio"]] {
                let cli = parse(args).expect("stdio");
                assert!(matches!(cli.command, Some(Command::Stdio(_))), "{args:?}");
            }
        }
    }

    /// Under the launcher no command is a request for help, not a server
    /// waiting on the terminal's stdin.
    #[test]
    fn under_the_launcher_no_command_prints_help() {
        let err = parse_launched(&[]).unwrap_err();
        assert_eq!(
            err.kind(),
            clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
        let help = err.to_string();
        assert!(help.contains("stdio"), "{help}");
        assert!(help.contains("check"), "{help}");
        assert!(help.contains("Usage: surreal-lsp <COMMAND>\n"), "{help}");
        // The legacy flag stays out of the help it prints.
        assert!(!help.contains("--stdio"), "{help}");
    }

    /// The legacy flag still serves under the launcher, so an editor set up
    /// with `surreal lsp --stdio` keeps working.
    #[test]
    fn under_the_launcher_the_legacy_flag_still_serves() {
        let cli = parse_launched(&["--stdio"]).expect("--stdio");
        assert!(cli.stdio);
        assert!(cli.command.is_none());
    }

    #[test]
    fn only_a_non_empty_launcher_variable_counts() {
        use std::ffi::OsStr;
        assert!(launched(Some(OsStr::new("/usr/local/bin/surreal"))));
        assert!(!launched(Some(OsStr::new(""))));
        assert!(!launched(None));
    }

    /// A typo must be refused rather than start a server that waits on the
    /// terminal's stdin.
    #[test]
    fn an_unknown_argument_is_a_usage_error() {
        let err = parse(&["--frobnicate"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
        assert_eq!(err.exit_code(), 2);
        let err = parse(&["serve"]).unwrap_err();
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn check_defaults_are_text_format_and_fail_on_error() {
        let options = check_options(&["queries.surql"]);
        assert_eq!(options.output_format(), OutputFormat::Text);
        assert_eq!(options.fail_on, 1);
        assert_eq!(options.paths, vec![PathBuf::from("queries.surql")]);
    }

    #[test]
    fn every_check_flag_parses() {
        let options = check_options(&[
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
        assert_eq!(options.output_format(), OutputFormat::Json);
        assert_eq!(options.config, Some(PathBuf::from("surql.json")));
        assert_eq!(options.params, vec!["id", "limit"]);
        assert_eq!(options.fail_on, 2);
    }

    /// `--json` is the documented spelling of `--format json`, which stays
    /// accepted for the scripts that already pass it.
    #[test]
    fn check_json_is_format_json() {
        for flag in ["--json", "-j"] {
            let options = check_options(&["a.surql", flag]);
            assert_eq!(options.output_format(), OutputFormat::Json, "{flag}");
        }
        let options = check_options(&["a.surql", "--format", "json"]);
        assert_eq!(options.output_format(), OutputFormat::Json);
        let options = check_options(&["a.surql", "--format", "text"]);
        assert_eq!(options.output_format(), OutputFormat::Text);
        // Asking for both is a contradiction, not a precedence rule.
        let err = parse(&["check", "a.surql", "--json", "--format", "text"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
        assert_eq!(err.exit_code(), 2);
    }

    /// Saying JSON twice is redundant, not a contradiction.
    #[test]
    fn check_json_with_format_json_is_json() {
        for args in [
            &["a.surql", "--json", "--format", "json"][..],
            &["a.surql", "--format", "json", "-j"],
            &["a.surql", "--format=json", "--json"],
        ] {
            assert_eq!(
                check_options(args).output_format(),
                OutputFormat::Json,
                "{args:?}"
            );
        }
    }

    /// Help documents `--json`, worded as in every other component, and not
    /// `--format`.
    #[test]
    fn check_help_offers_json_and_hides_format() {
        let err = parse(&["check", "--help"]).unwrap_err();
        let help = err.to_string();
        assert!(
            help.contains("-j, --json") && help.contains(JSON_HELP),
            "{help}"
        );
        assert!(!help.contains("--format"), "{help}");
    }

    #[test]
    fn every_fail_on_severity_has_its_rank() {
        for (word, rank) in [("error", 1), ("warning", 2), ("info", 3), ("hint", 4)] {
            assert_eq!(check_options(&["a.surql", "--fail-on", word]).fail_on, rank);
        }
    }

    /// Exit code 2 is the `check` contract for a usage error, and clap's
    /// own exit code for one, so every refusal below exits 2.
    #[test]
    fn check_usage_errors_name_the_fault_and_exit_two() {
        let fault = |args: &[&str]| {
            let mut argv = vec!["check"];
            argv.extend_from_slice(args);
            let err = parse(&argv).unwrap_err();
            assert_eq!(err.exit_code(), 2, "{args:?}");
            err.to_string()
        };
        assert!(fault(&[]).contains("PATHS"));
        assert!(fault(&["--stdin", "a.surql"]).contains("--stdin"));
        assert!(fault(&["--stdin-filename", "a.surql", "a.surql"]).contains("--stdin"));
        assert!(fault(&["--nope", "a.surql"]).contains("--nope"));
        assert!(fault(&["--format"]).contains("--format"));
        assert!(fault(&["--format", "yaml", "a.surql"]).contains("yaml"));
        assert!(fault(&["--fail-on", "fatal", "a.surql"]).contains("fatal"));
    }

    #[test]
    fn help_is_not_a_usage_error() {
        for args in [
            &["--help"][..],
            &["-h"],
            &["check", "--help"],
            &["check", "-h"],
        ] {
            let err = parse(args).unwrap_err();
            assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
            assert_eq!(err.exit_code(), 0);
        }
    }

    /// The launcher finds a release's version as the first semver token
    /// `--version` prints, and refuses an install whose version disagrees
    /// with the one it asked for.
    #[test]
    fn the_version_line_leads_with_the_release() {
        assert!(LONG_VERSION.starts_with(&format!("{VERSION} (")));
        assert_eq!(LONG_VERSION, crate::core::server::build_version());
    }

    #[test]
    fn the_version_command_takes_json_long_and_short() {
        for (args, json) in [
            (&["version"][..], false),
            (&["version", "--json"], true),
            (&["version", "-j"], true),
        ] {
            match parse(args).expect("version").command {
                Some(Command::Version(options)) => assert_eq!(options.json, json, "{args:?}"),
                other => panic!("expected version, got {other:?}"),
            }
        }
    }

    /// The JSON leads with the release `-V` prints, and carries the same
    /// build and grammar `--version` shows in brackets.
    #[test]
    fn the_version_report_agrees_with_the_version_flag() {
        let report = VersionReport::current();
        assert_eq!(report.version, VERSION);
        let build = report.build.unwrap_or("unknown");
        let grammar = report.grammar.unwrap_or("unknown");
        assert_eq!(
            LONG_VERSION,
            format!("{VERSION} ({build}, grammar {grammar})")
        );
        let value = serde_json::to_value(&report).expect("serializes");
        let keys: Vec<&str> = value
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        for key in ["version", "commit", "target", "build", "grammar"] {
            assert!(keys.contains(&key), "{key} missing from {keys:?}");
        }
    }

    #[test]
    fn a_launcher_name_is_used_only_when_it_is_plausible() {
        assert_eq!(invoked_as(Some("surreal lsp")), Some("surreal lsp".into()));
        assert_eq!(
            invoked_as(Some("  surreal lsp ")),
            Some("surreal lsp".into())
        );
        assert_eq!(invoked_as(None), None);
        assert_eq!(invoked_as(Some("")), None);
        assert_eq!(invoked_as(Some("surreal\nlsp")), None);
        assert_eq!(invoked_as(Some("surreal \x1b[31mlsp")), None);
        assert_eq!(invoked_as(Some(&"x".repeat(65))), None);
    }
}
