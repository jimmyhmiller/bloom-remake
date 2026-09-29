//! The `.bls` backends of `corpus` (PLAN §5, tests/corpus/README.md): `oracle` (synchronous rounds on the oracle
//! against `[[expect]]`, `[[expect_send]]` and `[[expect_error]]`) and `compile` (the frontend against
//! `[[expect_diag]]`, compared exactly as a multiset). Every expectation kind of schema v1 that these backends own is
//! checked; an expectation the runner cannot check fails the case, never is skipped.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use blossom_artifact::bls::BlsArtifact;
use blossom_base::{Diagnostic, RelId, SourceDb};
use blossom_driver::bls::compile_file;
use blossom_driver::render::{is_not_implemented, render};
use blossom_front::api::{BlsError, NodeSpec};
use blossom_ir::core::LatticeCtor;
use blossom_oracle::OracleError;
use blossom_sim::bls::{BlsSim, InputEvent};
use blossom_sim::{FaultSchedule, Omission, SimError, SyncRun};
use blossom_value::time::{Duration, Instant, NodeId, Tick};
use blossom_value::value::{IntValue, LatValue};
use blossom_value::{TypeDef, TypeTable, Value};

type Pattern = Vec<Option<Value>>;

/// What the expectation checks need from a compiled case: node and relation names, and row decoding. Implemented
/// for `.bls` artifacts here and for `.ded` artifacts in `corpus`.
pub(super) trait Subject {
    fn node(&self, name: &str) -> Option<NodeId>;
    fn node_name(&self, n: NodeId) -> String;
    fn node_count(&self) -> usize;
    /// The relation holding `name`'s tuples at a node.
    fn rel(&self, name: &str) -> Result<RelId, String>;
    /// The relation whose messages carry `name`'s tuples.
    fn channel(&self, name: &str) -> Result<RelId, String>;
    /// A manifest row as an IR-ordered pattern (`None` for a column the manifest does not write).
    fn row(&self, rel: RelId, v: &toml::Value) -> Result<Pattern, String>;
}

impl Subject for BlsArtifact {
    fn node(&self, name: &str) -> Option<NodeId> {
        self.node_id(name)
    }
    fn node_name(&self, n: NodeId) -> String {
        self.nodes
            .get(n.0 as usize)
            .map_or_else(|| "?".to_owned(), |s| s.as_str().to_owned())
    }
    fn node_count(&self) -> usize {
        self.nodes.len()
    }
    fn rel(&self, name: &str) -> Result<RelId, String> {
        self.rel_named(name).ok_or_else(|| format!("no relation `{name}`"))
    }
    fn channel(&self, name: &str) -> Result<RelId, String> {
        Subject::rel(self, name)
    }
    fn row(&self, rel: RelId, v: &toml::Value) -> Result<Pattern, String> {
        row(self, rel, v)
    }
}

use super::corpus::Outcome;

/// Runs backend `backend` of the `.bls` case at `case`.
pub(super) fn run(case: &Path, m: &toml::Table, program: &str, backend: &str) -> Outcome {
    let root = case.join(program);
    let root = root.to_string_lossy().into_owned();
    match backend {
        "oracle" => oracle(&root, m, false),
        "interp" => oracle(&root, m, true),
        "compile" => compile_backend(&root, m),
        "sim" => Outcome::NotRunnable("the seeded asynchronous simulator (TEST-001) arrives with a later slice".into()),
        other => Outcome::NotRunnable(format!("the `{other}` backend arrives with a later slice")),
    }
}

/// The deployment of `[deploy] nodes`, or the default single node `n1` of the program's only role.
fn deployment(m: &toml::Table) -> Result<Vec<NodeSpec>, Outcome> {
    let Some(nodes) = m.get("deploy").and_then(|d| d.get("nodes")) else {
        return Ok(vec![NodeSpec {
            name: "n1".into(),
            role: None,
        }]);
    };
    let Some(nodes) = nodes.as_array() else {
        return Err(Outcome::Fail("[deploy] nodes is not an array".into()));
    };
    let mut out = Vec::new();
    for n in nodes {
        let Some(name) = n.get("name").and_then(toml::Value::as_str) else {
            return Err(Outcome::Fail("a [deploy] node without a name".into()));
        };
        out.push(NodeSpec {
            name: name.to_owned(),
            role: n.get("role").and_then(toml::Value::as_str).map(str::to_owned),
        });
    }
    Ok(out)
}

