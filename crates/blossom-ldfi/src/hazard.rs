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

use blossom_base::internal_error;
use blossom_prov::{GoalId, Premise, ProvGraph, Support};
use blossom_sat::{Lit, SatOutcome, SatSolver, SolveLimits, Var, card};
use blossom_sim::{FaultSchedule, Omission};
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
    negative_support: bool,
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
}

impl<'a> Encoder<'a> {
    pub fn new(
        graph: &'a ProvGraph,
        spec: &'a FailureSpec,
        preds: &'a Preds,
        negative_support: bool,
        solver: &'a mut dyn SatSolver,
        vars: &'a mut FaultVars,
    ) -> Encoder<'a> {
        Encoder {
            graph,
            spec,
            preds,
            negative_support,
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
            Premise::Neg { logical, tick } => self.negative_support(logical, tick),
            Premise::CrashOracle { node } => {
                let last = Tick(self.spec.eot.0.saturating_sub(1));
                match node {
                    Some(n) => self.vars.k(self.solver, self.spec, n, last),
                    None => {
                        let mut options = Vec::new();
                        for n in (0..self.spec.nodes).map(NodeId) {
                            options.push(self.vars.k(self.solver, self.spec, n, last)?);
                        }
                        self.or(options)
                    }
                }
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
        if !self.negative_support {
            return Ok(Hazard::False);
        }
        if let Some(h) = self.neg_memo.get(&(logical, tick)) {
            return Ok(*h);
        }
        let sources: Vec<(u32, bool)> = self.preds.of_rel(logical).collect();
        let (h, independent) = self.tracked(|e| {
            let mut options = Vec::with_capacity(sources.len());
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

/// The seeded enumeration (ARCHITECTURE §8.4, TEST-028): for each of `goals`, the minimal fault sets that falsify it
/// according to `graph` and extend `seed`, the run's own faults. A goal the seed already falsifies (according to the
/// lineage) contributes nothing.
pub fn minimal_extensions(
    graph: &ProvGraph,
    spec: &FailureSpec,
    preds: &Preds,
    negative_support: bool,
    solver: &mut dyn SatSolver,
    seed: &FaultSchedule,
    goals: &[GoalId],
) -> Result<Vec<FaultSchedule>, LdfiError> {
    let mut vars = FaultVars::new();
    vars.cover(solver, spec, seed)?;
    let mut roots = Vec::with_capacity(goals.len());
    {
        let mut enc = Encoder::new(graph, spec, preds, negative_support, solver, &mut vars);
        for g in goals {
            roots.push(enc.hazard(*g)?);
        }
    }
    let mut out = Vec::new();
    for root in roots {
        let Hazard::Lit(l) = root else { continue };
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
            out.push(vars.schedule(seed, &model));
        }
    }
    Ok(out)
}
