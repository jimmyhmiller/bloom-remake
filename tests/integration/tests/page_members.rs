//! S21: the page as a client member (docs/design/CLIENTS.md §5), without a browser. A real node serves the chat example
//! with `--web`; two pages fetch `/blossom/app.json`, compile the program for the deployment it names, open their links
//! and run as members of `Browser`: a line typed in one appears in both, a page whose link is down queues its line and
//! sends it on the next connection, and a reloaded page resumes its identity. The shared TodoMVC keeps two pages'
//! lists the same through adds, toggles, deletes and an offline edit.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use blossom_driver::bls::compile_deployed;
use blossom_front::api::NodeSpec;
use blossom_integration_tests::ws::Ws;
use blossom_runtime::deploy::DeploymentSpec;
use blossom_runtime::server::{Server, ServerConfig, WebConfig};
use blossom_store::OpenMode;
use blossom_value::time::Instant;
use blossom_web::link::LinkState;
use blossom_web::{App, ClientDeployment, Compiled, Event, Patch};

#[cfg(test)]
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// The example `program` (examples/web/PROGRAM.bls) deployed on one server node `s`, serving its page on `port`.
#[cfg(test)]
fn serve(name: &str, program: &str, port: u16) -> Server {
    let dir = std::env::temp_dir().join(format!("blossom-pages-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let secrets = dir.join("chat.secrets");
    std::fs::write(&secrets, "seed = \"00112233445566778899aabbccddeeff\"\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../examples/web/{program}.bls"));
    let text = format!(
        "format = 1\n[deployment]\nid = \"{program}-pages\"\nprogram = \"{program}\"\nversion = 1\nsource = \"{}\"\n\
         secrets = \"chat.secrets\"\n[[node]]\nname = \"s\"\nrole = \"Server\"\naddr = \"127.0.0.1:{}\"\n\
         principal = \"spiffe://test/{program}/Server/s\"\n[security]\nmode = \"insecure-dev\"\n[storage]\ndata_dir = \"data\"\n",
        source.display(),
        free_port(),
    );
    let spec = DeploymentSpec::parse(&text, &dir).unwrap();
    let nodes: Vec<NodeSpec> = spec
        .nodes
        .iter()
        .map(|n| NodeSpec {
            name: n.name.clone(),
            role: n.role.clone(),
        })
        .collect();
    let (compiled, sources) = compile_deployed(&spec.source.to_string_lossy(), &nodes, &BTreeMap::new());
    let artifact = Arc::new(compiled.unwrap().0);
    let app = blossom_runtime::web::app_json(&spec, &sources, "s").unwrap();
    Server::start(ServerConfig {
        spec,
        artifact,
        node: "s".into(),
        mode: OpenMode::InitFresh,
        dir: None,
        backend: blossom_node::Backend::Engine,
        externs: Arc::new(blossom_std_host::registry().unwrap()),
        record: None,
        web: Some(WebConfig {
            addr: format!("127.0.0.1:{port}").parse().unwrap(),
            root: None,
            app,
        }),
    })
    .unwrap()
}

/// `GET path` from the node's web listener: the body.
#[cfg(test)]
fn get(port: u16, path: &str) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(s, "GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
    let mut all = String::new();
    s.read_to_string(&mut all).unwrap();
    let (head, body) = all.split_once("\r\n\r\n").unwrap();
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    body.to_owned()
}

/// What the page compiles, from `/blossom/app.json`.
#[cfg(test)]
fn compiled(port: u16) -> Compiled {
    let app: serde_json::Value = serde_json::from_str(&get(port, "/blossom/app.json")).unwrap();
    let files: BTreeMap<String, String> = serde_json::from_value(app["files"].clone()).unwrap();
    let root = app["root"].as_str().unwrap();
    let deployment: ClientDeployment = serde_json::from_value(app.clone()).unwrap();
    blossom_web::compile_client(root, &files, &deployment).unwrap()
}

/// A member page: its app, its link's connection, and the texts its patches set.
#[cfg(test)]
struct Tab {
    app: App,
    ws: Option<Ws>,
    texts: BTreeMap<String, String>,
    /// The elements on the page, and the `checked` attributes.
    alive: std::collections::BTreeSet<String>,
    checked: BTreeMap<String, String>,
    now: i64,
}

#[cfg(test)]
impl Tab {
    /// Opens a page: its link (resuming `state`), the handshake, and the program's start.
    fn open(port: u16, state: Option<&LinkState>) -> Tab {
        let c = compiled(port);
        let mut link = c.link(state).unwrap().unwrap();
        let mut ws = Ws::connect(port).unwrap();
        ws.send_bytes(&link.hello()).unwrap();
        let mut early = Vec::new();
        loop {
            let (heard, frames) = link.recv(&ws.recv_bytes().unwrap()).unwrap();
            for f in frames {
                early.push(f);
            }
            if matches!(heard, blossom_web::link::Heard::Welcome { .. }) {
                break;
            }
        }
        let app = App::member(c, link).unwrap();
        let mut tab = Tab {
            app,
            ws: Some(ws),
            texts: BTreeMap::new(),
            alive: Default::default(),
            checked: BTreeMap::new(),
            now: 1_000,
        };
        for f in early {
            tab.write(&f);
        }
        let started = tab.app.start(None, "", Instant(tab.now)).unwrap();
        tab.apply(&started.patches);
        tab.flush();
        tab
    }

    fn write(&mut self, frame: &[u8]) {
        if let Some(ws) = self.ws.as_mut() {
            ws.send_bytes(frame).unwrap();
        }
    }

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
                Patch::Attr { id, name, value } if name == "checked" => {
                    self.checked.insert(id.clone(), value.clone());
                }
                Patch::Unattr { id, name } if name == "checked" => {
                    self.checked.remove(id);
                }
                _ => {}
            }
        }
    }

    /// Writes what the rounds want sent.
    fn flush(&mut self) {
        for f in self.app.take_frames() {
            self.write(&f);
        }
    }

    /// Takes what the server sends until it has been quiet for `ms` milliseconds.
    fn pump(&mut self, ms: u64) {
        while let Some(ws) = self.ws.as_mut() {
            let Some(bytes) = ws.recv_bytes_within(Duration::from_millis(ms)).unwrap() else {
                break;
            };
            self.now += 1;
            let patches = self.app.link_recv(&bytes, Instant(self.now)).unwrap();
            self.apply(&patches);
            self.flush();
        }
    }

    fn say(&mut self, text: &str) {
        self.now += 1;
        self.event(Event::Keydown {
            id: "say".into(),
            key: "Enter".into(),
            value: text.into(),
        });
    }

    fn event(&mut self, event: Event) {
        self.now += 1;
        let patches = self.app.dispatch(&event, Instant(self.now)).unwrap();
        self.apply(&patches);
        self.flush();
    }

    /// The todos' titles on the page, sorted, with whether each is done.
    fn todos(&self) -> Vec<(String, bool)> {
        let mut out: Vec<(String, bool)> = Vec::new();
        for (id, title) in &self.texts {
            let Some(k) = id.strip_prefix("label-") else { continue };
            if !self.alive.contains(&format!("todo-{k}")) {
                continue;
            }
            let done = self.checked.get(&format!("toggle-{k}")).is_some_and(|c| c == "true");
            out.push((title.clone(), done));
        }
        out.sort();
        out
    }

    /// The element id of the todo titled `title`'s `part` (`toggle`, `destroy`, `label`).
    fn todo_id(&self, title: &str, part: &str) -> String {
        let k = self
            .texts
            .iter()
            .filter(|(_, t)| t.as_str() == title)
            .filter_map(|(id, _)| id.strip_prefix("label-"))
            .find(|k| self.alive.contains(&format!("todo-{k}")))
            .unwrap();
        format!("{part}-{k}")
    }

    fn add(&mut self, title: &str) {
        self.event(Event::Keydown {
            id: "new-todo".into(),
            key: "Enter".into(),
            value: title.into(),
        });
    }

    /// The lines on the page, sorted.
    fn lines(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .texts
            .iter()
            .filter(|(id, _)| id.starts_with("text-"))
            .map(|(_, t)| t.clone())
            .collect();
        out.sort();
        out
    }

    /// The link goes down (the connection is dropped).
    fn disconnect(&mut self) {
        self.ws = None;
        self.now += 1;
        let patches = self.app.link_down(Instant(self.now)).unwrap();
        self.apply(&patches);
    }

    /// The link comes back on a new connection.
    fn reconnect(&mut self, port: u16) {
        let mut ws = Ws::connect(port).unwrap();
        ws.send_bytes(&self.app.link_hello().unwrap()).unwrap();
        self.ws = Some(ws);
        self.pump(500);
    }
}

