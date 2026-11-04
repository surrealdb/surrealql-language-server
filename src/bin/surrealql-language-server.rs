//! `surrealql-language-server`: `surreal-lsp` under the name the editor
//! extensions look for, on PATH and in the GitHub release assets they
//! download. The same program, and silent about the rename: an editor shows
//! every stderr line, and stdout is the JSON-RPC channel.

#[cfg(all(not(target_arch = "wasm32"), feature = "server"))]
#[tokio::main]
async fn main() -> std::process::ExitCode {
    surrealql_language_server::native::cli::run("surrealql-language-server").await
}

#[cfg(all(not(target_arch = "wasm32"), not(feature = "server")))]
fn main() -> std::process::ExitCode {
    eprintln!("surrealql-language-server requires the `server` feature");
    std::process::ExitCode::from(1)
}

#[cfg(target_arch = "wasm32")]
fn main() {}
