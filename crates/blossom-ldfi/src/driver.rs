//! The forward/backward loop (ARCHITECTURE §8.5, TEST-029) and falsifier enumeration (TEST-028).

use std::collections::{BTreeMap, BTreeSet};

use blossom_artifact::sim::SimArtifact;
use blossom_base::internal_error;
use blossom_oracle::Row;
use blossom_prov::{GoalId, GoalKey, ProvGraph, Space};
use blossom_sat::select_backend;
use blossom_sim::FaultSchedule;
use blossom_sim::SyncRun;
use blossom_sim::spec::{Outcome, SpecSim, is_good};

use crate::LdfiError;
use crate::faults::{FailureSpec, order_key};
use crate::lineage;
use crate::reach::Preds;

/// How to search.
#[derive(Clone, Debug)]
pub struct LdfiConfig {
    pub spec: FailureSpec,
    /// How negated reads are supported (TEST-025).
    pub negative_support: crate::hazard::NegSupport,
    /// Keep searching after the first counterexample (FindMode::All).
    pub find_all: bool,
    /// Give up without a verdict after this many runs.
    pub max_runs: u64,
    /// The SAT backend (`blossom_sat::select_backend`).
    pub sat: String,
    /// Worker threads that process upcoming hypotheses speculatively; 1 runs everything on the calling thread.
    /// Results do not depend on it.
    pub workers: usize,
    /// When the lineage-driven search exhausts `max_runs` without a verdict, decide by exhaustive certification
    /// ([`crate::certify`]) within this many states; `None` reports the budget error instead.
    pub exhaustive_fallback: Option<u64>,
}

impl LdfiConfig {
    pub fn new(spec: FailureSpec) -> LdfiConfig {
        LdfiConfig {
            spec,
            negative_support: crate::hazard::NegSupport::Precise,
            find_all: false,
            max_runs: 100_000,
            sat: "cadical-plain".into(),
            workers: 1,
            exhaustive_fallback: Some(1_000_000),
        }
    }
}

/// The verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Every hypothesis the lineage suggested ran good: the program tolerates every fault the spec allows.
    NoCounterexample,
    /// Some run violated the outcome spec.
    Counterexample,
}

/// A run LDFI made, and what it concluded.
#[derive(Clone, Debug)]
pub struct Counterexample {
    pub faults: FaultSchedule,
    pub outcome: Outcome,
    pub run: SyncRun,
    /// The failure-free `post` tuples the run lost while `pre` held them.
    pub violated: Vec<Row>,
}

/// The result of an LDFI search.
#[derive(Clone, Debug)]
pub struct LdfiReport {
    pub verdict: Verdict,
    /// How the verdict was reached.
    pub method: Method,
    pub counterexamples: Vec<Counterexample>,
    /// Concrete executions of the lineage-driven search, the failure-free run included.
    pub runs: u64,
    pub stats: SearchStats,
    pub failure_free: Outcome,
    pub failure_free_run: SyncRun,
    pub failure_free_graph: ProvGraph,
}

/// Which search reached a verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// The lineage-driven search (ARCHITECTURE §8.5): it found a counterexample, or exhausted its hypotheses.
    Lineage,
    /// Exhaustive certification decided, after the lineage-driven search gave up.
    Exhaustive {
        states: u64,
        schedules: u64,
        after: Fallback,
    },
}

/// Why the lineage-driven search handed over to exhaustive certification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fallback {
    /// It spent its run budget.
    RunBudget,
    /// It found no counterexample, but its lineage was incomplete.
    IncompleteLineage,
}

/// How a search went.
#[derive(Clone, Debug, Default)]
pub struct SearchStats {
    /// Hypotheses the lineage suggested, before deduplication against the explored set.
    pub suggested: u64,
    /// The largest number of hypotheses waiting at once.
    pub queue_peak: usize,
    /// Executed fault sets by their number of faults.
    pub by_size: BTreeMap<usize, u64>,
}

/// The read-only state every hypothesis is processed against. Processing a hypothesis is a pure function of its
/// fault set, so workers can process hypotheses in any order and on any thread.
struct Search<'a> {
    sim: &'a SpecSim<'a>,
    artifact: &'a SimArtifact,
    config: &'a LdfiConfig,
    preds: Preds,
    rules: lineage::ArtifactRules<'a>,
}