#[test]
fn two_pages_chat_through_the_server_and_a_page_offline_catches_up() {
    let port = free_port();
    let server = serve("chat", "chat", port);
    let mut a = Tab::open(port, None);
    let mut b = Tab::open(port, None);
    a.pump(300);
    b.pump(300);
    assert_eq!(a.texts.get("status").map(String::as_str), Some("online, 2 here"));
    // A line typed in one page appears in both.
    a.say("hello");
    a.pump(500);
    b.pump(500);
    assert_eq!(a.lines(), ["hello"]);
    assert_eq!(b.lines(), ["hello"]);
    // A page whose link is down queues its line; it goes out when the link is back.
    b.disconnect();
    assert_eq!(
        b.texts.get("status").map(String::as_str),
        Some("offline: lines wait until the server is back")
    );
    b.say("from the train");
    a.pump(300);
    assert_eq!(a.lines(), ["hello"], "nothing reached the server while b was offline");
    b.reconnect(port);
    a.pump(500);
    b.pump(300);
    assert_eq!(a.lines(), ["from the train", "hello"]);
    assert_eq!(b.lines(), ["from the train", "hello"]);
    // A reload: a new page from the stored link state is the same member, and its link resumes.
    let state = b.app.link_state().unwrap();
    let before = state.member;
    drop(b);
    let mut b2 = Tab::open(port, Some(&state));
    b2.pump(300);
    assert_eq!(b2.app.link_state().unwrap().member, before);
    a.say("welcome back");
    a.pump(500);
    b2.pump(500);
    assert!(b2.lines().contains(&"welcome back".to_owned()), "{:?}", b2.lines());
    drop(a);
    drop(b2);
    server.stop().unwrap();
}

