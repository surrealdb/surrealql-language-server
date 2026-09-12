//! End-to-end tests for the `check` subcommand, driving the real binary.
//!
//! These spawn `surrealql-language-server check …` the way an agent, a CI
//! job, or a pre-commit hook does, and assert on the observable contract:
//! exit codes, output formats, and the flag surface. The JSON *shape* is
//! pinned separately in `tests/compat.rs`; this file covers behavior.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_surrealql-language-server")
}

/// A unique per-test scratch directory. Tests run concurrently in one
/// process, so the name carries the test's own tag.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("surql-check-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(&path, content).expect("write fixture");
    path
}

fn run_check(cwd: &Path, args: &[&str]) -> Output {
    Command::new(binary())
        .arg("check")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn check")
}

fn exit_code(output: &Output) -> i32 {
    output.status.code().expect("exit code")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn a_clean_file_exits_zero() {
    let dir = scratch("clean");
    write(&dir, "schema.surql", "DEFINE TABLE person SCHEMALESS;\n");
    let output = run_check(&dir, &["schema.surql"]);
    assert_eq!(exit_code(&output), 0, "stderr: {:?}", output);
    assert!(stdout(&output).contains("0 errors, 0 warnings in 1 file"));
}

#[test]
fn a_parse_error_exits_one() {
    let dir = scratch("parse-error");
    write(&dir, "broken.surql", "SELEC * FRM person\n");
    let output = run_check(&dir, &["broken.surql"]);
    assert_eq!(exit_code(&output), 1);
    let text = stdout(&output);
    assert!(text.contains("error[parse]"), "got: {text}");
    assert!(text.contains("broken.surql:1:"), "got: {text}");
}

#[test]
fn an_unknown_table_is_a_warning_and_fail_on_raises_it() {
    let dir = scratch("unknown-table");
    // Schema inference from usage is a feature, so a bare unknown name
    // stays silent; the diagnostic fires when the name reads as a typo
    // of an explicitly defined table.
    write(
        &dir,
        "query.surql",
        "DEFINE TABLE person SCHEMALESS;\nSELECT * FROM persn;\n",
    );
    // `unknown-table` is a warning by design, so the default threshold
    // (error) lets it pass…
    let lenient = run_check(&dir, &["query.surql"]);
    assert_eq!(exit_code(&lenient), 0, "stdout: {}", stdout(&lenient));
    assert!(stdout(&lenient).contains("warning[unknown-table]"));
    // …and `--fail-on warning` turns it into a failure.
    let strict = run_check(&dir, &["query.surql", "--fail-on", "warning"]);
    assert_eq!(exit_code(&strict), 1);
}

#[test]
fn a_missing_file_exits_two() {
    let dir = scratch("missing");
    let output = run_check(&dir, &["not-there.surql"]);
    assert_eq!(exit_code(&output), 2);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not-there.surql"),
        "stderr names the file"
    );
}

#[test]
fn an_unknown_flag_exits_two() {
    let dir = scratch("unknown-flag");
    write(&dir, "schema.surql", "DEFINE TABLE person SCHEMALESS;\n");
    let output = run_check(&dir, &["schema.surql", "--frobnicate"]);
    assert_eq!(exit_code(&output), 2);
    assert!(String::from_utf8_lossy(&output.stderr).contains("--frobnicate"));
}