/// What processing one hypothesis found.
enum Processed {
    /// The run is good; these are the hypotheses its lineage suggests, and whether its lineage was incomplete.
    Good(BTreeSet<FaultSchedule>, bool),
    Bad(Box<Counterexample>),
}

impl<'a> Search<'a> {
    fn new(sim: &'a SpecSim<'a>, config: &'a LdfiConfig) -> Result<Search<'a>, LdfiError> {
        let artifact = sim.artifact();
        if artifact.profile.frozen() && config.negative_support == crate::hazard::NegSupport::Conservative {
            // Relation-level support does not account for the state a frozen crash preserves.
            return Err(blossom_base::unimplemented_error!(
                "TEST-025",
                "relation-level negative support under the frozen crash view (use precise)"
            )
            .into());
        }
        if artifact.spec.is_none() {
            return Err(LdfiError::NoSpec);
        }
        let nodes = u32::try_from(artifact.nodes.len()).map_err(|_| internal_error!("too many nodes"))?;
        if nodes != config.spec.nodes {
            return Err(LdfiError::Spec(format!(
                "the failure spec has {} node(s), the program was compiled for {nodes}",
                config.spec.nodes
            )));
        }
        Ok(Search {
            sim,
            artifact,
            config,
            preds: Preds::of(artifact),
            rules: lineage::ArtifactRules::new(artifact)?,
        })
    }

    /// Runs the program under `faults` and judges it.
    fn execute(&self, faults: &FaultSchedule) -> Result<(SyncRun, Outcome), LdfiError> {
        let eot = self.config.spec.eot;
        let run = self.sim.run(eot, faults, true)?;
        let outcome = self.sim.outcome(&run, eot, true)?;
        Ok((run, outcome))
    }

    fn graph(&self, run: &SyncRun, outcome: &Outcome) -> Result<ProvGraph, LdfiError> {
        Ok(lineage::build(self.artifact, run, outcome)?)
    }

    fn post_goal(&self, graph: &ProvGraph, row: &Row) -> Result<Option<GoalId>, LdfiError> {
        let post = self.artifact.spec.as_ref().map(|s| s.post).ok_or(LdfiError::NoSpec)?;
        Ok(graph.find(&GoalKey {
            space: Space::Spec,
            rel: post,
            node: None,
            tick: self.config.spec.eot,
            row: row.clone(),
        }))
    }

    /// The minimal hypotheses extending `seed` that falsify one of `goals`, or bring back one of the `revive`
    /// tuples of `pre`, according to `graph`; and whether the lineage was incomplete.
    fn hypotheses(
        &self,
        graph: &ProvGraph,
        goals: &[Row],
        revive: &[Row],
        seed: &FaultSchedule,
    ) -> Result<(BTreeSet<FaultSchedule>, bool), LdfiError> {
        let mut solver = select_backend(&self.config.sat)?;
        let mut targets = Vec::with_capacity(goals.len() + revive.len());
        for row in goals {
            if let Some(goal) = self.post_goal(graph, row)? {
                targets.push(crate::hazard::Target::Goal(goal));
            }
        }
        if !revive.is_empty() {
            let pre = self.artifact.spec.as_ref().map(|s| s.pre).ok_or(LdfiError::NoSpec)?;
            for row in revive {
                targets.push(crate::hazard::Target::Appears {
                    rel: pre,
                    row: row.to_vec(),
                });
            }
        }
        let setting = crate::hazard::Setting {
            spec: &self.config.spec,
            preds: &self.preds,
            neg: self.config.negative_support,
            rules: Some(&self.rules),
            frozen: self.artifact.profile.frozen(),
        };
        let found = crate::hazard::minimal_extensions(graph, setting, solver.as_mut(), seed, &targets)?;
        let admitted = found
            .hypotheses
            .into_iter()
            .filter(|h| self.config.spec.admits(h))
            .collect();
        Ok((admitted, found.incomplete))
    }

    /// Runs `h`, judges it against the failure-free `post`, and for a good run derives the next hypotheses.
    fn process(&self, h: &FaultSchedule, ff_post: &BTreeSet<Row>) -> Result<Processed, LdfiError> {
        let (run, outcome) = self.execute(h)?;
        if !is_good(ff_post, &outcome) {
            let violated = ff_post
                .iter()
                .filter(|g| !outcome.post.contains(*g) && outcome.pre.contains(*g))
                .cloned()
                .collect();
            return Ok(Processed::Bad(Box::new(Counterexample {
                faults: h.clone(),
                outcome,
                run,
                violated,
            })));
        }
        let graph = self.graph(&run, &outcome)?;
        let goals: Vec<Row> = outcome.post.iter().cloned().collect();
        // A failure-free `post` tuple this run lost together with its `pre` tuple: a larger fault set that brings
        // the `pre` tuple back while the `post` tuple stays lost is a counterexample.
        let revive: Vec<Row> = ff_post
            .iter()
            .filter(|g| !outcome.post.contains(*g) && !outcome.pre.contains(*g))
            .cloned()
            .collect();
        let (next, incomplete) = self.hypotheses(&graph, &goals, &revive, h)?;
        Ok(Processed::Good(next, incomplete))
    }
}

/// The hypothesis queue, ordered by [`order_key`], with an explored set.
struct Queue<'s> {
    spec: &'s FailureSpec,
    order: BTreeSet<(usize, usize, FaultSchedule)>,
    explored: BTreeSet<FaultSchedule>,
}