fn compile(root: &str, nodes: &[NodeSpec]) -> Result<BlsArtifact, Outcome> {
    let (result, sources) = compile_file(root, nodes);
    match result {
        Ok((a, _warnings)) => Ok(a),
        Err(BlsError::Rejected(d)) => {
            let text: String = d.iter().map(|x| render(x, &sources)).collect();
            if d.iter().any(is_not_implemented) {
                Err(Outcome::NotRunnable(text))
            } else {
                Err(Outcome::Fail(format!("compile error:\n{text}")))
            }
        }
        Err(e @ BlsError::Internal(_)) => Err(Outcome::Fail(e.to_string())),
    }
}

// ---------------------------------------------------------------------------------------------- values

/// The scripted faults of `[[fault]]` (PLAN §5.1): `omit` and `crash`. Other kinds arrive with a later slice.
pub(super) fn faults(a: &dyn Subject, m: &toml::Table) -> Result<FaultSchedule, Outcome> {
    let mut out = FaultSchedule::default();
    for f in m.get("fault").and_then(toml::Value::as_array).into_iter().flatten() {
        let kind = f.get("kind").and_then(toml::Value::as_str).unwrap_or("");
        let node = |k: &str| -> Result<NodeId, Outcome> {
            let name = f
                .get(k)
                .and_then(toml::Value::as_str)
                .ok_or_else(|| Outcome::Fail(format!("[[fault]] {kind} without `{k}`")))?;
            a.node(name)
                .ok_or_else(|| Outcome::Fail(format!("[[fault]] names unknown node `{name}`")))
        };
        let tick = |k: &str| -> Result<Tick, Outcome> {
            f.get(k)
                .and_then(toml::Value::as_integer)
                .and_then(|t| u64::try_from(t).ok())
                .map(Tick)
                .ok_or_else(|| Outcome::Fail(format!("[[fault]] {kind} without `{k}`")))
        };
        match kind {
            "omit" => {
                out.omissions.insert(Omission {
                    from: node("from")?,
                    to: node("to")?,
                    send: tick("send_tick")?,
                });
            }
            "crash" => {
                out.crashes.insert(node("node")?, tick("tick")?);
            }
            other => {
                return Err(Outcome::NotRunnable(format!(
                    "`{other}` faults in the synchronous harness arrive with a later slice"
                )));
            }
        }
    }
    Ok(out)
}