#[test]
fn an_unknown_top_level_argument_exits_two() {
    let output = Command::new(binary())
        .arg("--frobnicate")
        .output()
        .expect("spawn");
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn version_prints_and_exits_zero() {
    let output = Command::new(binary())
        .arg("--version")
        .output()
        .expect("spawn");
    assert_eq!(output.status.code(), Some(0));
    assert!(!output.stdout.is_empty());
}

#[test]
fn json_format_is_one_parseable_object() {
    let dir = scratch("json");
    write(&dir, "broken.surql", "SELEC * FRM person\n");
    let output = run_check(&dir, &["broken.surql", "--format", "json"]);
    assert_eq!(exit_code(&output), 1);
    let value: serde_json::Value =
        serde_json::from_str(&stdout(&output)).expect("stdout is one JSON object");
    assert_eq!(value["exitCode"], 1);
    assert_eq!(value["summary"]["filesChecked"], 1);
    assert_eq!(value["files"][0]["path"], "broken.surql");
    // The wire diagnostic rides along verbatim: stable code, 0-based range.
    assert_eq!(value["files"][0]["diagnostics"][0]["code"], "parse");
    assert_eq!(
        value["files"][0]["diagnostics"][0]["range"]["start"]["line"],
        0
    );
}

#[test]
fn a_directory_target_walks_only_surql_files() {
    let dir = scratch("dir-walk");
    write(&dir, "queries/a.surql", "DEFINE TABLE person SCHEMALESS;\n");
    write(
        &dir,
        "queries/b.surrealql",
        "DEFINE TABLE animal SCHEMALESS;\n",
    );
    write(&dir, "queries/readme.md", "not sql\n");
    let output = run_check(&dir, &["queries", "--format", "json"]);
    assert_eq!(exit_code(&output), 0);
    let value: serde_json::Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(value["summary"]["filesChecked"], 2);
}

#[test]
fn workspace_context_resolves_a_cross_file_table() {
    let dir = scratch("workspace-context");
    // Checked alone, `persn` reads as a typo of the explicit `person`;
    // the schema directory defines `persn` for real, so context resolves
    // it without being reported on itself.
    write(
        &dir,
        "schema/tables.surql",
        "DEFINE TABLE persn SCHEMALESS;\n",
    );
    write(
        &dir,
        "queries/feed.surql",
        "DEFINE TABLE person SCHEMALESS;\nSELECT * FROM persn;\n",
    );
    let alone = run_check(&dir, &["queries/feed.surql", "--fail-on", "warning"]);
    assert_eq!(exit_code(&alone), 1, "stdout: {}", stdout(&alone));
    let with_context = run_check(
        &dir,
        &[
            "queries/feed.surql",
            "--workspace",
            "schema",
            "--fail-on",
            "warning",
            "--format",
            "json",
        ],
    );
    assert_eq!(
        exit_code(&with_context),
        0,
        "stdout: {}",
        stdout(&with_context)
    );
    let value: serde_json::Value = serde_json::from_str(&stdout(&with_context)).expect("json");
    assert_eq!(
        value["summary"]["filesChecked"], 1,
        "context files are not reported"
    );
}

#[test]
fn stdin_with_filename_shadows_the_on_disk_file() {
    let dir = scratch("stdin-shadow");
    let on_disk = write(&dir, "buffer.surql", "SELEC * FRM person\n");
    // Checking the saved file fails…
    let saved = run_check(&dir, &["buffer.surql"]);
    assert_eq!(exit_code(&saved), 1);
    // …but the same path checked from stdin (the unsaved buffer) is clean.
    let mut child = Command::new(binary())
        .args([
            "check",
            "--stdin",
            "--stdin-filename",
            on_disk.to_str().expect("utf-8 path"),
        ])
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"DEFINE TABLE person SCHEMALESS;\n")
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait");
    assert_eq!(output.status.code(), Some(0), "stderr: {output:?}");
}

#[test]
fn stdin_rejects_positional_paths() {
    let dir = scratch("stdin-positional");
    let output = run_check(&dir, &["--stdin", "also-a-path.surql"]);
    assert_eq!(exit_code(&output), 2);
}

#[test]
fn param_declares_a_caller_bound_variable() {
    let dir = scratch("param");
    write(&dir, "query.surql", "RETURN $id;\n");
    let without = run_check(&dir, &["query.surql", "--format", "json"]);
    assert!(
        stdout(&without).contains("undefined-variable"),
        "got: {}",
        stdout(&without)
    );
    let with = run_check(&dir, &["query.surql", "--param", "id", "--format", "json"]);
    assert!(
        !stdout(&with).contains("undefined-variable"),
        "got: {}",
        stdout(&with)
    );
}

