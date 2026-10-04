//! The hazard encoding (ARCHITECTURE §8.3) and minimal enumeration (§8.4), under Molly's crash view.
//!
//! The hazard of a node of the provenance graph means "the fault variables make this unprovable":
//!
//! | graph element | hazard |
//! |---|---|
//! | goal | AND over its firings: every alternative derivation must fail |
//! | firing | OR over its premises: losing any premise kills the firing |
//! | message leaf `from -> to` at `s` | `O(from,to,s)` when the omission is allowed, OR `K(from,s)` |
//! | leaf (input, static fact, positive crash read) | false (CR-22) |
//! | negated read of `p` at `t` | conservative negative support (TEST-025, CR-31): OR of the hazards of the facts of every relation that reaches `p`, at ticks before `t`, or at `t` along a purely deductive path |
//! | negated crash-oracle read `notin crash(_, n, _)` | `K(n, EOT-1)`: crashing `n` at the last tick falsifies it |
//!
//! `K(n,t)` means "n crashed at or before t", so `K(n,t) -> K(n,t+1)`, and the crash budget is a totalizer over
//! `K(n, EOT-1)`. The formula is monotone, so each node gets one auxiliary variable with implications in one
//! direction (Plaisted–Greenbaum); minimal models of the fault variables are the minimal falsifiers.
//!
//! A derivation that needs the goal it derives is not a support (derivation trees are finite): a goal met again on
//! the current path counts as already falsified, and a goal whose encoding depended on the path is not shared.
//!
//! [`FaultVars`] owns the fault variables of one solver; [`Encoder`] encodes one run's graph into a solver;
//! [`minimal_extensions`] is the seeded enumeration of §8.4.

use std::collections::{BTreeMap, BTreeSet};

use blossom_base::{ConstId, InternalError, RelId, RuleId, internal_error};
use blossom_ir::core::{Expr, HeadArg, Literal, Rule, Term};
use blossom_prov::{GoalId, Loc, Premise, ProvGraph, Space, Support};
use blossom_sat::{Lit, SatOutcome, SatSolver, SolveLimits, Var, card};
use blossom_sim::{FaultSchedule, Omission};
use blossom_value::Value;
use blossom_value::time::{NodeId, Tick};

use crate::LdfiError;
use crate::faults::FailureSpec;
use crate::reach::Preds;

/// A hazard: a constant, or a literal of the solver.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Hazard {
    /// Nothing the spec allows falsifies it.
    False,
    /// Already falsified.
    True,
    Lit(Lit),
}

/// How negated reads are supported (TEST-025).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum NegSupport {
    /// Negated reads are never falsified. Unsound for non-monotone programs (it misses the 3PC and Kafka
    /// counterexamples); for experiments and Molly comparisons only.
    Off,
    /// Relation-level (CR-31): the facts of every relation that reaches the negated one, at earlier ticks or along a
    /// deductive path at the same tick.
    Conservative,
    /// Tuple-level: a negated read is falsified only if a tuple matching its pattern can appear, which needs some rule
    /// that derives such a tuple to gain a satisfying valuation: one of its positive body atoms gains a matching
    /// tuple, or a tuple blocking one of its negated atoms is lost (see [`Encoder`]).
    Precise,
}

/// How the tuples of a program's relations come about, for tuple-level negative support.
pub trait Rules {
    /// The deployment's node count.
    fn nodes(&self) -> u32;
    fn origin(&self, space: Space, rel: RelId) -> Result<Origin<'_>, InternalError>;
    fn rule(&self, space: Space, id: RuleId) -> Option<&Rule>;
    /// The source-level relation a relation implements, for relation-level support.
    fn logical(&self, space: Space, rel: RelId) -> Option<u32>;
    fn constant(&self, space: Space, id: ConstId) -> Option<&Value>;
    /// The lattice columns of a lattice-valued relation (empty for a set relation): its rows are merged per key, so a
    /// cell's row changes when a contribution is gained or lost (SEM-100).
    fn lattice_cols(&self, space: Space, rel: RelId) -> Vec<usize>;
    /// Whether a relation is durable (a restart keeps it).
    fn durable(&self, space: Space, rel: RelId) -> bool;
    /// A relation's number of columns.
    fn arity(&self, space: Space, rel: RelId) -> usize;
    /// The relations of requests to the host (a stream's `write`, `close`, `dial`, `pause`, `resume`).
    fn host_requests(&self) -> Vec<RelId>;
    /// The relations of stream events (`opened`, `data`, `closed`, `failed`).
    fn stream_events(&self) -> Vec<RelId>;
}

/// What a seeded enumeration looks for.
#[derive(Clone, Debug)]
pub enum Target {
    /// Fault sets that make a goal of the run underivable.
    Goal(GoalId),
    /// Fault sets that can make a tuple of `rel` (a spec relation, at EOT) appear: `pre` tuples the run lost
    /// together with their `post` tuple, which a larger fault set might bring back.
    Appears { rel: RelId, row: Vec<Value> },
}

/// What every encoding of one search shares: the failure spec, the relation graph, the negative-support mode, and the
/// program's rules (needed for tuple-level support).
#[derive(Clone, Copy)]
pub struct Setting<'a> {
    pub spec: &'a FailureSpec,
    pub preds: &'a Preds,
    pub neg: NegSupport,
    pub rules: Option<&'a dyn Rules>,
    /// The frozen crash view (CR-20): a crashed node keeps the state of the tick before its crash, so a crash can
    /// also make a tuple appear (one that would have been deleted), which tuple-level support accounts for.
    pub frozen: bool,
    /// A monotonic clock in nanoseconds, to time the encoding and the enumeration (diagnostics only).
    pub clock: Option<&'a (dyn Fn() -> u64 + Sync)>,
}

/// Where a relation's tuples come from.
pub enum Origin<'r> {
    /// Input events and static facts: faults create none.
    Input,
    /// Derived by rules: deductive and inductive ones, and for a channel the async rules that send to it.
    Rules {
        deductive: &'r [&'r Rule],
        inductive: &'r [&'r Rule],
        asynchronous: &'r [&'r Rule],
    },
    /// A copy of a protocol relation's tuples (the spec's inputs): column 0 is the node; at a fixed tick, or at the
    /// read's own tick.
    Snapshot { protocol: RelId, tick: Option<Tick> },
    /// The crash oracle `crash(Observer, Node, Time)`.
    Crash,
    /// The crash oracle `crashed(Node)` of a Blossom spec: every node that is down at EOT.
    Crashed,
    /// `boot()` and `recovered()`: a restart raises them, in its tick.
    Restart,
    /// A physical timer's firings: a restart starts its count again, and a guarded timer fires while its guard held
    /// at the end of the node's previous tick.
    Timer { guard: Option<RelId> },
    /// A stream event (FOREIGN-PROTOCOLS §1): faults can reset a connection (a lost message, a crash) or fail a
    /// dial, and new requests to the host (writes, closes, dials) make new events.
    Stream,
}

type Pattern = Vec<Option<Value>>;
type PlaceKey = (Space, RelId, Loc, Tick, Pattern);

/// The fault variables of one solver: `O(from,to,send)` per allowed omission, and per node its crash variables,
/// created on first use. Under crash-stop they are the crash-order variables `K(n,t)` ("crashed at or before `t`")
/// for `t` in `1..EOT`, with `K(n,t) -> K(n,t+1)`. Under crash-restart they are the crash-time variables `X(n,c)`
/// ("crashes at `c`"), at most one per node: a node is down at `t` when it crashed in the `restart` ticks up to `t`,
/// which is a disjunction of `X` variables, so the encoding stays monotone in the fault variables.
#[derive(Debug, Default)]
pub struct FaultVars {
    omission: BTreeMap<Omission, Var>,
    crash: BTreeMap<NodeId, NodeCrash>,
    /// Disjunctions of a node's crash-time variables over a tick range (crash-restart).
    ranges: BTreeMap<(NodeId, u64, u64), Hazard>,
    /// Every gate of the encoding, by its variable: a conjunction or a disjunction of literals created before it
    /// (one-directional, Plaisted–Greenbaum), for [`FaultVars::single_hitters`].
    gates: BTreeMap<Var, Gate>,
    /// The nodes the last crash budget covered, and the omission variables the last omission budget covered.
    budget_nodes: BTreeSet<NodeId>,
    budget_omissions: usize,
}

/// A gate: `v -> AND(children)` or `v -> OR(children)`.
#[derive(Debug)]
struct Gate {
    all: bool,
    children: Vec<Lit>,
}

/// One node's crash variables (`K` or `X`, by tick) and the literal that holds when it crashes at all.
#[derive(Debug)]
struct NodeCrash {
    vars: Vec<(Tick, Var)>,
    any: Lit,
}

impl FaultVars {
    pub fn new() -> FaultVars {
        FaultVars::default()
    }

    fn omission_var(&mut self, solver: &mut dyn SatSolver, o: Omission) -> Result<Var, LdfiError> {
        if let Some(v) = self.omission.get(&o) {
            return Ok(*v);
        }
        let v = solver.new_var();
        // Fault variables lean false, so a model the search finds is close to minimal (fewer shrinking calls).
        solver.prefer(v.negative())?;
        self.omission.insert(o, v);
        Ok(v)
    }