/// Decodes a manifest value by the column's type (PLAN §5.1).
fn value(a: &BlsArtifact, types: &TypeTable, v: &toml::Value, ty: blossom_base::TypeId) -> Result<Value, String> {
    let def = types.get(ty).ok_or("unknown type")?;
    let bad = || format!("{v} does not decode as {def:?}");
    Ok(match (v, def) {
        (toml::Value::Integer(i), TypeDef::Int(t)) => {
            Value::Int(IntValue::from_i128(*t, i128::from(*i)).ok_or_else(|| format!("{i} does not fit {}", t.name()))?)
        }
        (toml::Value::Integer(i), TypeDef::Duration) => Value::Duration(Duration::from_nanos(*i)),
        (toml::Value::Integer(i), TypeDef::Instant) => Value::Instant(Instant(*i)),
        (toml::Value::String(s), TypeDef::Node(_)) => {
            Value::Node(a.node_id(s).ok_or_else(|| format!("unknown node `{s}`"))?)
        }
        (toml::Value::String(s), TypeDef::Str) => Value::Str(s.as_str().into()),
        (toml::Value::String(s), TypeDef::Bytes) => Value::Bytes(s.as_bytes().into()),
        (toml::Value::Boolean(b), TypeDef::Bool) => Value::Bool(*b),
        (toml::Value::Array(xs), TypeDef::Tuple(ts)) => {
            if xs.len() != ts.len() {
                return Err(bad());
            }
            let mut out = Vec::new();
            for (x, t) in xs.iter().zip(ts) {
                out.push(value(a, types, x, *t)?);
            }
            Value::Tuple(out.into())
        }
        (toml::Value::Array(xs), TypeDef::Vec(t)) => {
            let mut out = Vec::new();
            for x in xs {
                out.push(value(a, types, x, *t)?);
            }
            Value::Vec(out.into())
        }
        (toml::Value::Array(xs), TypeDef::Set(t)) => {
            let mut out = BTreeSet::new();
            for x in xs {
                out.insert(value(a, types, x, *t)?);
            }
            Value::Set(Arc::new(out))
        }
        (toml::Value::Table(t), TypeDef::Option(inner)) => {
            if let Some(x) = t.get("some") {
                Value::some(value(a, types, x, *inner)?)
            } else if t.get("none").and_then(toml::Value::as_bool) == Some(true) {
                Value::none()
            } else {
                return Err(bad());
            }
        }
        (toml::Value::Table(t), TypeDef::Enum(e)) => {
            let name = t.get("variant").and_then(toml::Value::as_str).ok_or_else(bad)?;
            let var = e.variants.iter().find(|x| x.name.as_str() == name).ok_or_else(bad)?;
            let fields = t
                .get("fields")
                .and_then(toml::Value::as_array)
                .cloned()
                .unwrap_or_default();
            if fields.len() != var.payload.len() {
                return Err(bad());
            }
            let mut out = Vec::new();
            for (x, f) in fields.iter().zip(&var.payload) {
                out.push(value(a, types, x, f.ty)?);
            }
            Value::Enum {
                variant: var.number,
                fields: out.into(),
            }
        }
        (toml::Value::Table(t), TypeDef::Bytes) if t.contains_key("bytes_hex") => {
            let hex = t.get("bytes_hex").and_then(toml::Value::as_str).ok_or_else(bad)?;
            let mut out = Vec::new();
            let bytes = hex.as_bytes();
            if bytes.len() % 2 != 0 {
                return Err(bad());
            }
            for pair in bytes.chunks(2) {
                let s = std::str::from_utf8(pair).map_err(|_| bad())?;
                out.push(u8::from_str_radix(s, 16).map_err(|_| bad())?);
            }
            Value::Bytes(out.into())
        }
        // A lattice column is written as its revealed value (a set as an array, a map as `[key, value]` pairs).
        (_, TypeDef::Lattice(id)) => Value::Lattice(lattice_value(a, types, v, *id)?),
        (toml::Value::Table(t), _) if t.contains_key("blossom") => {
            return Err("`{ blossom = … }` row values arrive with a later slice".into());
        }
        _ => return Err(bad()),
    })
}

/// A lattice value from its revealed form.
fn lattice_value(
    a: &BlsArtifact,
    types: &TypeTable,
    v: &toml::Value,
    id: blossom_base::LatticeTypeId,
) -> Result<LatValue, String> {
    let def = a.program.get().lattices.get(id).ok_or("unknown lattice")?;
    let bad = || format!("{v} does not decode as the lattice {}", def.name);
    Ok(match (&def.ctor, v) {
        (LatticeCtor::Bool, toml::Value::Boolean(b)) => LatValue::Bool(*b),
        (LatticeCtor::Max(e) | LatticeCtor::Min(e) | LatticeCtor::Point(e), x) => {
            LatValue::Elem(Arc::new(value(a, types, x, *e)?))
        }
        (LatticeCtor::Set(e) | LatticeCtor::PSet(e), toml::Value::Array(xs)) => {
            let mut out = BTreeSet::new();
            for x in xs {
                out.insert(value(a, types, x, *e)?);
            }
            LatValue::Set(Arc::new(out))
        }
        (LatticeCtor::Map(k, inner), toml::Value::Array(pairs)) => {
            let mut out = std::collections::BTreeMap::new();
            for pair in pairs {
                let [key, val] = pair.as_array().map(Vec::as_slice).ok_or_else(bad)? else {
                    return Err(bad());
                };
                out.insert(value(a, types, key, *k)?, lattice_value(a, types, val, *inner)?);
            }
            LatValue::Map(Arc::new(out))
        }
        _ => return Err(bad()),
    })
}

/// A manifest row of `rel`'s declared columns, as IR-ordered values: `None` where the IR has a column the surface
/// does not (a direction-form channel's destination).
fn row(a: &BlsArtifact, rel: RelId, v: &toml::Value) -> Result<Vec<Option<Value>>, String> {
    let p = a.program.get();
    let decl = p.rels.get(rel).ok_or("unknown relation")?;
    let map = a.surface_columns(rel).ok_or("not a surface relation")?;
    let items = v.as_array().ok_or_else(|| format!("row {v} is not an array"))?;
    if items.len() != map.len() {
        return Err(format!(
            "row {v} has {} value(s) for {} column(s)",
            items.len(),
            map.len()
        ));
    }
    let mut out = vec![None; decl.schema.cols.len()];
    for (x, ir) in items.iter().zip(map) {
        let ty = decl.schema.cols.get(*ir).ok_or("column out of range")?.ty;
        let slot = out.get_mut(*ir).ok_or("column out of range")?;
        *slot = Some(value(a, &p.types, x, ty)?);
    }
    Ok(out)
}

