//! Serving over stdio writes the protocol and nothing else.
//!
//! An editor starts the server and speaks JSON-RPC over its stdin and
//! stdout, and many editors also parse stderr, showing every line as a
//! server message or treating it as a failure. So a session that works puts
//! only framed JSON-RPC messages on stdout and not one byte on stderr: no
//! banner, no notice, and no deprecation line under the old name.
//!
//! Each test drives one whole session — `initialize`, `initialized`,
//! `shutdown`, `exit` — under each name the server is started by: the
//! `surreal-lsp` executable, the `surrealql-language-server` one the editor
//! extensions find, and `surreal-lsp` as the `surreal` launcher starts it.

use std::io::{Read, Write as _};
use std::process::{Command, Output, Stdio};

/// One JSON-RPC message, framed as an editor frames it.
fn frame(body: &str) -> Vec<u8> {
    format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
}

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":null,"rootUri":null,"capabilities":{}}}"#;

/// The rest of a session, sent once `initialize` has been answered, as an
/// editor sends it: a server may cancel a request still in flight at `exit`.
const REST: &[&str] = &[
    r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
    r#"{"jsonrpc":"2.0","id":2,"method":"shutdown"}"#,
    r#"{"jsonrpc":"2.0","method":"exit"}"#,
];

/// Read one framed message's bytes, header and body, from `reader`.
fn read_frame(reader: &mut impl Read) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut byte = [0_u8; 1];
    while !bytes.ends_with(b"\r\n\r\n") {
        reader.read_exact(&mut byte).expect("a header byte");
        bytes.push(byte[0]);
    }
    let header = String::from_utf8_lossy(&bytes).into_owned();
    let length: usize = header
        .lines()
        .find_map(|line| line.strip_prefix("Content-Length: "))
        .unwrap_or_else(|| panic!("a header without Content-Length: {header:?}"))
        .parse()
        .expect("a numeric Content-Length");
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body).expect("the body");
    bytes.extend(body);
    bytes
}

/// Run `binary` with `args` and `env` over one session, returning every byte
/// it wrote.
///
/// The environment is cleared so nothing the developer or the CI runner has
/// set — a `SURREALDB_ENDPOINT`, a `RUST_LOG` — decides the outcome.
fn serve(binary: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut child = Command::new(binary)
        .args(args)
        .env_clear()
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the server");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = child.stdout.take().expect("stdout");

    stdin
        .write_all(&frame(INITIALIZE))
        .expect("write initialize");
    let mut written = Vec::new();
    loop {
        let message = read_frame(&mut stdout);
        written.extend_from_slice(&message);
        if messages(&message)[0]["id"] == 1 {
            break;
        }
    }
    for body in REST {
        stdin.write_all(&frame(body)).expect("write the session");
    }
    drop(stdin);
    stdout.read_to_end(&mut written).expect("read stdout");

    let mut output = child.wait_with_output().expect("wait for the server");
    output.stdout = written;
    output
}

/// Split stdout into its framed messages, failing on any byte that is not
/// part of one.
fn messages(stdout: &[u8]) -> Vec<serde_json::Value> {
    let mut rest = stdout;
    let mut found = Vec::new();
    while !rest.is_empty() {
        let end = rest
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap_or_else(|| {
                panic!(
                    "unframed bytes on stdout: {:?}",
                    String::from_utf8_lossy(rest)
                )
            });
        let header = std::str::from_utf8(&rest[..end]).expect("an ASCII header");
        let length: usize = header
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length: "))
            .unwrap_or_else(|| panic!("a header without Content-Length: {header:?}"))
            .parse()
            .expect("a numeric Content-Length");
        let body = &rest[end + 4..end + 4 + length];
        found.push(serde_json::from_slice(body).expect("a JSON body"));
        rest = &rest[end + 4 + length..];
    }
    found
}