#[test]
fn a_config_file_is_read_with_the_lsp_settings_shape() {
    let dir = scratch("config");
    write(&dir, "query.surql", "RETURN $id;\n");
    // The same nested shape an editor sends in initializationOptions.
    write(
        &dir,
        "surql.json",
        r#"{ "surrealql": { "analysis": { "externalParams": ["id"] } } }"#,
    );
    let output = run_check(
        &dir,
        &["query.surql", "--config", "surql.json", "--format", "json"],
    );
    assert!(
        !stdout(&output).contains("undefined-variable"),
        "got: {}",
        stdout(&output)
    );
}

#[test]
fn an_unreadable_config_exits_two() {
    let dir = scratch("config-bad");
    write(&dir, "query.surql", "RETURN 1;\n");
    write(&dir, "surql.json", "{ not json ");
    let output = run_check(&dir, &["query.surql", "--config", "surql.json"]);
    assert_eq!(exit_code(&output), 2);
}

// ──────────────────────────────────────────────────────────────────────
// `--format json` prints exactly one JSON object, for every exit code
// ──────────────────────────────────────────────────────────────────────

/// The invariant an agent depends on.
///
/// Four paths used to write a sentence to stderr and exit 2 with stdout empty,
/// so every JSON consumer had to special-case "no output": the exact ambiguity
/// the exit-code contract exists to remove. Each failure kind is a separate case
/// here because each was a separate early return.
#[test]
fn json_format_always_prints_one_object_even_when_the_run_fails() {
    let dir = scratch("json-on-failure");

    // 1. A config file that is not JSON.
    let bad_config = write(&dir, "bad.json", "{ not json");
    let good = write(&dir, "ok.surql", "DEFINE TABLE t SCHEMAFULL;\n");
    let output = run_check(
        &dir,
        &[
            good.to_str().expect("utf8"),
            "--config",
            bad_config.to_str().expect("utf8"),
            "--format",
            "json",
        ],
    );
    assert_failure_report(&output, "invalid-config");

    // 2. A target that does not exist.
    let output = run_check(
        &dir,
        &[
            dir.join("missing.surql").to_str().expect("utf8"),
            "--format",
            "json",
        ],
    );
    assert_failure_report(&output, "unreadable-input");
}

/// Every failure report is one parseable object carrying `exitCode: 2` and a
/// machine-readable `error.kind`.
fn assert_failure_report(output: &Output, expected_kind: &str) {
    assert_eq!(
        output.status.code(),
        Some(2),
        "a run that cannot complete must exit 2"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|error| {
        panic!("`--format json` must print one JSON object; got {stdout:?} ({error})")
    });

    assert_eq!(report["exitCode"], 2);
    assert_eq!(
        report["error"]["kind"], expected_kind,
        "error.kind is the stable field a repair keys on"
    );
    assert!(
        report["error"]["message"].is_string(),
        "the failure must carry prose too"
    );
    assert_eq!(
        report["files"].as_array().map(Vec::len),
        Some(0),
        "a failed run must not claim to have checked anything"
    );

    // The human still gets the message.
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("error:"),
        "the message must reach stderr as well"
    );
}

/// A clean run must keep serialising exactly as it always has: no `error` key.
#[test]
fn a_successful_report_carries_no_error_field() {
    let dir = scratch("json-clean");
    let file = write(&dir, "clean.surql", "DEFINE TABLE t SCHEMAFULL;\n");
    let output = run_check(&dir, &[file.to_str().expect("utf8"), "--format", "json"]);

    assert_eq!(output.status.code(), Some(0));
    let report: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("one JSON object");
    assert!(
        report.get("error").is_none(),
        "a clean report gained an `error` key: {report}"
    );
}

// ──────────────────────────────────────────────────────────────────────
// explain, filters, and the one safe fix
// ──────────────────────────────────────────────────────────────────────

/// `explain` prints the same prose the `codeDescription` link points at, so an
/// agent offline or behind a proxy reads exactly what a human would.
#[test]
fn explain_prints_a_codes_documentation() {
    let dir = scratch("explain");
    let output = run_check(&dir, &["explain", "renamed-function"]);
    assert_eq!(output.status.code(), Some(0));

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("## renamed-function"));
    assert!(
        stdout.contains("rename table"),
        "the explanation must say where the replacement comes from: {stdout}"
    );
    assert!(
        !stdout.contains("## not-callable"),
        "the slice must stop at the next section: {stdout}"
    );
}

