//! Durable Objects reaching each other by RPC (docs/design/KEYED.md §4), without the platform: the objects of a
//! deployment of `relay.bls` (a lobby node and keyed games) as `Object`s over storages in memory, their messages to
//! each other routed by name as the Worker routes them. The lobby starts two games; each answers it and pings the
//! `ledger` game, which tells the lobby who pinged; every object then starts again from its storage alone.

use std::collections::BTreeMap;
use std::path::Path;

use blossom_do::{Object, OutputItem, decode_output, instant_of_ms};
use blossom_value::Value;

#[cfg(test)]
const SEED: [u8; 16] = [7; 16];

/// A deployment's objects and their storages, as the platform keeps them.
#[cfg(test)]
struct Platform {
    files: BTreeMap<String, String>,
    deploy: String,
    objects: BTreeMap<String, Object>,
    storage: BTreeMap<String, BTreeMap<String, Vec<u8>>>,
    now: f64,
}

#[cfg(test)]
impl Platform {
    fn entries(&self, name: &str) -> Vec<u8> {
        let mut out = Vec::new();
        for (k, v) in self.storage.get(name).into_iter().flatten() {
            out.push(0);
            out.extend_from_slice(&(k.len() as u32).to_le_bytes());
            out.extend_from_slice(k.as_bytes());
            out.extend_from_slice(&(v.len() as u32).to_le_bytes());
            out.extend_from_slice(v);
        }
        out
    }

    /// Object `name`, started from its storage if it is not running.
    fn object(&mut self, name: &str) -> &mut Object {
        if !self.objects.contains_key(name) {
            self.now += 1.0;
            let entries = self.entries(name);
            let o = Object::open(
                &self.files,
                &self.deploy,
                name,
                SEED,
                &entries,
                instant_of_ms(self.now),
                1,
            )
            .unwrap_or_else(|e| panic!("{name}: {e}"));
            self.objects.insert(name.to_owned(), o);
        }
        self.objects.get_mut(name).unwrap()
    }

    /// Commits object `name`'s writes; its messages to other objects.
    fn flush(&mut self, name: &str) -> Vec<(String, String, Vec<u8>)> {
        let o = self.objects.get_mut(name).unwrap();
        let writes = o.take_writes().unwrap();
        let out = decode_output(&o.take_output().unwrap()).unwrap();
        let store = self.storage.entry(name.to_owned()).or_default();
        let mut b = writes.as_slice();
        while let Some((&op, rest)) = b.split_first() {
            let klen = u32::from_le_bytes(rest[..4].try_into().unwrap()) as usize;
            let key = String::from_utf8(rest[4..4 + klen].to_vec()).unwrap();
            b = &rest[4 + klen..];
            if op == 0 {
                let vlen = u32::from_le_bytes(b[..4].try_into().unwrap()) as usize;
                store.insert(key, b[4..4 + vlen].to_vec());
                b = &b[4 + vlen..];
            } else {
                store.remove(&key);
            }
        }
        out.into_iter()
            .filter_map(|o| match o {
                OutputItem::Rpc { to, from, frame } => Some((to, from, frame)),
                _ => None,
            })
            .collect()
    }

    /// Wakes `name`, then delivers every message until none moves.
    fn run(&mut self, name: &str) {
        self.now += 1.0;
        let now = instant_of_ms(self.now);
        self.object(name).wake(now).unwrap();
        let mut queue = self.flush(name);
        while let Some((to, from, frame)) = queue.pop() {
            self.now += 1.0;
            let now = instant_of_ms(self.now);
            let sender = (!from.is_empty()).then_some(from.as_str());
            self.object(&to).rpc(sender, &frame, now).unwrap();
            queue.extend(self.flush(&to));
        }
    }
}

#[cfg(test)]
fn platform() -> Platform {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/integration/fixtures/keyed/relay.bls");
    let mut files = BTreeMap::new();
    files.insert("relay.bls".to_owned(), std::fs::read_to_string(root).unwrap());
    let node = |name: &str, role: &str| {
        format!(
            "[[node]]\nname = \"{name}\"\nrole = \"{role}\"\naddr = \"127.0.0.1:1\"\n\
             principal = \"spiffe://object/relay/{role}/{name}\"\n"
        )
    };
    let deploy = format!(
        "format = 1\n[deployment]\nid = \"relay-object\"\nprogram = \"relay\"\nversion = 1\nsource = \"relay.bls\"\n{}{}\
         [security]\nmode = \"insecure-dev\"\n[storage]\ndata_dir = \"data\"\n",
        node("g", "Game"),
        node("lobby", "Lobby"),
    );
    Platform {
        files,
        deploy,
        objects: BTreeMap::new(),
        storage: BTreeMap::new(),
        now: 1_000.0,
    }
}

#[test]
fn objects_message_each_other_and_keep_it_across_a_restart() {
    let mut p = platform();
    p.run("node/lobby");
    let lobby = p.objects.get("node/lobby").unwrap();
    let mut heard = lobby.rows("heard").unwrap();
    heard.sort();
    let u = |n: u64| Value::Int(blossom_value::value::IntValue::U64(n));
    assert_eq!(heard, vec![vec![Value::str("a"), u(1)], vec![Value::str("b"), u(2)]]);
    let pongs: Vec<String> = lobby
        .rows("pongs")
        .unwrap()
        .iter()
        .map(|r| match &r[0] {
            Value::Member(m) => format!("{}/{}", m.role_name, m.key),
            other => panic!("{other:?}"),
        })
        .collect();
    let mut pongs = pongs;
    pongs.sort();
    assert_eq!(pongs, ["Game/a", "Game/b"]);
    // Each game is an object of its own, and the ledger one more.
    let mut names: Vec<&String> = p.objects.keys().collect();
    names.sort();
    assert_eq!(
        names,
        ["member/Game/a", "member/Game/b", "member/Game/ledger", "node/lobby"]
    );
    assert_eq!(p.objects["member/Game/a"].rows("began").unwrap(), vec![vec![u(1)]]);
    // Every object starts again from its storage alone.
    p.objects.clear();
    assert_eq!(p.object("member/Game/b").rows("began").unwrap(), vec![vec![u(2)]]);
    assert_eq!(p.object("node/lobby").rows("heard").unwrap().len(), 2);
}
