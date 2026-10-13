//! `surrealql-language-server`: `surreal-lsp` under the name the editor
//! extensions look for, on PATH and in the GitHub release assets they
//! download. The same program, and silent about the rename: an editor shows
//! every stderr line, and stdout is the JSON-RPC channel.

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> std::process::ExitCode {
    surrealql_language_server::native::cli::run("surrealql-language-server").await
}

#[cfg(target_arch = "wasm32")]
fn main() {}
