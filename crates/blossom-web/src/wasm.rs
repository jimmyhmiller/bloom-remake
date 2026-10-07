//! The page's API (BROWSER.md "Architecture"): the host core behind `wasm-bindgen`, strings and JSON in and out. The
//! page (`web/host.js`) applies the patches to the DOM, keeps the saved state in `localStorage`, and reports events.

#[cfg(feature = "compiler")]
use std::collections::BTreeMap;
use std::collections::VecDeque;

use wasm_bindgen::prelude::*;

use blossom_value::time::Instant;

use crate::link::{Heard, Link, LinkState};
use crate::{App, ClientDeployment, Compiled, Event, HostError};

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
    /// Frames to write to the server, taken from the app one at a time.
    frames: VecDeque<Vec<u8>>,
}

#[cfg(feature = "compiler")]
fn diags_error(diags: Vec<crate::Diag>) -> JsValue {
    match json(&diags) {
        Ok(text) => JsValue::from_str(&text),
        Err(e) => e,
    }
}

/// The root of a program's randomness, from 32 hex digits (the page draws them from `crypto.getRandomValues`).
#[cfg(feature = "compiler")]
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
/// diagnostics, as JSON, on failure; warnings are in [`WebApp::warnings`]. (Not in a member page's build.)
#[cfg(feature = "compiler")]
#[wasm_bindgen]
pub fn compile(root: &str, files_json: &str, seed_hex: &str) -> Result<WebApp, JsValue> {
    let files: BTreeMap<String, String> = serde_json::from_str(files_json).map_err(js_error)?;
    let compiled = crate::compile(root, &files).map_err(diags_error)?;
    Ok(WebApp {
        app: App::new(compiled, seed(seed_hex)?).map_err(js_error)?,
        frames: VecDeque::new(),
    })
}

/// What a client member's page runs (docs/design/CLIENTS.md §8), before it runs.
#[wasm_bindgen]
pub struct WebClient {
    compiled: Compiled,
}

/// Reads the client artifact `bytes` the server served, for the deployment `app_json` names (the server's
/// `/blossom/app.json`). Throws when it cannot be read.
#[wasm_bindgen(js_name = loadClient)]
pub fn load_client(bytes: &[u8], app_json: &str) -> Result<WebClient, JsValue> {
    let deployment: ClientDeployment = serde_json::from_str(app_json).map_err(js_error)?;
    let compiled = crate::load_client(bytes, &deployment).map_err(js_error)?;
    Ok(WebClient { compiled })
}

/// A refusal as the page reads it: `{reason, detail}`.
fn refusal(reason: &str, detail: &str) -> serde_json::Value {
    serde_json::json!({ "reason": reason, "detail": detail })
}

#[wasm_bindgen]
impl WebClient {
    /// The compile's warnings, as JSON.
    pub fn warnings(&self) -> Result<String, JsValue> {
        json(&self.compiled.warnings)
    }

    /// The page's link to its server, resuming `state_json` (the last [`WebApp::link_state`], or empty).
    pub fn link(&self, state_json: &str) -> Result<WebLink, JsValue> {
        let state: Option<LinkState> = if state_json.is_empty() {
            None
        } else {
            Some(serde_json::from_str(state_json).map_err(js_error)?)
        };
        let link = self
            .compiled
            .link(state.as_ref())
            .map_err(js_error)?
            .ok_or_else(|| JsValue::from_str("the program was not compiled for a client member"))?;
        Ok(WebLink {
            link,
            frames: VecDeque::new(),
        })
    }
}

/// A member page's link before the page runs: the first connection's handshake, which gives the page its identity.
#[wasm_bindgen]
pub struct WebLink {
    link: Link,
    frames: VecDeque<Vec<u8>>,
}

#[wasm_bindgen]
impl WebLink {
    /// Whether the link knows its member (stored from an earlier page): the page can then run before it connects.
    #[wasm_bindgen(js_name = hasMember)]
    pub fn has_member(&self) -> bool {
        self.link.member().is_some()
    }

    /// The first frame of a connection.
    pub fn hello(&mut self) -> Vec<u8> {
        self.link.hello()
    }

