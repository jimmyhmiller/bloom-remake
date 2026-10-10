//! Stateless hosting (docs/design/STATELESS.md §10): the keyed chat (examples/web/keyed_chat.bls) on several hosts
//! (`Objects`) that share nothing but one state store, with pages (the browser host's engine, `blossom_web::App`)
//! whose requests go to a host chosen per request. Hosts forget everything now and then (an instance that
//! restarts), and the store fails commits, or loses their answers, under a seed. Whatever happens, every tab ends with
//! every line said in its room exactly once and none from another, and lists the other rooms (which only the lobby's
//! messages, delivered through the outboxes, tell it).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use blossom_driver::bls::compile_deployed;
use blossom_front::api::NodeSpec;
use blossom_runtime::deploy::DeploymentSpec;
use blossom_runtime::object::Cursor;
use blossom_runtime::stateless::{Deployment, Objects, ServeError};
use blossom_runtime::web::Transport;
use blossom_statestore::{Fault, MemStore, StateStore};
use blossom_value::time::Instant;
use blossom_web::link::{Heard, LinkState};
use blossom_web::{App, ClientDeployment, Event, Patch};

#[cfg(test)]
fn deployment() -> Arc<Deployment> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/web/keyed_chat.deploy.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    let spec = DeploymentSpec::parse(&text, path.parent().unwrap()).unwrap();
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
    Arc::new(
        Deployment::new(
            spec,
            artifact,
            blossom_value::Seed([9; 16]),
            Arc::new(blossom_std_host::registry().unwrap()),
            "rooms",
            Transport::Http,
        )
        .unwrap(),
    )
}

#[cfg(test)]
/// A small deterministic generator (the test's own choices, not the system's).
struct Rng(u64);

#[cfg(test)]
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[cfg(test)]
/// The hosts, and which one takes the next request.
struct Hosts {
    hosts: Vec<Objects>,
    rng: Rng,
}

#[cfg(test)]
impl Hosts {
    fn pick(&mut self) -> &Objects {
        // Now and then a host restarts: it knows nothing of any object.
        if self.rng.below(10) == 0 {
            let i = self.rng.below(self.hosts.len() as u64) as usize;
            self.hosts[i].forget_all().unwrap();
        }
        let i = self.rng.below(self.hosts.len() as u64) as usize;
        &self.hosts[i]
    }
}

#[cfg(test)]
/// A tab: its app (once its link was welcomed; its compiled program and link before), its room, and its session
/// while it has one.
struct Tab {
    app: Option<App>,
    pre: Option<(blossom_web::Compiled, blossom_web::link::Link)>,
    /// Frames the handshake wrote, sent with the next send.
    out: Vec<Vec<u8>>,
    room: String,
    object: String,
    session: Option<(u64, [u8; 16], Cursor)>,
    now: i64,
    texts: BTreeMap<String, String>,
    alive: BTreeSet<String>,
}

#[cfg(test)]
impl Tab {
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

    fn tick(&mut self) -> Instant {
        self.now += 1;
        Instant(self.now)
    }

    /// The link is lost (a failed request, an ended session): the app hears so; the next pump opens another.
    fn lost(&mut self) {
        if self.session.take().is_some() {
            let now = self.tick();
            if let Some(app) = self.app.as_mut() {
                let patches = app.link_down(now).unwrap();
                self.apply(&patches);
            } else if let Some((_, link)) = self.pre.as_mut() {
                link.down();
            }
        }
    }

    /// A frame from the room: to the app, or, before the link was welcomed, to the link (the app starts at the
    /// `WELCOME`).
    fn take(&mut self, f: &[u8]) {
        let now = self.tick();
        if let Some(app) = self.app.as_mut() {
            let patches = app.link_recv(f, now).unwrap();
            self.apply(&patches);
            return;
        }
        let (compiled, mut link) = self.pre.take().unwrap();
        let (heard, frames) = link.recv(f).unwrap();
        self.out.extend(frames);
        if matches!(heard, Heard::Welcome { .. }) {
            let mut app = App::member(compiled, link).unwrap();
            let started = app.start(None, "", now).unwrap();
            self.app = Some(app);
            self.apply(&started.patches);
        } else {
            self.pre = Some((compiled, link));
        }
    }

