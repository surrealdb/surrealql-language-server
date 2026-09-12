//! Native binary entry point: the LSP server by default, plus the
//! headless subcommands.
//!
//! With no arguments (or `--stdio`, which LSP clients commonly append)
//! this serves the LSP over stdio exactly as before — editors depend on
//! that default. `check` runs the one-shot diagnostics mode from
//! [`surrealql_language_server::native::check`]. Anything else is a
//! usage error on stderr with exit code 2, so a typo cannot silently
//! start a server that eats the terminal's stdin.
//!
//! The browser build produces a `cdylib` instead and never enters this
//! file; see [`surrealql_language_server::wasm`] for the `wasm-bindgen`
//! surface.

#[cfg(not(target_arch = "wasm32"))]
const USAGE: &str = "\
Usage: surrealql-language-server [COMMAND]

Commands:
  (none) | --stdio   Serve the Language Server Protocol over stdio.
  check              Check .surql files and report diagnostics
                     (see `check --help`).

Options:
  -V, --version      Print the version and build revision.
  -h, --help         Print this help.";

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> std::process::ExitCode {
    use std::process::ExitCode;

    use surrealql_language_server::core::server::build_version;
    use surrealql_language_server::native::{Backend, check};
    use tower_lsp_server::{LspService, Server};

    // The release profile aborts on panic, so this hook is the only
    // trace a crash leaves. It must write to stderr: stdout carries
    // the LSP wire (or the check report) and editors surface stderr
    // in their output panel.
    std::panic::set_hook(Box::new(|info| {
        eprintln!("surrealql-language-server panicked: {info}");
    }));

    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        // The editor path. Editors launch the bare binary; `--stdio` is
        // tolerated because LSP clients append it by convention.
        None | Some("--stdio") => {
            let stdin = tokio::io::stdin();
            let stdout = tokio::io::stdout();

            let (service, socket) = LspService::new(Backend::new);
            Server::new(stdin, stdout, socket).serve(service).await;
            ExitCode::SUCCESS
        }
        Some("check") => match check::parse_args(args) {
            Ok(check::Parsed::Run(options)) => check::run(options).await,
            Ok(check::Parsed::Help) => {
                println!("{}", check::USAGE);
                ExitCode::SUCCESS
            }
            Ok(check::Parsed::Explain(code)) => match check::explain(&code) {
                Some(prose) => {
                    println!("{prose}");
                    ExitCode::SUCCESS
                }
                None => {
                    eprintln!("error: `{code}` is not a diagnostic code this server emits");
                    eprintln!("known codes: {}", check::known_codes().join(", "));
                    ExitCode::from(2)
                }
            },
            Err(message) => {
                eprintln!("error: {message}");
                eprintln!("{}", check::USAGE);
                ExitCode::from(2)
            }
        },
        Some("--version") | Some("-V") => {
            println!("{}", build_version());
            ExitCode::SUCCESS
        }
        Some("--help") | Some("-h") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("error: unknown argument `{other}`");
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {}