    /// Takes a frame from the server: `{welcome, refused}`. `welcome` is true when it finished the handshake (the page
    /// can then run); `refused` is `{reason, detail}` when the server refused the page (`program`: the page was built
    /// from another program, and loading it again gets the current one). Throws on anything else that goes wrong.
    pub fn recv(&mut self, bytes: &[u8]) -> Result<String, JsValue> {
        let (heard, frames) = match self.link.recv(bytes) {
            Ok(ok) => ok,
            Err(HostError::Refused { reason, detail }) => {
                return json(&serde_json::json!({ "welcome": false, "refused": refusal(&reason, &detail) }));
            }
            Err(e) => return Err(js_error(e)),
        };
        self.frames.extend(frames);
        let welcome = match heard {
            Heard::Welcome { .. } => true,
            Heard::Nothing => false,
            Heard::Deliveries(_) => {
                return Err(JsValue::from_str(
                    "the server sent messages before the handshake finished",
                ));
            }
        };
        json(&serde_json::json!({ "welcome": welcome, "refused": null }))
    }

    /// The connection ended before the handshake finished.
    pub fn down(&mut self) {
        self.link.down();
    }
}

#[wasm_bindgen]
impl WebApp {
    /// A client member's page: `client`'s program running as the member `link` knows (it takes both). The frames the
    /// link's handshake left to write come first from [`WebApp::take_frame`].
    pub fn member(client: WebClient, link: WebLink) -> Result<WebApp, JsValue> {
        let WebLink { link, frames } = link;
        Ok(WebApp {
            app: App::member(client.compiled, link).map_err(js_error)?,
            frames,
        })
    }

    /// The first frame of a connection to the server.
    #[wasm_bindgen(js_name = linkHello)]
    pub fn link_hello(&mut self) -> Result<Vec<u8>, JsValue> {
        self.app.link_hello().map_err(js_error)
    }

    /// Takes a frame from the server at `now_ms`: `{patches, restart, refused}` (`refused` as for [`WebLink::recv`]). `restart` is true when the server gave the
    /// page another identity (it lost the old one): the page's state is the old identity's, and it must start over.
    #[wasm_bindgen(js_name = linkRecv)]
    pub fn link_recv(&mut self, bytes: &[u8], now_ms: f64) -> Result<String, JsValue> {
        match self.app.link_recv(bytes, instant(now_ms)?) {
            Ok(patches) => json(&serde_json::json!({ "patches": patches, "restart": false, "refused": null })),
            Err(HostError::Identity { .. }) => {
                json(&serde_json::json!({ "patches": [], "restart": true, "refused": null }))
            }
            Err(HostError::Refused { reason, detail }) => json(&serde_json::json!({
                "patches": [], "restart": false, "refused": refusal(&reason, &detail)
            })),
            Err(e) => Err(js_error(e)),
        }
    }

    /// The connection to the server ended at `now_ms`: the patches.
    #[wasm_bindgen(js_name = linkDown)]
    pub fn link_down(&mut self, now_ms: f64) -> Result<String, JsValue> {
        json(&self.app.link_down(instant(now_ms)?).map_err(js_error)?)
    }

    /// The next frame to write to the server (`undefined` when there is none). The page writes them after every
    /// call that runs rounds.
    #[wasm_bindgen(js_name = takeFrame)]
    pub fn take_frame(&mut self) -> Option<Vec<u8>> {
        if self.frames.is_empty() {
            self.frames.extend(self.app.take_frames());
        }
        self.frames.pop_front()
    }

    /// What the page stores to resume its link after a reload, as JSON (empty for a page on its own).
    #[wasm_bindgen(js_name = linkState)]
    pub fn link_state(&self) -> Result<String, JsValue> {
        match self.app.link_state() {
            Some(s) => json(&s),
            None => Ok(String::new()),
        }
    }

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

    /// The durable rows to save since the last call, as JSON (`App::save_changes`).
    #[wasm_bindgen(js_name = saveChanges)]
    pub fn save_changes(&mut self) -> Result<String, JsValue> {
        let changes = self.app.save_changes().map_err(js_error)?;
        json(&changes)
    }
}
