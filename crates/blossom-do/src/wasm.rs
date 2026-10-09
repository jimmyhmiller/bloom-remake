//! The Worker's API (wasm-bindgen): numbers as JavaScript has them (`f64` milliseconds and connection ids), bytes as
//! `Uint8Array`s, errors as exceptions carrying the message.

use std::collections::BTreeMap;

use wasm_bindgen::prelude::*;

use crate::{Object, Site, instant_of_ms};

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
    /// `files`: the program's sources as JSON (`{"path": "text"}`); `deploy`: the deployment spec's text; `name`: the
    /// object's (`node/NAME`, `member/ROLE/KEY`); `seed`: the deployment's, 16 bytes; `entries`: what the object's
    /// storage holds (the crate's encoding).
    #[wasm_bindgen(constructor)]
    pub fn new(
        files: &str,
        deploy: &str,
        name: &str,
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
        Object::open(&files, deploy, name, seed, entries, instant_of_ms(now_ms), nonce as u64)
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

    /// A frame another object sent by RPC; `sender`: the deployment node that sent a `BATCH` (none for a member's).
    pub fn rpc(&mut self, sender: Option<String>, bytes: &[u8], now_ms: f64) -> Result<(), JsValue> {
        self.0
            .rpc(
                sender.as_deref().filter(|s| !s.is_empty()),
                bytes,
                instant_of_ms(now_ms),
            )
            .map_err(js)
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

/// What the Worker serves for the deployment itself (no object): `app.json`, client parts, the keyed role pages link
/// to.
#[wasm_bindgen]
pub struct DoSite(Site);

#[wasm_bindgen]
impl DoSite {
    #[wasm_bindgen(constructor)]
    pub fn new(files: &str, deploy: &str, node: &str) -> Result<DoSite, JsValue> {
        let files: BTreeMap<String, String> =
            serde_json::from_str(files).map_err(|e| js(format!("the sources: {e}")))?;
        Site::open(&files, deploy, node).map(DoSite).map_err(js)
    }

    #[wasm_bindgen(js_name = appJson)]
    pub fn app_json(&self) -> String {
        self.0.app_json().to_owned()
    }

    #[wasm_bindgen(js_name = clientPart)]
    pub fn client_part(&self, role: &str) -> Option<Vec<u8>> {
        self.0.client_part(role).map(<[u8]>::to_vec)
    }

    #[wasm_bindgen(js_name = keyedRole)]
    pub fn keyed_role(&self) -> Option<String> {
        self.0.keyed_role().map(str::to_owned)
    }
}

/// The registry's mint: a page's token for `role` and `serial`, signed for the deployment seeded `seed`.
#[wasm_bindgen(js_name = mintToken)]
pub fn mint_token(seed: &[u8], role: &str, serial: f64) -> Result<Vec<u8>, JsValue> {
    let seed: [u8; 16] = seed
        .try_into()
        .map_err(|_| js(format!("a seed of {} bytes, not 16", seed.len())))?;
    if serial.fract() != 0.0 || !(0.0..f64::from(blossom_value::time::NodeId::CLIENT_SERIALS)).contains(&serial) {
        return Err(js(format!("{serial} is not a page serial")));
    }
    Ok(crate::mint_token(seed, role, serial as u32))
}
