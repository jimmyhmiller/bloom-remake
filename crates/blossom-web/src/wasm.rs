//! The page's API (BROWSER.md "Architecture"): the host core behind `wasm-bindgen`, strings and JSON in and out. The
//! page (`web/host.js`) applies the patches to the DOM, keeps the saved state in `localStorage`, and reports events.

use std::collections::BTreeMap;

use wasm_bindgen::prelude::*;

use crate::{App, Event};

fn js_error(e: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&e.to_string())
}

fn json(v: &impl serde::Serialize) -> Result<String, JsValue> {
    serde_json::to_string(v).map_err(js_error)
}

/// A program compiled and ready to start, or the diagnostics of its compile.
#[wasm_bindgen]
pub struct WebApp {
    app: App,
}

/// Compiles `root` of `files_json` (a JSON object, path → source). Throws the diagnostics, as JSON, on failure;
/// warnings are in [`WebApp::warnings`].
#[wasm_bindgen]
pub fn compile(root: &str, files_json: &str) -> Result<WebApp, JsValue> {
    let files: BTreeMap<String, String> = serde_json::from_str(files_json).map_err(js_error)?;
    let compiled = crate::compile(root, &files).map_err(|diags| match json(&diags) {
        Ok(text) => JsValue::from_str(&text),
        Err(e) => e,
    })?;
    Ok(WebApp {
        app: App::new(compiled).map_err(js_error)?,
    })
}

#[wasm_bindgen]
impl WebApp {
    /// The compile's warnings, as JSON.
    pub fn warnings(&self) -> Result<String, JsValue> {
        json(&self.app.compiled().warnings)
    }

    /// The DOM events the program listens to, as JSON (`["click", …]`).
    pub fn listens(&self) -> Result<String, JsValue> {
        json(&self.app.compiled().listens())
    }

    /// Starts the program from `saved` (the last [`WebApp::saved`], or empty) at `hash`: `{patches, notes}`.
    pub fn start(&mut self, saved: &str, hash: &str) -> Result<String, JsValue> {
        let saved = (!saved.is_empty()).then_some(saved);
        json(&self.app.start(saved, hash).map_err(js_error)?)
    }

    /// Runs one event (`{kind, …}`, see [`Event`]): the patches.
    pub fn dispatch(&mut self, event_json: &str) -> Result<String, JsValue> {
        let event: Event = serde_json::from_str(event_json).map_err(js_error)?;
        json(&self.app.dispatch(&event).map_err(js_error)?)
    }

    /// The durable tables, as JSON, for `localStorage`.
    pub fn saved(&self) -> Result<String, JsValue> {
        self.app.saved().map_err(js_error)
    }
}