    /// Opens a session (the page's `HELLO` to any host); false if the request failed.
    fn open(&mut self, hosts: &mut Hosts) -> bool {
        let hello = match (self.app.as_mut(), self.pre.as_mut()) {
            (Some(app), _) => app.link_hello().unwrap(),
            (None, Some((_, link))) => link.hello(),
            (None, None) => panic!("a tab has its app or its link"),
        };
        let opened = match hosts.pick().open(&self.object, &hello) {
            Ok(o) => o,
            Err(ServeError::Unavailable(_)) => return false,
            Err(e) => panic!("opening {}: {e}", self.object),
        };
        let Some(session) = opened.session else {
            panic!("{} refused a page", self.object)
        };
        self.session = Some(session);
        for f in opened.frames {
            self.take(&f);
        }
        true
    }

    /// Sends what the app wrote; a failed send loses the link (the page resends after it resumes).
    fn send(&mut self, hosts: &mut Hosts) {
        let mut frames = std::mem::take(&mut self.out);
        if let Some(app) = self.app.as_mut() {
            frames.extend(app.take_frames());
        }
        let Some((conn, secret, _)) = self.session else {
            // Sent once a session is open: the link keeps what is not acknowledged and resends it.
            return;
        };
        if frames.is_empty() {
            return;
        }
        match hosts.pick().send(&self.object, conn, &secret, &frames) {
            Ok(()) => {}
            Err(ServeError::Unavailable(_)) | Err(ServeError::Gone(_)) => self.lost(),
            Err(e) => panic!("sending to {}: {e}", self.object),
        }
    }

    /// One receive without waiting; whether it brought anything.
    fn receive(&mut self, hosts: &mut Hosts) -> bool {
        let Some((conn, secret, at)) = self.session else {
            return false;
        };
        match hosts.pick().receive(&self.object, conn, &secret, at, Duration::ZERO) {
            Ok((frames, next)) => {
                self.session = Some((conn, secret, next));
                let any = !frames.is_empty();
                for f in frames {
                    self.take(&f);
                }
                any
            }
            Err(ServeError::Unavailable(_)) | Err(ServeError::Gone(_)) => {
                self.lost();
                true
            }
            Err(e) => panic!("receiving from {}: {e}", self.object),
        }
    }

    fn say(&mut self, hosts: &mut Hosts, text: &str) {
        let now = self.tick();
        let patches = self
            .app
            .as_mut()
            .expect("the tab's app runs before it says anything")
            .dispatch(
                &Event::Keydown {
                    id: "say".into(),
                    key: "Enter".into(),
                    value: text.into(),
                },
                now,
            )
            .unwrap();
        self.apply(&patches);
        self.send(hosts);
    }

    /// The lines the tab shows: their texts.
    fn lines(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .texts
            .iter()
            .filter(|(id, _)| id.starts_with("text-") && self.alive.contains(*id))
            .map(|(_, t)| t.clone())
            .collect();
        out.sort();
        out
    }

    fn listed_rooms(&self) -> BTreeSet<String> {
        self.alive
            .iter()
            .filter_map(|id| id.strip_prefix("go-").map(str::to_owned))
            .collect()
    }
}

#[cfg(test)]
/// A tab in `room`: its token from the registry, then its app (the link opens on the first pump).
fn tab(hosts: &mut Hosts, deploy: &Deployment, room: &str) -> Tab {
    let objects = hosts.pick();
    let token = loop {
        match objects.mint("Browser") {
            Ok(t) => break t,
            Err(ServeError::Unavailable(_)) => continue,
            Err(e) => panic!("minting a token: {e}"),
        }
    };
    let mut desc: ClientDeployment = serde_json::from_str(deploy.app_json()).unwrap();
    desc.member = Some(room.to_owned());
    let compiled = blossom_web::load_client(deploy.client_part("Browser").unwrap(), &desc).unwrap();
    let state = LinkState {
        member: None,
        token: token.iter().map(|b| format!("{b:02x}")).collect(),
        seed: String::new(),
        received: 0,
        acked: 0,
        out_next: 1,
        unacked: Vec::new(),
    };
    let link = compiled.link(Some(&state)).unwrap().unwrap();
    let object = deploy.page_object(Some(room)).unwrap();
    // The handshake runs on the first open; the app starts once the link welcomed it.
    Tab {
        app: None,
        pre: Some((compiled, link)),
        out: Vec::new(),
        room: room.to_owned(),
        object,
        session: None,
        now: 1_000,
        texts: BTreeMap::new(),
        alive: BTreeSet::new(),
    }
}

