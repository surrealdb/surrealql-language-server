//! The command-line front-end.
//!
//! The same analysis the editor gets, reachable from a terminal — which is
//! what lets a build pipeline defend a repository rather than only decorating
//! a buffer. Every subcommand goes through
//! [`crate::semantic::pipeline::diagnostics_for_document`], so the CLI and the
//! editor cannot disagree about what is wrong with a file.
//!
//! The argument parser is written by hand. The surface is small, the release
//! profile is size-optimised (`opt-level = 'z'`, `lto`, `strip`), and
//! `src/core/dispatch.rs` sets the precedent for a hand-written table over a
//! dependency. If this grows past a handful of options, reach for `clap` in the
//! native-only dependency block — never the shared one.

mod check;
mod fmt;
mod report;

use std::process::ExitCode;

/// Exit codes. `0` clean, `1` findings, `2` the tool could not run.
pub const EXIT_OK: u8 = 0;
pub const EXIT_FINDINGS: u8 = 1;
pub const EXIT_USAGE: u8 = 2;

const USAGE: &str = "\
surrealql-language-server — SurrealQL language server and checker

USAGE:
    surrealql-language-server                       Serve LSP over stdio.
    surrealql-language-server check [PATH]...       Analyze files and directories.
    surrealql-language-server format [PATH]...      Format files in place.
    surrealql-language-server rules                 List every rule.
    surrealql-language-server explain <RULE>        Describe one rule.

CHECK OPTIONS:
    --format human|json   Output shape. Default: human.
    --rule ID=SEVERITY    Override one rule. Repeatable. Highest precedence.
    --no-config           Ignore surrealql.toml.
    --quiet               Print findings only, no summary.

    A path may be a file or a directory. With no path, the working directory is
    used. Human output is 1-based, the way an editor shows a position. JSON
    output is 0-based, matching the Language Server Protocol.

    Exit code 0 when nothing is reported, 1 when any finding is an error, and 2
    on a usage or file error.

FORMAT OPTIONS:
    --check               Do not write. Exit 1 if any file would change.

    A file the grammar cannot parse is left exactly as it is, and reported.

GLOBAL OPTIONS:
    -h, --help            Print this message.
    -V, --version         Print the version.
";

/// Run the CLI. `args` excludes the program name.
///
/// Everything this prints outside the `check` renderer goes to stderr. Stdout
/// belongs to the LSP wire, and a server started with a stray argument must
/// never corrupt it.
pub async fn run(args: Vec<String>) -> ExitCode {
    match args.first().map(String::as_str) {
        Some("-h" | "--help" | "help") => {
            print!("{USAGE}");
            ExitCode::from(EXIT_OK)
        }
        Some("-V" | "--version" | "version") => {
            println!("{}", crate::core::server::build_version());
            ExitCode::from(EXIT_OK)
        }
        Some("check") => check::run(&args[1..]).await,
        Some("format") => fmt::run(&args[1..]).await,
        Some("rules") => report::list_rules(),
        Some("explain") => match args.get(1) {
            Some(id) => report::explain(id),
            None => fail("explain needs a rule id. Try `rules` for the list."),
        },
        Some(other) => fail(&format!("unknown command `{other}`.")),
        None => fail("no command given."),
    }
}

fn fail(message: &str) -> ExitCode {
    eprintln!("surrealql-language-server: {message}");
    eprintln!();
    eprint!("{USAGE}");
    ExitCode::from(EXIT_USAGE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `main` routes an empty argument list to the stdio server and never
    /// reaches here. If that guard is ever removed, this makes the mistake
    /// loud rather than letting the process exit silently having served
    /// nothing.
    #[tokio::test]
    async fn an_empty_argument_list_is_a_usage_error_here() {
        assert_eq!(
            format!("{:?}", run(Vec::new()).await),
            format!("{:?}", ExitCode::from(EXIT_USAGE))
        );
    }

    #[tokio::test]
    async fn help_and_version_succeed() {
        for arg in ["--help", "-h", "help", "--version", "-V", "version"] {
            assert_eq!(
                format!("{:?}", run(vec![arg.to_string()]).await),
                format!("{:?}", ExitCode::from(EXIT_OK)),
                "`{arg}` should succeed"
            );
        }
    }

    #[tokio::test]
    async fn an_unknown_command_is_a_usage_error() {
        assert_eq!(
            format!("{:?}", run(vec!["frobnicate".to_string()]).await),
            format!("{:?}", ExitCode::from(EXIT_USAGE))
        );
    }

    #[tokio::test]
    async fn explain_needs_a_rule_and_rejects_an_unknown_one() {
        assert_eq!(
            format!("{:?}", run(vec!["explain".to_string()]).await),
            format!("{:?}", ExitCode::from(EXIT_USAGE))
        );
        assert_eq!(
            format!(
                "{:?}",
                run(vec!["explain".to_string(), "not-a-rule".to_string()]).await
            ),
            format!("{:?}", ExitCode::from(EXIT_USAGE))
        );
        assert_eq!(
            format!(
                "{:?}",
                run(vec!["explain".to_string(), "let-type".to_string()]).await
            ),
            format!("{:?}", ExitCode::from(EXIT_OK))
        );
    }
}