#[test]
fn two_pages_share_one_todo_list_through_the_server() {
    let port = free_port();
    let server = serve("todos", "todos_shared", port);
    let mut a = Tab::open(port, None);
    let mut b = Tab::open(port, None);
    a.pump(300);
    b.pump(300);
    assert_eq!(a.texts.get("sync").map(String::as_str), Some("synced with the server"));
    a.add("buy milk");
    a.pump(400);
    b.pump(400);
    b.add("walk the dog");
    b.pump(400);
    a.pump(400);
    let both = [("buy milk".to_owned(), false), ("walk the dog".to_owned(), false)];
    assert_eq!(a.todos(), both);
    assert_eq!(b.todos(), both);
    // b toggles a's todo; a sees it done.
    let id = b.todo_id("buy milk", "toggle");
    b.event(Event::Change { id, checked: true });
    b.pump(400);
    a.pump(400);
    assert_eq!(a.todos()[0], ("buy milk".to_owned(), true));
    // An edit made offline reaches the other page when the link is back.
    a.disconnect();
    let id = a.todo_id("walk the dog", "destroy");
    a.event(Event::Click { id });
    a.add("call mum");
    b.pump(300);
    assert_eq!(b.todos().len(), 2, "nothing reached the server while a was offline");
    a.reconnect(port);
    b.pump(500);
    a.pump(300);
    let after = [("buy milk".to_owned(), true), ("call mum".to_owned(), false)];
    assert_eq!(a.todos(), after);
    assert_eq!(b.todos(), after);
    drop(a);
    drop(b);
    server.stop().unwrap();
}
