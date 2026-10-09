//! Blossom in the browser (docs/design/BROWSER.md).
//!
//! A browser app is an ordinary single-node Blossom program. [`compile`] compiles it from sources in memory;
//! [`App::start`] restores its durable tables and runs its first rounds; [`App::dispatch`] runs one round per DOM
//! event, and [`App::advance`] one per instant its physical timers fire (the page's clock; LANGUAGE §15.2). The
//! program describes its page with the outputs `elem`, `attr`, `text` and `focus` ([`page`]), and hears the world
//! through the inputs it declares (`route`, `click`, `dblclick`, `press`, `typed`, `keydown`, `blur`, `change`);
//! each round's page is diffed against the last into DOM patches. The core here is plain Rust, which native tests
//! drive; `wasm` is the page's API over it.
//!
//! A client member's page (docs/design/CLIENTS.md §8) runs the part of a deployment's program its server projected
//! onto the client role: [`load_client`] reads it, with no compiler. Without the `compiler` feature (the member page's
//! build) the crate holds no compiler at all.

#[cfg(feature = "compiler")]
mod compile;
pub mod link;
pub mod page;
pub mod store;
#[cfg(target_arch = "wasm32")]
mod wasm;
pub mod why;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_artifact::client::ClientArtifact;
use blossom_base::RelId;
use blossom_engine::{Engine, EngineConfig};
use blossom_ir::core::RelClass;
use blossom_ir::tick::{Instance, Row, StepInput};
use blossom_ir::timers::TimerTable;
use blossom_value::Value;
use blossom_value::time::{Instant, NodeId, Tick};
use blossom_value::types::{IntTy, TypeDef};
use serde::{Deserialize, Serialize};

#[cfg(feature = "compiler")]
pub use compile::compile;
pub use page::{Page, Patch};

/// What can go wrong running a program in the browser.
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    /// The program's page outputs hold something the page cannot show.
    #[error("the page: {0}")]
    Page(String),
    /// The program declares a page relation or an event input with another schema than the host's.
    #[error("the program's interface: {0}")]
    Interface(String),
    /// A round failed (a runtime error of the program, BLSRnnn).
    #[error("round {tick}: {error}")]
    Round { tick: u64, error: String },
    /// An event's rounds did not settle: the state kept changing.
    #[error("the state still changes after {0} rounds without events: the program does not settle")]
    Unsettled(u32),
    /// The saved state could not be read or written.
    #[error("the saved state: {0}")]
    Store(String),
    /// The link to the server (a client member's, docs/design/CLIENTS.md §5) failed.
    #[error("the link: {0}")]
    Link(String),
    /// The server refused the page's link: `reason` as the protocol names it (`program`, `deployment`, …).
    #[error("the server refused the page ({reason}): {detail}")]
    Refused { reason: String, detail: String },
    /// What the server gave the page to run (its client artifact) could not be read.
    #[error("the page's program: {0}")]
    Artifact(String),
    /// The server gave the page another identity than the one it ran as (it lost the old one): the page's state
    /// belongs to the old identity, so it must start over.
    #[error("the server gave this page another identity ({given:?}; it ran as {ran:?}): start it over")]
    Identity { given: NodeId, ran: NodeId },
}

/// Who a program's rounds run as: its node, and the deployment's nodes and roles (for the evaluators and the
/// inspector).
#[derive(Clone, Debug)]
pub struct Who {
    pub node: NodeId,
    pub roles: Vec<Option<blossom_base::RoleId>>,
    pub names: Vec<Arc<str>>,
    /// The client role, for a client member (whose node is outside the deployment).
    pub client_role: Option<blossom_base::RoleId>,
    /// The keyed member the page's server is, when it links to one (docs/design/KEYED.md): the server's id names it.
    pub members: Arc<blossom_ir::members::Members>,
}

/// A diagnostic of a compile, for the editor: its rendering (as `blossom check` prints it) and where it points.
#[derive(Clone, Debug, Serialize)]
pub struct Diag {
    pub severity: String,
    pub code: String,
    pub message: String,
    pub rendered: String,
    /// The source file and the 1-based line and column of the primary span, when it has one.
    pub file: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    /// The primary span in the file's text, as UTF-16 offsets (an editor's, in JavaScript): `[start, end)`.
    pub range: Option<(u32, u32)>,
}

