//! `JsValue` shim over the transport-agnostic JSON-RPC dispatcher.
//!
//! The actual method table lives in [`crate::core::dispatch`] so the
//! wire behavior is exercised by native `cargo test`; this layer only
//! converts the result into what `wasm-bindgen` expects: a response
//! string for requests, `undefined` for notifications.

use wasm_bindgen::prelude::*;

use crate::core::dispatch::{DispatchOutput, dispatch_json_rpc};
use crate::wasm::server::WasmCore;

pub async fn handle_message(core: &WasmCore, json_text: &str) -> Result<JsValue, JsValue> {
    match dispatch_json_rpc(core, json_text).await {
        DispatchOutput::None => Ok(JsValue::UNDEFINED),
        DispatchOutput::Response(response) => Ok(JsValue::from_str(&response)),
    }
}
