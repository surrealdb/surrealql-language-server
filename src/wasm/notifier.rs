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
    /// Optional `window/showMessage` surface. Hosts that omit it get
    /// the toast content routed through `onLogMessage` instead.
    pub show_message: Option<SendWrapper<Function>>,
    pub request_configuration: SendWrapper<Function>,
}

impl JsCallbacks {
    /// Decode a JS object literal of the shape documented on
    /// [`JsCallbacks`]. Missing or non-function fields produce a
    /// descriptive `JsValue` error, except `onShowMessage` which may
    /// be omitted entirely.
    pub fn from_object(value: &JsValue) -> Result<Self, JsValue> {
        let publish_diagnostics = require_function(value, "onPublishDiagnostics")?;
        let log_message = require_function(value, "onLogMessage")?;
        let show_message = optional_function(value, "onShowMessage")?;
        let request_configuration = require_function(value, "onRequestConfiguration")?;
        Ok(Self {
            publish_diagnostics: SendWrapper::new(publish_diagnostics),
            log_message: SendWrapper::new(log_message),
            show_message: show_message.map(SendWrapper::new),
            request_configuration: SendWrapper::new(request_configuration),
        })
    }
}

impl JsCallbacks {
    /// Synchronous log emission for contexts where awaiting the
    /// [`LspNotifier`] method isn't possible (`spawn_local` closures,
    /// failure paths inside other callbacks). Never throws back into
    /// Rust — a broken `onLogMessage` is the host's problem.
    fn emit_log(&self, level: MessageType, message: &str) {
        let level = serde_wasm_bindgen::to_value(&level).unwrap_or(JsValue::NULL);
        let message = JsValue::from_str(message);
        let _ = self.log_message.call2(&JsValue::NULL, &level, &message);
    }
}

fn require_function(value: &JsValue, key: &str) -> Result<Function, JsValue> {
    let property = js_sys::Reflect::get(value, &JsValue::from_str(key))?;
    property
        .dyn_into::<Function>()
        .map_err(|_| JsValue::from_str(&format!("callbacks.{key} must be a function")))
}

/// Absent / `null` / `undefined` is a valid "not provided"; anything
/// else that isn't a function is a host bug worth failing loudly on.
fn optional_function(value: &JsValue, key: &str) -> Result<Option<Function>, JsValue> {
    let property = js_sys::Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        return Ok(None);
    }
    property
        .dyn_into::<Function>()
        .map(Some)
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
        let count = diagnostics.len();
        let uri_js = JsValue::from_str(uri.as_str());
        let diagnostics = match serde_wasm_bindgen::to_value(&diagnostics) {
            Ok(value) => value,
            Err(error) => {
                // Never hand the host NULL in place of a diagnostics
                // array — keep its previous set and say why.
                self.callbacks.emit_log(
                    MessageType::ERROR,
                    &format!(
                        "SurrealQL: failed to serialise {count} diagnostics for {}: {error}",
                        uri.as_str()
                    ),
                );
                return;
            }
        };
        if self
            .callbacks
            .publish_diagnostics
            .call2(&JsValue::NULL, &uri_js, &diagnostics)
            .is_err()
        {
            self.callbacks.emit_log(
                MessageType::WARNING,
                &format!("SurrealQL: onPublishDiagnostics threw for {}", uri.as_str()),
            );
        }
    }

    async fn log_message(&self, level: MessageType, message: String) {
        // `MessageType` serialises as an integer matching the LSP wire
        // format (1 = Error, 2 = Warning, 3 = Info, 4 = Log).
        self.callbacks.emit_log(level, &message);
    }

    async fn show_message(&self, level: MessageType, message: String) {
        let Some(show_message) = &self.callbacks.show_message else {
            // No toast surface on this host — degrade to the log.
            self.log_message(level, message).await;
            return;
        };
        let level_js = serde_wasm_bindgen::to_value(&level).unwrap_or(JsValue::NULL);
        let message_js = JsValue::from_str(&message);
        if show_message
            .call2(&JsValue::NULL, &level_js, &message_js)
            .is_err()
        {
            // The toast callback threw — the message still matters,
            // so fall back to the log channel.
            self.callbacks.emit_log(level, &message);
        }
    }

    async fn request_configuration(&self) -> Option<Value> {
        // `JsFuture` is `!Send`, but the trait future returned by
        // `async_trait` requires `Send`. Run the JS-touching work on
        // `spawn_local` (which doesn't need Send) and bridge the
        // result back through a Send-friendly oneshot channel.
        let callbacks = self.callbacks.clone();
        let (tx, rx) = crate::runtime::sync::oneshot::channel::<Option<Value>>();
        wasm_bindgen_futures::spawn_local(async move {
            let value = invoke_request_configuration(&callbacks).await;
            let _ = tx.send(value);
        });
        rx.await.ok().flatten()
    }
}

/// Call the host's `onRequestConfiguration`. Every failure branch
/// logs a distinct reason before falling back to `None` — previously
/// a throwing/rejecting/garbage-returning host silently left the
/// server running on default settings.
async fn invoke_request_configuration(callbacks: &JsCallbacks) -> Option<Value> {
    let result = match callbacks.request_configuration.call0(&JsValue::NULL) {
        Ok(result) => result,
        Err(error) => {
            callbacks.emit_log(
                MessageType::WARNING,
                &format!(
                    "SurrealQL: onRequestConfiguration threw: {}; using existing settings",
                    describe_js_error(&error)
                ),
            );
            return None;
        }
    };

    // The host may return the configuration directly or hand back a
    // Promise; await both transparently.
    let value = if let Some(promise) = result.dyn_ref::<js_sys::Promise>() {
        match JsFuture::from(promise.clone()).await {
            Ok(value) => value,
            Err(error) => {
                callbacks.emit_log(
                    MessageType::WARNING,
                    &format!(
                        "SurrealQL: onRequestConfiguration promise rejected: {}; \
                         using existing settings",
                        describe_js_error(&error)
                    ),
                );
                return None;
            }
        }
    } else {
        result
    };

    if value.is_null() || value.is_undefined() {
        return None;
    }

    match serde_wasm_bindgen::from_value(value) {
        Ok(value) => Some(value),
        Err(error) => {
            callbacks.emit_log(
                MessageType::WARNING,
                &format!(
                    "SurrealQL: onRequestConfiguration returned an unparseable payload: \
                     {error}; using existing settings"
                ),
            );
            None
        }
    }
}

fn describe_js_error(error: &JsValue) -> String {
    error
        .as_string()
        .or_else(|| {
            error
                .dyn_ref::<js_sys::Error>()
                .map(|error| String::from(error.message()))
        })
        .unwrap_or_else(|| format!("{error:?}"))
}