impl<'s> Queue<'s> {
    fn new(spec: &'s FailureSpec) -> Queue<'s> {
        let mut explored = BTreeSet::new();
        explored.insert(FaultSchedule::default());
        Queue {
            spec,
            order: BTreeSet::new(),
            explored,
        }
    }

    fn push(&mut self, hypotheses: BTreeSet<FaultSchedule>) {
        for h in hypotheses {
            if self.explored.insert(h.clone()) {
                self.order.insert(order_key(self.spec, &h));
            }
        }
    }

    fn pop(&mut self) -> Option<FaultSchedule> {
        self.order.pop_first().map(|(_, _, h)| h)
    }

    /// The next `n` hypotheses, in order, without removing them.
    fn upcoming(&self, n: usize) -> impl Iterator<Item = &FaultSchedule> {
        self.order.iter().take(n).map(|(_, _, h)| h)
    }
}

/// LDFI on a compiled `.ded` program (ARCHITECTURE §8.5): the lineage-driven search, falling back on exhaustive
/// certification (see [`LdfiConfig::exhaustive_fallback`]) when it runs out of runs, or when it finds no
/// counterexample but its lineage was incomplete, so that it cannot certify the program by itself.
pub fn run(sim: &SpecSim<'_>, config: &LdfiConfig) -> Result<LdfiReport, LdfiError> {
    let (runs, why) = match lineage_search(sim, config) {
        Err(LdfiError::RunBudget(runs)) => (runs, Fallback::RunBudget),
        Err(LdfiError::Incomplete) => (0, Fallback::IncompleteLineage),
        other => return other,
    };
    match config.exhaustive_fallback {
        Some(max_states) => certify_exhaustively(sim, config, runs, why, max_states),
        None => Err(match why {
            Fallback::RunBudget => LdfiError::RunBudget(runs),
            Fallback::IncompleteLineage => LdfiError::Incomplete,
        }),
    }
}

/// Exhaustive certification after the lineage-driven search gave up.
fn certify_exhaustively(
    sim: &SpecSim<'_>,
    config: &LdfiConfig,
    runs: u64,
    why: Fallback,
    max_states: u64,
) -> Result<LdfiReport, LdfiError> {
    let search = Search::new(sim, config)?;
    let (ff_run, ff) = search.execute(&FaultSchedule::default())?;
    let ff_graph = search.graph(&ff_run, &ff)?;
    let cert = crate::certify::exhaustive(sim, &config.spec, &ff.post, config.workers.max(1), max_states)?;
    let mut counterexamples = Vec::new();
    if let Some(faults) = cert.counterexample {
        let (run, outcome) = search.execute(&faults)?;
        if is_good(&ff.post, &outcome) {
            return Err(internal_error!("exhaustive certification's counterexample does not reproduce").into());
        }
        let violated = ff
            .post
            .iter()
            .filter(|g| !outcome.post.contains(*g) && outcome.pre.contains(*g))
            .cloned()
            .collect();
        counterexamples.push(Counterexample {
            faults,
            outcome,
            run,
            violated,
        });
    }
    Ok(LdfiReport {
        verdict: if counterexamples.is_empty() {
            Verdict::NoCounterexample
        } else {
            Verdict::Counterexample
        },
        method: Method::Exhaustive {
            states: cert.states,
            schedules: cert.schedules,
            after: why,
        },
        counterexamples,
        runs,
        stats: SearchStats::default(),
        failure_free: ff,
        failure_free_run: ff_run,
        failure_free_graph: ff_graph,
    })
}

/// The lineage-driven search (ARCHITECTURE §8.5).
///
/// Hypotheses are committed one at a time in queue order, exactly as the sequential algorithm does, so the verdict,
/// the counterexamples and the run count never depend on thread timing. With `config.workers > 1`, worker threads
/// process the next hypotheses in the queue speculatively (TEST-033: hypotheses run in parallel); a result is used
/// when its hypothesis reaches the head of the queue.
fn lineage_search(sim: &SpecSim<'_>, config: &LdfiConfig) -> Result<LdfiReport, LdfiError> {
    let search = Search::new(sim, config)?;
    let (ff_run, ff) = search.execute(&FaultSchedule::default())?;
    let ff_graph = search.graph(&ff_run, &ff)?;
    let ff_goals: Vec<Row> = ff.post.iter().cloned().collect();
    let mut queue = Queue::new(&config.spec);
    let (first, mut incomplete) = search.hypotheses(&ff_graph, &ff_goals, &[], &FaultSchedule::default())?;
    queue.push(first);
    let mut runs: u64 = 1;
    let mut counterexamples = Vec::new();
    let mut stats = SearchStats {
        queue_peak: queue.order.len(),
        ..SearchStats::default()
    };
    let workers = config.workers.max(1);
    let ff_post = &ff.post;
    let search = &search;
    let mut commit =
        |h: &FaultSchedule, processed: Processed, runs: &mut u64, queue: &mut Queue<'_>| -> Result<bool, LdfiError> {
            *runs += 1;
            *stats.by_size.entry(h.len()).or_insert(0) += 1;
            match processed {
                Processed::Bad(ce) => {
                    counterexamples.push(*ce);
                    Ok(!config.find_all)
                }
                Processed::Good(next, run_incomplete) => {
                    incomplete |= run_incomplete;
                    stats.suggested += next.len() as u64;
                    queue.push(next);
                    stats.queue_peak = stats.queue_peak.max(queue.order.len());
                    Ok(false)
                }
            }
        };
    if workers == 1 {
        while let Some(h) = queue.pop() {
            if runs >= config.max_runs {
                return Err(LdfiError::RunBudget(config.max_runs));
            }
            if commit(&h, search.process(&h, ff_post)?, &mut runs, &mut queue)? {
                break;
            }
        }
    } else {
        std::thread::scope(|scope| -> Result<(), LdfiError> {
            let (task_tx, task_rx) = std::sync::mpsc::channel::<FaultSchedule>();
            let task_rx = std::sync::Arc::new(std::sync::Mutex::new(task_rx));
            let (result_tx, result_rx) = std::sync::mpsc::channel::<(FaultSchedule, Result<Processed, LdfiError>)>();
            for _ in 0..workers {
                let task_rx = std::sync::Arc::clone(&task_rx);
                let result_tx = result_tx.clone();
                scope.spawn(move || {
                    loop {
                        let next = match task_rx.lock() {
                            Ok(rx) => rx.recv(),
                            Err(_) => return,
                        };
                        let Ok(h) = next else { return };
                        // A panicking worker must not leave the committer waiting for its hypothesis.
                        let result =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| search.process(&h, ff_post)))
                                .unwrap_or_else(|_| Err(internal_error!("an LDFI worker panicked").into()));
                        if result_tx.send((h, result)).is_err() {
                            return;
                        }
                    }
                });
            }
            drop(result_tx);
            let mut dispatched: BTreeSet<FaultSchedule> = BTreeSet::new();
            let mut ready: BTreeMap<FaultSchedule, Result<Processed, LdfiError>> = BTreeMap::new();
            let lookahead = workers * 2;
            let outcome = loop {
                for h in queue.upcoming(lookahead) {
                    if dispatched.insert(h.clone()) && task_tx.send(h.clone()).is_err() {
                        break;
                    }
                }
                let Some(h) = queue.pop() else { break Ok(()) };
                if runs >= config.max_runs {
                    break Err(LdfiError::RunBudget(config.max_runs));
                }
                let processed = loop {
                    if let Some(r) = ready.remove(&h) {
                        break r;
                    }
                    match result_rx.recv() {
                        Ok((done, r)) => {
                            ready.insert(done, r);
                        }
                        Err(_) => break Err(internal_error!("the LDFI workers stopped").into()),
                    }
                };
                dispatched.remove(&h);
                match processed.and_then(|p| commit(&h, p, &mut runs, &mut queue)) {
                    Ok(true) => break Ok(()),
                    Ok(false) => {}
                    Err(e) => break Err(e),
                }
            };
            // Closing the task channel stops the workers; their pending results are discarded.
            drop(task_tx);
            outcome
        })?;
    }
    // Hypotheses exhausted without a counterexample prove nothing if some lineage was incomplete.
    if counterexamples.is_empty() && incomplete {
        return Err(LdfiError::Incomplete);
    }
    Ok(LdfiReport {
        verdict: if counterexamples.is_empty() {
            Verdict::NoCounterexample
        } else {
            Verdict::Counterexample
        },
        method: Method::Lineage,
        counterexamples,
        runs,
        stats,
        failure_free: ff,
        failure_free_run: ff_run,
        failure_free_graph: ff_graph,
    })
}

