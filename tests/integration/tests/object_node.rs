//! A node as a Durable Object hosts it (docs/design/DURABLE-OBJECTS.md), without the platform: the polls app's server
//! on an `ObjectNode` over a key-value store (`KvFs` on `MemKv`), driven by calls, and two pages (the browser host's
//! engine, `blossom_web::App` as client members) whose link frames go back and forth in memory. A vote reaches the other
//! page; after the object restarts from nothing but the keys and values, the pages reconnect and everything is there.

use std::path::Path;
use std::sync::Arc;

use blossom_driver::bls::compile_deployed;
use blossom_front::api::NodeSpec;
use blossom_runtime::deploy::DeploymentSpec;
use blossom_runtime::object::{ObjectConfig, ObjectNode, Output};
use blossom_store::{KvFs, KvStore, MemKv};
use blossom_value::time::Instant;
use blossom_web::{App, ClientDeployment, Event, Patch};

#[cfg(test)]
fn spec() -> DeploymentSpec {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/web/polls.bls");
    let text = format!(
        "format = 1\n[deployment]\nid = \"polls-object\"\nprogram = \"polls\"\nversion = 1\nsource = \"{}\"\n\
         [[node]]\nname = \"s\"\nrole = \"Server\"\naddr = \"127.0.0.1:1\"\nprincipal = \"spiffe://test/polls/Server/s\"\n\
         [security]\nmode = \"insecure-dev\"\n[storage]\ndata_dir = \"data\"\n",
        source.display()
    );
    DeploymentSpec::parse(&text, &std::env::temp_dir()).unwrap()
}

#[cfg(test)]
fn open(kv: &Arc<MemKv>, now: i64) -> ObjectNode {
    let spec = spec();
    let nodes: Vec<NodeSpec> = spec
        .nodes
        .iter()
        .map(|n| NodeSpec {
            name: n.name.clone(),
            role: n.role.clone(),
        })
        .collect();
    let (compiled, _) = compile_deployed(&spec.source.to_string_lossy(), &nodes, &Default::default());
    let artifact = Arc::new(compiled.unwrap().0);
    let mut counter = 0u8;
    ObjectNode::open(ObjectConfig {
        spec,
        artifact,
        node: "s".into(),
        member: None,
        members: Arc::new(blossom_ir::members::Members::open()),
        fs: Arc::new(KvFs::open(kv.clone() as Arc<dyn KvStore>).unwrap()),
        dir: "/s".into(),
        seed: blossom_value::Seed([5; 16]),
        now: Instant(now),
        nonce: now as u64,
        // Tokens' secrets: distinct, not secret (a test).
        random: Box::new(move |buf| {
            for b in buf.iter_mut() {
                counter = counter.wrapping_add(1);
                *b = counter;
            }
            Ok(())
        }),
        externs: Arc::new(blossom_std_host::registry().unwrap()),
    })
    .unwrap()
}

/// A page, as the browser host runs it: its app and the connection its link is on.
struct Page {
    app: App,
    conn: Option<u64>,
    now: i64,
    texts: std::collections::BTreeMap<String, String>,
    alive: std::collections::BTreeSet<String>,
}

#[cfg(test)]
impl Page {
    fn apply(&mut self, patches: &[Patch]) {
        for p in patches {
            match p {
                Patch::Text { id, text } => {
                    self.texts.insert(id.clone(), text.clone());
                }
                Patch::Create { id, .. } => {
                    self.alive.insert(id.clone());
                }
                Patch::Remove { id } => {
                    self.alive.remove(id);
                    self.texts.remove(id);
                }
                _ => {}
            }
        }
    }

    fn event(&mut self, obj: &mut ObjectNode, e: Event) {
        self.now += 1;
        let patches = self.app.dispatch(&e, Instant(self.now)).unwrap();
        self.apply(&patches);
        self.send(obj);
    }

    /// Writes what the page's rounds want sent to its connection.
    fn send(&mut self, obj: &mut ObjectNode) {
        let frames = self.app.take_frames();
        if let Some(conn) = self.conn {
            for f in frames {
                obj.frame(conn, &f, Instant(self.now)).unwrap();
            }
        }
    }

    fn typed(&mut self, obj: &mut ObjectNode, id: &str, value: &str) {
        self.event(
            obj,
            Event::Input {
                id: id.into(),
                value: value.into(),
            },
        );
    }

    fn texts_with(&self, prefix: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .texts
            .iter()
            .filter(|(id, _)| id.starts_with(prefix) && self.alive.contains(*id))
            .map(|(_, t)| t.clone())
            .collect();
        out.sort();
        out
    }
}