/// A DOM event, as the page reports it.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Event {
    Route {
        hash: String,
    },
    Click {
        id: String,
    },
    Dblclick {
        id: String,
    },
    Press {
        id: String,
    },
    Input {
        id: String,
        value: String,
    },
    Keydown {
        id: String,
        key: String,
        value: String,
    },
    Blur {
        id: String,
        value: String,
    },
    Change {
        id: String,
        checked: bool,
    },
    /// An element dragged onto another (HTML drag and drop): the dragged element's id and the target's.
    Drop {
        id: String,
        target: String,
    },
}

impl Event {
    /// The input relation the event goes into.
    fn input(&self) -> &'static str {
        match self {
            Event::Route { .. } => "route",
            Event::Click { .. } => "click",
            Event::Dblclick { .. } => "dblclick",
            Event::Press { .. } => "press",
            Event::Input { .. } => "typed",
            Event::Keydown { .. } => "keydown",
            Event::Blur { .. } => "blur",
            Event::Change { .. } => "change",
            Event::Drop { .. } => "drop",
        }
    }

    /// The event's row.
    fn row(&self) -> Row {
        let s = |x: &str| Value::Str(x.into());
        let values = match self {
            Event::Route { hash } => vec![s(hash)],
            Event::Click { id } | Event::Dblclick { id } | Event::Press { id } => vec![s(id)],
            Event::Input { id, value } | Event::Blur { id, value } => vec![s(id), s(value)],
            Event::Keydown { id, key, value } => vec![s(id), s(key), s(value)],
            Event::Change { id, checked } => vec![s(id), Value::Bool(*checked)],
            Event::Drop { id, target } => vec![s(id), s(target)],
        };
        Arc::from(values)
    }
}

/// The schema (column types) of each relation the host knows: `String`, `i64` or `bool`, by column.
const STR: &str = "String";
const I64: &str = "i64";
const BOOL: &str = "bool";
const OUTPUTS: [(&str, &[&str]); 4] = [
    ("elem", &[STR, STR, I64, STR]),
    ("attr", &[STR, STR, STR]),
    ("text", &[STR, STR]),
    ("focus", &[STR]),
];
const INPUTS: [(&str, &[&str]); 9] = [
    ("route", &[STR]),
    ("click", &[STR]),
    ("dblclick", &[STR]),
    ("press", &[STR]),
    ("typed", &[STR, STR]),
    ("keydown", &[STR, STR, STR]),
    ("blur", &[STR, STR]),
    ("change", &[STR, BOOL]),
    ("drop", &[STR, STR]),
];

/// A program compiled for the browser.
#[derive(Clone)]
pub struct Compiled {
    artifact: BlsArtifact,
    /// The page outputs the program declares (missing ones are empty), by name.
    outputs: BTreeMap<&'static str, RelId>,
    /// The event inputs it declares, by name.
    inputs: BTreeMap<&'static str, RelId>,
    pub warnings: Vec<Diag>,
    /// What a client member's page plays (CLIENTS.md §5); `None` for a page on its own.
    client: Option<ClientPart>,
}

/// The part a client member's page plays in a deployment.
#[derive(Clone, Debug)]
struct ClientPart {
    role: blossom_base::RoleId,
    role_name: String,
    /// The server node the page connects to, and the connection identity it checks.
    server: NodeId,
    identity: blossom_wire::link::Identity,
    /// The digest of the part of the program the page runs, which its link presents.
    part: [u8; 16],
    /// The link events of its link to the server: `connected`, `disconnected`.
    connected: Option<RelId>,
    disconnected: Option<RelId>,
    /// The keyed member the page links to, when its server is a host of a keyed role (docs/design/KEYED.md).
    keyed: Option<blossom_value::time::MemberRef>,
}

/// The deployment a client member's page runs in (`/blossom/app.json`, CLIENTS.md §4, §8): the server node that
/// served it, and the connection identity (hex): the deployment id and the node directory's digest.
#[derive(Clone, Debug, Deserialize)]
pub struct ClientDeployment {
    pub node: String,
    pub deployment: String,
    pub directory: String,
    /// The keyed role whose members the node hosts (docs/design/KEYED.md), and the member the page links to (its
    /// URL's `?member=KEY`, which the host adds).
    #[serde(default)]
    pub keyed: Option<String>,
    #[serde(default)]
    pub member: Option<String>,
}

