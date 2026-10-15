//! The JSON error envelope: how a failure reads to a caller that asked for
//! `--json`.
//!
//! Every component of the `surreal` CLI writes the same object, so a script
//! reads a failure from `surreal lsp` exactly as it reads one from `surreal
//! cloud`: one object on **stderr**, with every key present and `null` when
//! unknown, and nothing on stdout. Success is the bare report on stdout, never
//! enveloped; the stream and the exit code already say which one happened.
//!
//! The shape is the one `surreal_cli_out::error::envelope` (surrealdb/cli)
//! builds. That crate is private to the CLI repository, so the object is
//! restated here, with a test that pins its keys.

use serde::Serialize;

/// The `kind` of a bad invocation: clap's own errors and the rules clap
/// cannot state.
pub const USAGE: &str = "usage";
/// The `kind` of a path that does not exist.
pub const NOT_FOUND: &str = "not_found";
/// The `kind` of input that exists but cannot be used: invalid JSON, a file
/// over the size limit.
pub const INVALID: &str = "invalid";
/// The `kind` of anything else.
pub const UNKNOWN: &str = "unknown";

/// One failure, as the `--json` error envelope.
///
/// Field order is the order the keys are written in. The fields this program
/// never fills (`status`, `code`, `request_id`, `docs`, `retry_after_secs`)
/// belong to components that talk to an API; they are written as `null`
/// rather than left out, so a consumer can index without checking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Envelope {
    pub kind: &'static str,
    pub message: String,
    pub status: Option<u16>,
    pub code: Option<String>,
    pub request_id: Option<String>,
    pub hint: Option<String>,
    pub command: Option<String>,
    pub docs: Option<String>,
    pub retry_after_secs: Option<u64>,
    pub exit_code: u8,
}

impl Envelope {
    /// A failure of `kind` with nothing else known about it.
    pub fn new(kind: &'static str, message: impl Into<String>, exit_code: u8) -> Self {
        Self {
            kind,
            message: message.into(),
            status: None,
            code: None,
            request_id: None,
            hint: None,
            command: None,
            docs: None,
            retry_after_secs: None,
            exit_code,
        }
    }

