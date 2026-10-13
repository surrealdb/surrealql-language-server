//! `surreal-lsp`: the language server, which the `surreal` launcher runs as
//! `surreal lsp`. The command line lives in
//! [`surrealql_language_server::native::cli`], shared with the
//! `surrealql-language-server` executable.
//!
//! The browser build produces a `cdylib` instead and never enters this
//! file; see [`surrealql_language_server::wasm`] for the `wasm-bindgen`
//! surface.

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> std::process::ExitCode {
    surrealql_language_server::native::cli::run("surreal-lsp").await
}

#[cfg(target_arch = "wasm32")]
fn main() {}
