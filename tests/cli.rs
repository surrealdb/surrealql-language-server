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