/// A full row (every IR column given), for input events.
fn full_row(a: &BlsArtifact, rel: RelId, v: &toml::Value) -> Result<Vec<Value>, String> {
    row(a, rel, v)?
        .into_iter()
        .map(|x| x.ok_or_else(|| "a row for a relation with a hidden column".to_owned()))
        .collect()
}

/// Whether an IR row matches a pattern row (`None` matches anything).
fn matches_row(pattern: &[Option<Value>], row: &[Value]) -> bool {
    pattern.len() == row.len() && pattern.iter().zip(row).all(|(p, v)| p.as_ref().is_none_or(|p| p == v))
}

/// `k`, `a..=b`, `a..` (to the last tick), `..=b` (from tick 0), `a..b`.
pub(super) fn ticks_of(range: &toml::Value, last: u64) -> Result<Vec<u64>, String> {
    let text = match range {
        toml::Value::Integer(i) => return u64::try_from(*i).map(|t| vec![t]).map_err(|e| e.to_string()),
        toml::Value::String(s) => s.clone(),
        other => return Err(format!("bad tick range {other}")),
    };
    let num = |s: &str| s.parse::<u64>().map_err(|e| format!("bad tick range `{text}`: {e}"));
    match text.split_once("..") {
        None => Ok(vec![num(&text)?]),
        Some((lo, hi)) => {
            let lo = if lo.is_empty() { 0 } else { num(lo)? };
            let hi = if hi.is_empty() {
                last
            } else if let Some(h) = hi.strip_prefix('=') {
                num(h)?
            } else {
                num(hi)?
                    .checked_sub(1)
                    .ok_or_else(|| format!("empty tick range `{text}`"))?
            };
            Ok((lo..=hi).collect())
        }
    }
}

// ---------------------------------------------------------------------------------------------- oracle