/// Opens a page on the object: the handshake, then its app as a member.
#[cfg(test)]
fn page(obj: &mut ObjectNode) -> Page {
    let app_json = obj.app_json().to_owned();
    let deployment: ClientDeployment = serde_json::from_str(&app_json).unwrap();
    let compiled = blossom_web::load_client(obj.client_part("Browser").unwrap(), &deployment).unwrap();
    let mut link = compiled.link(None).unwrap().unwrap();
    let conn = obj.connect();
    obj.frame(conn, &link.hello(), Instant(1_000)).unwrap();
    let mut early = Vec::new();
    let mut welcomed = false;
    for o in obj.take_output() {
        if let Output::Frame { conn: c, bytes } = o
            && c == conn
        {
            if welcomed {
                early.push(bytes);
                continue;
            }
            let (heard, frames) = link.recv(&bytes).unwrap();
            early.extend(frames);
            welcomed = matches!(heard, blossom_web::link::Heard::Welcome { .. });
        }
    }
    assert!(welcomed, "the object welcomed the page");
    let mut p = Page {
        app: App::member(compiled, link).unwrap(),
        conn: Some(conn),
        now: 1_000,
        texts: Default::default(),
        alive: Default::default(),
    };
    let started = p.app.start(None, "", Instant(p.now)).unwrap();
    p.apply(&started.patches);
    for f in early {
        let patches = p.app.link_recv(&f, Instant(p.now)).unwrap();
        p.apply(&patches);
    }
    p.send(obj);
    p
}

/// Delivers the object's output to the pages, and the pages' replies to the object, until nothing moves.
#[cfg(test)]
fn pump(obj: &mut ObjectNode, pages: &mut [&mut Page]) {
    loop {
        let out = obj.take_output();
        if out.is_empty() {
            return;
        }
        for o in out {
            match o {
                Output::Frame { conn, bytes } => {
                    if let Some(p) = pages.iter_mut().find(|p| p.conn == Some(conn)) {
                        p.now += 1;
                        let patches = p.app.link_recv(&bytes, Instant(p.now)).unwrap();
                        p.apply(&patches);
                        p.send(obj);
                    }
                }
                Output::Close { conn } => {
                    if let Some(p) = pages.iter_mut().find(|p| p.conn == Some(conn)) {
                        p.conn = None;
                    }
                }
                Output::Send { to, .. } => panic!("the polls server sent to node {}", to.0),
            }
        }
    }
}

#[cfg(test)]
fn sign_in(obj: &mut ObjectNode, p: &mut Page, name: &str) {
    p.event(
        obj,
        Event::Keydown {
            id: "name".into(),
            key: "Enter".into(),
            value: name.into(),
        },
    );
}

#[test]
fn the_polls_app_runs_on_an_object_and_survives_its_restart() {
    let kv = Arc::new(MemKv::default());
    let mut obj = open(&kv, 1_000);
    let mut a = page(&mut obj);
    let mut b = page(&mut obj);
    pump(&mut obj, &mut [&mut a, &mut b]);
    sign_in(&mut obj, &mut a, "Ada");
    sign_in(&mut obj, &mut b, "Bob");
    pump(&mut obj, &mut [&mut a, &mut b]);
    a.typed(&mut obj, "question", "Lunch?");
    a.typed(&mut obj, "opt-0", "Tacos");
    a.typed(&mut obj, "opt-1", "Ramen");
    a.event(&mut obj, Event::Click { id: "ask".into() });
    pump(&mut obj, &mut [&mut a, &mut b]);
    // Bob's page shows Ada's poll, and votes for Ramen.
    let ramen = b
        .alive
        .iter()
        .find(|id| id.starts_with("vote-") && id.ends_with("-1"))
        .cloned()
        .unwrap_or_else(|| panic!("no choice on Bob's page: {:?}", b.alive));
    b.event(&mut obj, Event::Click { id: ramen.clone() });
    pump(&mut obj, &mut [&mut a, &mut b]);
    assert_eq!(obj.rows("Server.votes").unwrap().len(), 1);
    assert_eq!(a.texts_with(&format!("{ramen}/span.2")), ["1"]);
    // The object restarts from nothing but the keys and values: the pages' links drop, and they reconnect.
    drop(obj);
    let mut obj = open(&kv, 5_000);
    assert_eq!(obj.rows("Server.polls").unwrap().len(), 1);
    assert_eq!(obj.rows("Server.votes").unwrap().len(), 1);
    for p in [&mut a, &mut b] {
        p.now += 1;
        let patches = p.app.link_down(Instant(p.now)).unwrap();
        p.apply(&patches);
        let conn = obj.connect();
        p.conn = Some(conn);
        obj.frame(conn, &p.app.link_hello().unwrap(), Instant(5_000)).unwrap();
    }
    pump(&mut obj, &mut [&mut a, &mut b]);
    // The same members (their tokens are in the stored registry): Ada votes Ramen too, and both pages count two.
    let ramen_a = a
        .alive
        .iter()
        .find(|id| id.starts_with("vote-") && id.ends_with("-1"))
        .cloned()
        .unwrap();
    a.event(&mut obj, Event::Click { id: ramen_a.clone() });
    pump(&mut obj, &mut [&mut a, &mut b]);
    assert_eq!(obj.rows("Server.votes").unwrap().len(), 2);
    assert_eq!(b.texts_with(&format!("{ramen}/span.2")), ["2"]);
}
