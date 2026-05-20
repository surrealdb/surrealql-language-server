//! SurrealQL Language Server.
//!
//! The crate is organised so the same business logic feeds both the
//! native LSP binary (this crate's `[[bin]]` target, see
//! [`crate::native::Backend`]) and a `wasm-bindgen` browser package
//! (built with `scripts/build-wasm.sh`, see
//! [`crate::wasm::WasmLanguageServer`]).
//!
//! * [`config`], [`grammar`], [`semantic`] hold the portable data
//!   types and analyzer logic.
//! * [`core`] hosts the transport-agnostic [`core::LanguageServerCore`]
//!   plus the three boundary traits (
//!   [`core::LspNotifier`], [`core::WorkspaceLoader`],
//!   [`core::MetadataProvider`]).
//! * [`runtime`] is a cfg-switched re-export of either `tokio` or
//!   `tokio_with_wasm` so the core can use the same module paths on
//!   both targets.
//! * [`native`] / [`wasm`] are the per-target adapters.

pub mod config;
pub mod core;
pub mod grammar;
pub mod runtime;
pub mod semantic;

#[cfg(not(target_arch = "wasm32"))]
pub mod native;

#[cfg(target_arch = "wasm32")]
pub mod wasm;