#[cfg(test)]
/// Requests until nothing moves: every tab has a session, has sent what it wrote, and has received everything.
fn pump(hosts: &mut Hosts, tabs: &mut [Tab]) {
    for _round in 0..400 {
        let mut moved = false;
        for t in tabs.iter_mut() {
            if t.session.is_none() {
                moved = true;
                if !t.open(hosts) {
                    continue;
                }
            }
            t.send(hosts);
            if t.receive(hosts) {
                moved = true;
            }
            t.send(hosts);
        }
        if !moved {
            return;
        }
    }
    panic!("the tabs never settled");
}

#[cfg(test)]
fn run(seed: u64, faults: bool) {
    let deploy = deployment();
    let store = MemStore::new();
    let shared: Arc<dyn StateStore> = Arc::new(store.clone());
    let mut hosts = Hosts {
        hosts: (0..3).map(|_| Objects::new(deploy.clone(), shared.clone())).collect(),
        rng: Rng(seed | 1),
    };
    if faults {
        let rng = Mutex::new(Rng(seed.wrapping_mul(31) | 1));
        store
            .set_faults(move |_, _| match rng.lock().unwrap().below(20) {
                0 => Fault::Fail,
                1 => Fault::Lost,
                _ => Fault::Pass,
            })
            .unwrap();
    }
    let mut tabs: Vec<Tab> = ["lunch", "lunch", "dinner", "lunch", "dinner"]
        .iter()
        .map(|r| tab(&mut hosts, &deploy, r))
        .collect();
    pump(&mut hosts, &mut tabs);
    let mut said: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for i in 0..30 {
        let who = (hosts.rng.below(tabs.len() as u64)) as usize;
        let text = format!("line {i} from tab {who}");
        tabs[who].say(&mut hosts, &text);
        said.entry(tabs[who].room.clone()).or_default().push(text);
        if hosts.rng.below(3) == 0 {
            pump(&mut hosts, &mut tabs);
        }
    }
    // The store behaves from here: everything settles. Outboxes a failure left undelivered wait for a sweep (an
    // instance's sweeper, as `blossom serve` runs one), a second after their commit.
    store.clear_faults().unwrap();
    pump(&mut hosts, &mut tabs);
    for _ in 0..4 {
        std::thread::sleep(Duration::from_millis(1100));
        let woken = hosts.pick().sweep(1000).unwrap();
        pump(&mut hosts, &mut tabs);
        if woken.is_empty() {
            break;
        }
    }
    for (i, t) in tabs.iter().enumerate() {
        let mut want = said.get(&t.room).cloned().unwrap_or_default();
        want.sort();
        assert_eq!(t.lines(), want, "seed {seed}: tab {i} in {}", t.room);
        let other: BTreeSet<String> = ["lunch", "dinner"]
            .iter()
            .filter(|r| **r != t.room)
            .map(|r| r.to_string())
            .collect();
        assert_eq!(
            t.listed_rooms(),
            other,
            "seed {seed}: tab {i} in {} lists the other room",
            t.room
        );
    }
    // The rooms' logs hold each line once: as the node reads them, and as SQL rows in the `log` table, whose
    // typed `text` column holds the lines; the rooms' entries hold no LSM (their database is the tables).
    let log = store
        .table_defs()
        .unwrap()
        .into_iter()
        .find(|d| d.view.as_deref() == Some("log"))
        .expect("a table whose view is `log`");
    let text = log
        .columns
        .iter()
        .position(|(c, _)| c == "text")
        .expect("a `text` column");
    for (room, lines) in &said {
        let rows = hosts.hosts[0].rows(&format!("member/Room/{room}"), "log").unwrap();
        assert_eq!(rows.len(), lines.len(), "seed {seed}: room {room}'s log");
        let owner = blossom_statestore::Owner {
            node: "rooms".into(),
            member: room.clone(),
        };
        let mut sql: Vec<String> = store
            .open_rows(&log.name, &owner)
            .unwrap()
            .into_iter()
            .map(|(_, v)| match &v[text] {
                blossom_statestore::SqlValue::Text(t) => t.clone(),
                other => panic!("a text column holding {other:?}"),
            })
            .collect();
        sql.sort();
        let mut want = lines.clone();
        want.sort();
        assert_eq!(sql, want, "seed {seed}: room {room}'s SQL rows");
        let entries = store.load(&format!("member/Room/{room}")).unwrap().entries;
        let lsm: Vec<&String> = entries.iter().map(|(k, _)| k).filter(|k| k.contains("/db/")).collect();
        assert!(lsm.is_empty(), "seed {seed}: room {room} keeps an LSM: {lsm:?}");
    }
}

