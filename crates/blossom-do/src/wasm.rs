//! The Worker's API (wasm-bindgen): numbers as JavaScript has them (`f64` milliseconds and connection ids), bytes as
//! `Uint8Array`s, errors as exceptions carrying the message.

use std::collections::BTreeMap;

use wasm_bindgen::prelude::*;

use crate::{Object, instant_of_ms};

fn js(e: String) -> JsValue {
    JsValue::from_str(&e)
}

/// A connection id as JavaScript passes it: a whole, non-negative number.
fn conn_of(id: f64) -> Result<u64, JsValue> {
    if id.fract() != 0.0 || !(0.0..=9_007_199_254_740_991.0).contains(&id) {
        return Err(js(format!("{id} is not a connection id")));
    }
    Ok(id as u64)
}

#[wasm_bindgen]
pub struct DoNode(Object);

#[wasm_bindgen]
impl DoNode {
    /// `files`: the program's sources as JSON (`{"path": "text"}`); `deploy`: the deployment spec's text; `seed`: 16
    /// bytes; `entries`: what the object's storage holds (the crate's encoding).
    #[wasm_bindgen(constructor)]
    pub fn new(
        files: &str,
        deploy: &str,
        node: &str,
        seed: &[u8],
        entries: &[u8],
        now_ms: f64,
        nonce: f64,
    ) -> Result<DoNode, JsValue> {
        let files: BTreeMap<String, String> =
            serde_json::from_str(files).map_err(|e| js(format!("the sources: {e}")))?;
        let seed: [u8; 16] = seed
            .try_into()
            .map_err(|_| js(format!("a seed of {} bytes, not 16", seed.len())))?;
        Object::open(&files, deploy, node, seed, entries, instant_of_ms(now_ms), nonce as u64)
            .map(DoNode)
            .map_err(js)
    }

    #[wasm_bindgen(js_name = appJson)]
    pub fn app_json(&self) -> String {
        self.0.app_json().to_owned()
    }

    #[wasm_bindgen(js_name = clientPart)]
    pub fn client_part(&self, role: &str) -> Option<Vec<u8>> {
        self.0.client_part(role).map(<[u8]>::to_vec)
    }

    pub fn connect(&mut self) -> f64 {
        self.0.connect() as f64
    }

    pub fn frame(&mut self, conn: f64, bytes: &[u8], now_ms: f64, entropy: &[u8]) -> Result<(), JsValue> {
        self.0
            .frame(conn_of(conn)?, bytes, instant_of_ms(now_ms), entropy)
            .map_err(js)
    }

    pub fn closed(&mut self, conn: f64, now_ms: f64) -> Result<(), JsValue> {
        self.0.closed(conn_of(conn)?, instant_of_ms(now_ms)).map_err(js)
    }

    pub fn wake(&mut self, now_ms: f64) -> Result<(), JsValue> {
        self.0.wake(instant_of_ms(now_ms)).map_err(js)
    }

    #[wasm_bindgen(js_name = nextWake)]
    pub fn next_wake(&self) -> Result<Option<f64>, JsValue> {
        self.0.next_wake_ms().map_err(js)
    }

    #[wasm_bindgen(js_name = takeWrites)]
    pub fn take_writes(&mut self) -> Result<Vec<u8>, JsValue> {
        self.0.take_writes().map_err(js)
    }

    #[wasm_bindgen(js_name = takeOutput)]
    pub fn take_output(&mut self) -> Result<Vec<u8>, JsValue> {
        self.0.take_output().map_err(js)
    }
}
