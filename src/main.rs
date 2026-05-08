//! Native LSP binary entry point.
//!
//! The browser build produces a `cdylib` instead and never enters this
//! file; see [`surrealql_language_server::wasm`] for the
//! `wasm-bindgen` surface.

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() {
    use surrealql_language_server::native::Backend;
    use tower_lsp_server::{LspService, Server};

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}

#[cfg(target_arch = "wasm32")]
fn main() {}