fn hex16(s: &str) -> Result<[u8; 16], String> {
    let bytes: Option<Vec<u8>> = (0..s.len())
        .step_by(2)
        .map(|i| s.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()))
        .collect();
    bytes
        .and_then(|b| <[u8; 16]>::try_from(b).ok())
        .ok_or_else(|| format!("`{s}` is not 16 bytes of hex"))
}

/// The name of a type, as the host's schemas name it.
fn type_name(program: &blossom_ir::core::Program, ty: blossom_base::TypeId) -> String {
    match program.types.get(ty) {
        Some(TypeDef::Str) => STR.to_owned(),
        Some(TypeDef::Bool) => BOOL.to_owned(),
        Some(TypeDef::Int(IntTy::I64)) => I64.to_owned(),
        other => format!("{other:?}"),
    }
}

/// An error of the host's own, as a diagnostic.
fn host_diag(message: String) -> Diag {
    Diag {
        severity: "error".to_owned(),
        code: String::new(),
        rendered: format!("error: {message}"),
        message,
        file: None,
        line: None,
        column: None,
        range: None,
    }
}

/// A compiled program as the page runs it: its page outputs and event inputs, checked against the host's schemas.
fn interface(artifact: BlsArtifact, warnings: Vec<Diag>) -> Result<Compiled, Vec<Diag>> {
    let program = artifact.program.get();
    let interface = |name: &str, schema: &[&str], output: bool| -> Result<Option<RelId>, String> {
        let Some((id, rel)) = program.rels.iter_enumerated().find(|(_, r)| r.name.to_string() == name) else {
            return Ok(None);
        };
        let right_kind = if output {
            !matches!(rel.class, RelClass::Event(_))
        } else {
            matches!(rel.class, RelClass::Event(blossom_ir::core::EventSource::Input))
        };
        let got: Vec<String> = rel.schema.cols.iter().map(|c| type_name(program, c.ty)).collect();
        if !right_kind || got != schema {
            return Err(format!(
                "`{name}` is a page {} of ({}), not {} of ({})",
                if output { "output" } else { "input" },
                schema.join(", "),
                if output { "a relation" } else { "an input" },
                got.join(", ")
            ));
        }
        Ok(Some(id))
    };
    let mut problems = Vec::new();
    let mut outputs = BTreeMap::new();
    for (name, schema) in OUTPUTS {
        match interface(name, schema, true) {
            Ok(Some(id)) => {
                outputs.insert(name, id);
            }
            Ok(None) => {}
            Err(e) => problems.push(e),
        }
    }
    // An input no rule reads is not listened to (`ui.bls` declares them all).
    let read = |rel: RelId| {
        program.rules.iter().any(|r| {
            r.body.lits.iter().any(|l| match l {
                blossom_ir::core::Literal::Pos(a) | blossom_ir::core::Literal::Neg(a) => a.rel == rel,
                _ => false,
            })
        })
    };
    let mut inputs = BTreeMap::new();
    for (name, schema) in INPUTS {
        match interface(name, schema, false) {
            Ok(Some(id)) if read(id) => {
                inputs.insert(name, id);
            }
            Ok(Some(_)) => {}
            Ok(None) => {}
            Err(e) => problems.push(e),
        }
    }
    if !problems.is_empty() {
        return Err(problems.into_iter().map(host_diag).collect());
    }
    Ok(Compiled {
        warnings,
        artifact,
        outputs,
        inputs,
        client: None,
    })
}

