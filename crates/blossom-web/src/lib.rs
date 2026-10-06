//! Blossom in the browser (docs/design/BROWSER.md).
//!
//! A browser app is an ordinary single-node Blossom program. [`compile`] compiles it from sources in memory;
//! [`App::start`] restores its durable tables and runs its first rounds; [`App::dispatch`] runs one round per DOM
//! event, and [`App::advance`] one per instant its physical timers fire (the page's clock; LANGUAGE §15.2). The
//! program describes its page with the outputs `elem`, `attr`, `text` and `focus` ([`page`]), and hears the world
//! through the inputs it declares (`route`, `click`, `dblclick`, `press`, `typed`, `keydown`, `blur`, `change`);
//! each round's page is diffed against the last into DOM patches. The core here is plain Rust, which native tests
//! drive; `wasm` is the page's API over it.

pub mod page;
pub mod store;
#[cfg(target_arch = "wasm32")]
mod wasm;
pub mod why;

use std::collections::BTreeMap;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_base::{RelId, SourceDb};
use blossom_engine::{Engine, EngineConfig};
use blossom_front::api::{BlsError, NodeSpec};
use blossom_front::ded::LoadedFile;
use blossom_front::modules::Loader;
use blossom_ir::core::RelClass;
use blossom_ir::tick::{Instance, Row, StepInput};
use blossom_ir::timers::TimerTable;
use blossom_value::Value;
use blossom_value::time::{Instant, NodeId, Tick};
use blossom_value::types::{IntTy, TypeDef};
use serde::{Deserialize, Serialize};

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
    Route { hash: String },
    Click { id: String },
    Dblclick { id: String },
    Press { id: String },
    Input { id: String, value: String },
    Keydown { id: String, key: String, value: String },
    Blur { id: String, value: String },
    Change { id: String, checked: bool },
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
const INPUTS: [(&str, &[&str]); 8] = [
    ("route", &[STR]),
    ("click", &[STR]),
    ("dblclick", &[STR]),
    ("press", &[STR]),
    ("typed", &[STR, STR]),
    ("keydown", &[STR, STR, STR]),
    ("blur", &[STR, STR]),
    ("change", &[STR, BOOL]),
];

/// A program compiled for the browser.
pub struct Compiled {
    artifact: BlsArtifact,
    /// The page outputs the program declares (missing ones are empty), by name.
    outputs: BTreeMap<&'static str, RelId>,
    /// The event inputs it declares, by name.
    inputs: BTreeMap<&'static str, RelId>,
    pub warnings: Vec<Diag>,
}

/// Sources in memory: `path` → text.
struct Files<'a>(&'a BTreeMap<String, String>);

impl Loader for Files<'_> {
    fn load(&mut self, _from: Option<&str>, path: &str) -> Result<LoadedFile, String> {
        let path = path.trim_start_matches("./");
        self.0
            .get(path)
            .map(|text| LoadedFile {
                key: Arc::from(path),
                text: text.clone(),
            })
            .ok_or_else(|| format!("no file `{path}`"))
    }
}

fn diags(found: &blossom_base::Diagnostics, sources: &SourceDb) -> Vec<Diag> {
    found
        .iter()
        .map(|d| {
            let at = d.primary.and_then(|s| {
                let lc = sources.line_col(s.file, s.lo).ok()?;
                let file = sources.path(s.file).ok()?.to_string();
                // An editor's offsets (JavaScript's: UTF-16 code units).
                let text = sources.text(s.file).ok()?;
                let utf16 = |byte: u32| u32::try_from(text.get(..byte as usize)?.encode_utf16().count()).ok();
                Some((file, lc.line, lc.column, utf16(s.lo)?, utf16(s.hi)?))
            });
            Diag {
                severity: format!("{:?}", d.severity).to_lowercase(),
                code: d.code.as_str().to_owned(),
                message: d.message.clone(),
                rendered: blossom_driver::render::render(d, sources),
                file: at.as_ref().map(|a| a.0.clone()),
                line: at.as_ref().map(|a| a.1),
                column: at.as_ref().map(|a| a.2),
                range: at.as_ref().map(|a| (a.3, a.4)),
            }
        })
        .collect()
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

/// Compiles the program `root` of `files` (`path` → source) for the browser: one node, no roles. Its diagnostics
/// on failure.
pub fn compile(root: &str, files: &BTreeMap<String, String>) -> Result<Compiled, Vec<Diag>> {
    let nodes = [NodeSpec {
        name: "app".to_owned(),
        role: None,
    }];
    let (result, sources) = blossom_driver::bls::compile_with_loader(root, &nodes, &BTreeMap::new(), &mut Files(files));
    let (artifact, warnings) = match result {
        Ok(ok) => ok,
        Err(BlsError::Rejected(found)) => return Err(diags(&found, &sources)),
        Err(e) => {
            return Err(vec![Diag {
                severity: "error".to_owned(),
                code: String::new(),
                message: e.to_string(),
                rendered: e.to_string(),
                file: None,
                line: None,
                column: None,
                range: None,
            }]);
        }
    };
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
        return Err(problems
            .into_iter()
            .map(|p| Diag {
                severity: "error".to_owned(),
                code: String::new(),
                message: p.clone(),
                rendered: format!("error: {p}"),
                file: None,
                line: None,
                column: None,
                range: None,
            })
            .collect());
    }
    Ok(Compiled {
        warnings: diags(&warnings, &sources),
        artifact,
        outputs,
        inputs,
    })
}

