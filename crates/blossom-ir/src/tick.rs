//! One node's tick, as every evaluator sees it (ARCHITECTURE §5.1): what a tick reads ([`TickInput`]) and what it
//! produces ([`TickOutput`]). The reference oracle and the engine both implement it, and neither depends on the
//! other (ARCH-16).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use blossom_base::RelId;
use blossom_value::{
    Value,
    time::{Instant, NodeId, Tick},
    value::SessionId,
};

use blossom_base::{InternalError, Unimplemented};

use crate::obs::{FiringRecord, ProgramErrorRecord};

/// A tuple.
pub type Row = Arc<[Value]>;

/// Relation contents: every non-empty relation's rows.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instance {
    pub rels: BTreeMap<RelId, BTreeSet<Row>>,
}

impl Instance {
    /// The rows of `rel` (empty when it has none).
    pub fn rows(&self, rel: RelId) -> impl Iterator<Item = &Row> {
        self.rels.get(&rel).into_iter().flatten()
    }

    /// Whether `rel` holds `row`.
    pub fn contains(&self, rel: RelId, row: &[Value]) -> bool {
        self.rels.get(&rel).is_some_and(|rows| rows.contains(row))
    }

    /// Adds a row; whether it was new.
    pub fn insert(&mut self, rel: RelId, row: Row) -> bool {
        self.rels.entry(rel).or_default().insert(row)
    }

    /// Whether no relation has a row.
    pub fn is_empty(&self) -> bool {
        self.rels.values().all(BTreeSet::is_empty)
    }
}

/// A channel tuple delivered to the node this tick. Column 0 of `row` is the destination, the node itself.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Delivery {
    pub rel: RelId,
    pub from: NodeId,
    pub row: Row,
}

/// A channel tuple the node sends: column 0 of `row` is the destination.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Send {
    pub rel: RelId,
    pub to: NodeId,
    pub row: Row,
}

/// A message from an external client session on a channel whose source role is `external` (LANGUAGE §18.4).
/// Column 0 of `row` is the destination, the node itself.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ingress {
    pub rel: RelId,
    pub session: SessionId,
    pub row: Row,
}

/// A reply to an external client session: column 0 of `row` is the session.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Egress {
    pub rel: RelId,
    pub session: SessionId,
    pub row: Row,
}

/// Everything one tick of one node reads.
#[derive(Clone, Debug)]
pub struct TickInput<'a> {
    /// `$self`.
    pub node: NodeId,
    /// The node's incarnation (its restart count; 1 on the first boot): `rand` draws differ across incarnations.
    pub incarnation: u64,
    pub tick: Tick,
    /// `$now`: the tick's clock sample.
    pub now: Instant,
    /// Last tick's `@next` heads.
    pub carried: &'a Instance,
    /// The tick's input events.
    pub events: &'a [(RelId, Row)],
    /// The channel tuples delivered this tick.
    pub delivered: &'a [Delivery],
    /// The messages client sessions sent this tick.
    pub ingress: &'a [Ingress],
    /// Whether to report the tick's firings.
    pub capture: bool,
}

/// Everything one tick of one node produces.
#[derive(Clone, Debug, Default)]
pub struct TickOutput {
    /// The tick's final instance: every relation's contents at this tick.
    pub instance: Instance,
    /// The inductive heads: next tick's carried state.
    pub next: Instance,
    /// The async heads to nodes.
    pub outbox: BTreeSet<Send>,
    /// The async heads to client sessions.
    pub egress: BTreeSet<Egress>,
    /// The distinct firings of the tick, in evaluation order (deterministic); empty unless capture was requested.
    pub firings: Vec<FiringRecord>,
}