/// Reads what a client member's page runs (CLIENTS.md §8): the encoded [`ClientArtifact`] its server serves, for the
/// deployment `/blossom/app.json` names.
pub fn load_client(bytes: &[u8], deployment: &ClientDeployment) -> Result<Compiled, HostError> {
    let client = ClientArtifact::decode(bytes).map_err(|e| HostError::Artifact(e.to_string()))?;
    let artifact = client.artifact();
    let p = artifact.program.get();
    let Some(server) = artifact.node_id(&deployment.node) else {
        return Err(HostError::Artifact(format!(
            "the deployment has no node `{}`",
            deployment.node
        )));
    };
    let server_role = artifact.roles.get(server.0 as usize).copied().flatten();
    let link = |up: bool| {
        p.rels
            .iter_enumerated()
            .find(|(_, r)| {
                matches!(&r.class, RelClass::Event(blossom_ir::core::EventSource::Link { peer, up: u })
                    if Some(*peer) == server_role && *u == up)
            })
            .map(|(id, _)| id)
    };
    let (connected, disconnected) = (link(true), link(false));
    // A host of a keyed role serves its members' pages: the page names the member it links to.
    let keyed = match (server_role.filter(|r| p.is_keyed(*r)), &deployment.member) {
        (Some(role), Some(key)) => Some(p.member(role, key.as_str())),
        (Some(role), None) => {
            let name = p.roles.get(role).map(|r| r.name.to_string()).unwrap_or_default();
            return Err(HostError::Artifact(format!(
                "node `{}` hosts members of the keyed role `{name}`: the page names the one it links to \
                 (`?member=KEY`)",
                deployment.node
            )));
        }
        (None, Some(key)) => {
            return Err(HostError::Artifact(format!(
                "node `{}` runs no keyed members, so no member `{key}`",
                deployment.node
            )));
        }
        (None, None) => None,
    };
    let identity = blossom_wire::link::Identity {
        deployment: hex16(&deployment.deployment).map_err(HostError::Artifact)?,
        program_id: p.meta.program_id,
        program_version: p.meta.version,
        directory: hex16(&deployment.directory).map_err(HostError::Artifact)?,
    };
    let part = ClientPart {
        role: client.role,
        role_name: client.role_name.clone(),
        server,
        identity,
        part: client.part(),
        connected,
        disconnected,
        keyed,
    };
    let mut compiled = interface(artifact, Vec::new())
        .map_err(|diags| HostError::Interface(diags.into_iter().map(|d| d.message).collect::<Vec<_>>().join("; ")))?;
    compiled.client = Some(part);
    Ok(compiled)
}

impl Compiled {
    /// The compiled program.
    pub fn artifact(&self) -> &BlsArtifact {
        &self.artifact
    }

    /// The DOM events the program listens to: the inputs it declares and reads (`route` included).
    pub fn listens(&self) -> Vec<&'static str> {
        self.inputs.keys().copied().collect()
    }

    /// The link a client member's page opens to its server, resuming `state` (`None` for a page on its own).
    pub fn link(&self, state: Option<&link::LinkState>) -> Result<Option<link::Link>, HostError> {
        match &self.client {
            None => Ok(None),
            Some(c) => link::Link::new(
                &self.artifact,
                &c.role_name,
                c.part,
                c.server,
                c.identity.clone(),
                c.keyed.as_ref().map(|m| (m.role_name.to_string(), m.key.to_string())),
                state,
            )
            .map(Some),
        }
    }
}

/// A durable relation's rows added and removed since the host last saved.
type Unsaved = (BTreeSet<Row>, BTreeSet<Row>);

/// The most rounds without events an event's effects may take to settle.
pub const SETTLE: u32 = 1000;

/// A running program.
pub struct App {
    compiled: Compiled,
    engine: Engine,
    /// The root of the program's randomness (`rand`, `rand_float`, seeded choices): the inspector's replays use it too.
    seed: blossom_value::Seed,
    /// The next round, and the page the last one left.
    tick: u64,
    page: Page,
    /// The program's physical timers, anchored at the start; and the clock as of the latest round (it never goes
    /// back: an earlier instant from the page is taken as this one).
    timers: Option<TimerTable>,
    now: Instant,
    /// The rounds the inspector can explain.
    history: why::History,
    /// The durable rows added and removed since the host last saved (`None`: it must save them all).
    unsaved: Option<BTreeMap<RelId, Unsaved>>,
    /// Who the rounds run as.
    who: Who,
    /// A client member's link to its server, the messages it brought for the next round, the sends of the rounds
    /// since the last flush, and the frames to write.
    link: Option<link::Link>,
    inbox: Vec<blossom_ir::tick::Delivery>,
    outbox: Vec<blossom_ir::tick::Send>,
    frames: Vec<Vec<u8>>,
}