#[test]
fn the_keyed_chat_runs_on_hosts_that_share_only_the_store() {
    run(7, false);
}

#[test]
fn the_keyed_chat_survives_failed_and_lost_commits_and_restarting_hosts() {
    // The fast tier runs three seeds; the full tier (BLOSSOM_FULL=1) twenty.
    let seeds = if std::env::var("BLOSSOM_FULL").is_ok() {
        1..=20
    } else {
        1..=3
    };
    for seed in seeds {
        run(seed, true);
    }
}

#[test]
fn a_session_not_seen_for_its_lease_ends_at_the_sweep() {
    let deploy = deployment();
    let shared: Arc<dyn StateStore> = Arc::new(MemStore::new());
    let mut hosts = Hosts {
        hosts: vec![Objects::new(deploy.clone(), shared.clone())],
        rng: Rng(5),
    };
    let mut tabs = vec![tab(&mut hosts, &deploy, "quiet")];
    pump(&mut hosts, &mut tabs);
    let (conn, _, _) = tabs[0].session.unwrap();
    // The page's presence is 31 s old: at the session's check the room hears it go.
    let object = tabs[0].object.clone();
    let stale =
        blossom_runtime::stateless::objects::ms_of(blossom_runtime::stateless::objects::now().unwrap()) - 31_000;
    shared
        .put_side(
            &blossom_runtime::stateless::presence_key(&object, conn),
            &stale.to_le_bytes(),
        )
        .unwrap();
    hosts.hosts[0].expire_sessions_now(&object).unwrap();
    let (_, secret, at) = tabs[0].session.unwrap();
    match hosts.hosts[0].receive(&object, conn, &secret, at, Duration::ZERO) {
        Err(ServeError::Gone(_)) => {}
        other => panic!(
            "a session past its lease still answers: {:?}",
            other.map(|(f, _)| f.len())
        ),
    }
    // A fresh presence keeps it.
    let mut tabs = vec![tab(&mut hosts, &deploy, "quiet")];
    pump(&mut hosts, &mut tabs);
    let (conn, secret, at) = tabs[0].session.unwrap();
    hosts.hosts[0].touch(&object, conn).unwrap();
    hosts.hosts[0].expire_sessions_now(&object).unwrap();
    hosts.hosts[0]
        .receive(&object, conn, &secret, at, Duration::ZERO)
        .unwrap();
}

#[test]
fn an_instance_keeps_at_most_its_cache_of_objects_and_loads_the_others_again() {
    let deploy = deployment();
    let shared: Arc<dyn StateStore> = Arc::new(MemStore::new());
    let mut hosts = Hosts {
        hosts: vec![Objects::new(deploy.clone(), shared.clone()).with_cache(2)],
        rng: Rng(3),
    };
    let mut tabs: Vec<Tab> = ["a", "b", "c", "a"]
        .iter()
        .map(|r| tab(&mut hosts, &deploy, r))
        .collect();
    pump(&mut hosts, &mut tabs);
    for (i, t) in tabs.iter_mut().enumerate() {
        t.say(&mut hosts, &format!("line from tab {i}"));
    }
    pump(&mut hosts, &mut tabs);
    assert!(
        hosts.hosts[0].cached().unwrap() <= 2,
        "the cache keeps at most 2 objects"
    );
    assert_eq!(tabs[0].lines(), ["line from tab 0", "line from tab 3"]);
    assert_eq!(tabs[3].lines(), ["line from tab 0", "line from tab 3"]);
    assert_eq!(tabs[1].lines(), ["line from tab 1"]);
    assert_eq!(tabs[2].lines(), ["line from tab 2"]);
}
