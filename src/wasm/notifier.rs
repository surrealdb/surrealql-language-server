//! [`LspNotifier`] implementation backed by JavaScript callbacks.
//!
//! The host (a Web Worker in Surrealist) supplies three async-friendly
//! `Function`s at construction time:
//!
//! ```js
//! new WasmLanguageServer({
//!   onPublishDiagnostics: (uri, diagnostics) => { ... },
//!   onLogMessage: (level, message) => { ... },
//!   onRequestConfiguration: async () => ({ ... }) | null,
//! });
//! ```
//!
//! `js_sys::Function` is `!Send + !Sync`, but wasm32 has exactly one
//! thread, so wrapping each callback in [`SendWrapper`] is sound and
//! satisfies the trait bounds the portable core requires.

use async_trait::async_trait;
use js_sys::Function;
use ls_types::{Diagnostic, MessageType, Uri};
use send_wrapper::SendWrapper;
use serde_json::Value;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::core::client::LspNotifier;

/// Bundle of JavaScript callbacks passed in from the host.
#[derive(Clone)]
pub struct JsCallbacks {
    pub publish_diagnostics: SendWrapper<Function>,
    pub log_message: SendWrapper<Function>,
    pub request_configuration: SendWrapper<Function>,
}

impl JsCallbacks {
    /// Decode a JS object literal of the shape documented on
    /// [`JsCallbacks`]. Missing or non-function fields produce a
    /// descriptive `JsValue` error.
    pub fn from_object(value: &JsValue) -> Result<Self, JsValue> {
        let publish_diagnostics = require_function(value, "onPublishDiagnostics")?;
        let log_message = require_function(value, "onLogMessage")?;
        let request_configuration = require_function(value, "onRequestConfiguration")?;
        Ok(Self {
            publish_diagnostics: SendWrapper::new(publish_diagnostics),
            log_message: SendWrapper::new(log_message),
            request_configuration: SendWrapper::new(request_configuration),
        })
    }
}

fn require_function(value: &JsValue, key: &str) -> Result<Function, JsValue> {
    let property = js_sys::Reflect::get(value, &JsValue::from_str(key))?;
    property
        .dyn_into::<Function>()
        .map_err(|_| JsValue::from_str(&format!("callbacks.{key} must be a function")))
}

pub struct JsCallbackNotifier {
    callbacks: JsCallbacks,
}

impl JsCallbackNotifier {
    pub fn new(callbacks: JsCallbacks) -> Self {
        Self { callbacks }
    }
}

#[async_trait]
impl LspNotifier for JsCallbackNotifier {
    async fn publish_diagnostics(&self, uri: Uri, diagnostics: Vec<Diagnostic>) {
        let uri = JsValue::from_str(uri.as_str());
        let diagnostics = match serde_wasm_bindgen::to_value(&diagnostics) {
            Ok(value) => value,
            Err(_) => JsValue::NULL,
        };
        let _ = self
            .callbacks
            .publish_diagnostics
            .call2(&JsValue::NULL, &uri, &diagnostics);
    }

    async fn log_message(&self, level: MessageType, message: String) {
        // `MessageType` serialises as an integer matching the LSP wire
        // format (1 = Error, 2 = Warning, 3 = Info, 4 = Log).
        let level = serde_wasm_bindgen::to_value(&level).unwrap_or(JsValue::NULL);
        let message = JsValue::from_str(&message);
        let _ = self
            .callbacks
            .log_message
            .call2(&JsValue::NULL, &level, &message);
    }

    async fn request_configuration(&self) -> Option<Value> {
        // `JsFuture` is `!Send`, but the trait future returned by
        // `async_trait` requires `Send`. Run the JS-touching work on
        // `spawn_local` (which doesn't need Send) and bridge the
        // result back through a Send-friendly oneshot channel.
        let callback = self.callbacks.request_configuration.clone();
        let (tx, rx) = crate::runtime::sync::oneshot::channel::<Option<Value>>();
        wasm_bindgen_futures::spawn_local(async move {
            let value = invoke_request_configuration(&callback).await;
            let _ = tx.send(value);
        });
        rx.await.ok().flatten()
    }
}

async fn invoke_request_configuration(callback: &Function) -> Option<Value> {
    let result = callback.call0(&JsValue::NULL).ok()?;

    // The host may return the configuration directly or hand back a
    // Promise; await both transparently.
    let value = if let Some(promise) = result.dyn_ref::<js_sys::Promise>() {
        JsFuture::from(promise.clone()).await.ok()?
    } else {
        result
    };

    if value.is_null() || value.is_undefined() {
        return None;
    }

    serde_wasm_bindgen::from_value(value).ok()
}