/// Why an evaluator (the oracle or the engine) could not evaluate a tick.
#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    /// The deductive rules do not stratify: a negated or aggregated read on a same-tick cycle (SEM-020).
    #[error("the program does not stratify: {0}")]
    NotStratifiable(String),
    /// A deploy-time parameter without a default that the deployment does not bind.
    #[error("the deployment does not bind the parameter `{0}`, which has no default")]
    Unbound(String),
    /// Host functions the program declares that the evaluator's registry does not provide with that signature
    /// (checked when the program is loaded, LANG-181).
    #[error("unbound host functions: {}", .0.join("; "))]
    Externs(Vec<String>),
    /// A runtime hard error of the program at this tick (BLSRnnn, ARCHITECTURE §6.6).
    #[error("{} at tick {}: {}", .error.code, .tick.0, .error.detail)]
    Program { tick: Tick, error: ProgramErrorRecord },
    #[error(transparent)]
    Unimplemented(#[from] Unimplemented),
    #[error(transparent)]
    Internal(#[from] InternalError),
}

/// A tick's changes to a node's carried state: per relation, the rows added and the rows removed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub inserted: BTreeMap<RelId, Vec<Row>>,
    pub deleted: BTreeMap<RelId, Vec<Row>>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.inserted.values().all(Vec::is_empty) && self.deleted.values().all(Vec::is_empty)
    }

    /// The changes that turn `old` into `new`.
    pub fn between(old: &Instance, new: &Instance) -> Changes {
        let empty = BTreeSet::new();
        let mut out = Changes::default();
        for (rel, rows) in &new.rels {
            let before = old.rels.get(rel).unwrap_or(&empty);
            let added: Vec<Row> = rows.difference(before).cloned().collect();
            if !added.is_empty() {
                out.inserted.insert(*rel, added);
            }
        }
        for (rel, rows) in &old.rels {
            let after = new.rels.get(rel).unwrap_or(&empty);
            let removed: Vec<Row> = rows.difference(after).cloned().collect();
            if !removed.is_empty() {
                out.deleted.insert(*rel, removed);
            }
        }
        out
    }

    /// Applies the changes to `state`.
    pub fn apply(&self, state: &mut Instance) {
        for (rel, rows) in &self.deleted {
            if let Some(set) = state.rels.get_mut(rel) {
                for r in rows {
                    set.remove(r);
                }
            }
        }
        for (rel, rows) in &self.inserted {
            let set = state.rels.entry(*rel).or_default();
            for r in rows {
                set.insert(r.clone());
            }
        }
        state.rels.retain(|_, rows| !rows.is_empty());
    }
}

/// What one tick of a stateful executor reads: the tick's inputs, without the carried state (the executor keeps it).
#[derive(Clone, Debug)]
pub struct StepInput<'a> {
    pub node: NodeId,
    pub incarnation: u64,
    pub tick: Tick,
    pub now: Instant,
    pub events: &'a [(RelId, Row)],
    pub delivered: &'a [Delivery],
    pub ingress: &'a [Ingress],
}

/// What one tick of a stateful executor produces.
#[derive(Clone, Debug, Default)]
pub struct StepOutput {
    /// The changes to the carried state: what the next tick starts from, relative to what this one started from.
    pub changes: Changes,
    pub outbox: BTreeSet<Send>,
    pub egress: BTreeSet<Egress>,
    /// The final contents, at this tick, of the relations the caller asked to observe.
    pub observed: BTreeMap<RelId, Vec<Row>>,
}

/// Checks that `registry` provides every host function `program` declares (`extern fn`), with the declared
/// signature. The error lists every one that is missing or differs, so a program never loads with a host call that
/// cannot run.
pub fn bind_externs(
    program: &crate::core::Program,
    registry: &blossom_value::ExternRegistry,
) -> Result<(), EvalError> {
    let mut problems = Vec::new();
    for f in program.fns.iter() {
        if let crate::core::FnBody::Extern { path, .. } = &f.body {
            let params: Vec<blossom_base::TypeId> = f.params.iter().map(|p| p.1).collect();
            if let Err(e) = registry.bind(path, &program.types, &params, &[f.ret], false) {
                problems.push(format!("`{}` ({path}): {e}", f.name));
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(EvalError::Externs(problems))
    }
}