/// Runs the case on the oracle, or (`engine`) on the engine checked against the oracle at every tick, and checks its
/// expectations on that run.
pub(super) fn oracle(root: &str, m: &toml::Table, engine: bool) -> Outcome {
    let nodes = match deployment(m) {
        Ok(n) => n,
        Err(o) => return o,
    };
    let Some(ticks) = m
        .get("run")
        .and_then(|r| r.get("ticks"))
        .and_then(toml::Value::as_integer)
    else {
        return Outcome::Fail("an oracle case needs [run] ticks".into());
    };
    let Ok(last) = u64::try_from(ticks - 1) else {
        return Outcome::Fail("[run] ticks must be at least 1".into());
    };
    let artifact = match compile(root, &nodes) {
        Ok(a) => a,
        Err(o) => return o,
    };
    let seed = m
        .get("deploy")
        .and_then(|d| d.get("seed"))
        .and_then(toml::Value::as_integer)
        .map_or(Ok(0), u64::try_from);
    let Ok(seed) = seed else {
        return Outcome::Fail("[deploy] seed must be a non-negative integer".into());
    };
    let sim = match BlsSim::new(&artifact, blossom_value::Seed::from_u64(seed)) {
        Ok(s) => s,
        Err(SimError::Unimplemented(u)) => return Outcome::NotRunnable(u.to_string()),
        Err(SimError::Load(OracleError::Unimplemented(u))) => return Outcome::NotRunnable(u.to_string()),
        Err(e) => return Outcome::Fail(e.to_string()),
    };
    let mut inputs = Vec::new();
    for x in m.get("input").and_then(toml::Value::as_array).into_iter().flatten() {
        let (Some(node), Some(tick), Some(rname), Some(rows)) = (
            x.get("node").and_then(toml::Value::as_str),
            x.get("tick").and_then(toml::Value::as_integer),
            x.get("rel").and_then(toml::Value::as_str),
            x.get("rows").and_then(toml::Value::as_array),
        ) else {
            return Outcome::Fail(format!("malformed [[input]] {x}"));
        };
        let Some(node) = artifact.node_id(node) else {
            return Outcome::Fail(format!("[[input]] names unknown node `{node}`"));
        };
        let r = match Subject::rel(&artifact, rname) {
            Ok(r) => r,
            Err(e) => return Outcome::Fail(e),
        };
        let Ok(tick) = u64::try_from(tick) else {
            return Outcome::Fail("negative input tick".into());
        };
        for rv in rows {
            match full_row(&artifact, r, rv) {
                Ok(vals) => inputs.push(InputEvent {
                    node,
                    tick: Tick(tick),
                    rel: r,
                    row: Arc::from(vals),
                }),
                Err(e) => return Outcome::Fail(format!("[[input]] {rname}: {e}")),
            }
        }
    }
    let schedule = match faults(&artifact, m) {
        Ok(f) => f,
        Err(o) => return o,
    };
    let round = Duration::from_nanos(1_000_000_000);
    let run = if engine {
        let reference = sim.run(&inputs, Tick(last), round, &schedule, false);
        if let Err(SimError::Node {
            error: OracleError::Unimplemented(u),
            ..
        }) = &reference
        {
            return Outcome::NotRunnable(format!("the oracle does not run it: {u}"));
        }
        let cfg = super::corpus_interp::engine_config(&artifact.roles, &artifact.nodes, blossom_value::Seed::from_u64(seed));
        let ev = blossom_node::EngineEvaluator::new(artifact.program.clone(), cfg);
        let mine = sim.run_on(&ev, &inputs, Tick(last), round, &schedule, false);
        if let Err(d) = super::corpus_interp::compare(&reference, &mine) {
            return Outcome::Fail(format!("the engine differs from the oracle: {d}"));
        }
        mine
    } else {
        sim.run(&inputs, Tick(last), round, &schedule, false)
    };
    let expected_errors = m
        .get("expect_error")
        .and_then(toml::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let run = match run {
        Ok(r) => {
            if !expected_errors.is_empty() {
                return Outcome::Fail("an [[expect_error]] runtime error did not happen".into());
            }
            r
        }
        Err(SimError::Node { node, tick, error }) => {
            if let OracleError::Unimplemented(u) = &error {
                return Outcome::NotRunnable(u.to_string());
            }
            let code = match &error {
                OracleError::Program { error, .. } => Some(error.code),
                _ => None,
            };
            let matched = expected_errors.iter().any(|x| {
                let want_node = x
                    .get("node")
                    .and_then(toml::Value::as_str)
                    .and_then(|n| artifact.node_id(n));
                let want_tick = x.get("tick").and_then(toml::Value::as_integer);
                x.get("code").and_then(toml::Value::as_str) == code
                    && want_node.is_none_or(|n| n == node)
                    && want_tick.is_none_or(|t| u64::try_from(t).ok() == Some(tick.0))
            });
            if matched && expected_errors.len() == 1 {
                return Outcome::Pass(format!("the expected runtime error {} happened", code.unwrap_or("?")));
            }
            return Outcome::Fail(format!("runtime error: {error} (node {}, tick {})", node.0, tick.0));
        }
        Err(SimError::Unimplemented(u)) => return Outcome::NotRunnable(u.to_string()),
        Err(e) => return Outcome::Fail(e.to_string()),
    };
    let mut bad = Vec::new();
    let mut checked = 0;
    for x in m.get("expect").and_then(toml::Value::as_array).into_iter().flatten() {
        checked += 1;
        if let Err(e) = expect(&artifact, &run, last, x) {
            bad.push(e);
        }
    }
    for x in m
        .get("expect_send")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
    {
        checked += 1;
        if let Err(e) = expect_send(&artifact, &run, x) {
            bad.push(e);
        }
    }
    if bad.is_empty() {
        Outcome::Pass(format!("{checked} expectation(s) hold"))
    } else {
        Outcome::Fail(bad.join("; "))
    }
}

fn node_rows(run: &SyncRun, t: u64, node: NodeId, rel: RelId) -> Vec<Vec<Value>> {
    run.node_tick(Tick(t), node)
        .filter(|nt| nt.ran)
        .map(|nt| nt.instance.rows(rel).map(|r| r.to_vec()).collect())
        .unwrap_or_default()
}

fn rows_of(a: &dyn Subject, rel: RelId, v: Option<&toml::Value>) -> Result<Vec<Pattern>, String> {
    let rows = v.and_then(toml::Value::as_array).ok_or("expected `rows`")?;
    rows.iter().map(|r| a.row(rel, r)).collect()
}

/// Whether the rows are exactly the patterns: every row matches one pattern and every pattern one row.
fn same_rows(have: &[Vec<Value>], want: &[Pattern]) -> bool {
    have.len() == want.len()
        && have.iter().all(|h| want.iter().any(|w| matches_row(w, h)))
        && want.iter().all(|w| have.iter().any(|h| matches_row(w, h)))
}

/// One `[[expect]]` of the four shapes.
pub(super) fn expect(a: &dyn Subject, run: &SyncRun, last: u64, x: &toml::Value) -> Result<(), String> {
    if let Some(q) = x.get("quiescent_from") {
        let q = q
            .as_integer()
            .and_then(|q| u64::try_from(q).ok())
            .ok_or("bad quiescent_from")?;
        return quiescent(a, run, q, last);
    }
    let node_name = x
        .get("node")
        .and_then(toml::Value::as_str)
        .ok_or("an [[expect]] without `node`")?;
    let rel_name = x
        .get("rel")
        .and_then(toml::Value::as_str)
        .ok_or("an [[expect]] without `rel`")?;
    let node = a.node(node_name).ok_or_else(|| format!("unknown node `{node_name}`"))?;
    let r = a.rel(rel_name)?;
    if x.get("final").and_then(toml::Value::as_bool) == Some(true) {
        let want = rows_of(a, r, x.get("rows"))?;
        // The contents at the end of the run: the last tick the node ran.
        let t = (0..=last)
            .rev()
            .find(|t| run.node_tick(Tick(*t), node).is_some_and(|nt| nt.ran))
            .ok_or("the node never ran")?;
        let have = node_rows(run, t, node, r);
        return if same_rows(&have, &want) {
            Ok(())
        } else {
            Err(format!("{rel_name}@{node_name} final: have {have:?}, want {want:?}"))
        };
    }
    if let Some(t) = x.get("tick") {
        let t = t.as_integer().and_then(|t| u64::try_from(t).ok()).ok_or("bad tick")?;
        let want = rows_of(a, r, x.get("rows"))?;
        let have = node_rows(run, t, node, r);
        return if same_rows(&have, &want) {
            Ok(())
        } else {
            Err(format!("{rel_name}@{node_name} tick {t}: have {have:?}, want {want:?}"))
        };
    }
    let want = a.row(
        r,
        x.get("row")
            .ok_or("an [[expect]] without `row`, `rows` or `quiescent_from`")?,
    )?;
    let mut any = false;
    let mut errors = Vec::new();
    for (key, present) in [("holds", true), ("absent", false)] {
        if let Some(range) = x.get(key) {
            any = true;
            for t in ticks_of(range, last)? {
                if t > last {
                    return Err(format!("{rel_name}@{node_name}: tick {t} is after the run"));
                }
                if node_rows(run, t, node, r).iter().any(|h| matches_row(&want, h)) != present {
                    errors.push(t);
                }
            }
            if !errors.is_empty() {
                return Err(format!(
                    "{rel_name}{want:?}@{node_name}: expected {} at tick(s) {errors:?}",
                    if present { "present" } else { "absent" }
                ));
            }
        }
    }
    if !any {
        return Err(format!(
            "{rel_name}@{node_name}: an [[expect]] row without `holds` or `absent`"
        ));
    }
    Ok(())
}

/// From tick `q` on, on every node, no relation differs from the previous tick and no message is in flight.
fn quiescent(a: &dyn Subject, run: &SyncRun, q: u64, last: u64) -> Result<(), String> {
    if q == 0 || q > last {
        return Err(format!("quiescent_from = {q} is outside the run 1..={last}"));
    }
    for n in 0..a.node_count() {
        let node = NodeId(u32::try_from(n).map_err(|e| e.to_string())?);
        for t in q..=last {
            let (Some(prev), Some(now)) = (run.node_tick(Tick(t - 1), node), run.node_tick(Tick(t), node)) else {
                return Err(format!("no round {t}"));
            };
            if prev.instance != now.instance {
                return Err(format!("node {} changes at tick {t}", a.node_name(node)));
            }
        }
    }
    if let Some(m) = run.messages.iter().find(|m| m.send.0 + 1 >= q) {
        return Err(format!(
            "a message is sent at tick {} (from quiescent_from - 1 on)",
            m.send.0
        ));
    }
    Ok(())
}

/// `[[expect_send]]`: from, to, channel, row; optional send tick and count.
pub(super) fn expect_send(a: &dyn Subject, run: &SyncRun, x: &toml::Value) -> Result<(), String> {
    let get = |k: &str| {
        x.get(k)
            .and_then(toml::Value::as_str)
            .ok_or_else(|| format!("[[expect_send]] without `{k}`"))
    };
    let from = a.node(get("from")?).ok_or("unknown `from` node")?;
    let to = a.node(get("to")?).ok_or("unknown `to` node")?;
    let ch_name = get("channel")?;
    let ch = a.channel(ch_name)?;
    let want = a.row(ch, x.get("row").ok_or("[[expect_send]] without `row`")?)?;
    let tick = x
        .get("tick")
        .and_then(toml::Value::as_integer)
        .and_then(|t| u64::try_from(t).ok());
    let count = run
        .messages
        .iter()
        .filter(|m| {
            m.rel == ch
                && m.from == from
                && m.to == to
                && tick.is_none_or(|t| m.send.0 == t)
                && matches_row(&want, &m.row)
        })
        .count();
    match x.get("count").and_then(toml::Value::as_integer) {
        Some(c) if usize::try_from(c).ok() != Some(count) => Err(format!(
            "{ch_name}{want:?} {}→{}: sent {count} time(s), want {c}",
            a.node_name(from),
            a.node_name(to)
        )),
        Some(_) => Ok(()),
        None if count == 0 => Err(format!(
            "{ch_name}{want:?} {}→{} was not sent{}",
            a.node_name(from),
            a.node_name(to),
            tick.map(|t| format!(" at tick {t}")).unwrap_or_default()
        )),
        None => Ok(()),
    }
}

// ---------------------------------------------------------------------------------------------- compile

/// Every diagnostic the frontend reports, compared with `[[expect_diag]]` exactly as a multiset.
fn compile_backend(root: &str, m: &toml::Table) -> Outcome {
    let nodes = match deployment(m) {
        Ok(n) => n,
        Err(o) => return o,
    };
    let (result, sources) = compile_file(root, &nodes);
    let diags: Vec<Diagnostic> = match result {
        Ok((_, warnings)) => warnings.iter().cloned().collect(),
        Err(BlsError::Rejected(d)) => d.iter().cloned().collect(),
        Err(e @ BlsError::Internal(_)) => return Outcome::Fail(e.to_string()),
    };
    if diags.iter().any(is_not_implemented) {
        let text: String = diags.iter().map(|x| render(x, &sources)).collect();
        return Outcome::NotRunnable(text);
    }
    compare_diags(&diags, &sources, m)
}

/// Every reported diagnostic against `[[expect_diag]]`, exactly as a multiset (tests/corpus/README.md, Diagnostics).
pub(super) fn compare_diags(diags: &[Diagnostic], sources: &SourceDb, m: &toml::Table) -> Outcome {
    let mut have: Vec<(String, Option<usize>, String)> = diags
        .iter()
        .map(|d| (d.code.as_str().to_owned(), line_of(d, sources), d.severity.to_string()))
        .collect();
    let want = m
        .get("expect_diag")
        .and_then(toml::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut missing = Vec::new();
    for w in &want {
        let code = w.get("code").and_then(toml::Value::as_str).unwrap_or("");
        let line = w
            .get("line")
            .and_then(toml::Value::as_integer)
            .and_then(|l| usize::try_from(l).ok());
        let severity = w.get("severity").and_then(toml::Value::as_str);
        let pos = have
            .iter()
            .position(|(c, l, s)| c == code && line.is_none_or(|x| *l == Some(x)) && severity.is_none_or(|x| s == x));
        match pos {
            Some(i) => {
                have.remove(i);
            }
            None => missing.push(format!(
                "{code}{}",
                line.map(|l| format!(" at line {l}")).unwrap_or_default()
            )),
        }
    }
    if missing.is_empty() && have.is_empty() {
        return Outcome::Pass(format!("{} diagnostic(s) as expected", want.len()));
    }
    let text: String = diags.iter().map(|x| render(x, sources)).collect();
    Outcome::Fail(format!(
        "missing {missing:?}; unexpected {:?}\n{text}",
        have.iter().map(|(c, l, _)| format!("{c}@{l:?}")).collect::<Vec<_>>()
    ))
}

fn line_of(d: &Diagnostic, sources: &SourceDb) -> Option<usize> {
    let span = d.primary?;
    sources.line_col(span.file, span.lo).ok().map(|lc| lc.line as usize)
}