    /// The crash variables of `node`, created on first use: the `K` chain under crash-stop; under crash-restart the
    /// `X` variables, held to at most one by a ladder `S(n,c)` ("crashed at or before `c`") whose last variable
    /// counts the node for the crash budget.
    fn crash_vars(
        &mut self,
        solver: &mut dyn SatSolver,
        spec: &FailureSpec,
        node: NodeId,
    ) -> Result<&NodeCrash, LdfiError> {
        let entry = match self.crash.entry(node) {
            std::collections::btree_map::Entry::Occupied(e) => return Ok(e.into_mut()),
            std::collections::btree_map::Entry::Vacant(e) => e,
        };
        let mut vars: Vec<(Tick, Var)> = Vec::new();
        for t in spec.crash_ticks() {
            let v = solver.new_var();
            solver.prefer(v.negative())?;
            vars.push((t, v));
        }
        let any = if spec.restart.is_none() {
            for w in vars.windows(2) {
                if let [(_, a), (_, b)] = w {
                    solver.add_clause(&[a.negative(), b.positive()])?;
                }
            }
            vars.last().map(|(_, v)| v.positive())
        } else {
            let mut prev: Option<Var> = None;
            for (_, x) in &vars {
                let ladder = solver.new_var();
                solver.add_clause(&[x.negative(), ladder.positive()])?;
                if let Some(p) = prev {
                    solver.add_clause(&[p.negative(), ladder.positive()])?;
                    solver.add_clause(&[p.negative(), x.negative()])?;
                }
                prev = Some(ladder);
            }
            prev.map(|v| v.positive())
        };
        let any = any.ok_or_else(|| internal_error!("a run too short for any crash has crash variables"))?;
        Ok(entry.insert(NodeCrash { vars, any }))
    }

    /// `K(n,t)` (crash-stop): `node` crashed at or before `t`.
    fn k(
        &mut self,
        solver: &mut dyn SatSolver,
        spec: &FailureSpec,
        node: NodeId,
        t: Tick,
    ) -> Result<Hazard, LdfiError> {
        if spec.max_crashes == 0 {
            return Ok(Hazard::False);
        }
        let vars = &self.crash_vars(solver, spec, node)?.vars;
        Ok(vars
            .iter()
            .find(|(tick, _)| *tick == t)
            .map_or(Hazard::False, |(_, v)| Hazard::Lit(v.positive())))
    }

    /// Under crash-restart: `node` crashes at some tick of `lo..=hi`.
    fn crash_in(
        &mut self,
        solver: &mut dyn SatSolver,
        spec: &FailureSpec,
        node: NodeId,
        lo: u64,
        hi: u64,
    ) -> Result<Hazard, LdfiError> {
        if spec.max_crashes == 0 {
            return Ok(Hazard::False);
        }
        let lo = lo.max(1);
        let hi = hi.min(spec.eot.0.saturating_sub(1));
        if lo > hi {
            return Ok(Hazard::False);
        }
        if let Some(h) = self.ranges.get(&(node, lo, hi)) {
            return Ok(*h);
        }
        let lits: Vec<Lit> = self
            .crash_vars(solver, spec, node)?
            .vars
            .iter()
            .filter(|(t, _)| (lo..=hi).contains(&t.0))
            .map(|(_, v)| v.positive())
            .collect();
        let h = match lits.as_slice() {
            [] => Hazard::False,
            [one] => Hazard::Lit(*one),
            _ => {
                let h = solver.new_var().positive();
                let mut clause = vec![!h];
                clause.extend(lits.iter().copied());
                solver.add_clause(&clause)?;
                self.gates.insert(
                    h.var(),
                    Gate {
                        all: false,
                        children: lits,
                    },
                );
                Hazard::Lit(h)
            }
        };
        self.ranges.insert((node, lo, hi), h);
        Ok(h)
    }

    /// `node` is down at `t`: crashed at or before it, and (crash-restart) not restarted since.
    pub fn down(
        &mut self,
        solver: &mut dyn SatSolver,
        spec: &FailureSpec,
        node: NodeId,
        t: Tick,
    ) -> Result<Hazard, LdfiError> {
        match spec.restart {
            None => self.k(solver, spec, node, t),
            Some(d) => self.crash_in(solver, spec, node, (t.0 + 1).saturating_sub(d), t.0),
        }
    }

    /// `node` is down at some tick of `from..=to`.
    pub fn down_during(
        &mut self,
        solver: &mut dyn SatSolver,
        spec: &FailureSpec,
        node: NodeId,
        from: Tick,
        to: Tick,
    ) -> Result<Hazard, LdfiError> {
        match spec.restart {
            None => self.k(solver, spec, node, to),
            Some(d) => self.crash_in(solver, spec, node, (from.0 + 1).saturating_sub(d), to.0),
        }
    }

    /// `node` restarts at `t` (never under crash-stop).
    pub fn restart_at(
        &mut self,
        solver: &mut dyn SatSolver,
        spec: &FailureSpec,
        node: NodeId,
        t: Tick,
    ) -> Result<Hazard, LdfiError> {
        match spec.restart {
            Some(d) if t.0 > d => self.crash_in(solver, spec, node, t.0 - d, t.0 - d),
            _ => Ok(Hazard::False),
        }
    }

    /// `node` has restarted at or before `t` (never under crash-stop).
    pub fn restarted_by(
        &mut self,
        solver: &mut dyn SatSolver,
        spec: &FailureSpec,
        node: NodeId,
        t: Tick,
    ) -> Result<Hazard, LdfiError> {
        self.restarted_between(solver, spec, node, Tick(0), t)
    }

    /// `node` restarts at some tick after `from` and at or before `to` (never under crash-stop).
    pub fn restarted_between(
        &mut self,
        solver: &mut dyn SatSolver,
        spec: &FailureSpec,
        node: NodeId,
        from: Tick,
        to: Tick,
    ) -> Result<Hazard, LdfiError> {
        match spec.restart {
            Some(d) if to.0 > d => self.crash_in(solver, spec, node, (from.0 + 1).saturating_sub(d), to.0 - d),
            _ => Ok(Hazard::False),
        }
    }

    /// `node` crashes at `c` (under crash-stop `K(n,c)`, which a crash at `c` implies: the encoding is monotone).
    fn crash_at(
        &mut self,
        solver: &mut dyn SatSolver,
        spec: &FailureSpec,
        node: NodeId,
        c: Tick,
    ) -> Result<Hazard, LdfiError> {
        match spec.restart {
            None => self.k(solver, spec, node, c),
            Some(_) => self.crash_in(solver, spec, node, c.0, c.0),
        }
    }

    /// Asserts that at most `max_crashes` nodes crash: a totalizer over every node's crash literal, asserted again
    /// when crash variables exist for more nodes.
    pub fn crash_budget(&mut self, solver: &mut dyn SatSolver, spec: &FailureSpec) -> Result<(), LdfiError> {
        let nodes: BTreeSet<NodeId> = self.crash.keys().copied().collect();
        if nodes.is_empty() || nodes == self.budget_nodes {
            return Ok(());
        }
        let any: Vec<Lit> = self.crash.values().map(|c| c.any).collect();
        let k = spec.max_crashes;
        let outputs = card::totalizer(solver, &any, k)?;
        if let Some(over) = outputs.get(k as usize) {
            solver.add_clause(&[!*over])?;
        }
        self.budget_nodes = nodes;
        Ok(())
    }

    /// Asserts that at most `max_omissions` messages are lost (when the spec bounds them): a totalizer over every
    /// omission variable, asserted again when there are more.
    pub fn omission_budget(&mut self, solver: &mut dyn SatSolver, spec: &FailureSpec) -> Result<(), LdfiError> {
        let Some(k) = spec.max_omissions else { return Ok(()) };
        if self.omission.len() == self.budget_omissions {
            return Ok(());
        }
        let lits: Vec<Lit> = self.omission.values().map(|v| v.positive()).collect();
        let outputs = card::totalizer(solver, &lits, k)?;
        if let Some(over) = outputs.get(k as usize) {
            solver.add_clause(&[!*over])?;
        }
        self.budget_omissions = self.omission.len();
        Ok(())
    }

    /// Makes sure the nodes `faults` crashes have crash variables, and (when the spec bounds lost messages) that its
    /// omissions have variables, so the budgets count them.
    pub fn cover(
        &mut self,
        solver: &mut dyn SatSolver,
        spec: &FailureSpec,
        faults: &FaultSchedule,
    ) -> Result<(), LdfiError> {
        if spec.max_omissions.is_some() {
            for o in &faults.omissions {
                self.omission_var(solver, *o)?;
            }
        }
        if spec.max_crashes == 0 {
            return Ok(());
        }
        for n in faults.crashes.keys() {
            self.crash_vars(solver, spec, *n)?;
        }
        Ok(())
    }

    /// Every fault variable.
    pub fn all(&self) -> Vec<Var> {
        self.omission
            .values()
            .copied()
            .chain(self.crash.values().flat_map(|c| c.vars.iter().map(|(_, v)| *v)))
            .collect()
    }

    /// The literals asserting `faults` (those with variables here).
    pub fn lits_of(&self, spec: &FailureSpec, faults: &FaultSchedule) -> Vec<Lit> {
        let mut out = Vec::new();
        for o in &faults.omissions {
            if let Some(v) = self.omission.get(o) {
                out.push(v.positive());
            }
        }
        for (n, c) in &faults.crashes {
            let vars = self.crash.get(n).map(|nc| nc.vars.as_slice()).unwrap_or_default();
            let found = match spec.restart {
                None => vars.iter().find(|(t, _)| t >= c),
                Some(_) => vars.iter().find(|(t, _)| t == c),
            };
            if let Some((_, v)) = found {
                out.push(v.positive());
            }
        }
        out
    }

    /// Every fault variable `faults` makes true (under crash-stop a crash makes its later crash-order variables true
    /// too).
    pub fn implied_by(&self, spec: &FailureSpec, faults: &FaultSchedule) -> BTreeSet<Var> {
        let mut out = BTreeSet::new();
        for o in &faults.omissions {
            if let Some(v) = self.omission.get(o) {
                out.insert(*v);
            }
        }
        for (n, c) in &faults.crashes {
            for (t, v) in self.crash.get(n).into_iter().flat_map(|nc| nc.vars.iter()) {
                let implied = match spec.restart {
                    None => t >= c,
                    Some(_) => t == c,
                };
                if implied {
                    out.insert(*v);
                }
            }
        }
        out
    }

