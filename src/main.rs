//! Native LSP binary entry point.
//!
//! The browser build produces a `cdylib` instead and never enters this
//! file; see [`surrealql_language_server::wasm`] for the
//! `wasm-bindgen` surface.

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> std::process::ExitCode {
    use surrealql_language_server::native::{Backend, cli};
    use tower_lsp_server::{LspService, Server};

    // The release profile aborts on panic, so this hook is the only
    // trace a crash leaves. It must write to stderr: stdout carries
    // the LSP wire and editors surface stderr in their output panel.
    std::panic::set_hook(Box::new(|info| {
        eprintln!("surrealql-language-server panicked: {info}");
    }));

    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        return cli::run(args).await;
    }

    // No arguments: serve LSP over stdio. This branch is the compatibility
    // guarantee for every editor already launching the binary bare — nothing
    // may be added to it that writes to stdout.
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
    std::process::ExitCode::SUCCESS
}

#[cfg(target_arch = "wasm32")]
fn main() {}
