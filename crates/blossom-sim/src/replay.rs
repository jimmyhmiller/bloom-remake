//! Replaying one node's recorded trace (`blossom run --record`, ARCHITECTURE §6.4) in-process.
//!
//! A trace holds what an incarnation started from and every input of every tick ([`blossom_trace::node`]). The
//! replay runs the ticks again with the engine, as the node did, and checks each tick's outcome against the one
//! recorded: a tick that changes or sends anything else is a divergence (a nondeterminism, or a recording that does
//! not hold everything a tick reads), reported with the tick, never passed over. Any tick can also be examined:
//! run once more by the oracle from the same state, which gives the tick's whole instance (views included) and its
//! rule firings, for `blossom trace show`, `why` and `whynot`.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_base::{FnId, RelId, RuleId};
use blossom_ir::tick::{
    Changes, Delivery, EvalError, FnWork, Ingress, Instance, Row, RuleWork, Send, StepInput, TickInput, TickOutput,
};
use blossom_node::{Backend, Executor, Executors};
use blossom_oracle::Oracle;
use blossom_trace::node::{NodeRecord, NodeTraceHeader, TraceError, TraceReader, outcome_digest};
use blossom_value::blob::BlobMap;
use blossom_value::time::{Instant, NodeId, Tick};
use blossom_value::{BlobRef, ExternRegistry, Seed};