/// The Appendix-B-minimal falsifiers of the failure-free run's `post` goals, unioned over the goals (TEST-028): for
/// each goal, the admissible fault sets after which it no longer holds at EOT, minimal by the clock facts they
/// remove. Each goal is searched like LDFI does, with a concrete run confirming every candidate.
pub fn falsifiers(sim: &SpecSim<'_>, config: &LdfiConfig) -> Result<Vec<FaultSchedule>, LdfiError> {
    let search = Search::new(sim, config)?;
    let (ff_run, ff) = search.execute(&FaultSchedule::default())?;
    let ff_graph = search.graph(&ff_run, &ff)?;
    let mut runs: u64 = 1;
    let mut union: BTreeMap<BTreeSet<blossom_sim::Omission>, FaultSchedule> = BTreeMap::new();
    for goal in &ff.post {
        let target = std::slice::from_ref(goal);
        let mut found: Vec<FaultSchedule> = Vec::new();
        let mut queue = Queue::new(&config.spec);
        let (first, incomplete) = search.hypotheses(&ff_graph, target, &[], &FaultSchedule::default())?;
        if incomplete {
            return Err(LdfiError::Incomplete);
        }
        queue.push(first);
        while let Some(h) = queue.pop() {
            if runs >= config.max_runs {
                return Err(LdfiError::RunBudget(config.max_runs));
            }
            runs += 1;
            let (run, outcome) = search.execute(&h)?;
            if !outcome.post.contains(goal) {
                found.push(h);
                continue;
            }
            let graph = search.graph(&run, &outcome)?;
            let (next, incomplete) = search.hypotheses(&graph, target, &[], &h)?;
            if incomplete {
                return Err(LdfiError::Incomplete);
            }
            queue.push(next);
        }
        for f in minimal_by_clocks(&config.spec, found) {
            let key = config.spec.removed_clocks(&f);
            union.entry(key).or_insert(f);
        }
    }
    let mut out: Vec<FaultSchedule> = union.into_values().collect();
    out.sort();
    Ok(out)
}

/// The minimal fault sets under inclusion of the clock facts they remove; among sets removing the same facts, the
/// one with the fewest faults (then the canonical order) represents them.
fn minimal_by_clocks(spec: &FailureSpec, sets: Vec<FaultSchedule>) -> Vec<FaultSchedule> {
    let mut by_clocks: BTreeMap<BTreeSet<blossom_sim::Omission>, FaultSchedule> = BTreeMap::new();
    for f in sets {
        let key = spec.removed_clocks(&f);
        match by_clocks.get(&key) {
            Some(cur) if (cur.len(), cur) <= (f.len(), &f) => {}
            _ => {
                by_clocks.insert(key, f);
            }
        }
    }
    let mut keys: Vec<&BTreeSet<blossom_sim::Omission>> = by_clocks.keys().collect();
    keys.sort_by_key(|k| k.len());
    let mut out = Vec::new();
    for (i, k) in keys.iter().enumerate() {
        let dominated = keys.iter().take(i).any(|o| o.len() < k.len() && o.is_subset(k));
        if !dominated && let Some(f) = by_clocks.get(*k) {
            out.push(f.clone());
        }
    }
    out
}
