//! Queries of a node's database (docs/design/DATABASE.md §5).
//!
//! A query arrives compiled: the least program computing its view from the durable relations it reads, each an input
//! of it (`ValidatedProgram::query`), with the view's name. The node gives the oracle, for one tick, the rows of those
//! relations as of the tick asked for (the newest released, unless another inside the history kept), and answers the
//! view's rows. A relation is found by name and must have the node's schema: a query compiled against another
//! version of the program is refused. Where every read of a relation binds its leading columns to the same constants,
//! only the rows with them are read (a prefix scan of the database).

use std::collections::BTreeMap;
use std::ops::Bound;
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

/// The constants every positive read of `rel` binds its leading columns to, when all agree and every read of it is
/// an atom (a lookup or an expression reading it needs every row).
pub fn leading_constants(program: &ValidatedProgram, rel: RelId) -> Vec<Value> {
    if !program.read_only_by_atoms(rel) {
        return Vec::new();
    }
    let q = program.get();
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

/// What a query reads of a relation: the rows whose leading columns are `leading` and whose next column lies within
/// `lo` and `hi`.
#[derive(Clone, Debug, PartialEq)]
pub struct Scan {
    pub leading: Vec<Value>,
    pub lo: Bound<Value>,
    pub hi: Bound<Value>,
}

/// Whether the database's key order is the value order for `v` (and a comparison of it a range of keys).
fn ranged(v: &Value) -> bool {
    matches!(
        v,
        Value::Bool(_) | Value::Int(_) | Value::Str(_) | Value::Bytes(_) | Value::Duration(_) | Value::Instant(_)
    )
}

/// The tighter of two lower bounds (`upper`: of two upper bounds).
fn tighter(a: Bound<Value>, b: Bound<Value>, upper: bool) -> Bound<Value> {
    use Bound::*;
    match (&a, &b) {
        (Unbounded, _) => b,
        (_, Unbounded) => a,
        (Included(x) | Excluded(x), Included(y) | Excluded(y)) => {
            let (x_wins, tie) = if upper { (x < y, x == y) } else { (x > y, x == y) };
            if tie {
                // At the same value an excluded bound is the tighter.
                if matches!(a, Excluded(_)) { a } else { b }
            } else if x_wins {
                a
            } else {
                b
            }
        }
    }
}

/// What the query must read of `rel`: its leading constants and, when the relation has one read (an atom) and that
/// read's rule compares the next column with constants, the range they allow. Reading only these rows is exact: every
/// derivation through the atom satisfies the comparisons, which the query's rules still check.
pub fn scan_of(program: &ValidatedProgram, rel: RelId) -> Scan {
    let leading = leading_constants(program, rel);
    let mut scan = Scan {
        leading,
        lo: Bound::Unbounded,
        hi: Bound::Unbounded,
    };
    if !program.read_only_by_atoms(rel) {
        return scan;
    }
    let q = program.get();
    let mut reads = Vec::new();
    for r in q.rules.iter() {
        for lit in &r.body.lits {
            match lit {
                Literal::Pos(a) if a.rel == rel => reads.push((r, a)),
                Literal::Neg(a) if a.rel == rel => return scan,
                _ => {}
            }
        }
    }
    let [(rule, atom)] = reads.as_slice() else {
        return scan;
    };
    let Some(Term::Var(x)) = atom.args.get(scan.leading.len()) else {
        return scan;
    };
    for lit in &rule.body.lits {
        let Literal::Guard(blossom_ir::core::Expr::Binary { op, lhs, rhs }) = lit else {
            continue;
        };
        use blossom_ir::core::{BinOp, Expr};
        // `x op c`, or `c op x` read the other way round.
        let (op, c) = match (&**lhs, &**rhs) {
            (Expr::Term(Term::Var(v)), Expr::Term(Term::Const(c))) if v == x => (op.clone(), *c),
            (Expr::Term(Term::Const(c)), Expr::Term(Term::Var(v))) if v == x => {
                let flipped = match op {
                    BinOp::Lt => BinOp::Gt,
                    BinOp::Le => BinOp::Ge,
                    BinOp::Gt => BinOp::Lt,
                    BinOp::Ge => BinOp::Le,
                    other => other.clone(),
                };
                (flipped, *c)
            }
            _ => continue,
        };
        let Some(c) = q.consts.get(c).cloned().filter(ranged) else {
            continue;
        };
        let (lo, hi) = match op {
            BinOp::Gt => (Bound::Excluded(c), Bound::Unbounded),
            BinOp::Ge => (Bound::Included(c), Bound::Unbounded),
            BinOp::Lt => (Bound::Unbounded, Bound::Excluded(c)),
            BinOp::Le => (Bound::Unbounded, Bound::Included(c)),
            BinOp::Eq => (Bound::Included(c.clone()), Bound::Included(c)),
            _ => continue,
        };
        scan.lo = tighter(scan.lo, lo, false);
        scan.hi = tighter(scan.hi, hi, true);
    }
    scan
}

/// Answers `req` from `db`, as of the tick it asks: the query's rules run as the node `me` at that tick (the node's
/// program `node`, its node names `names`, the host functions `externs`, the instant `now` the query runs at).
#[allow(clippy::too_many_arguments)]
pub fn answer(
    req: QueryRequest,
    db: &Database,
    me: NodeId,
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
    let tick = match (req.as_of, applied) {
        (Some(t), _) => t,
        (None, Some(a)) => a,
        // Nothing applied yet: the database is empty as of its floor.
        (None, None) => floor,
    };
    if tick < floor || applied.map_or(tick != floor, |a| tick > a) {
        return Err(RuntimeError::Config(match applied {
            Some(a) => format!("tick {tick} is outside the database's history (ticks {floor} to {a})"),
            None => format!("tick {tick} is outside the database's history (no tick yet)"),
        }));
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
        let scan = scan_of(&program, qid);
        let rows = if matches!((&scan.lo, &scan.hi), (Bound::Unbounded, Bound::Unbounded)) {
            db.rows(nid, &scan.leading, tick)?
        } else {
            db.rows_range(nid, &scan.leading, scan.lo.as_ref(), scan.hi.as_ref(), tick)?
        };
        for row in rows {
            events.push((qid, row));
        }
    }
    let oracle = blossom_oracle::Oracle::with_externs(program.clone(), blossom_oracle::Limits::default(), externs)?;
    let out = oracle.tick(&TickInput {
        node: me,
        incarnation: 1,
        tick: Tick(tick),
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
