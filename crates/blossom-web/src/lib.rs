//! Blossom in the browser (docs/design/BROWSER.md).
//!
//! A browser app is an ordinary single-node Blossom program. [`compile`] compiles it from sources in memory;
//! [`App::start`] restores its durable tables and runs its first rounds; [`App::dispatch`] runs one round per DOM
//! event. The program describes its page with the outputs `elem`, `attr`, `text` and `focus` ([`page`]), and hears
//! the world through the inputs it declares (`route`, `click`, `dblclick`, `typed`, `keydown`, `blur`, `change`);
//! each round's page is diffed against the last into DOM patches. The core here is plain Rust, which native tests
//! drive; `wasm` is the page's API over it.

pub mod page;
pub mod store;
#[cfg(target_arch = "wasm32")]
mod wasm;

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
}

/// A DOM event, as the page reports it.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Event {
    Route { hash: String },
    Click { id: String },
    Dblclick { id: String },
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
            Event::Click { id } | Event::Dblclick { id } => vec![s(id)],
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
const INPUTS: [(&str, &[&str]); 7] = [
    ("route", &[STR]),
    ("click", &[STR]),
    ("dblclick", &[STR]),
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
                Some((file, lc.line, lc.column))
            });
            Diag {
                severity: format!("{:?}", d.severity).to_lowercase(),
                code: d.code.as_str().to_owned(),
                message: d.message.clone(),
                rendered: blossom_driver::render::render(d, sources),
                file: at.as_ref().map(|a| a.0.clone()),
                line: at.as_ref().map(|a| a.1),
                column: at.as_ref().map(|a| a.2),
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
    /// The next round, and the page the last one left.
    tick: u64,
    page: Page,
}

/// What starting a program did: the first page, and what the restore could not keep.
#[derive(Debug, Serialize)]
pub struct Started {
    pub patches: Vec<Patch>,
    pub notes: Vec<String>,
}

impl App {
    /// A program ready to start.
    pub fn new(compiled: Compiled) -> Result<App, HostError> {
        let engine = Engine::new(
            compiled.artifact.program.clone(),
            NodeId(0),
            EngineConfig {
                roles: compiled.artifact.roles.clone(),
                node_names: vec![Arc::from("app")],
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
            tick: 0,
            page: Page::default(),
        })
    }

    /// Starts the program: restores the durable tables `saved` holds (when given), then runs the boot round and the
    /// `route` round of `hash`. Returns the patches that draw the first page.
    pub fn start(&mut self, saved: Option<&str>, hash: &str) -> Result<Started, HostError> {
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
        let boot: Vec<(RelId, Row)> = self
            .compiled
            .artifact
            .boot()
            .map(|b| (b, Arc::from(Vec::new())))
            .into_iter()
            .collect();
        let mut patches = self.settle(&boot)?;
        patches.extend(self.dispatch(&Event::Route { hash: hash.to_owned() })?);
        Ok(Started { patches, notes })
    }

    /// Runs one event (an event the program does not listen to runs nothing) until its effects settle; returns the
    /// patches from the page before it to the page after.
    pub fn dispatch(&mut self, event: &Event) -> Result<Vec<Patch>, HostError> {
        match self.compiled.inputs.get(event.input()) {
            Some(rel) => self.settle(&[(*rel, event.row())]),
            None => Ok(Vec::new()),
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

    /// Runs one round with `events`, leaving its page in `self.page`; whether the state changed.
    fn round(&mut self, events: &[(RelId, Row)]) -> Result<bool, HostError> {
        let tick = self.tick;
        let observe: Vec<RelId> = self.compiled.outputs.values().copied().collect();
        let out = self
            .engine
            .step(
                &StepInput {
                    node: NodeId(0),
                    incarnation: 1,
                    tick: Tick(tick),
                    now: Instant(i64::try_from(tick).unwrap_or(i64::MAX).saturating_mul(1_000_000)),
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
