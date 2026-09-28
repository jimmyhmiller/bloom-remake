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
//! `K(n, EOT-1)`. Crash variables exist only for senders of message leaves and nodes the crash oracle names (Molly's
//! crash view). The formula is monotone, so each node gets one auxiliary variable with implications in one direction
//! (Plaisted–Greenbaum); minimal models of the fault variables are exactly the Appendix-B-minimal falsifiers.
//!
//! A derivation that needs the goal it derives is not a support (derivation trees are finite): a goal met again on
//! the current path counts as already falsified, and a goal whose encoding depended on the path is not shared.

use std::collections::{BTreeMap, BTreeSet};

use blossom_base::internal_error;
use blossom_prov::{GoalId, Premise, ProvGraph, Support};
use blossom_sat::{Lit, SatOutcome, SatSolver, SolveLimits, Var, card};
use blossom_sim::{FaultSchedule, Omission};
use blossom_value::time::{NodeId, Tick};

use crate::LdfiError;
use crate::faults::{FailureSpec, canonical};
use crate::reach::Preds;

/// A hazard: constant, or a literal.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum H {
    False,
    True,
    Lit(Lit),
}

/// Encodes the hazards of one run's provenance graph.
pub struct Encoder<'a> {
    graph: &'a ProvGraph,
    spec: &'a FailureSpec,
    preds: &'a Preds,
    negative_support: bool,
    solver: Box<dyn SatSolver>,
    omission: BTreeMap<Omission, Var>,
    /// `K(n, t)` for `t` in `1..EOT`, per node.
    crash: BTreeMap<NodeId, Vec<(Tick, Var)>>,
    /// Per goal (dense by id): its memoized hazard, and its depth on the current path.
    memo: Vec<Option<H>>,
    on_path: Vec<Option<usize>>,
    depth: usize,
    neg_memo: BTreeMap<(u32, Tick), H>,
    /// The disjunction of the hazards of every goal of a relation at one tick, and at every tick up to one.
    tick_memo: BTreeMap<(u32, Tick), H>,
    prefix_memo: BTreeMap<(u32, Tick), H>,
    low: usize,
    /// The nodes the last crash budget covered.
    budget_nodes: BTreeSet<NodeId>,
    seed: FaultSchedule,
}