impl Compiled {
    /// The DOM events the program listens to: the inputs it declares and reads (`route` included).
    pub fn listens(&self) -> Vec<&'static str> {
        self.inputs.keys().copied().collect()
    }
}

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
        let engine = Engine::new(
            compiled.artifact.program.clone(),
            NodeId(0),
            EngineConfig {
                roles: compiled.artifact.roles.clone(),
                node_names: vec![Arc::from("app")],
                seed: Some(seed),
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
        })
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
        self.engine.reset(carried).map_err(|e| HostError::Round {
            tick: 0,
            error: e.to_string(),
        })?;
        self.tick = 0;
        self.page = Page::default();
        self.history.clear();
        self.now = now;
        self.timers = Some(
            TimerTable::new(self.compiled.artifact.program.get(), None, now).map_err(|e| HostError::Round {
                tick: 0,
                error: e.to_string(),
            })?,
        );
        let boot: Vec<(RelId, Row)> = self
            .compiled
            .artifact
            .boot()
            .map(|b| (b, Arc::from(Vec::new())))
            .into_iter()
            .collect();
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
        let before = self.page.clone();
        let mut changed = self.round(events)?;
        let mut quiet_rounds = 0;
        while changed {
            if quiet_rounds == SETTLE {
                return Err(HostError::Unsettled(SETTLE));
            }
            changed = self.round(&[])?;
            quiet_rounds += 1;
        }
        Ok(before.diff(&self.page))
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
        let before = self.engine.carried_instance();
        let guards: Vec<RelId> = self.timers.iter().flat_map(|t| t.guards()).collect();
        let observe: Vec<RelId> = self.compiled.outputs.values().copied().chain(guards).collect();
        let out = self
            .engine
            .step(
                &StepInput {
                    node: NodeId(0),
                    incarnation: 1,
                    tick: Tick(tick),
                    now,
                    events,
                    delivered: &[],
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
        self.history.push(why::Round {
            tick,
            now,
            before,
            events: events.to_vec(),
            inserted: out
                .changes
                .inserted
                .iter()
                .flat_map(|(rel, rows)| rows.iter().map(|r| (*rel, Arc::clone(r))))
                .collect(),
        });
        let rows = |name: &str| -> &[Row] {
            self.compiled
                .outputs
                .get(name)
                .and_then(|r| out.observed.get(r))
                .map_or(&[], Vec::as_slice)
        };
        self.page = Page::of(rows("elem"), rows("attr"), rows("text"), rows("focus"))?;
        Ok(!out.changes.inserted.is_empty() || !out.changes.deleted.is_empty())
    }

    /// Why the element `id` is on the page as it is: an explanation of each of its rows (`elem`, `attr`, `text`) in the
    /// last round.
    pub fn why(&self, id: &str) -> Result<Vec<why::Why>, HostError> {
        let explainer = why::Explainer::new(
            self.compiled.artifact.program.get(),
            self.compiled.artifact.program.clone(),
            self.compiled.artifact.roles.clone(),
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

    /// The durable tables, as JSON (for `localStorage`).
    pub fn saved(&self) -> Result<String, HostError> {
        store::save(self.compiled.artifact.program.get(), &self.engine.carried_instance())
    }

    /// The page the last round left.
    pub fn page(&self) -> &Page {
        &self.page
    }

    pub fn compiled(&self) -> &Compiled {
        &self.compiled
    }
}