/// An unknown code is a usage error that names the alternatives, not a silent
/// empty answer.
#[test]
fn explain_rejects_a_code_that_does_not_exist() {
    let dir = scratch("explain-bad");
    let output = run_check(&dir, &["explain", "not-a-code"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not a diagnostic code"));
    assert!(
        stderr.contains("unknown-table"),
        "it must list the real ones"
    );
}

/// `--only` reports one code; `--ignore` reports everything else. Both record
/// what they hid, because a filtered clean run is not a clean run.
#[test]
fn only_and_ignore_filter_reporting_and_say_so() {
    let dir = scratch("filters");
    let file = write(
        &dir,
        "mixed.surql",
        "RETURN type::thing('person', '1');\nRETURN \"a\" + 1;\n",
    );
    let path = file.to_str().expect("utf8");

    let all = run_check(&dir, &[path, "--format", "json"]);
    let all: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&all.stdout)).expect("json");
    let total = all["files"][0]["diagnostics"]
        .as_array()
        .expect("array")
        .len();
    assert!(total >= 2, "fixture must produce more than one code");
    assert!(
        all.get("filters").is_none(),
        "an unfiltered run must not claim filters"
    );

    let only = run_check(
        &dir,
        &[path, "--format", "json", "--only", "renamed-function"],
    );
    let only: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&only.stdout)).expect("json");
    let kept = only["files"][0]["diagnostics"].as_array().expect("array");
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0]["code"], "renamed-function");
    assert_eq!(only["filters"]["only"][0], "renamed-function");
    assert_eq!(
        only["filters"]["suppressed"].as_u64(),
        Some((total - 1) as u64),
        "the report must say how many it did not show"
    );

    let ignored = run_check(
        &dir,
        &[path, "--format", "json", "--ignore", "renamed-function"],
    );
    let ignored: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&ignored.stdout)).expect("json");
    assert!(
        ignored["files"][0]["diagnostics"]
            .as_array()
            .expect("array")
            .iter()
            .all(|d| d["code"] != "renamed-function"),
    );
}

/// A typo'd code is a usage error. Silently filtering nothing is the failure a
/// CI filter can least afford.
#[test]
fn an_unknown_code_in_a_filter_is_rejected() {
    let dir = scratch("filter-typo");
    let file = write(&dir, "q.surql", "RETURN 1;\n");
    let output = run_check(
        &dir,
        &[file.to_str().expect("utf8"), "--ignore", "parse-error"],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("not a diagnostic code"),);
}

/// `--fix renamed-function` rewrites the file, re-analyses it, and says what it
/// did. The replacement comes from SurrealDB's own rename table.
#[test]
fn fix_rewrites_a_renamed_builtin() {
    let dir = scratch("fix");
    let file = write(&dir, "old.surql", "RETURN type::thing('person', '1');\n");
    let path = file.to_str().expect("utf8");

    let output = run_check(
        &dir,
        &[path, "--fix", "renamed-function", "--format", "json"],
    );
    assert_eq!(output.status.code(), Some(0));

    let report: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("json");
    assert_eq!(report["fixed"].as_u64(), Some(1));
    assert!(
        report["files"][0]["diagnostics"]
            .as_array()
            .expect("array")
            .is_empty(),
        "the report must describe the file as it now is, not as it was"
    );

    assert_eq!(
        fs::read_to_string(&file).expect("read"),
        "RETURN type::record('person', '1');\n",
    );
}

/// Every other code is a suggestion, and applying one unattended can change what
/// a query means: `unknown-table`'s fix is a string-distance guess that could
/// repoint a query at a different real table.
#[test]
fn fix_refuses_any_code_but_the_mechanical_one() {
    let dir = scratch("fix-refuse");
    let file = write(&dir, "q.surql", "SELECT * FROM persn;\n");
    let output = run_check(
        &dir,
        &[file.to_str().expect("utf8"), "--fix", "unknown-table"],
    );

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("is not supported"),
        "the refusal must explain itself: {stderr}"
    );
    assert_eq!(
        fs::read_to_string(&file).expect("read"),
        "SELECT * FROM persn;\n",
        "a refused fix must not touch the file"
    );
}
