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
use crate::faults::{FailureSpec, canonical};
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
}

/// Where a relation's tuples come from.
pub enum Origin<'r> {
    /// Input events and static facts: faults create none.
    Input,
    /// Derived by rules: deductive and inductive ones, and for a channel the async rules that send to it.
    Rules {
        deductive: Vec<&'r Rule>,
        inductive: Vec<&'r Rule>,
        asynchronous: Vec<&'r Rule>,
    },
    /// A copy of a protocol relation's tuples (the spec's inputs): column 0 is the node; at a fixed tick, or at the
    /// read's own tick.
    Snapshot { protocol: RelId, tick: Option<Tick> },
    /// The crash oracle `crash(Observer, Node, Time)`.
    Crash,
    /// The crash oracle `crashed(Node)` of a Blossom spec: every node that crashed.
    Crashed,
}

type Pattern = Vec<Option<Value>>;
type PlaceKey = (Space, RelId, Loc, Tick, Pattern);

/// The fault variables of one solver: `O(from,to,send)` per allowed omission, and the crash-order variables
/// `K(n,t)` for `t` in `1..EOT` per node, created on first use.
#[derive(Debug, Default)]
pub struct FaultVars {
    omission: BTreeMap<Omission, Var>,
    crash: BTreeMap<NodeId, Vec<(Tick, Var)>>,
    /// The nodes the last crash budget covered.
    budget_nodes: BTreeSet<NodeId>,
}

impl FaultVars {
    pub fn new() -> FaultVars {
        FaultVars::default()
    }

    fn omission_var(&mut self, solver: &mut dyn SatSolver, o: Omission) -> Var {
        if let Some(v) = self.omission.get(&o) {
            return *v;
        }
        let v = solver.new_var();
        self.omission.insert(o, v);
        v
    }