    /// The single faults that, added to `seed`, make `root` hold: the minimal models of size one, computed from the
    /// gates (a monotone circuit over the fault variables) without the solver. Each gate's set is the faults that make
    /// it hold alone (with the seed): the intersection of its children's for a conjunction, the union for a
    /// disjunction; a fault variable holds by itself, or (a crash-order variable `K(n,t)`) by a crash at or before `t`;
    /// a variable of the seed holds whatever is added. A fault is a candidate only if the spec admits it with the seed
    /// (a node crashes once under crash-restart; the crash budget). `None` when the circuit has a negative literal.
    pub fn single_hitters(&self, spec: &FailureSpec, seed: &FaultSchedule, root: Lit) -> Option<Vec<Var>> {
        let seeded = self.implied_by(spec, seed);
        let budget_left = (seed.crashes.len() as u32) < spec.max_crashes;
        let omissions_left = spec
            .max_omissions
            .is_none_or(|k| u32::try_from(seed.omissions.len()).is_ok_and(|n| n < k));
        // The candidate faults, by index.
        let mut atoms: Vec<Var> = self
            .omission
            .values()
            .copied()
            .filter(|v| omissions_left && !seeded.contains(v))
            .collect();
        for (n, nc) in &self.crash {
            let allowed = match (spec.restart, seed.crashes.contains_key(n)) {
                (Some(_), true) => false,
                (_, false) => budget_left,
                (None, true) => true,
            };
            if allowed {
                atoms.extend(nc.vars.iter().map(|(_, v)| *v).filter(|v| !seeded.contains(v)));
            }
        }
        let words = atoms.len().div_ceil(64).max(1);
        let index: BTreeMap<Var, usize> = atoms.iter().enumerate().map(|(i, v)| (*v, i)).collect();
        let full = {
            let mut b = vec![u64::MAX; words];
            if let Some(last) = b.last_mut()
                && !atoms.len().is_multiple_of(64)
            {
                *last = (1u64 << (atoms.len() % 64)) - 1;
            }
            b
        };
        let single = |i: usize| {
            let mut b = vec![0u64; words];
            if let Some(w) = b.get_mut(i / 64) {
                *w |= 1u64 << (i % 64);
            }
            b
        };
        // The crash-order variables under crash-stop: `K(n,t)` holds after a crash of `n` at any `c <= t`.
        let mut chain: BTreeMap<Var, Vec<usize>> = BTreeMap::new();
        if spec.restart.is_none() {
            for nc in self.crash.values() {
                for (t, v) in &nc.vars {
                    let at_or_before: Vec<usize> = nc
                        .vars
                        .iter()
                        .filter(|(c, _)| c <= t)
                        .filter_map(|(_, w)| index.get(w).copied())
                        .collect();
                    chain.insert(*v, at_or_before);
                }
            }
        }
        let mut sets: BTreeMap<Var, Vec<u64>> = BTreeMap::new();
        let leaf = |v: Var, sets: &BTreeMap<Var, Vec<u64>>| -> Vec<u64> {
            if seeded.contains(&v) {
                return full.clone();
            }
            if let Some(s) = sets.get(&v) {
                return s.clone();
            }
            if let Some(ix) = chain.get(&v) {
                let mut b = vec![0u64; words];
                for i in ix {
                    if let Some(w) = b.get_mut(i / 64) {
                        *w |= 1u64 << (i % 64);
                    }
                }
                return b;
            }
            index.get(&v).map_or_else(|| vec![0u64; words], |i| single(*i))
        };
        // Children are created before their gate: increasing variable order is a topological order.
        for (v, gate) in &self.gates {
            let mut acc = if gate.all { full.clone() } else { vec![0u64; words] };
            for l in &gate.children {
                if l.is_negative() {
                    return None;
                }
                let c = leaf(l.var(), &sets);
                for (a, b) in acc.iter_mut().zip(&c) {
                    if gate.all {
                        *a &= *b;
                    } else {
                        *a |= *b;
                    }
                }
            }
            sets.insert(*v, acc);
        }
        if root.is_negative() {
            return None;
        }
        let root_set = leaf(root.var(), &sets);
        let hits = |v: &Var| {
            index
                .get(v)
                .is_some_and(|i| root_set.get(i / 64).is_some_and(|w| w & (1u64 << (i % 64)) != 0))
        };
        // Under crash-stop a crash at `c` sets every `K(n,t)` from `c`: it is a minimal model only when a crash a tick
        // later is not one too (a crash is tried at its latest useful tick).
        let later: BTreeMap<Var, Var> = if spec.restart.is_none() {
            self.crash
                .values()
                .flat_map(|nc| {
                    nc.vars.windows(2).filter_map(|w| match w {
                        [(_, v), (_, next)] => Some((*v, *next)),
                        _ => None,
                    })
                })
                .collect()
        } else {
            BTreeMap::new()
        };
        Some(
            atoms
                .iter()
                .filter(|v| hits(v) && !later.get(v).is_some_and(&hits))
                .copied()
                .collect(),
        )
    }

    /// The fault schedule `base` plus the faults of the true variables `model`.
    pub fn schedule(&self, spec: &FailureSpec, base: &FaultSchedule, model: &BTreeSet<Var>) -> FaultSchedule {
        let mut out = base.clone();
        for (o, v) in &self.omission {
            if model.contains(v) {
                out.omissions.insert(*o);
            }
        }
        for (n, nc) in &self.crash {
            if let Some((t, _)) = nc.vars.iter().find(|(_, v)| model.contains(v)) {
                let entry = out.crashes.entry(*n).or_insert(*t);
                if *t < *entry {
                    *entry = *t;
                }
            }
        }
        spec.canonical(out)
    }
}

/// Encodes the hazards of one run's provenance graph into a solver whose fault variables `vars` owns.
pub struct Encoder<'a> {
    graph: &'a ProvGraph,
    spec: &'a FailureSpec,
    preds: &'a Preds,
    neg: NegSupport,
    rules: Option<&'a dyn Rules>,
    appear_memo: BTreeMap<PlaceKey, Hazard>,
    appear_path: BTreeMap<PlaceKey, usize>,
    remove_memo: BTreeMap<PlaceKey, Hazard>,
    solver: &'a mut dyn SatSolver,
    vars: &'a mut FaultVars,
    /// Per goal (dense by id): its memoized hazard, and its depth on the current path.
    memo: Vec<Option<Hazard>>,
    on_path: Vec<Option<usize>>,
    depth: usize,
    neg_memo: BTreeMap<(u32, Tick), Hazard>,
    /// The disjunction of the hazards of every goal of a relation at one tick, and at every tick up to one.
    tick_memo: BTreeMap<(u32, Tick), Hazard>,
    prefix_memo: BTreeMap<(u32, Tick), Hazard>,
    low: usize,
    /// The run's own crashes: hypotheses extend them (a crash may only move earlier).
    seed_crashes: BTreeMap<NodeId, Tick>,
    /// The run's own omissions: what they lose the run already lost.
    seed_omissions: BTreeSet<Omission>,
    /// Whether a stream event can appear at a node and tick, and whether traffic can leave a node in a tick.
    stream_memo: BTreeMap<(NodeId, Tick), Hazard>,
    traffic_memo: BTreeMap<(NodeId, Tick), Hazard>,
    frozen: bool,
}

