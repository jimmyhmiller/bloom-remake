//! Queries of a node's database (docs/design/DATABASE.md §5).
//!
//! A query arrives compiled: the least program computing its view from the durable relations it reads, each an input
//! of it (`ValidatedProgram::query`), with the view's name. The node gives the oracle, for one tick, the rows of those
//! relations as of the tick asked for (the newest released, unless another inside the history kept), and answers the
//! view's rows. A relation is found by name and must have the node's schema: a query compiled against another
//! version of the program is refused. Where every read of a relation binds its leading columns to the same constants,
//! only the rows with them are read (a prefix scan of the database).

use std::collections::BTreeMap;
use std::sync::Arc;

use blossom_base::RelId;
use blossom_ir::ValidatedProgram;
use blossom_ir::core::{EventSource, Literal, Program, RelClass, Term};
use blossom_ir::tick::{Instance, TickInput};
use blossom_value::Value;
use blossom_value::time::{Instant, NodeId, Tick};
use serde::{Deserialize, Serialize};

use crate::RuntimeError;
use crate::db::Database;

/// The largest query a node reads.
pub const MAX_REQUEST: usize = 16 << 20;

/// A compiled query, as the CLI sends it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueryRequest {
    /// The query program (see the module's documentation).
    pub program: Program,
    /// Its view.
    pub view: String,
    /// The tick to read as of (`None`: the newest released).
    pub as_of: Option<u64>,
}

impl QueryRequest {
    pub fn encode(&self) -> Result<Vec<u8>, RuntimeError> {
        postcard::to_allocvec(self).map_err(|e| RuntimeError::Config(format!("encoding the query: {e}")))
    }

    pub fn decode(bytes: &[u8]) -> Result<QueryRequest, RuntimeError> {
        if bytes.len() > MAX_REQUEST {
            return Err(RuntimeError::Config("a query over 16 MiB".into()));
        }
        postcard::from_bytes(bytes).map_err(|e| RuntimeError::Config(format!("a malformed query: {e}")))
    }
}

/// A query's answer: the tick it read as of, the view's columns, and its rows (each value as Blossom writes it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub tick: u64,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

/// The constants every positive read of `rel` binds its leading columns to, when all agree.
pub fn leading_constants(q: &Program, rel: RelId) -> Vec<Value> {
    let mut agreed: Option<Vec<Value>> = None;
    for r in q.rules.iter() {
        for lit in &r.body.lits {
            let atom = match lit {
                Literal::Pos(a) if a.rel == rel => a,
                // A negated read needs the whole relation.
                Literal::Neg(a) if a.rel == rel => return Vec::new(),
                _ => continue,
            };
            let mine: Vec<Value> = atom
                .args
                .iter()
                .map_while(|t| match t {
                    Term::Const(c) => q.consts.get(*c).cloned(),
                    _ => None,
                })
                .collect();
            agreed = Some(match agreed {
                None => mine,
                Some(prev) => prev
                    .iter()
                    .zip(&mine)
                    .take_while(|(a, b)| a == b)
                    .map(|(a, _)| a.clone())
                    .collect(),
            });
        }
    }
    agreed.unwrap_or_default()
}

/// Answers `req` from `db`, as of the tick it asks (the node's program `node`, its node names `names`, the host
/// functions `externs`, the instant `now` the query runs at).
pub fn answer(
    req: QueryRequest,
    db: &Database,
    node: &ValidatedProgram,
    names: &[Arc<str>],
    externs: Arc<blossom_value::ExternRegistry>,
    now: Instant,
) -> Result<Answer, RuntimeError> {
    let program = ValidatedProgram::validate(req.program).map_err(|errs| {
        RuntimeError::Config(format!(
            "the query program is not valid: {}",
            errs.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")
        ))
    })?;
    let q = program.get();
    let Some((view, decl)) = q.rels.iter_enumerated().find(|(_, r)| r.name.to_string() == req.view) else {
        return Err(RuntimeError::Config(format!(
            "the query program has no view `{}`",
            req.view
        )));
    };
    let (floor, applied) = db.range()?;
    let tick = req.as_of.unwrap_or(applied);
    if tick < floor || tick > applied {
        return Err(RuntimeError::Config(format!(
            "tick {tick} is outside the database's history (ticks {floor} to {applied})"
        )));
    }
    let durable: BTreeMap<String, RelId> = db.relations().into_iter().map(|(r, n)| (n.to_string(), r)).collect();
    let mut events = Vec::new();
    for (qid, r) in q.rels.iter_enumerated() {
        if !matches!(r.class, RelClass::Event(EventSource::Input)) {
            continue;
        }
        let name = r.name.to_string();
        let Some(nid) = durable.get(&name).copied() else {
            return Err(RuntimeError::Config(format!(
                "the query reads `{name}`, which is not a durable relation of this node's program"
            )));
        };
        if blossom_wire::catalog::schema_hash(q, qid) != blossom_wire::catalog::schema_hash(node.get(), nid) {
            return Err(RuntimeError::Config(format!(
                "the query was compiled against another schema of `{name}` than this node runs"
            )));
        }
        for row in db.rows(nid, &leading_constants(q, qid), tick)? {
            events.push((qid, row));
        }
    }
    let oracle = blossom_oracle::Oracle::with_externs(program.clone(), blossom_oracle::Limits::default(), externs)?;
    let out = oracle.tick(&TickInput {
        node: NodeId(0),
        incarnation: 1,
        tick: Tick(0),
        now,
        carried: &Instance::default(),
        events: &events,
        delivered: &[],
        ingress: &[],
        capture: false,
        blobs: &blossom_value::NoBlobs,
    })?;
    let node_name = |n: NodeId| blossom_ir::printer::node_text(n, names);
    let tys: Vec<_> = decl.schema.cols.iter().map(|c| c.ty).collect();
    let rows = out
        .instance
        .rows(view)
        .map(|row| {
            row.iter()
                .enumerate()
                .map(|(i, v)| blossom_ir::printer::value_text(Some(q), v, tys.get(i).copied(), &node_name))
                .collect()
        })
        .collect();
    Ok(Answer {
        tick,
        columns: decl.schema.cols.iter().map(|c| c.name.to_string()).collect(),
        rows,
    })
}