    /// The crash-order variables of `node`, created on first use with `K(n,t) -> K(n,t+1)`.
    fn crash_vars(
        &mut self,
        solver: &mut dyn SatSolver,
        spec: &FailureSpec,
        node: NodeId,
    ) -> Result<&[(Tick, Var)], LdfiError> {
        let vars = match self.crash.entry(node) {
            std::collections::btree_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::btree_map::Entry::Vacant(e) => {
                let mut vars: Vec<(Tick, Var)> = Vec::new();
                for t in spec.crash_ticks() {
                    vars.push((t, solver.new_var()));
                }
                for w in vars.windows(2) {
                    if let [(_, a), (_, b)] = w {
                        solver.add_clause(&[a.negative(), b.positive()])?;
                    }
                }
                e.insert(vars)
            }
        };
        Ok(vars.as_slice())
    }

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
        let vars = self.crash_vars(solver, spec, node)?;
        Ok(vars
            .iter()
            .find(|(tick, _)| *tick == t)
            .map_or(Hazard::False, |(_, v)| Hazard::Lit(v.positive())))
    }

    /// Asserts that at most `max_crashes` nodes crash: a totalizer over every node's last crash-order variable,
    /// asserted again when crash variables exist for more nodes.
    pub fn crash_budget(&mut self, solver: &mut dyn SatSolver, spec: &FailureSpec) -> Result<(), LdfiError> {
        let nodes: BTreeSet<NodeId> = self.crash.keys().copied().collect();
        if nodes.is_empty() || nodes == self.budget_nodes {
            return Ok(());
        }
        let last: Vec<Lit> = self
            .crash
            .values()
            .filter_map(|ks| ks.last().map(|(_, v)| v.positive()))
            .collect();
        let k = spec.max_crashes;
        let outputs = card::totalizer(solver, &last, k)?;
        if let Some(over) = outputs.get(k as usize) {
            solver.add_clause(&[!*over])?;
        }
        self.budget_nodes = nodes;
        Ok(())
    }

    /// Makes sure the nodes `faults` crashes have crash variables.
    pub fn cover(
        &mut self,
        solver: &mut dyn SatSolver,
        spec: &FailureSpec,
        faults: &FaultSchedule,
    ) -> Result<(), LdfiError> {
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
            .chain(self.crash.values().flatten().map(|(_, v)| *v))
            .collect()
    }

    /// The literals asserting `faults` (those with variables here).
    pub fn lits_of(&self, faults: &FaultSchedule) -> Vec<Lit> {
        let mut out = Vec::new();
        for o in &faults.omissions {
            if let Some(v) = self.omission.get(o) {
                out.push(v.positive());
            }
        }
        for (n, c) in &faults.crashes {
            if let Some((_, v)) = self.crash.get(n).and_then(|ks| ks.iter().find(|(t, _)| t >= c)) {
                out.push(v.positive());
            }
        }
        out
    }

    /// Every fault variable `faults` makes true (a crash makes its later crash-order variables true too).
    pub fn implied_by(&self, faults: &FaultSchedule) -> BTreeSet<Var> {
        let mut out = BTreeSet::new();
        for o in &faults.omissions {
            if let Some(v) = self.omission.get(o) {
                out.insert(*v);
            }
        }
        for (n, c) in &faults.crashes {
            for (t, v) in self.crash.get(n).into_iter().flatten() {
                if t >= c {
                    out.insert(*v);
                }
            }
        }
        out
    }

    /// The fault schedule `base` plus the faults of the true variables `model`.
    pub fn schedule(&self, base: &FaultSchedule, model: &BTreeSet<Var>) -> FaultSchedule {
        let mut out = base.clone();
        for (o, v) in &self.omission {
            if model.contains(v) {
                out.omissions.insert(*o);
            }
        }
        for (n, ks) in &self.crash {
            if let Some((t, _)) = ks.iter().find(|(_, v)| model.contains(v)) {
                let entry = out.crashes.entry(*n).or_insert(*t);
                if *t < *entry {
                    *entry = *t;
                }
            }
        }
        canonical(out)
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
        } = setting;
        Encoder {
            seed_crashes: seed.crashes.clone(),
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
                for l in lits {
                    self.solver.add_clause(&[!h, l])?;
                }
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
                clause.extend(lits);
                self.solver.add_clause(&clause)?;
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
                    let v = self.vars.omission_var(self.solver, Omission { from, to, send });
                    options.push(Hazard::Lit(v.positive()));
                }
                options.push(self.vars.k(self.solver, self.spec, from, send)?);
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
            Premise::Alive { node, tick } => self.vars.k(self.solver, self.spec, node, tick),
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
            (Origin::Input | Origin::Rules { .. }, Loc::Node(n)) if self.frozen && space == Space::Protocol => {
                self.frozen_appear(rel, n, tick, pattern)?
            }
            _ => Hazard::False,
        };
        if frozen == Hazard::True {
            return Ok(Hazard::True);
        }
        let derived = self.appear_origin(origin, space, rel, loc, tick, pattern)?;
        self.or(vec![frozen, derived])
    }

    /// A crash of `node` at some tick `c <= tick` whose previous tick held a matching tuple (the frozen view).
    fn frozen_appear(
        &mut self,
        rel: RelId,
        node: NodeId,
        tick: Tick,
        pattern: &[Option<Value>],
    ) -> Result<Hazard, LdfiError> {
        let mut options = Vec::new();
        for c in self.spec.crash_ticks().filter(|c| *c <= tick) {
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
                            for r in &asynchronous {
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
            let (atom, negated) = match lit {
                Literal::Pos(a) => (a, false),
                Literal::Neg(a) => (a, true),
                _ => continue,
            };
            let pat: Pattern = atom
                .args
                .iter()
                .map(|t| match t {
                    Term::Const(c) => rules.constant(space, *c).cloned(),
                    Term::Var(v) => sigma.get(v.index()).cloned().flatten(),
                    Term::Wild => None,
                })
                .collect();
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
                Ok(self.seed_crashes.keys().any(|n| node.is_none_or(|m| m == *n)))
            }
            Origin::Snapshot { protocol, tick: at } => {
                let (node_loc, rest) = split_node(pattern);
                self.exists(Space::Protocol, protocol, node_loc, at.unwrap_or(tick), rest)
            }
            Origin::Input | Origin::Rules { .. } => {
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
                                    .all(|(v, p)| p.as_ref().is_none_or(|p| p == v))
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
            Origin::Input | Origin::Rules { .. } => {}
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
                            .all(|(v, p)| p.as_ref().is_none_or(|p| p == v))
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
pub(crate) fn shrink(solver: &mut dyn SatSolver, assume: &[Lit], free: &[Var]) -> Result<BTreeSet<Var>, LdfiError> {
    let mut model = true_among(solver, free)?;
    let order: Vec<Var> = model.iter().copied().collect();
    for v in order {
        if !model.contains(&v) {
            continue;
        }
        let mut a = assume.to_vec();
        a.push(v.negative());
        a.extend(free.iter().filter(|u| !model.contains(u)).map(|u| u.negative()));
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
    let mut vars = FaultVars::new();
    vars.cover(solver, spec, seed)?;
    let mut roots = Vec::with_capacity(targets.len());
    {
        let mut enc = Encoder::new(graph, setting, solver, &mut vars, seed);
        for t in targets {
            roots.push(enc.target(t)?);
        }
    }
    let mut out = Extensions::default();
    for root in roots {
        let l = match root {
            Hazard::False => continue,
            Hazard::True => {
                out.incomplete = true;
                continue;
            }
            Hazard::Lit(l) => l,
        };
        let act = solver.new_var().positive();
        solver.add_clause(&[!act, l])?;
        let seed_lits = vars.lits_of(seed);
        let seeded = vars.implied_by(seed);
        let free: Vec<Var> = vars.all().into_iter().filter(|v| !seeded.contains(v)).collect();
        let mut base = vec![act];
        base.extend(seed_lits.iter().copied());
        let mut only_seed = base.clone();
        only_seed.extend(free.iter().map(|v| v.negative()));
        if solve(solver, &only_seed)? {
            out.incomplete = true;
            continue;
        }
        while solve(solver, &base)? {
            let model = shrink(solver, &base, &free)?;
            if model.is_empty() {
                break;
            }
            let mut block = vec![!act];
            block.extend(model.iter().map(|v| v.negative()));
            solver.add_clause(&block)?;
            out.hypotheses.push(vars.schedule(seed, &model));
        }
    }
    Ok(out)
}