/// What starting a program did: the first page, and what the restore could not keep.
#[derive(Debug, Serialize)]
pub struct Started {
    pub patches: Vec<Patch>,
    pub notes: Vec<String>,
}

impl App {
    /// A program ready to start, its randomness drawn from `seed`.
    pub fn new(compiled: Compiled, seed: blossom_value::Seed) -> Result<App, HostError> {
        let who = Who {
            node: NodeId(0),
            roles: compiled.artifact.roles.clone(),
            names: vec![Arc::from("app")],
            client_role: None,
            members: Arc::default(),
        };
        App::with(compiled, seed, who, None)
    }

    /// A client member's page (CLIENTS.md §5): it runs as the member `link` names, with the member's seed, and talks
    /// to its server over `link`.
    pub fn member(compiled: Compiled, link: link::Link) -> Result<App, HostError> {
        let client = compiled
            .client
            .clone()
            .ok_or_else(|| HostError::Link("the program was not compiled for a client member".into()))?;
        let member = link
            .member()
            .cloned()
            .ok_or_else(|| HostError::Link("the link has no member yet (no WELCOME)".into()))?;
        // The page's server, when it is a keyed member, is that member wherever the page's program sees it.
        let members = blossom_ir::members::Members::default();
        if let Some(m) = &client.keyed {
            members
                .insert(client.server, m.clone())
                .map_err(|e| HostError::Link(format!("the page's server: {e}")))?;
        }
        let who = Who {
            node: member.id,
            roles: compiled.artifact.roles.clone(),
            names: compiled.artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect(),
            client_role: Some(client.role),
            members: Arc::new(members),
        };
        App::with(compiled, blossom_value::Seed(member.seed), who, Some(link))
    }

    fn with(
        compiled: Compiled,
        seed: blossom_value::Seed,
        who: Who,
        link: Option<link::Link>,
    ) -> Result<App, HostError> {
        let engine = Engine::new(
            compiled.artifact.program.clone(),
            who.node,
            EngineConfig {
                roles: who.roles.clone(),
                node_names: who.names.clone(),
                seed: Some(seed),
                client_role: who.client_role,
                members: who.members.clone(),
                ..EngineConfig::default()
            },
        )
        .map_err(|e| HostError::Round {
            tick: 0,
            error: e.to_string(),
        })?;
        Ok(App {
            compiled,
            engine,
            seed,
            tick: 0,
            page: Page::default(),
            timers: None,
            now: Instant(0),
            history: why::History::new(),
            unsaved: None,
            who,
            link,
            inbox: Vec::new(),
            outbox: Vec::new(),
            frames: Vec::new(),
        })
    }

    /// The first frame of a connection to the server (a client member's page).
    pub fn link_hello(&mut self) -> Result<Vec<u8>, HostError> {
        self.link
            .as_mut()
            .map(link::Link::hello)
            .ok_or_else(|| HostError::Link("this page has no server".into()))
    }

    /// Takes a frame from the server at `now`: the handshake's `WELCOME` raises the link's `connected` event, and a
    /// batch's messages are delivered in a round; the patches.
    pub fn link_recv(&mut self, bytes: &[u8], now: Instant) -> Result<Vec<Patch>, HostError> {
        self.tick_to(now);
        let link = self
            .link
            .as_mut()
            .ok_or_else(|| HostError::Link("this page has no server".into()))?;
        let (heard, frames) = link.recv(bytes)?;
        let server = link.server();
        self.frames.extend(frames);
        match heard {
            link::Heard::Welcome { member, resumed } => {
                if member.id != self.who.node {
                    return Err(HostError::Identity {
                        given: member.id,
                        ran: self.who.node,
                    });
                }
                let event = self.compiled.client.as_ref().and_then(|c| c.connected).map(|rel| {
                    (
                        rel,
                        Arc::from(vec![self.who.members.value(server), Value::Bool(resumed)]),
                    )
                });
                self.settle(event.as_slice())
            }
            link::Heard::Deliveries(d) => {
                self.inbox.extend(d);
                self.settle(&[])
            }
            link::Heard::Nothing => Ok(Vec::new()),
        }
    }

