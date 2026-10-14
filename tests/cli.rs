//! End-to-end tests for the command line's shared surface, driving the real
//! binary: `version`, and how a usage error reads to a caller that asked for
//! JSON.
//!
//! These are the rules every component of the `surreal` CLI follows, so a
//! script can treat `surreal lsp` the way it treats `surreal cloud`. `check`
//! itself is covered by `tests/check.rs`.

use std::process::{Command, Output};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_surreal-lsp")
}

/// Run the binary with `args`, outside the launcher.
fn run(args: &[&str]) -> Output {
    Command::new(binary())
        .args(args)
        .env_remove("SURREAL_INVOKED_AS")
        .env_remove("SURREAL_CLI_LAUNCHER")
        .output()
        .expect("spawn")
}

/// What the `surreal` launcher sets on a component it runs.
const LAUNCHER_ENV: [(&str, &str); 2] = [
    ("SURREAL_INVOKED_AS", "surreal lsp"),
    ("SURREAL_CLI_LAUNCHER", "/usr/local/bin/surreal"),
];

/// Run the binary with `args`, as the `surreal` launcher runs it.
fn run_launched(args: &[&str]) -> Output {
    Command::new(binary())
        .args(args)
        .envs(LAUNCHER_ENV)
        .output()
        .expect("spawn")
}

/// stderr as the one JSON object it must be.
fn envelope(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stderr).unwrap_or_else(|err| {
        panic!(
            "stderr is not one JSON object ({err}): {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("utf-8 stdout")
}

#[test]
fn version_prints_what_the_version_flag_prints() {
    let flag = run(&["--version"]);
    let command = run(&["version"]);
    assert_eq!(command.status.code(), Some(0));
    assert_eq!(stdout(&command), stdout(&flag));
    assert!(command.stderr.is_empty(), "stderr: {:?}", command.stderr);
}

#[test]
fn version_json_is_one_object_that_leads_with_the_release() {
    for flag in ["--json", "-j"] {
        let output = run(&["version", flag]);
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stderr.is_empty(), "stderr: {:?}", output.stderr);
        let value: serde_json::Value =
            serde_json::from_str(&stdout(&output)).expect("stdout is one JSON object");
        assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
        // Every key is present, `null` when the build could not tell.
        for key in ["commit", "target", "build", "grammar"] {
            assert!(value.get(key).is_some(), "{key} missing: {value}");
        }
        // The release is what `-V` prints after the name.
        let short = stdout(&run(&["-V"]));
        assert_eq!(
            short.split_whitespace().nth(1),
            value["version"].as_str(),
            "{short}"
        );
    }
}

/// A caller that asked for JSON reads stderr as JSON, so clap's usage error
/// is the envelope there, naming the failing command's own help the way the
/// launcher says it was typed.
#[test]
fn a_usage_error_under_json_is_the_error_envelope() {
    for flag in ["--json", "-j"] {
        let output = run_launched(&["check", flag, "--bogus", "a.surql"]);
        assert_eq!(output.status.code(), Some(2), "{flag}");
        assert!(output.stdout.is_empty(), "stdout: {:?}", output.stdout);
        let value = envelope(&output);
        let expected = serde_json::json!({
            "kind": "usage",
            "message": "unexpected argument '--bogus' found",
            "status": null,
            "code": null,
            "request_id": null,
            "hint": "See the usage with:",
            "command": "surreal lsp check --help",
            "docs": null,
            "retry_after_secs": null,
            "exit_code": 2,
        });
        assert_eq!(value, expected, "{flag}");
    }
}

/// The rule clap cannot state, `--stdin-filename` without `--stdin`, reads
/// the same as clap's own refusals.
#[test]
fn a_usage_error_clap_cannot_state_is_the_error_envelope_too() {
    let output = run(&["check", "--json", "--stdin-filename", "a.surql", "a.surql"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty(), "stdout: {:?}", output.stdout);
    let value = envelope(&output);
    assert_eq!(value["kind"], "usage");
    assert_eq!(value["message"], "`--stdin-filename` needs `--stdin`");
    assert_eq!(value["command"], "surreal-lsp check --help");
    assert_eq!(value["exit_code"], 2);
}

/// Without JSON, and for a `--json` after `--`, a usage error stays clap's
/// own text.
#[test]
fn a_usage_error_without_json_is_clap_text() {
    for args in [
        &["check", "--bogus", "a.surql"][..],
        &["check", "--bogus", "--", "--json"],
    ] {
        let output = run_launched(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let text = String::from_utf8(output.stderr).expect("utf-8 stderr");
        assert!(
            text.starts_with("error: unexpected argument '--bogus' found"),
            "{text}"
        );
        assert!(text.contains("Usage: surreal lsp check"), "{text}");
    }
}

/// Help and `--version` are never errors: clap's text on stdout, exit 0,
/// whatever `--json` comes with them. (`--json` is not a top-level flag, so
/// it has to follow `--version` there, as any unknown flag would.)
#[test]
fn help_and_version_ignore_json() {
    for args in [
        &["check", "--json", "--help"][..],
        &["version", "-j", "--help"],
        &["--version", "--json"],
        &["-V", "-j"],
    ] {
        let output = run_launched(args);
        assert_eq!(output.status.code(), Some(0), "{args:?}");
        assert!(output.stderr.is_empty(), "stderr: {:?}", output.stderr);
        let text = stdout(&output);
        assert!(!text.trim_start().starts_with('{'), "{text}");
    }
}
