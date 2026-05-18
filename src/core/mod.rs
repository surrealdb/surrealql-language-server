//! Transport-agnostic language server core.
//!
//! Everything in this module compiles on both `cargo build`
//! (native binary) and `wasm-pack build --target web` (browser
//! package). The native and WASM front-ends live in [`crate::native`]
//! and [`crate::wasm`] respectively and adapt the core to their
//! transport.

pub mod client;
pub mod completion_context;
pub mod server;
pub mod state;

pub use client::{LspNotifier, MetadataProvider, WorkspaceLoader};
pub use server::LanguageServerCore;
