//! `surreal-lsp`: the language server, which the `surreal` launcher runs as
//! `surreal lsp`. The command line lives in
//! [`surrealql_language_server::native::cli`], shared with the
//! `surrealql-language-server` executable.
//!
//! The browser build produces a `cdylib` instead and never enters this
//! file; see [`surrealql_language_server::wasm`] for the `wasm-bindgen`
//! surface.

#[cfg(all(not(target_arch = "wasm32"), feature = "server"))]
#[tokio::main]
async fn main() -> std::process::ExitCode {
    surrealql_language_server::native::cli::run("surreal-lsp").await
}

// Cargo does not expose a bin that declares `required-features` to
// integration tests (`CARGO_BIN_EXE_*` stays unset). The feature gate
// lives here so `cargo test` still builds the real binary.
#[cfg(all(not(target_arch = "wasm32"), not(feature = "server")))]
fn main() -> std::process::ExitCode {
    eprintln!("surreal-lsp requires the `server` feature");
    std::process::ExitCode::from(1)
}

#[cfg(target_arch = "wasm32")]
fn main() {}