impl<'a> Encoder<'a> {
    /// An encoder for `graph`. Tuple-level negative support needs the program's `rules`; without them
    /// [`NegSupport::Precise`] falls back to relation-level support.
    pub fn new(
        graph: &'a ProvGraph,
        setting: Setting<'a>,
        solver: &'a mut dyn SatSolver,
        vars: &'a mut FaultVars,
        seed: &FaultSchedule,
    ) -> Encoder<'a> {
        let Setting {
            spec,
            preds,
            neg,
            rules,
            frozen,
            clock: _,
        } = setting;
        Encoder {
            seed_crashes: seed.crashes.clone(),
            seed_omissions: seed.omissions.clone(),
            stream_memo: BTreeMap::new(),
            traffic_memo: BTreeMap::new(),
            frozen,
            graph,
            spec,
            preds,
            neg,
            rules,
            appear_memo: BTreeMap::new(),
            appear_path: BTreeMap::new(),
            remove_memo: BTreeMap::new(),
            solver,
            vars,
            memo: vec![None; graph.goal_count()],
            on_path: vec![None; graph.goal_count()],
            depth: 0,
            neg_memo: BTreeMap::new(),
            tick_memo: BTreeMap::new(),
            prefix_memo: BTreeMap::new(),
            low: usize::MAX,
        }
    }

    /// The hazard of `goal`, with the crash budget asserted over every crash variable it created.
    pub fn hazard(&mut self, goal: GoalId) -> Result<Hazard, LdfiError> {
        let h = self.goal(goal)?;
        self.vars.crash_budget(self.solver, self.spec)?;
        Ok(h)
    }

    /// The hazard of a target: a goal's, or for an appearance, whether the fault variables can make the tuple
    /// appear (by the negative-support mode's rules).
    pub fn target(&mut self, target: &Target) -> Result<Hazard, LdfiError> {
        let h = match target {
            Target::Goal(g) => self.goal(*g)?,
            Target::Appears { rel, row } => {
                let pattern: Pattern = row.iter().cloned().map(Some).collect();
                self.appearance(Space::Spec, *rel, Loc::Global, self.spec.eot, &pattern)?
            }
        };
        self.vars.crash_budget(self.solver, self.spec)?;
        Ok(h)
    }

    /// Whether a tuple matching `pattern` can appear, by the negative-support mode.
    fn appearance(
        &mut self,
        space: Space,
        rel: RelId,
        loc: Loc,
        tick: Tick,
        pattern: &[Option<Value>],
    ) -> Result<Hazard, LdfiError> {
        match (self.neg, self.rules) {
            (NegSupport::Off, _) => Ok(Hazard::False),
            (NegSupport::Precise, Some(_)) => self.appear(space, rel, loc, tick, pattern),
            (NegSupport::Conservative, Some(rules)) => match rules.logical(space, rel) {
                Some(logical) => self.negative_support(logical, tick),
                None => Err(internal_error!("no source-level relation for {rel:?}").into()),
            },
            (NegSupport::Conservative | NegSupport::Precise, None) => {
                Err(internal_error!("the support of a target needs the program's rules").into())
            }
        }
    }

    /// A crash that the run does not already have, of `node` at `time` (any time when `None`): the run's own crash of
    /// `node` can only move earlier. Monotone: `K(n, k)` stands for "at `k`", which it implies.
    fn crash_appears(&mut self, node: NodeId, time: Option<Tick>) -> Result<Hazard, LdfiError> {
        if let Some(d) = self.spec.restart {
            // Crash-restart: a node crashes at most once, so the run's own crash of `node` stays as it is. With no
            // time, the crash that matters is one that leaves the node down at EOT (`crashed(n)` of a spec).
            if self.seed_crashes.contains_key(&node) {
                return Ok(Hazard::False);
            }
            let eot = self.spec.eot;
            return match time {
                Some(c) => self.vars.crash_at(self.solver, self.spec, node, c),
                None => self
                    .vars
                    .crash_in(self.solver, self.spec, node, (eot.0 + 1).saturating_sub(d), eot.0),
            };
        }
        let last = self.spec.eot.0.saturating_sub(1);
        let at_or_before = |t: u64| Tick(t.min(last));
        let bound = match (self.seed_crashes.get(&node).copied(), time) {
            (None, Some(k)) => Some(k.0),
            (None, None) => Some(last),
            (Some(c), Some(k)) => (k < c).then_some(k.0),
            (Some(c), None) => c.0.checked_sub(1),
        };
        match bound {
            Some(t) if t >= 1 => self.vars.k(self.solver, self.spec, node, at_or_before(t)),
            _ => Ok(Hazard::False),
        }
    }

    /// A crash of `node` at or before `t` that the run does not already have: under crash-stop its own crash moved
    /// earlier, under crash-restart none (a node crashes once).
    fn new_crash_by(&mut self, node: NodeId, t: Tick) -> Result<Hazard, LdfiError> {
        match (self.spec.restart, self.seed_crashes.get(&node).copied()) {
            (Some(_), Some(_)) => Ok(Hazard::False),
            (Some(_), None) => self.vars.crash_in(self.solver, self.spec, node, 1, t.0),
            (None, Some(c)) => match c.prev() {
                Some(before) if before.0 >= 1 => self.vars.k(self.solver, self.spec, node, before.min(t)),
                _ => Ok(Hazard::False),
            },
            (None, None) => self.vars.k(
                self.solver,
                self.spec,
                node,
                t.min(Tick(self.spec.eot.0.saturating_sub(1))),
            ),
        }
    }

    /// Whether faults can remove the crash tuple `crash(_, node, time)`: only by crashing `node` earlier.
    fn crash_removed(&mut self, node: NodeId, time: Tick) -> Result<Hazard, LdfiError> {
        match time.0.checked_sub(1) {
            Some(t) if t >= 1 => self.vars.k(self.solver, self.spec, node, Tick(t)),
            _ => Ok(Hazard::False),
        }
    }

    /// `crash_appears` for `node`, or for every node when `None`.
    fn crash_appears_any(&mut self, node: Option<NodeId>, time: Option<Tick>) -> Result<Hazard, LdfiError> {
        match node {
            Some(n) => self.crash_appears(n, time),
            None => {
                let mut options = Vec::new();
                for n in (0..self.spec.nodes).map(NodeId) {
                    options.push(self.crash_appears(n, time)?);
                }
                self.or(options)
            }
        }
    }

    fn and(&mut self, children: Vec<Hazard>) -> Result<Hazard, LdfiError> {
        if children.contains(&Hazard::False) {
            return Ok(Hazard::False);
        }
        let lits: Vec<Lit> = children
            .into_iter()
            .filter_map(|c| match c {
                Hazard::Lit(l) => Some(l),
                _ => None,
            })
            .collect();
        match lits.as_slice() {
            [] => Ok(Hazard::True),
            [one] => Ok(Hazard::Lit(*one)),
            _ => {
                let h = self.solver.new_var().positive();
                for l in &lits {
                    self.solver.add_clause(&[!h, *l])?;
                }
                self.vars.gates.insert(
                    h.var(),
                    Gate {
                        all: true,
                        children: lits,
                    },
                );
                Ok(Hazard::Lit(h))
            }
        }
    }

    fn or(&mut self, children: Vec<Hazard>) -> Result<Hazard, LdfiError> {
        if children.contains(&Hazard::True) {
            return Ok(Hazard::True);
        }
        let mut lits: Vec<Lit> = children
            .into_iter()
            .filter_map(|c| match c {
                Hazard::Lit(l) => Some(l),
                _ => None,
            })
            .collect();
        lits.sort();
        lits.dedup();
        match lits.as_slice() {
            [] => Ok(Hazard::False),
            [one] => Ok(Hazard::Lit(*one)),
            _ => {
                let h = self.solver.new_var().positive();
                let mut clause = vec![!h];
                clause.extend(lits.iter().copied());
                self.solver.add_clause(&clause)?;
                self.vars.gates.insert(
                    h.var(),
                    Gate {
                        all: false,
                        children: lits,
                    },
                );
                Ok(Hazard::Lit(h))
            }
        }
    }

    fn goal(&mut self, goal: GoalId) -> Result<Hazard, LdfiError> {
        let i = goal.0 as usize;
        if let Some(Some(h)) = self.memo.get(i) {
            return Ok(*h);
        }
        if let Some(Some(depth)) = self.on_path.get(i) {
            self.low = self.low.min(*depth);
            return Ok(Hazard::True);
        }
        let depth = self.depth;
        let slot = self
            .on_path
            .get_mut(i)
            .ok_or_else(|| internal_error!("unknown goal {goal:?}"))?;
        *slot = Some(depth);
        self.depth += 1;
        let saved = std::mem::replace(&mut self.low, usize::MAX);
        let result = self.goal_body(goal);
        self.depth -= 1;
        if let Some(slot) = self.on_path.get_mut(i) {
            *slot = None;
        }
        let result = result?;
        if self.low >= depth
            && let Some(slot) = self.memo.get_mut(i)
        {
            *slot = Some(result);
        }
        self.low = saved.min(self.low);
        Ok(result)
    }

    fn goal_body(&mut self, goal: GoalId) -> Result<Hazard, LdfiError> {
        let graph = self.graph;
        let g = graph
            .get(goal)
            .ok_or_else(|| internal_error!("unknown goal {goal:?}"))?;
        let firings = match &g.support {
            Support::Leaf => return Ok(Hazard::False),
            Support::Unknown => {
                return Err(internal_error!("lineage: no derivation recorded for {:?}", g.key).into());
            }
            Support::Derived(f) => f,
            // A frozen copy holds as long as the previous tick's tuple does (the crash itself only moves earlier).
            Support::Frozen(prev) => return self.goal(*prev),
        };
        let mut children = Vec::with_capacity(firings.len());
        for f in firings {
            let h = self.firing(*f)?;
            if h == Hazard::False {
                return Ok(Hazard::False);
            }
            children.push(h);
        }
        self.and(children)
    }

    fn firing(&mut self, id: blossom_prov::FiringId) -> Result<Hazard, LdfiError> {
        let graph = self.graph;
        let premises = &graph
            .firing(id)
            .ok_or_else(|| internal_error!("unknown firing {id:?}"))?
            .premises;
        let mut children = Vec::with_capacity(premises.len());
        for p in premises {
            let h = self.premise(*p)?;
            if h == Hazard::True {
                return Ok(Hazard::True);
            }
            children.push(h);
        }
        self.or(children)
    }

    fn premise(&mut self, p: Premise) -> Result<Hazard, LdfiError> {
        match p {
            Premise::Goal(g) => self.goal(g),
            Premise::Clock { from, to, send } => {
                let mut options = Vec::with_capacity(2);
                if self.spec.omission_allowed(from, to, send) {
                    let v = self.vars.omission_var(self.solver, Omission { from, to, send })?;
                    options.push(Hazard::Lit(v.positive()));
                }
                options.push(self.vars.down(self.solver, self.spec, from, send)?);
                self.or(options)
            }
            Premise::Neg(id) => {
                let graph = self.graph;
                let n = graph
                    .negated(id)
                    .ok_or_else(|| internal_error!("unknown negated read {id:?}"))?;
                match (self.neg, self.rules) {
                    (NegSupport::Off, _) => Ok(Hazard::False),
                    (NegSupport::Precise, Some(_)) => self.appear(n.space, n.rel, n.loc, n.tick, &n.pattern),
                    (NegSupport::Conservative | NegSupport::Precise, _) => self.negative_support(n.logical, n.tick),
                }
            }
            Premise::CrashAbsent { node, time } => self.crash_appears_any(node, time),
            Premise::Alive { node, tick } => self.vars.down(self.solver, self.spec, node, tick),
            Premise::Up { node, from, to } => self.vars.down_during(self.solver, self.spec, node, from, to),
            Premise::NoRestart { node, tick } => self.vars.restart_at(self.solver, self.spec, node, tick),
            Premise::NotRestarted { node, from, tick } => {
                self.vars.restarted_between(self.solver, self.spec, node, from, tick)
            }
            Premise::CrashPresent { node, time } => self.crash_removed(node, time),
            Premise::Aggregate(id) => {
                let graph = self.graph;
                let group = graph
                    .aggregate(id)
                    .ok_or_else(|| internal_error!("unknown aggregate group {id:?}"))?;
                self.contributor_joins(group)
            }
        }
    }

    /// Whether faults can make a new contributor join an aggregate group (changing its row): some body atom of the
    /// aggregate rule gains a matching tuple, or loses a blocking one, with the group's columns fixed.
    fn contributor_joins(&mut self, group: &blossom_prov::AggGroup) -> Result<Hazard, LdfiError> {
        let Some(rules) = self.rules else {
            return match self.neg {
                NegSupport::Off => Ok(Hazard::False),
                _ => Err(internal_error!("an aggregate's contributors need the program's rules").into()),
            };
        };
        let rule = rules
            .rule(group.space, group.rule)
            .ok_or_else(|| internal_error!("unknown aggregate rule {:?}", group.rule))?;
        match self.neg {
            NegSupport::Off => Ok(Hazard::False),
            NegSupport::Precise => self.rule_appear(rule, group.space, group.loc, group.tick, &group.key),
            NegSupport::Conservative => {
                let atom_loc = match group.space {
                    Space::Protocol => group.loc,
                    Space::Spec => Loc::Global,
                };
                let mut options = Vec::new();
                for lit in &rule.body.lits {
                    match lit {
                        Literal::Pos(a) => {
                            let logical = rules
                                .logical(group.space, a.rel)
                                .ok_or_else(|| internal_error!("no source-level relation for {:?}", a.rel))?;
                            options.push(self.negative_support(logical, group.tick)?);
                        }
                        Literal::Neg(a) => {
                            let open: Pattern = vec![None; a.args.len()];
                            options.push(self.remove(group.space, a.rel, atom_loc, group.tick, &open)?);
                        }
                        _ => {}
                    }
                }
                self.or(options)
            }
        }
    }

    /// Runs `f` and says whether its result is independent of the goals on the current path (so it may be
    /// memoized): a goal met again on the path counts as falsified only in this context.
    fn tracked(&mut self, f: impl FnOnce(&mut Self) -> Result<Hazard, LdfiError>) -> Result<(Hazard, bool), LdfiError> {
        let depth = self.depth;
        let saved = std::mem::replace(&mut self.low, usize::MAX);
        let result = f(self);
        let independent = self.low >= depth;
        self.low = saved.min(self.low);
        Ok((result?, independent))
    }

    /// Conservative negative support of a negated read of `logical` at `tick`: the facts of every relation that
    /// reaches it, at earlier ticks, or at `tick` along a purely deductive path.
    fn negative_support(&mut self, logical: u32, tick: Tick) -> Result<Hazard, LdfiError> {
        if let Some(h) = self.neg_memo.get(&(logical, tick)) {
            return Ok(*h);
        }
        let sources: Vec<(u32, bool)> = self.preds.of_rel(logical).collect();
        let crash_reaches = self.preds.crash_reaches(logical);
        let (h, independent) = self.tracked(|e| {
            let mut options = Vec::with_capacity(sources.len() + 1);
            // Faults add crash tuples: a relation the crash oracle reaches can gain tuples from a new crash.
            if crash_reaches {
                options.push(e.crash_appears_any(None, None)?);
            }
            for (src, deductive) in sources {
                let upto = if deductive { tick.0 } else { tick.0.saturating_sub(1) };
                let h = e.prefix(src, Tick(upto))?;
                if h == Hazard::True {
                    return Ok(Hazard::True);
                }
                options.push(h);
            }
            e.or(options)
        })?;
        if independent {
            self.neg_memo.insert((logical, tick), h);
        }
        Ok(h)
    }

    /// Tuple-level negative support: whether faults (supersets of the run's own) can make a tuple of `rel` matching
    /// `pattern` appear at `loc` and `tick`. Such a tuple has a derivation under the faults; if every positive body
    /// tuple of it held in the run and every negated body atom was absent, the run would have derived the tuple too.
    /// So one of its positive body atoms gains a matching tuple (recursively), or a tuple that blocks one of its
    /// negated atoms is lost (that tuple's hazard). A channel tuple appears only through a new send one tick earlier
    /// (a message the run sent and lost stays lost under a superset of its faults); inputs never appear. A derivation
    /// that needs its own appearance is no reason (least fixpoint): a place met again on the path contributes false.
    fn appear(
        &mut self,
        space: Space,
        rel: RelId,
        loc: Loc,
        tick: Tick,
        pattern: &[Option<Value>],
    ) -> Result<Hazard, LdfiError> {
        if loc == Loc::AnyNode {
            let nodes = self.rules.map_or(0, |r| r.nodes());
            let mut options = Vec::new();
            for n in (0..nodes).map(NodeId) {
                let h = self.appear(space, rel, Loc::Node(n), tick, pattern)?;
                if h == Hazard::True {
                    return Ok(Hazard::True);
                }
                options.push(h);
            }
            return self.or(options);
        }
        let key: PlaceKey = (space, rel, loc, tick, pattern.to_vec());
        if let Some(h) = self.appear_memo.get(&key) {
            return Ok(*h);
        }
        if let Some(depth) = self.appear_path.get(&key) {
            self.low = self.low.min(*depth);
            return Ok(Hazard::False);
        }
        let depth = self.depth;
        self.appear_path.insert(key.clone(), depth);
        self.depth += 1;
        let saved = std::mem::replace(&mut self.low, usize::MAX);
        let result = self.appear_body(space, rel, loc, tick, pattern);
        self.depth -= 1;
        self.appear_path.remove(&key);
        let result = result?;
        if self.low >= depth {
            self.appear_memo.insert(key, result);
        }
        self.low = saved.min(self.low);
        Ok(result)
    }

    fn appear_body(
        &mut self,
        space: Space,
        rel: RelId,
        loc: Loc,
        tick: Tick,
        pattern: &[Option<Value>],
    ) -> Result<Hazard, LdfiError> {
        let Some(rules) = self.rules else {
            return Err(internal_error!("tuple-level negative support without the program's rules").into());
        };
        let origin = rules.origin(space, rel)?;
        // Under the frozen crash view, a tuple a node held before some tick stays if the node crashes at that tick.
        let frozen = match (&origin, loc) {
            (
                Origin::Input | Origin::Rules { .. } | Origin::Restart | Origin::Timer { .. } | Origin::Stream,
                Loc::Node(n),
            ) if self.frozen && space == Space::Protocol => self.frozen_appear(rel, n, tick, pattern)?,
            _ => Hazard::False,
        };
        if frozen == Hazard::True {
            return Ok(Hazard::True);
        }
        // A lattice cell's row is the join of its contributions: a new row for a key appears when a contribution is
        // gained (with any value) or lost (the join shrinks).
        let lattice = rules.lattice_cols(space, rel);
        if !lattice.is_empty() {
            let key = key_pattern(pattern, &lattice);
            let gained = self.appear_origin(origin, space, rel, loc, tick, &key)?;
            if gained == Hazard::True {
                return Ok(Hazard::True);
            }
            let lost = self.remove(space, rel, loc, tick, &key)?;
            return self.or(vec![frozen, gained, lost]);
        }
        let derived = self.appear_origin(origin, space, rel, loc, tick, pattern)?;
        self.or(vec![frozen, derived])
    }

    /// A crash of `node` at some tick `c <= tick` whose previous tick held a matching tuple (the frozen view). Under
    /// crash-restart the node keeps such a tuple while it is down, and a durable one after its restart too.
    fn frozen_appear(
        &mut self,
        rel: RelId,
        node: NodeId,
        tick: Tick,
        pattern: &[Option<Value>],
    ) -> Result<Hazard, LdfiError> {
        let durable = self.rules.is_some_and(|r| r.durable(Space::Protocol, rel));
        let mut options = Vec::new();
        for c in self.spec.crash_ticks().filter(|c| *c <= tick) {
            if !durable && !self.spec.down(c, tick) {
                continue;
            }
            let Some(before) = c.prev() else { continue };
            if self.exists(Space::Protocol, rel, Loc::Node(node), before, pattern)? {
                options.push(self.crash_appears(node, Some(c))?);
            }
        }
        self.or(options)
    }

    fn appear_origin(
        &mut self,
        origin: Origin<'_>,
        space: Space,
        rel: RelId,
        loc: Loc,
        tick: Tick,
        pattern: &[Option<Value>],
    ) -> Result<Hazard, LdfiError> {
        let _ = rel;
        let Some(rules) = self.rules else {
            return Err(internal_error!("tuple-level negative support without the program's rules").into());
        };
        match origin {
            Origin::Input => Ok(Hazard::False),
            // A node crashes once: the run's own restart already raised its events.
            Origin::Restart => match loc {
                Loc::Node(n) if self.seed_crashes.contains_key(&n) => Ok(Hazard::False),
                Loc::Node(n) => self.vars.restart_at(self.solver, self.spec, n, tick),
                _ => Err(internal_error!("a restart event outside a node").into()),
            },
            Origin::Stream => match loc {
                Loc::Node(n) => self.stream_appear(n, tick),
                _ => Err(internal_error!("a stream event outside a node").into()),
            },
            Origin::Timer { guard } => {
                let Loc::Node(n) = loc else {
                    return Err(internal_error!("a timer firing outside a node").into());
                };
                let restarted = if self.seed_crashes.contains_key(&n) {
                    Hazard::False
                } else {
                    self.vars.restarted_by(self.solver, self.spec, n, tick)?
                };
                // A guarded timer fires when its guard held at the end of the previous tick: some guard tuple there.
                let guarded = match (guard, tick.prev()) {
                    (Some(g), Some(before)) => {
                        let open: Pattern = vec![None; rules.arity(Space::Protocol, g)];
                        self.appear(Space::Protocol, g, loc, before, &open)?
                    }
                    _ => Hazard::False,
                };
                self.or(vec![restarted, guarded])
            }
            Origin::Crash => {
                // A tuple crash(Observer, Node, Time) appears when that node crashes at that time.
                let (node, time) = crash_pattern(pattern)?;
                self.crash_appears_any(node, time)
            }
            // A tuple crashed(Node) appears when that node crashes, at any time.
            Origin::Crashed => self.crash_appears_any(crashed_node(pattern)?, None),
            Origin::Snapshot { protocol, tick: at } => {
                let (node_loc, rest) = split_node(pattern);
                self.appear(Space::Protocol, protocol, node_loc, at.unwrap_or(tick), rest)
            }
            Origin::Rules {
                deductive,
                inductive,
                asynchronous,
            } => {
                let mut options = Vec::new();
                for r in deductive {
                    options.push(self.rule_appear(r, space, loc, tick, pattern)?);
                    if options.last() == Some(&Hazard::True) {
                        return Ok(Hazard::True);
                    }
                }
                if let Some(earlier) = tick.prev() {
                    for r in inductive {
                        options.push(self.rule_appear(r, space, loc, earlier, pattern)?);
                        if options.last() == Some(&Hazard::True) {
                            return Ok(Hazard::True);
                        }
                    }
                    if !asynchronous.is_empty() {
                        // A channel tuple at its destination: column 0 is the destination.
                        let Loc::Node(dest) = loc else {
                            return Err(internal_error!("a channel read outside a node").into());
                        };
                        let mut sent: Pattern = pattern.to_vec();
                        match sent.first_mut() {
                            Some(slot @ None) => *slot = Some(Value::Node(dest)),
                            Some(Some(v)) if *v != Value::Node(dest) => return self.or(options),
                            _ => {}
                        }
                        for s in (0..rules.nodes()).map(NodeId) {
                            for r in asynchronous {
                                options.push(self.rule_appear(r, space, Loc::Node(s), earlier, &sent)?);
                                if options.last() == Some(&Hazard::True) {
                                    return Ok(Hazard::True);
                                }
                            }
                        }
                    }
                }
                self.or(options)
            }
        }
    }

    /// Whether faults beyond the run's own can make a stream event appear at `node` and `tick` (conservatively, any
    /// event): a message lost between `node` and another node before `tick` in a tick traffic can leave the sender
    /// (it resets a connection, or fails a dial), a crash of any node by `tick` (it resets connections, or fails a
    /// dial to it), or a new request to the host at any node before `tick`.
    fn stream_appear(&mut self, node: NodeId, tick: Tick) -> Result<Hazard, LdfiError> {
        if let Some(h) = self.stream_memo.get(&(node, tick)) {
            return Ok(*h);
        }
        let Some(rules) = self.rules else {
            return Err(internal_error!("tuple-level negative support without the program's rules").into());
        };
        let nodes = rules.nodes();
        let requests: Vec<(RelId, usize)> = rules
            .host_requests()
            .into_iter()
            .map(|r| (r, rules.arity(Space::Protocol, r)))
            .collect();
        let (h, independent) = self.tracked(|e| {
            let mut options = Vec::new();
            for send in (1..tick.0).map(Tick) {
                for other in (0..nodes).map(NodeId).filter(|m| *m != node) {
                    for (from, to) in [(node, other), (other, node)] {
                        let o = Omission { from, to, send };
                        if e.spec.omission_allowed(from, to, send) && !e.seed_omissions.contains(&o) {
                            let traffic = e.traffic(from, send)?;
                            if traffic == Hazard::False {
                                continue;
                            }
                            let v = e.vars.omission_var(e.solver, o)?;
                            let lost = e.and(vec![Hazard::Lit(v.positive()), traffic])?;
                            options.push(lost);
                        }
                    }
                }
            }
            for m in (0..nodes).map(NodeId) {
                options.push(e.new_crash_by(m, tick)?);
            }
            for before in (0..tick.0).map(Tick) {
                for (rel, arity) in &requests {
                    let open: Pattern = vec![None; *arity];
                    let h = e.appear(Space::Protocol, *rel, Loc::AnyNode, before, &open)?;
                    if h == Hazard::True {
                        return Ok(Hazard::True);
                    }
                    options.push(h);
                }
            }
            e.or(options)
        })?;
        if independent {
            self.stream_memo.insert((node, tick), h);
        }
        Ok(h)
    }

    /// Whether stream traffic can leave `node` in `tick` (so a message lost then can reset a connection): the run has
    /// it make a request to the host or take a stream event then (a retiring end tells its peer), or faults can make
    /// it make a new request then.
    fn traffic(&mut self, node: NodeId, tick: Tick) -> Result<Hazard, LdfiError> {
        if let Some(h) = self.traffic_memo.get(&(node, tick)) {
            return Ok(*h);
        }
        let Some(rules) = self.rules else {
            return Err(internal_error!("tuple-level negative support without the program's rules").into());
        };
        let requests: Vec<(RelId, usize)> = rules
            .host_requests()
            .into_iter()
            .map(|r| (r, rules.arity(Space::Protocol, r)))
            .collect();
        let held = requests
            .iter()
            .map(|(r, _)| *r)
            .chain(rules.stream_events())
            .any(|r| !self.graph.goals_at(Space::Protocol, r, Some(node), tick).is_empty());
        if held {
            self.traffic_memo.insert((node, tick), Hazard::True);
            return Ok(Hazard::True);
        }
        let (h, independent) = self.tracked(|e| {
            let mut options = Vec::new();
            for (rel, arity) in &requests {
                let open: Pattern = vec![None; *arity];
                let h = e.appear(Space::Protocol, *rel, Loc::Node(node), tick, &open)?;
                if h == Hazard::True {
                    return Ok(Hazard::True);
                }
                options.push(h);
            }
            e.or(options)
        })?;
        if independent {
            self.traffic_memo.insert((node, tick), h);
        }
        Ok(h)
    }

    /// Whether `rule`, evaluated at `loc` and `tick`, can gain a valuation that derives a head matching `pattern`.
    fn rule_appear(
        &mut self,
        rule: &Rule,
        space: Space,
        loc: Loc,
        tick: Tick,
        pattern: &[Option<Value>],
    ) -> Result<Hazard, LdfiError> {
        let Some(rules) = self.rules else {
            return Err(internal_error!("tuple-level negative support without the program's rules").into());
        };
        let mut sigma: Vec<Option<Value>> = vec![None; rule.body.vars.len()];
        let mut aggregate = false;
        for (arg, want) in rule.head.args.iter().zip(pattern) {
            match (arg, want) {
                (HeadArg::Agg(_), _) => aggregate = true,
                (HeadArg::Term(Term::Var(v)), Some(val)) => match sigma.get_mut(v.index()) {
                    Some(slot @ None) => *slot = Some(val.clone()),
                    Some(Some(bound)) if bound != val => return Ok(Hazard::False),
                    _ => {}
                },
                (HeadArg::Term(Term::Const(c)), Some(val)) if rules.constant(space, *c).is_some_and(|k| k != val) => {
                    return Ok(Hazard::False);
                }
                _ => {}
            }
        }
        // Bindings that follow from the location and from copies.
        loop {
            let mut changed = false;
            for lit in &rule.body.lits {
                let Literal::Bind {
                    pat: blossom_ir::core::Pattern::Var(v),
                    expr,
                } = lit
                else {
                    continue;
                };
                if sigma.get(v.index()).is_some_and(Option::is_some) {
                    continue;
                }
                // An ascription (an untyped constant's type) does not change the value.
                let mut expr = expr;
                while let Expr::Typed { expr: inner, .. } = expr {
                    expr = inner;
                }
                let value = match expr {
                    Expr::Scalar(blossom_ir::core::BuiltinScalar::SelfNode) => match loc {
                        Loc::Node(n) => Some(Value::Node(n)),
                        _ => None,
                    },
                    Expr::Term(Term::Const(c)) => rules.constant(space, *c).cloned(),
                    Expr::Term(Term::Var(w)) => sigma.get(w.index()).cloned().flatten(),
                    _ => None,
                };
                if let (Some(value), Some(slot)) = (value, sigma.get_mut(v.index())) {
                    *slot = Some(value);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let atom_loc = match space {
            Space::Protocol => loc,
            Space::Spec => Loc::Global,
        };
        // A positive atom with no matching tuple in the run must gain one: every new derivation needs it. Otherwise
        // some body literal must change in the enabling direction.
        let mut required = Vec::new();
        let mut options = Vec::new();
        for lit in &rule.body.lits {
            // A lookup reads a cell, which faults can grow or shrink (or make absent or present): either may enable
            // the rule, depending on how its value is used.
            if let Literal::Lookup { rel, key, .. } = lit {
                let n = rules.lattice_cols(space, *rel).len() + key.len();
                let mut pat: Pattern = key
                    .iter()
                    .map(|t| match t {
                        Term::Const(c) => rules.constant(space, *c).cloned(),
                        Term::Var(v) => sigma.get(v.index()).cloned().flatten(),
                        Term::Wild => None,
                    })
                    .collect();
                pat.resize(n, None);
                let pat = lookup_pattern(rules, space, *rel, &pat);
                options.push(self.appear(space, *rel, atom_loc, tick, &pat)?);
                options.push(self.remove(space, *rel, atom_loc, tick, &pat)?);
                continue;
            }
            let (atom, negated) = match lit {
                Literal::Pos(a) => (a, false),
                Literal::Neg(a) => (a, true),
                _ => continue,
            };
            let lattice = rules.lattice_cols(space, atom.rel);
            let pat: Pattern = atom
                .args
                .iter()
                .map(|t| match t {
                    Term::Const(c) => rules.constant(space, *c).cloned(),
                    Term::Var(v) => sigma.get(v.index()).cloned().flatten(),
                    Term::Wild => None,
                })
                .collect();
            // A lattice cell matches by its key: its value can change either way.
            let pat = if lattice.is_empty() {
                pat
            } else {
                key_pattern(&pat, &lattice)
            };
            if !lattice.is_empty() && !negated {
                if !self.exists(space, atom.rel, atom_loc, tick, &pat)? {
                    let h = self.appear(space, atom.rel, atom_loc, tick, &pat)?;
                    if h == Hazard::False {
                        return Ok(Hazard::False);
                    }
                    required.push(h);
                    continue;
                }
                options.push(self.appear(space, atom.rel, atom_loc, tick, &pat)?);
                options.push(self.remove(space, atom.rel, atom_loc, tick, &pat)?);
                continue;
            }
            if !negated && !aggregate && !self.exists(space, atom.rel, atom_loc, tick, &pat)? {
                let h = self.appear(space, atom.rel, atom_loc, tick, &pat)?;
                if h == Hazard::False {
                    return Ok(Hazard::False);
                }
                required.push(h);
                continue;
            }
            if !negated || aggregate {
                options.push(self.appear(space, atom.rel, atom_loc, tick, &pat)?);
            }
            if negated || aggregate {
                options.push(self.remove(space, atom.rel, atom_loc, tick, &pat)?);
            }
        }
        if !required.is_empty() {
            return self.and(required);
        }
        self.or(options)
    }

    /// Whether the run holds some tuple of `rel` matching `pattern` at `loc` and `tick`. Unknown places (the crash
    /// oracle's tuples, which are not goals) count as held, the conservative answer.
    fn exists(
        &self,
        space: Space,
        rel: RelId,
        loc: Loc,
        tick: Tick,
        pattern: &[Option<Value>],
    ) -> Result<bool, LdfiError> {
        let Some(rules) = self.rules else {
            return Err(internal_error!("tuple-level negative support without the program's rules").into());
        };
        match rules.origin(space, rel)? {
            Origin::Crash => {
                let (node, time) = crash_pattern(pattern)?;
                Ok(self
                    .seed_crashes
                    .iter()
                    .any(|(n, c)| node.is_none_or(|m| m == *n) && time.is_none_or(|t| t == *c)))
            }
            Origin::Crashed => {
                let node = crashed_node(pattern)?;
                let eot = self.spec.eot;
                Ok(self
                    .seed_crashes
                    .iter()
                    .any(|(n, c)| node.is_none_or(|m| m == *n) && self.spec.down(*c, eot)))
            }
            Origin::Snapshot { protocol, tick: at } => {
                let (node_loc, rest) = split_node(pattern);
                self.exists(Space::Protocol, protocol, node_loc, at.unwrap_or(tick), rest)
            }
            Origin::Input | Origin::Rules { .. } | Origin::Restart | Origin::Timer { .. } | Origin::Stream => {
                let nodes: Vec<Option<NodeId>> = match loc {
                    Loc::Node(n) => vec![Some(n)],
                    Loc::Global => vec![None],
                    Loc::AnyNode => (0..rules.nodes()).map(|n| Some(NodeId(n))).collect(),
                };
                Ok(nodes.into_iter().any(|node| {
                    self.graph.goals_at(space, rel, node, tick).iter().any(|g| {
                        self.graph.get(*g).is_some_and(|goal| {
                            goal.key.row.len() == pattern.len()
                                && goal
                                    .key
                                    .row
                                    .iter()
                                    .zip(pattern)
                                    .all(|(v, p)| p.as_ref().is_none_or(|p| matches_trace(p, v)))
                        })
                    })
                }))
            }
        }
    }

    /// Whether faults can make the run lose some tuple of `rel` matching `pattern` at `loc` and `tick`: the
    /// disjunction of those tuples' hazards.
    fn remove(
        &mut self,
        space: Space,
        rel: RelId,
        loc: Loc,
        tick: Tick,
        pattern: &[Option<Value>],
    ) -> Result<Hazard, LdfiError> {
        let Some(rules) = self.rules else {
            return Err(internal_error!("tuple-level negative support without the program's rules").into());
        };
        match rules.origin(space, rel)? {
            // A run's crash tuple goes only if a superset crashes the node earlier.
            Origin::Crash => {
                let (node, time) = crash_pattern(pattern)?;
                let present: Vec<(NodeId, Tick)> = self
                    .seed_crashes
                    .iter()
                    .filter(|(n, c)| node.is_none_or(|m| m == **n) && time.is_none_or(|t| t == **c))
                    .map(|(n, c)| (*n, *c))
                    .collect();
                let mut options = Vec::with_capacity(present.len());
                for (n, c) in present {
                    options.push(self.crash_removed(n, c)?);
                }
                return self.or(options);
            }
            Origin::Snapshot { protocol, tick: at } => {
                let (node_loc, rest) = split_node(pattern);
                return self.remove(Space::Protocol, protocol, node_loc, at.unwrap_or(tick), rest);
            }
            // A crashed node stays crashed in every superset of the run's faults.
            Origin::Crashed => return Ok(Hazard::False),
            Origin::Input | Origin::Rules { .. } | Origin::Restart | Origin::Timer { .. } | Origin::Stream => {}
        }
        if loc == Loc::AnyNode {
            let mut options = Vec::new();
            for n in (0..rules.nodes()).map(NodeId) {
                let h = self.remove(space, rel, Loc::Node(n), tick, pattern)?;
                if h == Hazard::True {
                    return Ok(Hazard::True);
                }
                options.push(h);
            }
            return self.or(options);
        }
        let key: PlaceKey = (space, rel, loc, tick, pattern.to_vec());
        if let Some(h) = self.remove_memo.get(&key) {
            return Ok(*h);
        }
        let node = match loc {
            Loc::Node(n) => Some(n),
            _ => None,
        };
        let graph = self.graph;
        let goals: Vec<GoalId> = graph
            .goals_at(space, rel, node, tick)
            .iter()
            .copied()
            .filter(|g| {
                graph.get(*g).is_some_and(|goal| {
                    goal.key.row.len() == pattern.len()
                        && goal
                            .key
                            .row
                            .iter()
                            .zip(pattern)
                            .all(|(v, p)| p.as_ref().is_none_or(|p| matches_trace(p, v)))
                })
            })
            .collect();
        let (h, independent) = self.tracked(|e| {
            let mut options = Vec::with_capacity(goals.len());
            for g in goals {
                let h = e.goal(g)?;
                if h == Hazard::True {
                    return Ok(Hazard::True);
                }
                options.push(h);
            }
            e.or(options)
        })?;
        if independent {
            self.remove_memo.insert(key, h);
        }
        Ok(h)
    }

    /// The disjunction of the hazards of every goal of `src` at ticks `1..=upto`.
    fn prefix(&mut self, src: u32, upto: Tick) -> Result<Hazard, LdfiError> {
        if upto.0 == 0 {
            return Ok(Hazard::False);
        }
        if let Some(h) = self.prefix_memo.get(&(src, upto)) {
            return Ok(*h);
        }
        let (h, independent) = self.tracked(|e| {
            let earlier = e.prefix(src, Tick(upto.0 - 1))?;
            if earlier == Hazard::True {
                return Ok(Hazard::True);
            }
            let now = e.at_tick(src, upto)?;
            e.or(vec![earlier, now])
        })?;
        if independent {
            self.prefix_memo.insert((src, upto), h);
        }
        Ok(h)
    }

    /// The disjunction of the hazards of every goal of `src` at `tick`.
    fn at_tick(&mut self, src: u32, tick: Tick) -> Result<Hazard, LdfiError> {
        if let Some(h) = self.tick_memo.get(&(src, tick)) {
            return Ok(*h);
        }
        let graph = self.graph;
        let goals = graph.goals_of(src, tick);
        let (h, independent) = self.tracked(|e| {
            let mut options = Vec::with_capacity(goals.len());
            for g in goals {
                let h = e.goal(*g)?;
                if h == Hazard::True {
                    return Ok(Hazard::True);
                }
                options.push(h);
            }
            e.or(options)
        })?;
        if independent {
            self.tick_memo.insert((src, tick), h);
        }
        Ok(h)
    }
}

/// The node column of a pattern over `crashed(Node)`.
fn crashed_node(pattern: &[Option<Value>]) -> Result<Option<NodeId>, LdfiError> {
    match pattern.first() {
        Some(Some(Value::Node(n))) => Ok(Some(*n)),
        Some(None) | None => Ok(None),
        Some(Some(other)) => Err(internal_error!("a crashed-oracle node column {other:?}").into()),
    }
}

/// The node and time columns of a pattern over `crash(Observer, Node, Time)`.
fn crash_pattern(pattern: &[Option<Value>]) -> Result<(Option<NodeId>, Option<Tick>), LdfiError> {
    let node = match pattern.get(1) {
        Some(Some(Value::Node(n))) => Some(*n),
        Some(None) | None => None,
        Some(Some(other)) => return Err(internal_error!("a crash-oracle node column {other:?}").into()),
    };
    let time = match pattern.get(2) {
        Some(Some(Value::Int(blossom_value::value::IntValue::I64(t)))) => Some(Tick(
            u64::try_from(*t).map_err(|_| internal_error!("a negative crash time {t}"))?,
        )),
        Some(None) | None => None,
        Some(Some(other)) => return Err(internal_error!("a crash-oracle time column {other:?}").into()),
    };
    Ok((node, time))
}

/// A snapshot row's location column and the rest of its pattern.
fn split_node(pattern: &[Option<Value>]) -> (Loc, &[Option<Value>]) {
    let loc = match pattern.first() {
        Some(Some(Value::Node(n))) => Loc::Node(*n),
        _ => Loc::AnyNode,
    };
    (loc, pattern.get(1..).unwrap_or(&[]))
}

pub(crate) fn solve(solver: &mut dyn SatSolver, assume: &[Lit]) -> Result<bool, LdfiError> {
    match solver.solve(assume, &SolveLimits::default())? {
        SatOutcome::Sat => Ok(true),
        SatOutcome::Unsat => Ok(false),
        SatOutcome::Unknown(hit) => Err(internal_error!("the SAT solver gave up ({hit:?}) without limits").into()),
    }
}

pub(crate) fn true_among(solver: &dyn SatSolver, vars: &[Var]) -> Result<BTreeSet<Var>, LdfiError> {
    let mut out = BTreeSet::new();
    for v in vars {
        if solver.value(*v)? {
            out.insert(*v);
        }
    }
    Ok(out)
}

/// Shrinks the current model to a minimal one under `assume` (greedy: a variable that can be dropped while a model
/// remains is dropped).
pub(crate) fn shrink(
    solver: &mut dyn SatSolver,
    assume: &[Lit],
    free: &[Var],
    solves: &mut u64,
) -> Result<BTreeSet<Var>, LdfiError> {
    let mut model = true_among(solver, free)?;
    let order: Vec<Var> = model.iter().copied().collect();
    for v in order {
        if !model.contains(&v) {
            continue;
        }
        let mut a = assume.to_vec();
        a.push(v.negative());
        a.extend(free.iter().filter(|u| !model.contains(u)).map(|u| u.negative()));
        *solves += 1;
        if solve(solver, &a)? {
            model = true_among(solver, free)?.intersection(&model).copied().collect();
        }
    }
    Ok(model)
}

/// What a seeded enumeration found.
#[derive(Debug, Default)]
pub struct Extensions {
    pub hypotheses: Vec<FaultSchedule>,
    /// Some target's hazard held already under the run's own faults (or was constant true): the encoding weakened
    /// it (a derivation met again on its own path counts as falsified), so the lineage gives no guidance for it and
    /// cannot certify the program by itself.
    pub incomplete: bool,
    /// The targets (by index) that were incomplete.
    pub incomplete_targets: Vec<usize>,
    /// The SAT calls made, and the time encoding the targets and enumerating their extensions took (with a clock).
    pub solves: u64,
    pub encode_ns: u64,
    pub enumerate_ns: u64,
}

/// The seeded enumeration (ARCHITECTURE §8.4, TEST-028): for each target, the minimal fault sets that reach it
/// according to `graph` and extend `seed`, the run's own faults.
pub fn minimal_extensions(
    graph: &ProvGraph,
    setting: Setting<'_>,
    solver: &mut dyn SatSolver,
    seed: &FaultSchedule,
    targets: &[Target],
) -> Result<Extensions, LdfiError> {
    let spec = setting.spec;
    let now = || setting.clock.map_or(0, |c| c());
    let start = now();
    let mut vars = FaultVars::new();
    vars.cover(solver, spec, seed)?;
    let mut roots = Vec::with_capacity(targets.len());
    {
        let mut enc = Encoder::new(graph, setting, solver, &mut vars, seed);
        for t in targets {
            roots.push(enc.target(t)?);
        }
    }
    vars.omission_budget(solver, spec)?;
    let encoded = now();
    let mut out = Extensions {
        encode_ns: encoded.saturating_sub(start),
        ..Extensions::default()
    };
    for (index, root) in roots.into_iter().enumerate() {
        let l = match root {
            Hazard::False => continue,
            Hazard::True => {
                out.incomplete = true;
                out.incomplete_targets.push(index);
                continue;
            }
            Hazard::Lit(l) => l,
        };
        let act = solver.new_var().positive();
        solver.add_clause(&[!act, l])?;
        let seed_lits = vars.lits_of(spec, seed);
        let seeded = vars.implied_by(spec, seed);
        let free: Vec<Var> = vars.all().into_iter().filter(|v| !seeded.contains(v)).collect();
        let mut base = vec![act];
        base.extend(seed_lits.iter().copied());
        let mut only_seed = base.clone();
        only_seed.extend(free.iter().map(|v| v.negative()));
        out.solves += 1;
        if solve(solver, &only_seed)? {
            out.incomplete = true;
            out.incomplete_targets.push(index);
            continue;
        }
        // The single faults, from the circuit; the solver then looks for larger minimal models only (a model holding
        // a single fault is not minimal unless it is that fault).
        if let Some(singles) = vars.single_hitters(spec, seed, l) {
            for v in singles {
                solver.add_clause(&[!act, v.negative()])?;
                out.hypotheses.push(vars.schedule(spec, seed, &BTreeSet::from([v])));
            }
        }
        loop {
            out.solves += 1;
            if !solve(solver, &base)? {
                break;
            }
            let model = shrink(solver, &base, &free, &mut out.solves)?;
            if model.is_empty() {
                break;
            }
            let mut block = vec![!act];
            block.extend(model.iter().map(|v| v.negative()));
            solver.add_clause(&block)?;
            out.hypotheses.push(vars.schedule(spec, seed, &model));
        }
    }
    out.enumerate_ns = now().saturating_sub(encoded);
    Ok(out)
}

/// `pattern` with the lattice columns open: a cell is identified by its key.
fn key_pattern(pattern: &[Option<Value>], lattice: &[usize]) -> Pattern {
    pattern
        .iter()
        .enumerate()
        .map(|(i, v)| if lattice.contains(&i) { None } else { v.clone() })
        .collect()
}

/// A lookup's key values placed at the relation's key columns (the columns that are not lattice columns, in order).
fn lookup_pattern(rules: &dyn Rules, space: Space, rel: RelId, key: &[Option<Value>]) -> Pattern {
    let lattice = rules.lattice_cols(space, rel);
    let n = key.len();
    let mut values = key.iter().cloned();
    (0..n)
        .map(|i| {
            if lattice.contains(&i) {
                None
            } else {
                values.next().flatten()
            }
        })
        .collect()
}

/// Whether a protocol value matches a pattern value, which a spec's trace may give as a blob's reference.
fn matches_trace(pattern: &Value, v: &Value) -> bool {
    pattern == v || (matches!(v, Value::Blob(_)) && *pattern == blossom_sim::spec::trace_value(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The single-fault minimal models the solver finds: each fault (a crash with every crash-order variable it sets)
    /// that, with the seed and nothing else, satisfies `root`, and of which no smaller variable set does.
    fn by_solver(
        solver: &mut dyn SatSolver,
        vars: &FaultVars,
        spec: &FailureSpec,
        seed: &FaultSchedule,
        root: Lit,
    ) -> BTreeSet<Var> {
        let seeded = vars.implied_by(spec, seed);
        let free: Vec<Var> = vars.all().into_iter().filter(|v| !seeded.contains(v)).collect();
        let mut models: Vec<(Var, BTreeSet<Var>)> = Vec::new();
        for v in &free {
            let with = vars.schedule(spec, seed, &BTreeSet::from([*v]));
            if !spec.admits(&with) {
                continue;
            }
            let on = vars.implied_by(spec, &with);
            let mut a = vec![root];
            a.extend(
                free.iter()
                    .map(|u| if on.contains(u) { u.positive() } else { u.negative() }),
            );
            a.extend(seeded.iter().map(|u| u.positive()));
            if solve(solver, &a).unwrap() {
                models.push((*v, on.difference(&seeded).copied().collect()));
            }
        }
        models
            .iter()
            .filter(|(_, m)| !models.iter().any(|(_, n)| n.len() < m.len() && n.is_subset(m)))
            .map(|(v, _)| *v)
            .collect()
    }

    fn gate(vars: &mut FaultVars, solver: &mut dyn SatSolver, all: bool, children: Vec<Lit>) -> Lit {
        let h = solver.new_var().positive();
        if all {
            for c in &children {
                solver.add_clause(&[!h, *c]).unwrap();
            }
        } else {
            let mut clause = vec![!h];
            clause.extend(children.iter().copied());
            solver.add_clause(&clause).unwrap();
        }
        vars.gates.insert(h.var(), Gate { all, children });
        h
    }

    #[test]
    fn single_hitters_are_the_solvers_single_fault_models() {
        let (a, b, c) = (NodeId(0), NodeId(1), NodeId(2));
        for restart in [None, Some(2)] {
            let mut spec = FailureSpec::new(8, 6, 1, 3).unwrap();
            if let Some(d) = restart {
                spec = spec.with_restart(d).unwrap();
            }
            let seeds = [FaultSchedule::default(), {
                let mut s = FaultSchedule::default();
                s.omissions.insert(Omission {
                    from: c,
                    to: a,
                    send: Tick(1),
                });
                s
            }];
            for seed in seeds {
                let mut solver = blossom_sat::select_backend("cadical-plain").unwrap();
                let solver = solver.as_mut();
                let mut vars = FaultVars::new();
                vars.cover(solver, &spec, &seed).unwrap();
                let o = |vars: &mut FaultVars, solver: &mut dyn SatSolver, from, to, send| {
                    vars.omission_var(
                        solver,
                        Omission {
                            from,
                            to,
                            send: Tick(send),
                        },
                    )
                    .unwrap()
                    .positive()
                };
                let lit = |h: Hazard| match h {
                    Hazard::Lit(l) => l,
                    other => panic!("not a literal: {other:?}"),
                };
                // (O(a,b,1) or O(a,b,2) or b down at 4) and (O(a,c,1) or O(a,b,1) or a down from 3 to 5) and
                // (O(b,c,3) or O(a,b,1) or c restarted by 7 or a down at 4).
                let o1 = o(&mut vars, solver, a, b, 1);
                let o2 = o(&mut vars, solver, a, b, 2);
                let o3 = o(&mut vars, solver, a, c, 1);
                let o4 = o(&mut vars, solver, b, c, 3);
                let down_b = lit(vars.down(solver, &spec, b, Tick(4)).unwrap());
                let span_a = lit(vars.down_during(solver, &spec, a, Tick(3), Tick(5)).unwrap());
                let down_a = lit(vars.down(solver, &spec, a, Tick(4)).unwrap());
                let mut third = vec![o4, o1, down_a];
                if let Hazard::Lit(l) = vars.restarted_by(solver, &spec, c, Tick(7)).unwrap() {
                    third.push(l);
                }
                let g1 = gate(&mut vars, solver, false, vec![o1, o2, down_b]);
                let g2 = gate(&mut vars, solver, false, vec![o3, o1, span_a]);
                let g3 = gate(&mut vars, solver, false, third);
                let root = gate(&mut vars, solver, true, vec![g1, g2, g3]);
                vars.crash_budget(solver, &spec).unwrap();
                let dp: BTreeSet<Var> = vars.single_hitters(&spec, &seed, root).unwrap().into_iter().collect();
                assert_eq!(
                    dp,
                    by_solver(solver, &vars, &spec, &seed, root),
                    "restart {restart:?}, seed {seed:?}"
                );
                assert!(!dp.is_empty(), "the example has single-fault models");
            }
        }
    }
}
