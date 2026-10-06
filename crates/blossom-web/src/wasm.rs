//! The page's API (BROWSER.md "Architecture"): the host core behind `wasm-bindgen`, strings and JSON in and out. The
//! page (`web/host.js`) applies the patches to the DOM, keeps the saved state in `localStorage`, and reports events.

use std::collections::BTreeMap;

use wasm_bindgen::prelude::*;

use blossom_value::time::Instant;

use crate::{App, Event};

/// The page's clock (milliseconds since the epoch, as `performance.timeOrigin + performance.now()` gives them) as
/// an instant.
fn instant(ms: f64) -> Result<Instant, JsValue> {
    if !ms.is_finite() {
        return Err(JsValue::from_str(&format!("the clock reads {ms}, not a time")));
    }
    // Saturates far outside any clock's range.
    Ok(Instant((ms * 1e6).round() as i64))
}

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

/// The root of a program's randomness, from 32 hex digits (the page draws them from `crypto.getRandomValues`).
fn seed(hex: &str) -> Result<blossom_value::Seed, JsValue> {
    let bad = || JsValue::from_str(&format!("a seed is 32 hex digits, not `{hex}`"));
    if hex.len() != 32 || !hex.is_ascii() {
        return Err(bad());
    }
    let mut bytes = [0u8; 16];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = hex
            .get(2 * i..2 * i + 2)
            .and_then(|pair| u8::from_str_radix(pair, 16).ok())
            .ok_or_else(bad)?;
    }
    Ok(blossom_value::Seed(bytes))
}

/// Compiles `root` of `files_json` (a JSON object, path → source), its randomness rooted at `seed_hex`. Throws the
/// diagnostics, as JSON, on failure; warnings are in [`WebApp::warnings`].
#[wasm_bindgen]
pub fn compile(root: &str, files_json: &str, seed_hex: &str) -> Result<WebApp, JsValue> {
    let files: BTreeMap<String, String> = serde_json::from_str(files_json).map_err(js_error)?;
    let compiled = crate::compile(root, &files).map_err(|diags| match json(&diags) {
        Ok(text) => JsValue::from_str(&text),
        Err(e) => e,
    })?;
    Ok(WebApp {
        app: App::new(compiled, seed(seed_hex)?).map_err(js_error)?,
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

    /// Starts the program at `now_ms` from `saved` (the last [`WebApp::saved`], or empty) at `hash`:
    /// `{patches, notes}`.
    pub fn start(&mut self, saved: &str, hash: &str, now_ms: f64) -> Result<String, JsValue> {
        let saved = (!saved.is_empty()).then_some(saved);
        json(&self.app.start(saved, hash, instant(now_ms)?).map_err(js_error)?)
    }

    /// Runs one event (`{kind, …}`, see [`Event`]) at `now_ms`: the patches.
    pub fn dispatch(&mut self, event_json: &str, now_ms: f64) -> Result<String, JsValue> {
        let event: Event = serde_json::from_str(event_json).map_err(js_error)?;
        json(&self.app.dispatch(&event, instant(now_ms)?).map_err(js_error)?)
    }

    /// Whether the program has physical timers: the page then calls [`WebApp::advance`] every animation frame.
    pub fn clocked(&self) -> bool {
        self.app.clocked()
    }

    /// Moves the clock to `now_ms`, running the timers due by then: the patches (`[]` when none was due).
    pub fn advance(&mut self, now_ms: f64) -> Result<String, JsValue> {
        json(&self.app.advance(instant(now_ms)?).map_err(js_error)?)
    }

    /// Why the element `id` is on the page as it is, as JSON: a tree of `{fact, how, round, because}` (see
    /// [`crate::why::Why`]).
    pub fn why(&self, id: &str) -> Result<String, JsValue> {
        json(&self.app.why(id).map_err(js_error)?)
    }

    /// The durable tables, as JSON, for `localStorage`.
    pub fn saved(&self) -> Result<String, JsValue> {
        self.app.saved().map_err(js_error)
    }
}