fn assert_only_the_protocol(output: &Output) {
    assert!(
        output.stderr.is_empty(),
        "editors parse stderr, so a working session must write nothing to it; got {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let messages = messages(&output.stdout);
    for message in &messages {
        assert_eq!(
            message["jsonrpc"], "2.0",
            "not a JSON-RPC message: {message}"
        );
    }
    let initialize = messages
        .iter()
        .find(|message| message["id"] == 1)
        .expect("a reply to initialize");
    assert!(
        initialize["result"]["serverInfo"]["version"]
            .as_str()
            .is_some_and(|version| version.starts_with(env!("CARGO_PKG_VERSION"))),
        "initialize reports the release: {initialize}"
    );
    assert!(
        messages.iter().any(|message| message["id"] == 2),
        "a reply to shutdown"
    );
    assert_eq!(output.status.code(), Some(0), "a clean exit");
}

#[test]
fn surreal_lsp_serves_only_the_protocol() {
    let output = serve(env!("CARGO_BIN_EXE_surreal-lsp"), &[], &[]);
    assert_only_the_protocol(&output);
}

/// LSP clients append `--stdio` by convention.
#[test]
fn surreal_lsp_serves_only_the_protocol_with_stdio() {
    let output = serve(env!("CARGO_BIN_EXE_surreal-lsp"), &["--stdio"], &[]);
    assert_only_the_protocol(&output);
}

/// The old name is how the editor extensions find the server today, and it
/// must not announce that it has been renamed.
#[test]
fn the_old_name_serves_only_the_protocol() {
    let binary = env!("CARGO_BIN_EXE_surrealql-language-server");
    assert_only_the_protocol(&serve(binary, &[], &[]));
    assert_only_the_protocol(&serve(binary, &["--stdio"], &[]));
}

/// The `stdio` command, the standard way to start the server, under both
/// names, with and without the `--stdio` an LSP client appends.
#[test]
fn the_stdio_command_serves_only_the_protocol() {
    for binary in [
        env!("CARGO_BIN_EXE_surreal-lsp"),
        env!("CARGO_BIN_EXE_surrealql-language-server"),
    ] {
        assert_only_the_protocol(&serve(binary, &["stdio"], &[]));
        assert_only_the_protocol(&serve(binary, &["stdio", "--stdio"], &[]));
    }
}

/// What the `surreal` launcher sets on a component it runs.
const LAUNCHER_ENV: [(&str, &str); 2] = [
    ("SURREAL_INVOKED_AS", "surreal lsp"),
    ("SURREAL_CLI_LAUNCHER", "/usr/local/bin/surreal"),
];

/// Under the launcher, `stdio` and the legacy `--stdio` both serve, and
/// neither variable changes what the server writes.
#[test]
fn started_by_the_launcher_it_serves_only_the_protocol() {
    let binary = env!("CARGO_BIN_EXE_surreal-lsp");
    assert_only_the_protocol(&serve(binary, &["stdio"], &LAUNCHER_ENV));
    assert_only_the_protocol(&serve(binary, &["--stdio"], &LAUNCHER_ENV));
}

/// Run `binary` with no arguments and no stdin, as somebody typing the bare
/// command would, and return what it wrote.
fn run_bare(binary: &str, env: &[(&str, &str)]) -> Output {
    Command::new(binary)
        .env_clear()
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .output()
        .expect("run the binary")
}

/// Under the launcher no command prints help, naming the command the way the
/// user typed it, instead of serving.
#[test]
fn started_by_the_launcher_with_no_command_it_prints_help() {
    let output = run_bare(env!("CARGO_BIN_EXE_surreal-lsp"), &LAUNCHER_ENV);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty(), "stdout: {:?}", output.stdout);
    let help = String::from_utf8(output.stderr).expect("utf-8 help");
    assert!(help.contains("Usage: surreal lsp <COMMAND>\n"), "{help}");
    assert!(help.contains("stdio"), "{help}");
    assert!(help.contains("check"), "{help}");
}

/// An empty launcher variable is not the launcher: no command keeps the
/// legacy flow and serves, so it answers on stdout and prints no help.
#[test]
fn an_empty_launcher_variable_keeps_the_legacy_flow() {
    let output = serve(
        env!("CARGO_BIN_EXE_surreal-lsp"),
        &[],
        &[("SURREAL_CLI_LAUNCHER", "")],
    );
    assert_only_the_protocol(&output);
}

/// Help reached through the launcher names the command the way the user
/// typed it; `surreal lsp check --help` answering `Usage: surreal-lsp …`
/// would send them to an executable they do not run directly.
#[test]
fn help_names_the_launcher_invocation_when_it_is_given_one() {
    let help = |binary: &str, invoked_as: Option<&str>| {
        let mut command = Command::new(binary);
        command
            .args(["check", "--help"])
            .env_remove("SURREAL_INVOKED_AS");
        if let Some(name) = invoked_as {
            command.env("SURREAL_INVOKED_AS", name);
        }
        let output = command.output().expect("the binary should run");
        assert!(output.status.success());
        String::from_utf8(output.stdout).expect("utf-8 help")
    };

    let lsp = env!("CARGO_BIN_EXE_surreal-lsp");
    let old = env!("CARGO_BIN_EXE_surrealql-language-server");
    assert!(help(lsp, Some("surreal lsp")).contains("Usage: surreal lsp check [OPTIONS]"));
    assert!(help(lsp, None).contains("Usage: surreal-lsp check [OPTIONS]"));
    assert!(help(old, None).contains("Usage: surrealql-language-server check [OPTIONS]"));
    // A value that could carry a terminal escape is not trusted with help.
    assert!(help(lsp, Some("surreal\x1b[2Jlsp")).contains("Usage: surreal-lsp check [OPTIONS]"));
}

/// The launcher smoke-tests an install by running `--version` and taking
/// the first version-shaped word, which must be the release it asked for.
#[test]
fn version_names_the_executable_and_the_release() {
    for (binary, name) in [
        (env!("CARGO_BIN_EXE_surreal-lsp"), "surreal-lsp"),
        (
            env!("CARGO_BIN_EXE_surrealql-language-server"),
            "surrealql-language-server",
        ),
    ] {
        let output = Command::new(binary)
            .arg("--version")
            .env_remove("SURREAL_INVOKED_AS")
            .output()
            .expect("spawn");
        assert_eq!(output.status.code(), Some(0));
        let line = String::from_utf8(output.stdout).expect("utf-8");
        let expected = format!("{name} {} (", env!("CARGO_PKG_VERSION"));
        assert!(line.starts_with(&expected), "got {line:?}");
    }
}