/// Why a replay stopped.
#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error(transparent)]
    Trace(#[from] TraceError),
    #[error("the trace is of {found}; the program given is {expected}")]
    Program { found: String, expected: String },
    #[error("the trace's records are out of order: {0}")]
    Order(String),
    #[error(transparent)]
    Eval(#[from] EvalError),
    /// The replayed tick did something else than the recorded one.
    #[error("tick {tick} diverged: {what}")]
    Diverged { tick: u64, what: String },
}

/// One tick's recorded inputs.
#[derive(Clone, Debug)]
pub struct Inputs {
    pub tick: Tick,
    pub now: Instant,
    pub events: Vec<(RelId, Row)>,
    pub delivered: Vec<Delivery>,
    pub ingress: Vec<Ingress>,
}

/// One replayed tick.
#[derive(Debug)]
pub struct Replayed {
    pub inputs: Inputs,
    /// What it changed in the carried state (`None`: it failed, as recorded).
    pub changes: Option<Changes>,
    /// The error it failed with, as recorded and replayed.
    pub failed: Option<String>,
    /// What it sent to other nodes.
    pub sent: Vec<Send>,
    /// The rows at the end of the tick of the relations observed ([`Replay::observe`]).
    pub observed: BTreeMap<RelId, Vec<Row>>,
    /// The work of each rule in the tick (rows examined, expression nodes evaluated), when profiling
    /// ([`Replay::profile`]).
    pub work: BTreeMap<RuleId, RuleWork>,
    /// The work of each function called in the tick, when profiling.
    pub fn_work: BTreeMap<FnId, FnWork>,
    /// The tick run by the oracle from the same state, when asked for ([`Replay::next`] with `examine`).
    pub examined: Option<TickOutput>,
}

/// A replay of one node's trace.
pub struct Replay<R: Read> {
    reader: TraceReader<R>,
    engine: Box<dyn Executor>,
    oracle: Arc<Oracle>,
    node: NodeId,
    incarnation: u64,
    /// The blobs recorded, and those the replayed ticks created.
    blobs: BTreeMap<BlobRef, Arc<[u8]>>,
    /// A record read ahead.
    ahead: Option<NodeRecord>,
    booted: bool,
    /// The relations whose rows each replayed tick reports.
    observe: Vec<RelId>,
    profile: bool,
}

impl<R: Read> Replay<R> {
    /// Opens a replay of the trace `inp` of a node running `artifact` (compiled for the trace's deployment).
    pub fn open(artifact: &BlsArtifact, inp: R, externs: Arc<ExternRegistry>) -> Result<Replay<R>, ReplayError> {
        let reader = TraceReader::open(inp)?;
        let h: NodeTraceHeader = reader.header().clone();
        let program = artifact.program.get();
        let nodes: Vec<Arc<str>> = artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect();
        let same = h.program.as_ref() == program.meta.name.as_str()
            && h.version == program.meta.version
            && h.digest == artifact.program.digest().0
            && h.nodes == nodes;
        if !same {
            return Err(ReplayError::Program {
                found: format!("{} version {} for nodes {:?}", h.program, h.version, h.nodes),
                expected: format!(
                    "{} version {} for nodes {:?}",
                    program.meta.name, program.meta.version, nodes
                ),
            });
        }
        let executors = Executors::new(
            Backend::Engine,
            artifact.program.clone(),
            artifact.roles.clone(),
            nodes,
            Seed(h.seed),
            externs,
        )?;
        Ok(Replay {
            engine: executors.make(h.node)?,
            oracle: executors.oracle().clone(),
            node: h.node,
            incarnation: h.incarnation,
            reader,
            blobs: BTreeMap::new(),
            ahead: None,
            booted: false,
            observe: Vec::new(),
            profile: false,
        })
    }

    pub fn header(&self) -> &NodeTraceHeader {
        self.reader.header()
    }

    /// Whether the trace ended inside a record (the node was killed while recording a tick).
    pub fn torn(&self) -> bool {
        self.reader.torn()
    }

    /// Reports the rows of `rels` at the end of every tick replayed from now on ([`Replayed::observed`]): views
    /// included, which the carried state does not hold.
    pub fn observe(&mut self, rels: Vec<RelId>) {
        self.observe = rels;
    }

    /// Reports each rule's and each function's work in every tick replayed from now on ([`Replayed::work`],
    /// [`Replayed::fn_work`]).
    pub fn profile(&mut self, on: bool) -> Result<(), ReplayError> {
        if on && (self.engine.work_by_rule().is_none() || !self.engine.profile_functions(true)) {
            return Err(ReplayError::Order(
                "the replaying executor does not measure its work".into(),
            ));
        }
        if !on {
            self.engine.profile_functions(false);
        }
        self.profile = on;
        Ok(())
    }

    /// The carried state the next tick starts from.
    pub fn carried(&self) -> Instance {
        self.engine.carried()
    }

    fn read(&mut self) -> Result<Option<NodeRecord>, ReplayError> {
        match self.ahead.take() {
            Some(r) => Ok(Some(r)),
            None => Ok(self.reader.next_record()?),
        }
    }

    /// The tick the next call to [`Replay::next`] replays (`None` at the end of the trace).
    pub fn peek_tick(&mut self) -> Result<Option<Tick>, ReplayError> {
        self.boot()?;
        if self.ahead.is_none() {
            self.ahead = self.reader.next_record()?;
        }
        match &self.ahead {
            Some(NodeRecord::Tick { tick, .. }) => Ok(Some(*tick)),
            None => Ok(None),
            Some(other) => Err(ReplayError::Order(format!("{other:?} where a tick was due"))),
        }
    }

    fn boot(&mut self) -> Result<(), ReplayError> {
        if self.booted {
            return Ok(());
        }
        match self.read()? {
            Some(NodeRecord::Boot { image }) => {
                let mut carried = Instance::default();
                for (rel, rows) in image {
                    for row in rows {
                        carried.insert(rel, row);
                    }
                }
                self.engine.reset(carried)?;
                self.booted = true;
                Ok(())
            }
            Some(other) => Err(ReplayError::Order(format!("a trace that starts with {other:?}"))),
            None => Err(ReplayError::Order("an empty trace (no boot record)".into())),
        }
    }

    /// Replays the next tick (`None` at the end of the trace). With `examine`, the oracle also runs it, from the same
    /// state, capturing its firings. A tick whose outcome the trace does not hold (the node was killed while it ran)
    /// is replayed unchecked.
    pub fn next(&mut self, examine: bool) -> Result<Option<Replayed>, ReplayError> {
        self.boot()?;
        let inputs = match self.read()? {
            None => return Ok(None),
            Some(NodeRecord::Tick {
                tick,
                now,
                events,
                delivered,
                ingress,
            }) => Inputs {
                tick,
                now,
                events,
                delivered,
                ingress,
            },
            Some(other) => return Err(ReplayError::Order(format!("{other:?} where a tick was due"))),
        };
        // The blobs the tick read, then its recorded outcome.
        let mut outcome = None;
        loop {
            match self.read()? {
                Some(NodeRecord::Blob { blob, bytes }) => {
                    self.blobs.insert(blob, Arc::from(bytes));
                }
                Some(NodeRecord::Outcome { tick, digest }) if tick == inputs.tick => {
                    outcome = Some(Ok(digest));
                    break;
                }
                Some(NodeRecord::Failed { tick, error }) if tick == inputs.tick => {
                    outcome = Some(Err(error));
                    break;
                }
                Some(r @ NodeRecord::Tick { .. }) => {
                    // The tick before ran without its outcome recorded (never written: the trace continues).
                    self.ahead = Some(r);
                    break;
                }
                Some(other) => return Err(ReplayError::Order(format!("{other:?} after tick {}", inputs.tick.0))),
                None => break,
            }
        }
        let blobs = BlobMap(self.blobs.clone());
        let examined = if examine {
            let carried = self.engine.carried();
            Some(self.oracle.tick(&TickInput {
                node: self.node,
                incarnation: self.incarnation,
                tick: inputs.tick,
                now: inputs.now,
                carried: &carried,
                events: &inputs.events,
                delivered: &inputs.delivered,
                ingress: &inputs.ingress,
                capture: true,
                blobs: &blobs,
            })?)
        } else {
            None
        };
        let work_before = if self.profile {
            // Counted afresh for this tick.
            self.engine.profile_functions(true);
            self.engine.work_by_rule()
        } else {
            None
        };
        let result = self.engine.step(
            &StepInput {
                node: self.node,
                incarnation: self.incarnation,
                tick: inputs.tick,
                now: inputs.now,
                events: &inputs.events,
                delivered: &inputs.delivered,
                ingress: &inputs.ingress,
                blobs: &blobs,
            },
            &self.observe,
        );
        let tick = inputs.tick.0;
        let mut work = BTreeMap::new();
        if let (Some(before), Some(after)) = (work_before, self.engine.work_by_rule()) {
            for (rule, n) in after {
                let b = before.get(&rule).copied().unwrap_or_default();
                let d = RuleWork {
                    rows: n.rows - b.rows,
                    steps: n.steps - b.steps,
                };
                if d != RuleWork::default() {
                    work.insert(rule, d);
                }
            }
        }
        let fn_work = if self.profile {
            self.engine.work_by_function().unwrap_or_default()
        } else {
            BTreeMap::new()
        };
        match (result, outcome) {
            (Ok(out), Some(Ok(digest))) => {
                let replayed = outcome_digest(&out.changes, &out.outbox, &out.egress, &out.host);
                if replayed != digest {
                    return Err(ReplayError::Diverged {
                        tick,
                        what: "its changes and sends differ from the recording".into(),
                    });
                }
                self.blobs.extend(out.blobs);
                Ok(Some(Replayed {
                    inputs,
                    changes: Some(out.changes),
                    failed: None,
                    sent: out.outbox.into_iter().collect(),
                    observed: out.observed,
                    work,
                    fn_work,
                    examined,
                }))
            }
            (Ok(out), None) => {
                self.blobs.extend(out.blobs);
                Ok(Some(Replayed {
                    inputs,
                    changes: Some(out.changes),
                    failed: None,
                    sent: out.outbox.into_iter().collect(),
                    observed: out.observed,
                    work,
                    fn_work,
                    examined,
                }))
            }
            (Ok(_), Some(Err(error))) => Err(ReplayError::Diverged {
                tick,
                what: format!("it succeeded, but the recorded tick failed: {error}"),
            }),
            (Err(e), Some(Err(error))) if e.to_string() == error => Ok(Some(Replayed {
                inputs,
                changes: None,
                failed: Some(error),
                sent: Vec::new(),
                observed: BTreeMap::new(),
                work,
                fn_work,
                examined,
            })),
            (Err(e), _) => Err(ReplayError::Diverged {
                tick,
                what: format!("it failed with {e}, which the recording did not"),
            }),
        }
    }

    /// Why each rule deriving `rel` does (not) derive a tuple matching `pattern` at the tick `replayed` (examined).
    pub fn why_not(
        &self,
        replayed: &Replayed,
        rel: RelId,
        pattern: &[Option<blossom_value::Value>],
        samples: usize,
    ) -> Result<Vec<blossom_oracle::WhyNot>, ReplayError> {
        let Some(out) = &replayed.examined else {
            return Err(ReplayError::Order(format!(
                "tick {} was not examined",
                replayed.inputs.tick.0
            )));
        };
        let blobs = BlobMap(self.blobs.clone());
        let carried = Instance::default();
        let input = TickInput {
            node: self.node,
            incarnation: self.incarnation,
            tick: replayed.inputs.tick,
            now: replayed.inputs.now,
            carried: &carried,
            events: &replayed.inputs.events,
            delivered: &replayed.inputs.delivered,
            ingress: &replayed.inputs.ingress,
            capture: false,
            blobs: &blobs,
        };
        Ok(self.oracle.why_not(&input, &out.instance, rel, pattern, samples)?)
    }

    /// The oracle (for its program, to name rules and relations).
    pub fn oracle(&self) -> &Oracle {
        &self.oracle
    }
}