impl<'a> Encoder<'a> {
    /// An encoder for the hypotheses that extend `seed`, the faults of the run `graph` comes from.
    pub fn new(
        graph: &'a ProvGraph,
        spec: &'a FailureSpec,
        preds: &'a Preds,
        negative_support: bool,
        solver: Box<dyn SatSolver>,
        seed: FaultSchedule,
    ) -> Result<Encoder<'a>, LdfiError> {
        let mut e = Encoder {
            graph,
            spec,
            preds,
            negative_support,
            solver,
            omission: BTreeMap::new(),
            crash: BTreeMap::new(),
            memo: vec![None; graph.goal_count()],
            on_path: vec![None; graph.goal_count()],
            depth: 0,
            neg_memo: BTreeMap::new(),
            tick_memo: BTreeMap::new(),
            prefix_memo: BTreeMap::new(),
            low: usize::MAX,
            budget_nodes: BTreeSet::new(),
            seed,
        };
        // The seed's crashes count against the budget, so their nodes always have crash variables.
        if spec.max_crashes > 0 {
            let nodes: Vec<NodeId> = e.seed.crashes.keys().copied().collect();
            for n in nodes {
                e.crash_vars(n)?;
            }
        }
        Ok(e)
    }

    /// The minimal fault sets that falsify `goal`, each a superset of `seed` (TEST-028): the minimal models of the
    /// goal's hazard with the seed's faults assumed. Empty when the seed already falsifies the goal according to the
    /// lineage (nothing new to try) or when no admissible fault set does.
    pub fn minimal_extensions(&mut self, goal: GoalId) -> Result<Vec<FaultSchedule>, LdfiError> {
        let seed = self.seed.clone();
        let seed = &seed;
        let root = self.goal(goal)?;
        self.crash_budget()?;
        let act = match root {
            H::False => return Ok(Vec::new()),
            H::True => return Ok(Vec::new()),
            H::Lit(l) => {
                let act = self.solver.new_var().positive();
                self.solver.add_clause(&[!act, l])?;
                act
            }
        };
        let seed_lits = self.seed_lits(seed);
        let seed_vars: BTreeSet<Var> = self.implied_by_seed(seed);
        let free: Vec<Var> = self
            .fault_vars()
            .into_iter()
            .filter(|v| !seed_vars.contains(v))
            .collect();
        let limits = SolveLimits::default();
        // The seed alone already falsifies the goal: no new hypothesis comes from it.
        let mut assume = vec![act];
        assume.extend(seed_lits.iter().copied());
        assume.extend(free.iter().map(|v| v.negative()));
        if self.solve(&assume, &limits)? {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        loop {
            let mut assume = vec![act];
            assume.extend(seed_lits.iter().copied());
            if !self.solve(&assume, &limits)? {
                break;
            }
            let mut model: BTreeSet<Var> = self.true_among(&free)?;
            // Greedy shrink: the formula is monotone, so the result is a minimal model.
            let order: Vec<Var> = model.iter().copied().collect();
            for v in order {
                if !model.contains(&v) {
                    continue;
                }
                let mut assume = vec![act, v.negative()];
                assume.extend(seed_lits.iter().copied());
                assume.extend(free.iter().filter(|u| !model.contains(u)).map(|u| u.negative()));
                if self.solve(&assume, &limits)? {
                    model = self.true_among(&free)?.intersection(&model).copied().collect();
                }
            }
            if model.is_empty() {
                break;
            }
            let mut block = vec![!act];
            block.extend(model.iter().map(|v| v.negative()));
            self.solver.add_clause(&block)?;
            out.push(self.schedule(seed, &model)?);
        }
        Ok(out)
    }

    fn solve(&mut self, assume: &[Lit], limits: &SolveLimits) -> Result<bool, LdfiError> {
        match self.solver.solve(assume, limits)? {
            SatOutcome::Sat => Ok(true),
            SatOutcome::Unsat => Ok(false),
            SatOutcome::Unknown(hit) => Err(internal_error!("the SAT solver gave up ({hit:?}) without limits").into()),
        }
    }

    fn true_among(&self, vars: &[Var]) -> Result<BTreeSet<Var>, LdfiError> {
        let mut out = BTreeSet::new();
        for v in vars {
            if self.solver.value(*v)? {
                out.insert(*v);
            }
        }
        Ok(out)
    }

    fn fault_vars(&self) -> Vec<Var> {
        self.omission
            .values()
            .copied()
            .chain(self.crash.values().flatten().map(|(_, v)| *v))
            .collect()
    }

    /// The literals asserting the seed's faults that exist in this encoding.
    fn seed_lits(&self, seed: &FaultSchedule) -> Vec<Lit> {
        let mut out = Vec::new();
        for o in &seed.omissions {
            if let Some(v) = self.omission.get(o) {
                out.push(v.positive());
            }
        }
        for (n, c) in &seed.crashes {
            if let Some((_, v)) = self.crash.get(n).and_then(|ks| ks.iter().find(|(t, _)| t >= c)) {
                out.push(v.positive());
            }
        }
        out
    }

    /// Every fault variable the seed makes true (a crash makes its later crash-order variables true too).
    fn implied_by_seed(&self, seed: &FaultSchedule) -> BTreeSet<Var> {
        let mut out = BTreeSet::new();
        for o in &seed.omissions {
            if let Some(v) = self.omission.get(o) {
                out.insert(*v);
            }
        }
        for (n, c) in &seed.crashes {
            for (t, v) in self.crash.get(n).into_iter().flatten() {
                if t >= c {
                    out.insert(*v);
                }
            }
        }
        out
    }

    /// The fault schedule `seed` plus the faults of `model`.
    fn schedule(&self, seed: &FaultSchedule, model: &BTreeSet<Var>) -> Result<FaultSchedule, LdfiError> {
        let mut out = seed.clone();
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
        Ok(canonical(out))
    }

    /// The crash-order variables of `node`, created on first use with `K(n,t) -> K(n,t+1)`.
    fn crash_vars(&mut self, node: NodeId) -> Result<&[(Tick, Var)], LdfiError> {
        if !self.crash.contains_key(&node) {
            let mut vars: Vec<(Tick, Var)> = Vec::new();
            for t in self.spec.crash_ticks() {
                vars.push((t, self.solver.new_var()));
            }
            for w in vars.windows(2) {
                if let [(_, a), (_, b)] = w {
                    self.solver.add_clause(&[a.negative(), b.positive()])?;
                }
            }
            self.crash.insert(node, vars);
        }
        Ok(self.crash.get(&node).map_or(&[][..], Vec::as_slice))
    }

    fn k(&mut self, node: NodeId, t: Tick) -> Result<H, LdfiError> {
        if self.spec.max_crashes == 0 {
            return Ok(H::False);
        }
        let vars = self.crash_vars(node)?;
        Ok(vars
            .iter()
            .find(|(tick, _)| *tick == t)
            .map_or(H::False, |(_, v)| H::Lit(v.positive())))
    }

    /// Asserts that at most `max_crashes` nodes crash: a totalizer over every node's last crash-order variable,
    /// asserted again when a later goal's encoding adds crash variables for more nodes.
    fn crash_budget(&mut self) -> Result<(), LdfiError> {
        let nodes: BTreeSet<NodeId> = self.crash.keys().copied().collect();
        if nodes.is_empty() || nodes == self.budget_nodes {
            return Ok(());
        }
        let last: Vec<Lit> = self
            .crash
            .values()
            .filter_map(|ks| ks.last().map(|(_, v)| v.positive()))
            .collect();
        let k = self.spec.max_crashes;
        let outputs = card::totalizer(self.solver.as_mut(), &last, k)?;
        if let Some(over) = outputs.get(k as usize) {
            self.solver.add_clause(&[!*over])?;
        }
        self.budget_nodes = nodes;
        Ok(())
    }

    fn omission_var(&mut self, o: Omission) -> Var {
        if let Some(v) = self.omission.get(&o) {
            return *v;
        }
        let v = self.solver.new_var();
        self.omission.insert(o, v);
        v
    }

    fn and(&mut self, children: Vec<H>) -> Result<H, LdfiError> {
        if children.contains(&H::False) {
            return Ok(H::False);
        }
        let lits: Vec<Lit> = children
            .into_iter()
            .filter_map(|c| match c {
                H::Lit(l) => Some(l),
                _ => None,
            })
            .collect();
        match lits.as_slice() {
            [] => Ok(H::True),
            [one] => Ok(H::Lit(*one)),
            _ => {
                let h = self.solver.new_var().positive();
                for l in lits {
                    self.solver.add_clause(&[!h, l])?;
                }
                Ok(H::Lit(h))
            }
        }
    }

    fn or(&mut self, children: Vec<H>) -> Result<H, LdfiError> {
        if children.contains(&H::True) {
            return Ok(H::True);
        }
        let mut lits: Vec<Lit> = children
            .into_iter()
            .filter_map(|c| match c {
                H::Lit(l) => Some(l),
                _ => None,
            })
            .collect();
        lits.sort();
        lits.dedup();
        match lits.as_slice() {
            [] => Ok(H::False),
            [one] => Ok(H::Lit(*one)),
            _ => {
                let h = self.solver.new_var().positive();
                let mut clause = vec![!h];
                clause.extend(lits);
                self.solver.add_clause(&clause)?;
                Ok(H::Lit(h))
            }
        }
    }

    fn goal(&mut self, goal: GoalId) -> Result<H, LdfiError> {
        let i = goal.0 as usize;
        if let Some(Some(h)) = self.memo.get(i) {
            return Ok(*h);
        }
        if let Some(Some(depth)) = self.on_path.get(i) {
            self.low = self.low.min(*depth);
            return Ok(H::True);
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

    fn goal_body(&mut self, goal: GoalId) -> Result<H, LdfiError> {
        let graph = self.graph;
        let g = graph
            .get(goal)
            .ok_or_else(|| internal_error!("unknown goal {goal:?}"))?;
        let firings = match &g.support {
            Support::Leaf => return Ok(H::False),
            Support::Unknown => {
                return Err(internal_error!("lineage: no derivation recorded for {:?}", g.key).into());
            }
            Support::Derived(f) => f,
        };
        let mut children = Vec::with_capacity(firings.len());
        for f in firings {
            let h = self.firing(*f)?;
            if h == H::False {
                return Ok(H::False);
            }
            children.push(h);
        }
        self.and(children)
    }

    fn firing(&mut self, id: blossom_prov::FiringId) -> Result<H, LdfiError> {
        let graph = self.graph;
        let premises = &graph
            .firing(id)
            .ok_or_else(|| internal_error!("unknown firing {id:?}"))?
            .premises;
        let mut children = Vec::with_capacity(premises.len());
        for p in premises {
            let h = self.premise(*p)?;
            if h == H::True {
                return Ok(H::True);
            }
            children.push(h);
        }
        self.or(children)
    }

    fn premise(&mut self, p: Premise) -> Result<H, LdfiError> {
        match p {
            Premise::Goal(g) => self.goal(g),
            Premise::Clock { from, to, send } => {
                let mut options = Vec::with_capacity(2);
                if self.spec.omission_allowed(from, to, send) {
                    options.push(H::Lit(self.omission_var(Omission { from, to, send }).positive()));
                }
                options.push(self.k(from, send)?);
                self.or(options)
            }
            Premise::Neg { logical, tick } => self.negative_support(logical, tick),
            Premise::CrashOracle { node } => {
                let last = Tick(self.spec.eot.0.saturating_sub(1));
                match node {
                    Some(n) => self.k(n, last),
                    None => {
                        let mut options = Vec::new();
                        for n in (0..self.spec.nodes).map(NodeId) {
                            options.push(self.k(n, last)?);
                        }
                        self.or(options)
                    }
                }
            }
        }
    }

    /// Runs `f` and says whether its result is independent of the goals on the current path (so it may be
    /// memoized): a goal met again on the path counts as falsified only in this context.
    fn tracked(&mut self, f: impl FnOnce(&mut Self) -> Result<H, LdfiError>) -> Result<(H, bool), LdfiError> {
        let depth = self.depth;
        let saved = std::mem::replace(&mut self.low, usize::MAX);
        let result = f(self);
        let independent = self.low >= depth;
        self.low = saved.min(self.low);
        Ok((result?, independent))
    }

    /// Conservative negative support of a negated read of `logical` at `tick`: the facts of every relation that
    /// reaches it, at earlier ticks, or at `tick` along a purely deductive path.
    fn negative_support(&mut self, logical: u32, tick: Tick) -> Result<H, LdfiError> {
        if !self.negative_support {
            return Ok(H::False);
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
                if h == H::True {
                    return Ok(H::True);
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
    fn prefix(&mut self, src: u32, upto: Tick) -> Result<H, LdfiError> {
        if upto.0 == 0 {
            return Ok(H::False);
        }
        if let Some(h) = self.prefix_memo.get(&(src, upto)) {
            return Ok(*h);
        }
        let (h, independent) = self.tracked(|e| {
            let earlier = e.prefix(src, Tick(upto.0 - 1))?;
            if earlier == H::True {
                return Ok(H::True);
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
    fn at_tick(&mut self, src: u32, tick: Tick) -> Result<H, LdfiError> {
        if let Some(h) = self.tick_memo.get(&(src, tick)) {
            return Ok(*h);
        }
        let graph = self.graph;
        let goals = graph.goals_of(src, tick);
        let (h, independent) = self.tracked(|e| {
            let mut options = Vec::with_capacity(goals.len());
            for g in goals {
                let h = e.goal(*g)?;
                if h == H::True {
                    return Ok(H::True);
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
