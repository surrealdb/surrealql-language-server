//! Cross-target async runtime alias.
//!
//! On native targets we use the real `tokio` crate. On
//! `wasm32-unknown-unknown` `tokio_with_wasm` exposes the same module
//! layout (`sync`, `time`, `task`, top-level `spawn`) backed by the
//! JavaScript event loop. Re-exporting the chosen crate as
//! `crate::runtime` lets every module in the portable core write
//! `runtime::sync::RwLock` and `runtime::spawn(...)` without sprinkling
//! `cfg` blocks throughout the implementation.

#[cfg(not(target_arch = "wasm32"))]
pub use tokio::{spawn, sync, task, time};

#[cfg(target_arch = "wasm32")]
pub use tokio_with_wasm::{spawn, sync, task, time};