    /// clap's usage error as an envelope.
    ///
    /// The message is clap's error without its `error: `: the first line,
    /// with the indented lines clap sets under it (the missing arguments, the
    /// possible values) joined on, and without the usage block and the tip
    /// that follow. The command is the failing command's own `--help`, read
    /// off clap's usage line, so `surreal lsp check --bogus` points at
    /// `surreal lsp check --help` rather than the root's. `fallback` is the
    /// program's name, for an error that carries no usage line.
    pub fn usage(err: &clap::Error, fallback: &str) -> Self {
        use clap::error::ErrorKind;

        let rendered = err.to_string();
        let path = rendered
            .lines()
            .find_map(|line| line.trim().strip_prefix("Usage: "))
            .map(|usage| {
                usage
                    .split_whitespace()
                    .take_while(|word| !word.starts_with(['[', '<', '-']))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|path| !path.is_empty());
        let message = match err.kind() {
            // What clap renders here is the help itself, whose first line is
            // the command's description: no use as a message.
            ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand | ErrorKind::MissingSubcommand => {
                match &path {
                    Some(path) => format!("`{path}` needs a command"),
                    None => String::from("a command is required"),
                }
            }
            _ => first_paragraph(&rendered),
        };
        let exit_code = u8::try_from(err.exit_code()).unwrap_or(2);
        Self {
            hint: Some(String::from("See the usage with:")),
            command: Some(format!("{} --help", path.as_deref().unwrap_or(fallback))),
            ..Self::new(USAGE, message, exit_code)
        }
    }

    /// The envelope as written: pretty-printed, as the other components
    /// write it, ending in a newline.
    pub fn render(&self) -> String {
        let mut out = serde_json::to_string_pretty(self).expect("the envelope serializes");
        out.push('\n');
        out
    }

    /// Write the envelope to stderr. A failure to write it is not worth a
    /// second error: the exit code still carries the outcome.
    pub fn eprint(&self) {
        use std::io::Write as _;
        let _ = std::io::stderr().write_all(self.render().as_bytes());
    }
}

/// clap's error text up to its first blank line, as one line: the first
/// line without its `error: `, then each indented line under it, trimmed.
fn first_paragraph(rendered: &str) -> String {
    let mut lines = rendered.lines().skip_while(|line| line.trim().is_empty());
    let first = lines.next().unwrap_or("");
    let first = first.strip_prefix("error: ").unwrap_or(first);
    std::iter::once(first)
        .chain(
            lines
                .take_while(|line| line.starts_with([' ', '\t']) && !line.trim().is_empty())
                .map(str::trim),
        )
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether the caller asked for JSON, read off the raw arguments up to a
/// `--`: `--json`, `-j` alone or in a cluster of short flags (`-xj`), or
/// `--format json` / `--format=json`, which `check` still accepts as the
/// older spelling of `--json`.
///
/// Read off the raw arguments because the parse that would have decided it
/// is the one that failed. `command` says which short flags take a value:
/// in a cluster, the letters after one of those are its value, not flags.
pub fn asks_for_json(command: &clap::Command, argv: &[std::ffi::OsString]) -> bool {
    let mut value_shorts = Vec::new();
    shorts_taking_values(command, &mut value_shorts);
    let args: Vec<Option<&str>> = argv
        .iter()
        .skip(1)
        .take_while(|arg| *arg != "--")
        .map(|arg| arg.to_str())
        .collect();
    args.iter().enumerate().any(|(at, arg)| match *arg {
        Some("--json" | "--format=json") => true,
        Some("--format") => args.get(at + 1) == Some(&Some("json")),
        Some(arg) => arg
            .strip_prefix('-')
            .filter(|cluster| {
                !cluster.is_empty() && cluster.bytes().all(|b| b.is_ascii_alphabetic())
            })
            .is_some_and(|cluster| {
                cluster
                    .chars()
                    .take_while(|short| !value_shorts.contains(short))
                    .any(|short| short == 'j')
            }),
        None => false,
    })
}

/// Every short flag in `command`'s tree that takes a value.
fn shorts_taking_values(command: &clap::Command, into: &mut Vec<char>) {
    for arg in command.get_arguments() {
        if arg.get_action().takes_values()
            && let Some(shorts) = arg.get_short_and_visible_aliases()
        {
            into.extend(shorts);
        }
    }
    for subcommand in command.get_subcommands() {
        shorts_taking_values(subcommand, into);
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn argv(args: &[&str]) -> Vec<OsString> {
        std::iter::once("surreal-lsp")
            .chain(args.iter().copied())
            .map(OsString::from)
            .collect()
    }

    /// Every key, in order, `null` when unknown: a consumer indexes the
    /// object without checking, so a vanished key is a second failure.
    #[test]
    fn the_envelope_writes_every_key_in_order() {
        let rendered = Envelope::new(NOT_FOUND, "cannot read `a.surql`", 2).render();
        let value: serde_json::Value = serde_json::from_str(&rendered).expect("one object");
        let keys = [
            "kind",
            "message",
            "status",
            "code",
            "request_id",
            "hint",
            "command",
            "docs",
            "retry_after_secs",
            "exit_code",
        ];
        assert_eq!(value.as_object().expect("an object").len(), keys.len());
        let positions: Vec<usize> = keys
            .iter()
            .map(|key| {
                rendered
                    .find(&format!("\"{key}\":"))
                    .unwrap_or_else(|| panic!("{key} missing: {rendered}"))
            })
            .collect();
        assert!(positions.is_sorted(), "{rendered}");
        assert_eq!(value["kind"], "not_found");
        assert_eq!(value["status"], serde_json::Value::Null);
        assert_eq!(value["exit_code"], 2);
        assert!(rendered.ends_with("}\n"), "{rendered}");
    }

    /// A command with a value-taking short (`-o`) beside the flags, which
    /// the real one does not have yet.
    fn command() -> clap::Command {
        use clap::{Arg, ArgAction, Command};
        Command::new("surreal-lsp").subcommand(
            Command::new("check")
                .arg(
                    Arg::new("json")
                        .long("json")
                        .short('j')
                        .action(ArgAction::SetTrue),
                )
                .arg(Arg::new("quiet").short('q').action(ArgAction::SetTrue))
                .arg(Arg::new("output").short('o').action(ArgAction::Set))
                .arg(Arg::new("format").long("format"))
                .arg(Arg::new("paths").num_args(0..)),
        )
    }

    fn asks(args: &[&str]) -> bool {
        asks_for_json(&command(), &argv(args))
    }

    #[test]
    fn json_is_asked_for_only_before_a_double_dash() {
        assert!(asks(&["check", "--json", "a.surql"]));
        assert!(asks(&["check", "a.surql", "-j"]));
        assert!(!asks(&["check", "a.surql"]));
        assert!(!asks(&["check", "--", "--json"]));
        assert!(!asks(&["check", "--", "--format", "json"]));
        assert!(!asks(&["check", "--jsonx"]));
    }

    /// `--format json` is the older spelling of `--json`, so it asks for the
    /// envelope too, in either form; any other format does not.
    #[test]
    fn format_json_asks_for_json_like_the_flag() {
        assert!(asks(&["check", "--format", "json", "--bogus"]));
        assert!(asks(&["check", "--format=json", "--bogus"]));
        assert!(!asks(&["check", "--format", "text", "--bogus"]));
        assert!(!asks(&["check", "--format=text", "--bogus"]));
        assert!(!asks(&["check", "--format"]));
        assert!(!asks(&["check", "json"]));
    }

    /// In a cluster of shorts, `j` counts until a short that takes a value:
    /// the rest of the cluster is that value.
    #[test]
    fn j_in_a_cluster_of_short_flags_asks_for_json() {
        assert!(asks(&["check", "-qj", "a.surql"]));
        assert!(asks(&["check", "-jq", "a.surql"]));
        assert!(asks(&["check", "-jo", "out"]));
        assert!(!asks(&["check", "-oj", "a.surql"]));
        assert!(!asks(&["check", "-qoj", "a.surql"]));
        assert!(!asks(&["check", "-", "a.surql"]));
        assert!(!asks(&["check", "-j1"]));
    }

    #[test]
    fn a_usage_error_names_the_failing_commands_help() {
        use clap::{Arg, Command};
        let mut command = Command::new("surreal-lsp")
            .bin_name("surreal lsp")
            .subcommand(Command::new("check").arg(Arg::new("paths")));
        let err = command
            .try_get_matches_from_mut(["surreal-lsp", "check", "--bogus"])
            .unwrap_err();
        let envelope = Envelope::usage(&err, "surreal lsp");
        assert_eq!(envelope.kind, "usage");
        assert_eq!(envelope.message, "unexpected argument '--bogus' found");
        assert_eq!(envelope.hint.as_deref(), Some("See the usage with:"));
        assert_eq!(
            envelope.command.as_deref(),
            Some("surreal lsp check --help")
        );
        assert_eq!(envelope.exit_code, 2);
    }

    /// clap's detail under its first line is the point of the message: which
    /// arguments are missing, which values are possible. The usage block and
    /// the tip are not.
    #[test]
    fn a_usage_message_keeps_the_lines_clap_sets_under_it() {
        use clap::{Arg, Command};
        let mut command = Command::new("surreal-lsp").subcommand(
            Command::new("check")
                .arg(
                    Arg::new("paths")
                        .value_name("PATHS")
                        .num_args(1..)
                        .required(true),
                )
                .arg(
                    Arg::new("format")
                        .long("format")
                        .value_parser(["text", "json"]),
                ),
        );
        let missing = command
            .try_get_matches_from_mut(["surreal-lsp", "check"])
            .unwrap_err();
        assert_eq!(
            Envelope::usage(&missing, "surreal-lsp").message,
            "the following required arguments were not provided: <PATHS>..."
        );
        let invalid = command
            .try_get_matches_from_mut(["surreal-lsp", "check", "a", "--format", "yaml"])
            .unwrap_err();
        assert_eq!(
            Envelope::usage(&invalid, "surreal-lsp").message,
            "invalid value 'yaml' for '--format <format>' [possible values: text, json]"
        );
        let unknown = command
            .try_get_matches_from_mut(["surreal-lsp", "check", "a", "--bogus"])
            .unwrap_err();
        assert_eq!(
            Envelope::usage(&unknown, "surreal-lsp").message,
            "unexpected argument '--bogus' found"
        );
    }
}