    /// The connection to the server ended at `now`: the link's `disconnected` event; the patches.
    pub fn link_down(&mut self, now: Instant) -> Result<Vec<Patch>, HostError> {
        self.tick_to(now);
        let Some(link) = self.link.as_mut() else {
            return Ok(Vec::new());
        };
        let was_up = link.is_up();
        link.down();
        let server = link.server();
        let event = self
            .compiled
            .client
            .as_ref()
            .and_then(|c| c.disconnected)
            .filter(|_| was_up)
            .map(|rel| (rel, Arc::from(vec![self.who.members.value(server)])));
        match event {
            Some(e) => self.settle(&[e]),
            None => Ok(Vec::new()),
        }
    }

    /// The frames to write to the server since the last call (acknowledgements, and the rounds' sends while the link
    /// is up; sends made while it is down wait in the link).
    pub fn take_frames(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.frames)
    }

    /// What the page stores to resume its link after a reload.
    pub fn link_state(&self) -> Option<link::LinkState> {
        self.link.as_ref().map(link::Link::state)
    }

    /// Hands the rounds' sends to the link.
    fn flush_sends(&mut self) -> Result<(), HostError> {
        let sends = std::mem::take(&mut self.outbox);
        if let Some(link) = self.link.as_mut() {
            let frames = link.send(&sends, self.tick)?;
            self.frames.extend(frames);
        }
        Ok(())
    }

    /// The rounds run so far.
    pub fn rounds(&self) -> u64 {
        self.tick
    }

    /// Each rule's work since the program was created, by rule label (the rules that did any): rows its probes
    /// returned, expression nodes evaluated, rows written (`blossom_engine::Engine::work_by_rule`).
    pub fn work_by_rule(&self) -> Vec<(Arc<str>, blossom_ir::tick::RuleWork)> {
        let program = self.compiled.artifact.program.get();
        self.engine
            .work_by_rule()
            .iter()
            .map(|(id, w)| {
                let label = program
                    .rules
                    .get(*id)
                    .map_or_else(|| Arc::from(format!("rule {}", id.index())), |r| r.label.text.clone());
                (label, *w)
            })
            .collect()
    }

    /// Whether the program has physical timers: the page then runs a clock and calls [`App::advance`].
    pub fn clocked(&self) -> bool {
        self.compiled
            .artifact
            .program
            .get()
            .rels
            .iter()
            .any(|r| matches!(r.class, RelClass::Event(blossom_ir::core::EventSource::Timer(_))))
    }

    /// Moves the clock to `now` (never back).
    fn tick_to(&mut self, now: Instant) {
        self.now = Instant(self.now.0.max(now.0));
    }

    /// Starts the program at `now`: restores the durable tables `saved` holds (when given), then runs the boot round
    /// and the `route` round of `hash`. Its timers count from `now` (LANGUAGE §15.2: each start is an incarnation).
    /// Returns the patches that draw the first page.
    pub fn start(&mut self, saved: Option<&str>, hash: &str, now: Instant) -> Result<Started, HostError> {
        let mut notes = Vec::new();
        let carried = match saved {
            Some(json) => {
                let restored = store::restore(self.compiled.artifact.program.get(), json)?;
                for t in restored.dropped {
                    notes.push(format!(
                        "table `{t}` was saved with another schema (or is gone): it starts empty"
                    ));
                }
                restored.carried
            }
            None => Instance::default(),
        };
        self.history.reset(carried.clone());
        self.engine.reset(carried).map_err(|e| HostError::Round {
            tick: 0,
            error: e.to_string(),
        })?;
        self.tick = 0;
        self.page = Page::default();
        self.unsaved = None;
        self.now = now;
        self.timers = Some(
            TimerTable::new(self.compiled.artifact.program.get(), None, now).map_err(|e| HostError::Round {
                tick: 0,
                error: e.to_string(),
            })?,
        );
        let mut boot: Vec<(RelId, Row)> = self
            .compiled
            .artifact
            .boot()
            .map(|b| (b, Arc::from(Vec::new())))
            .into_iter()
            .collect();
        // A member page made after its link's `WELCOME` (the first connection) starts with the link up.
        if let (Some(link), Some(rel)) = (
            self.link.as_ref().filter(|l| l.is_up()),
            self.compiled.client.as_ref().and_then(|c| c.connected),
        ) {
            boot.push((
                rel,
                Arc::from(vec![self.who.members.value(link.server()), Value::Bool(link.resumed())]),
            ));
        }
        let mut patches = self.settle(&boot)?;
        patches.extend(self.dispatch(&Event::Route { hash: hash.to_owned() }, now)?);
        Ok(Started { patches, notes })
    }

    /// Runs one event at `now` (an event the program does not listen to runs nothing) until its effects settle;
    /// returns the patches from the page before it to the page after.
    pub fn dispatch(&mut self, event: &Event, now: Instant) -> Result<Vec<Patch>, HostError> {
        self.tick_to(now);
        match self.compiled.inputs.get(event.input()) {
            Some(rel) => self.settle(&[(*rel, event.row())]),
            None => Ok(Vec::new()),
        }
    }

    /// Moves the clock to `now`: when timers are due, runs a round (which takes their firings, every one due by
    /// `now`, as a node delivers them) until its effects settle. The patches (none when nothing was due).
    pub fn advance(&mut self, now: Instant) -> Result<Vec<Patch>, HostError> {
        self.tick_to(now);
        let at = self.now;
        let timers = self.timers.as_ref().ok_or_else(|| HostError::Round {
            tick: self.tick,
            error: "the program has not started".to_owned(),
        })?;
        let due = timers.any_due(at).map_err(|e| HostError::Round {
            tick: self.tick,
            error: e.to_string(),
        })?;
        if !due {
            return Ok(Vec::new());
        }
        self.settle(&[])
    }

    /// When the next timer is due (none while no timer is active).
    pub fn next_deadline(&self) -> Result<Option<Instant>, HostError> {
        match &self.timers {
            Some(t) => t.next_deadline().map_err(|e| HostError::Round {
                tick: self.tick,
                error: e.to_string(),
            }),
            None => Ok(None),
        }
    }

    /// Runs a round with `events`, then rounds without events while the state changes (a write takes effect in the
    /// next round, LANGUAGE §7: the page shows an event's effects once they settle), at most [`SETTLE`] of them.
    fn settle(&mut self, events: &[(RelId, Row)]) -> Result<Vec<Patch>, HostError> {
        let mut changed = self.round(events)?;
        let mut quiet_rounds = 0;
        while changed {
            if quiet_rounds == SETTLE {
                return Err(HostError::Unsettled(SETTLE));
            }
            changed = self.round(&[])?;
            quiet_rounds += 1;
        }
        self.flush_sends()?;
        Ok(self.page.take_patches())
    }

    /// Runs one round with `events` and the timers' firings due by now (every round is a tick the timers count),
    /// leaving its page in `self.page`; whether the state changed.
    fn round(&mut self, events: &[(RelId, Row)]) -> Result<bool, HostError> {
        let tick = self.tick;
        let now = self.now;
        let firings = match self.timers.as_mut() {
            Some(t) => t.fire(now).map_err(|e| HostError::Round {
                tick,
                error: e.to_string(),
            })?,
            None => Vec::new(),
        };
        let events: Vec<(RelId, Row)> = firings.into_iter().chain(events.iter().cloned()).collect();
        let events = events.as_slice();
        // The page takes its outputs' changes; the timers' guards are read whole.
        let observe: Vec<RelId> = self.timers.iter().flat_map(|t| t.guards()).collect();
        let delivered = std::mem::take(&mut self.inbox);
        let out = self
            .engine
            .step(
                &StepInput {
                    node: self.who.node,
                    incarnation: 1,
                    tick: Tick(tick),
                    now,
                    events,
                    delivered: &delivered,
                    ingress: &[],
                    blobs: &blossom_value::NoBlobs,
                },
                &observe,
            )
            .map_err(|e| HostError::Round {
                tick,
                error: e.to_string(),
            })?;
        self.tick += 1;
        if let Some(timers) = self.timers.as_mut() {
            timers.observe(&out.observed).map_err(|e| HostError::Round {
                tick,
                error: e.to_string(),
            })?;
        }
        self.outbox.extend(out.outbox.iter().cloned());
        self.history.push(why::Round {
            tick,
            now,
            events: events.to_vec(),
            delivered,
            inserted: out
                .changes
                .inserted
                .iter()
                .flat_map(|(rel, rows)| rows.iter().map(|r| (*rel, Arc::clone(r))))
                .collect(),
            changes: out.changes.clone(),
        });
        let changes = |name: &str| -> (Vec<Row>, Vec<Row>) {
            self.compiled
                .outputs
                .get(name)
                .map(|r| self.engine.changes_of(*r))
                .unwrap_or_default()
        };
        let delta = page::Delta {
            elem: changes("elem"),
            attr: changes("attr"),
            text: changes("text"),
            focus: changes("focus"),
        };
        self.page.apply(&delta)?;
        // The durable rows the host has not saved: a row added and removed again since is neither.
        if let Some(unsaved) = self.unsaved.as_mut() {
            let program = self.compiled.artifact.program.get();
            let durable = |rel: &RelId| program.rels.get(*rel).is_some_and(|r| r.durable);
            for (rel, rows) in out.changes.inserted.iter().filter(|(r, _)| durable(r)) {
                let (ins, del) = unsaved.entry(*rel).or_default();
                for row in rows {
                    if !del.remove(row) {
                        ins.insert(Arc::clone(row));
                    }
                }
            }
            for (rel, rows) in out.changes.deleted.iter().filter(|(r, _)| durable(r)) {
                let (ins, del) = unsaved.entry(*rel).or_default();
                for row in rows {
                    if !ins.remove(row) {
                        del.insert(Arc::clone(row));
                    }
                }
            }
        }
        Ok(!out.changes.inserted.is_empty() || !out.changes.deleted.is_empty())
    }

    /// Why the element `id` is on the page as it is: an explanation of each of its rows (`elem`, `attr`, `text`) in the
    /// last round.
    pub fn why(&self, id: &str) -> Result<Vec<why::Why>, HostError> {
        let explainer = why::Explainer::new(
            self.compiled.artifact.program.get(),
            self.compiled.artifact.program.clone(),
            &self.who,
            self.seed,
            &self.history,
        )?;
        let Some(last) = explainer.last() else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for name in ["elem", "attr", "text"] {
            let Some(rel) = self.compiled.outputs.get(name) else {
                continue;
            };
            let rows = explainer.rows(*rel, last)?;
            for row in rows.iter().filter(|r| r.first() == Some(&Value::Str(id.into()))) {
                out.extend(explainer.why(*rel, row, last, 0)?);
            }
        }
        Ok(out)
    }

    /// The durable rows to add to and remove from the host's store since the last call: every row the first time after a
    /// start (`full`), then only those the rounds since changed (BROWSER.md "Persistence").
    pub fn save_changes(&mut self) -> Result<store::SaveChanges, HostError> {
        let program = self.compiled.artifact.program.get();
        let name = |rel: RelId| program.rels.get(rel).map(|r| r.name.to_string()).unwrap_or_default();
        let mut out = store::SaveChanges {
            full: self.unsaved.is_none(),
            tables: store::schemas(program),
            put: Vec::new(),
            delete: Vec::new(),
        };
        match self.unsaved.take() {
            None => {
                for (id, r) in program.rels.iter_enumerated() {
                    if r.durable {
                        let rows = self
                            .engine
                            .carried_rows(id)
                            .map_err(|e| HostError::Store(e.to_string()))?;
                        for row in rows {
                            out.put.push((name(id), store::row_json(&row)?));
                        }
                    }
                }
            }
            Some(changes) => {
                for (rel, (ins, del)) in changes {
                    for row in &ins {
                        out.put.push((name(rel), store::row_json(row)?));
                    }
                    for row in &del {
                        out.delete.push((name(rel), store::row_json(row)?));
                    }
                }
            }
        }
        self.unsaved = Some(BTreeMap::new());
        Ok(out)
    }

    /// The durable tables, as JSON (for `localStorage`).
    pub fn saved(&self) -> Result<String, HostError> {
        let carried = self
            .engine
            .carried_instance()
            .map_err(|e| HostError::Store(e.to_string()))?;
        store::save(self.compiled.artifact.program.get(), &carried)
    }

    /// The page the last round left.
    pub fn page(&self) -> &Page {
        &self.page
    }

    pub fn compiled(&self) -> &Compiled {
        &self.compiled
    }
}
