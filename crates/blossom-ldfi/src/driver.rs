//! The forward/backward loop (ARCHITECTURE §8.5, TEST-029) and falsifier enumeration (TEST-028).

use std::collections::{BTreeMap, BTreeSet};

use blossom_artifact::sim::SimArtifact;
use blossom_base::internal_error;
use blossom_oracle::Row;
use blossom_prov::{GoalId, GoalKey, Names, ProvGraph, Space};
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
    /// Told about every run the lineage-driven search commits (diagnostics; results do not depend on it).
    pub observer: Option<ObserverRef>,
    /// When the lineage-driven search exhausts `max_runs` without a verdict, decide by exhaustive certification
    /// ([`crate::certify`]) within this many states; `None` reports the budget error instead.
    pub exhaustive_fallback: Option<u64>,
    /// The fault schedules exhaustive certification may run when it enumerates them (a program it cannot step:
    /// [`crate::certify::steppable`]), or when enumeration is asked for directly ([`enumerate`]).
    pub max_schedules: u64,
    /// The hazard entries the runs of a search may share ([`crate::shared`]); 0 encodes every run from scratch.
    /// Results do not depend on it.
    pub shared_hazards: usize,
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
            max_schedules: 100_000,
            shared_hazards: 4_000_000,
            observer: None,
        }
    }
}

/// What watches a search as it goes ([`LdfiConfig::observer`]).
pub trait Observer: Send + Sync {
    /// Nanoseconds on a monotonic clock, to time the phases of each run (the search reads no clock itself).
    fn now_nanos(&self) -> u64;
    /// A run was committed.
    fn run_done(&self, progress: &RunProgress);
}

/// An [`Observer`], shared.
#[derive(Clone)]
pub struct ObserverRef(pub std::sync::Arc<dyn Observer>);

impl std::fmt::Debug for ObserverRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ObserverRef")
    }
}

/// One committed run of the lineage-driven search.
#[derive(Clone, Debug, Default)]
pub struct RunProgress {
    /// Runs so far (the failure-free run included), hypotheses waiting, counterexamples found.
    pub runs: u64,
    pub queue: usize,
    pub counterexamples: usize,
    /// The run's faults, and whether it kept the outcome spec.
    pub faults: FaultSchedule,
    pub good: bool,
    /// The size of its lineage, and the hypotheses it suggested (before deduplication).
    pub goals: usize,
    pub firings: usize,
    pub suggested: usize,
    /// Time spent running it, building its lineage, and finding its hypotheses (of which: encoding its hazards, and
    /// enumerating their minimal models, with this many SAT calls).
    pub execute_ns: u64,
    pub lineage_ns: u64,
    pub hypotheses_ns: u64,
    pub encode_ns: u64,
    pub enumerate_ns: u64,
    pub solves: u64,
}

/// What finding a run's hypotheses cost.
#[derive(Clone, Copy, Debug, Default)]
struct SatCost {
    solves: u64,
    encode_ns: u64,
    enumerate_ns: u64,
}

/// What processing a hypothesis cost, for the observer.
#[derive(Clone, Copy, Debug, Default)]
struct Cost {
    sat: SatCost,
    goals: usize,
    firings: usize,
    execute_ns: u64,
    lineage_ns: u64,
    hypotheses_ns: u64,
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
    /// A program error the run ended in (a node's runtime hard error, ARCHITECTURE §6.6: a verdict of its own); the
    /// run and outcome are then empty.
    pub failure: Option<String>,
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
    /// Every admissible fault schedule was run ([`crate::certify::enumerate`]): after the lineage-driven search gave
    /// up on a program the stepped search cannot step, or (`after: None`) because it was asked for.
    Enumerated { schedules: u64, after: Option<Fallback> },
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
    /// The hazards the runs share (S12), when the config allows any, and the row patterns they all intern.
    shared: Option<crate::shared::SharedHazards>,
    patterns: crate::patterns::Patterns,
}

/// A run of the program: done, or ended in a program error.
enum Ran {
    Done(Box<(SyncRun, Outcome)>),
    Failed(String),
}

/// A run judged against the failure-free `post`.
enum Judged {
    Good(Box<(SyncRun, Outcome)>),
    Bad(Box<Counterexample>),
}

/// The program error a simulation error is, if it is one: a node's runtime hard error (BLSRnnn), which under faults is
/// a verdict of its own rather than a failure of the search.
pub(crate) fn program_failure(e: &blossom_sim::SimError) -> Option<String> {
    match e {
        blossom_sim::SimError::Node {
            error: blossom_ir::tick::EvalError::Program { .. },
            ..
        } => Some(e.to_string()),
        _ => None,
    }
}

/// What processing one hypothesis found.
enum Processed {
    /// The run is good; these are the hypotheses its lineage suggests, and why its lineage was incomplete if it was.
    Good(BTreeSet<FaultSchedule>, Option<String>, Cost),
    Bad(Box<Counterexample>, Cost),
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
            shared: (config.shared_hazards > 0).then(|| crate::shared::SharedHazards::new(config.shared_hazards)),
            patterns: crate::patterns::Patterns::new(),
        })
    }

    /// Runs the program under `faults` and judges it.
    fn execute(&self, faults: &FaultSchedule) -> Result<(SyncRun, Outcome), LdfiError> {
        let eot = self.config.spec.eot;
        let run = self.sim.run(eot, faults, true)?;
        let outcome = self.sim.outcome(&run, eot, true)?;
        Ok((run, outcome))
    }

    /// Runs the program under `faults`: its run and outcome, or the program error it ended in.
    fn try_execute(&self, faults: &FaultSchedule) -> Result<Ran, LdfiError> {
        match self.execute(faults) {
            Ok(done) => Ok(Ran::Done(Box::new(done))),
            Err(LdfiError::Sim(e)) => match program_failure(&e) {
                Some(failure) => Ok(Ran::Failed(failure)),
                None => Err(LdfiError::Sim(e)),
            },
            Err(e) => Err(e),
        }
    }

    /// Runs `faults` and judges it against the failure-free `post`: its counterexample, when the run ends in a
    /// program error or loses a `post` tuple `pre` holds; else its run and outcome.
    fn judge(&self, faults: &FaultSchedule, ff_post: &BTreeSet<Row>) -> Result<Judged, LdfiError> {
        let (run, outcome) = match self.try_execute(faults)? {
            Ran::Failed(failure) => {
                return Ok(Judged::Bad(Box::new(Counterexample {
                    faults: faults.clone(),
                    outcome: Outcome::default(),
                    run: SyncRun::default(),
                    violated: Vec::new(),
                    failure: Some(failure),
                })));
            }
            Ran::Done(done) => *done,
        };
        if is_good(ff_post, &outcome) {
            return Ok(Judged::Good(Box::new((run, outcome))));
        }
        let violated = ff_post
            .iter()
            .filter(|g| !outcome.post.contains(*g) && outcome.pre.contains(*g))
            .cloned()
            .collect();
        Ok(Judged::Bad(Box::new(Counterexample {
            faults: faults.clone(),
            outcome,
            run,
            violated,
            failure: None,
        })))
    }

    fn graph(&self, run: &SyncRun, outcome: &Outcome) -> Result<ProvGraph, LdfiError> {
        lineage::build(self.artifact, run, outcome)
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
    /// tuples of `pre`, according to `graph`; and if the lineage was incomplete, why (for the first target it was
    /// incomplete for).
    fn hypotheses(
        &self,
        graph: &ProvGraph,
        goals: &[Row],
        revive: &[Row],
        seed: &FaultSchedule,
    ) -> Result<(BTreeSet<FaultSchedule>, Option<String>, SatCost), LdfiError> {
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
            patterns: &self.patterns,
            preds: &self.preds,
            neg: self.config.negative_support,
            rules: Some(&self.rules),
            frozen: self.artifact.profile.frozen(),
            clock: Some(&|| self.now()),
        };
        let found =
            crate::hazard::minimal_extensions(graph, setting, solver.as_mut(), seed, &targets, self.shared.as_ref())?;
        let sat = SatCost {
            solves: found.solves,
            encode_ns: found.encode_ns,
            enumerate_ns: found.enumerate_ns,
        };
        let admitted = found
            .hypotheses
            .into_iter()
            .filter(|h| self.config.spec.admits(h))
            .collect();
        let why = match found.incomplete_targets.first().and_then(|i| targets.get(*i)) {
            None if found.incomplete => Some("an incomplete target".to_owned()),
            None => None,
            Some(target) => {
                let names = crate::report::DedNames {
                    artifact: self.artifact,
                };
                let faults = crate::faults::labels(seed, &|n| names.node(n), &|p| {
                    crate::report::path_name(self.artifact, p)
                })
                .join(", ");
                Some(match target {
                    crate::hazard::Target::Goal(goal) => {
                        let lost = crate::explain::lost_under(graph, &self.config.spec, seed, *goal, &names);
                        let label = graph.get(*goal).map(|g| names.goal(&g.key)).unwrap_or_default();
                        match lost {
                            Some(path) => format!(
                                "under {{{faults}}} the lineage already counts {label} as lost, though the run holds \
                                 it:\n{path}"
                            ),
                            None => format!(
                                "under {{{faults}}} the encoding counts {label} as lost through a negated read or an \
                                 aggregate group"
                            ),
                        }
                    }
                    crate::hazard::Target::Appears { row, .. } => {
                        format!("under {{{faults}}} a lost `pre` tuple {row:?} counts as able to reappear already")
                    }
                })
            }
        };
        Ok((admitted, why, sat))
    }

    fn now(&self) -> u64 {
        self.config.observer.as_ref().map_or(0, |o| o.0.now_nanos())
    }

    /// Runs `h`, judges it against the failure-free `post`, and for a good run derives the next hypotheses.
    fn process(&self, h: &FaultSchedule, ff_post: &BTreeSet<Row>) -> Result<Processed, LdfiError> {
        let mut cost = Cost::default();
        let start = self.now();
        let judged = self.judge(h, ff_post)?;
        let executed = self.now();
        cost.execute_ns = executed.saturating_sub(start);
        let (run, outcome) = match judged {
            Judged::Bad(ce) => return Ok(Processed::Bad(ce, cost)),
            Judged::Good(done) => *done,
        };
        let graph = self.graph(&run, &outcome)?;
        let built = self.now();
        cost.lineage_ns = built.saturating_sub(executed);
        cost.goals = graph.goal_count();
        cost.firings = graph.firing_count();
        let goals: Vec<Row> = outcome.post.iter().cloned().collect();
        // A failure-free `post` tuple this run lost together with its `pre` tuple: a larger fault set that brings
        // the `pre` tuple back while the `post` tuple stays lost is a counterexample.
        let revive: Vec<Row> = ff_post
            .iter()
            .filter(|g| !outcome.post.contains(*g) && !outcome.pre.contains(*g))
            .cloned()
            .collect();
        let (next, incomplete, sat) = self.hypotheses(&graph, &goals, &revive, h)?;
        cost.hypotheses_ns = self.now().saturating_sub(built);
        cost.sat = sat;
        Ok(Processed::Good(next, incomplete, cost))
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
    let (runs, why, error) = match lineage_search(sim, config) {
        Err(LdfiError::RunBudget(runs)) => (runs, Fallback::RunBudget, LdfiError::RunBudget(runs)),
        Err(e @ LdfiError::Incomplete(_)) => (0, Fallback::IncompleteLineage, e),
        other => return other,
    };
    match config.exhaustive_fallback {
        Some(max_states) => certify_exhaustively(sim, config, runs, why, max_states),
        None => Err(error),
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
    if !crate::certify::steppable(sim, &config.spec) {
        return certify_by_enumeration(sim, config, runs, Some(why));
    }
    let search = Search::new(sim, config)?;
    let (ff_run, ff) = search.execute(&FaultSchedule::default())?;
    let ff_graph = search.graph(&ff_run, &ff)?;
    let cert = crate::certify::exhaustive(sim, &config.spec, &ff.post, config.workers.max(1), max_states)?;
    let mut counterexamples = Vec::new();
    if let Some(faults) = cert.counterexample {
        match search.judge(&faults, &ff.post)? {
            Judged::Bad(ce) => counterexamples.push(*ce),
            Judged::Good(..) => {
                return Err(internal_error!("exhaustive certification's counterexample does not reproduce").into());
            }
        }
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

/// Decides `config.spec` by the faster exact search for its size: by enumeration ([`enumerate`]) when its
/// admissible schedules fit `config.max_schedules`, else by the lineage-driven search ([`run`]). On specs small
/// enough to enumerate, running every schedule costs less than the lineage-driven search's runs with their encodings
/// (S11, S12: on the Kafka specs its lineage leaves a third to all of the schedules to run, at several times a
/// run's cost each); beyond that, its pruning is what makes a verdict reachable at all.
pub fn decide(sim: &SpecSim<'_>, config: &LdfiConfig) -> Result<LdfiReport, LdfiError> {
    if crate::certify::schedule_count(&config.spec, crate::certify::paths(sim).len())
        <= u128::from(config.max_schedules)
    {
        enumerate(sim, config)
    } else {
        run(sim, config)
    }
}

/// Decides `config.spec` by running every admissible fault schedule ([`crate::certify::enumerate`]), within
/// `config.max_schedules`: an oracle for the lineage-driven search on specs small enough to enumerate.
pub fn enumerate(sim: &SpecSim<'_>, config: &LdfiConfig) -> Result<LdfiReport, LdfiError> {
    certify_by_enumeration(sim, config, 0, None)
}

fn certify_by_enumeration(
    sim: &SpecSim<'_>,
    config: &LdfiConfig,
    runs: u64,
    after: Option<Fallback>,
) -> Result<LdfiReport, LdfiError> {
    let search = Search::new(sim, config)?;
    let (ff_run, ff) = search.execute(&FaultSchedule::default())?;
    let ff_graph = search.graph(&ff_run, &ff)?;
    let found = crate::certify::enumerate(sim, &config.spec, &ff.post, config.workers.max(1), config.max_schedules)?;
    let mut counterexamples = Vec::new();
    if let Some(faults) = found.counterexample {
        match search.judge(&faults, &ff.post)? {
            Judged::Bad(ce) => counterexamples.push(*ce),
            Judged::Good(..) => return Err(internal_error!("an enumerated counterexample does not reproduce").into()),
        }
    }
    Ok(LdfiReport {
        verdict: if counterexamples.is_empty() {
            Verdict::NoCounterexample
        } else {
            Verdict::Counterexample
        },
        method: Method::Enumerated {
            schedules: found.schedules,
            after,
        },
        counterexamples,
        runs,
        stats: SearchStats::default(),
        failure_free: ff,
        failure_free_run: ff_run,
        failure_free_graph: ff_graph,
    })
}

/// The lineage and hazards of delayed streams are not built yet: under the asynchronous model the lineage-driven
/// search refuses a program with streams rather than miss their delays. Enumeration decides it.
fn asynchronous_lineage(sim: &SpecSim<'_>, config: &LdfiConfig) -> Result<(), LdfiError> {
    let streams = crate::certify::paths(sim).contains(&blossom_sim::Path::Streams);
    if config.spec.delay.is_some() && streams {
        return Err(blossom_base::unimplemented_error!(
            "TEST-001",
            "the lineage-driven search over delayed streams (enumeration decides it: `--method enumerate`)"
        )
        .into());
    }
    Ok(())
}

/// The lineage-driven search (ARCHITECTURE §8.5).
///
/// Hypotheses are committed one at a time in queue order, exactly as the sequential algorithm does, so the verdict,
/// the counterexamples and the run count never depend on thread timing. With `config.workers > 1`, worker threads
/// process the next hypotheses in the queue speculatively (TEST-033: hypotheses run in parallel); a result is used
/// when its hypothesis reaches the head of the queue.
fn lineage_search(sim: &SpecSim<'_>, config: &LdfiConfig) -> Result<LdfiReport, LdfiError> {
    asynchronous_lineage(sim, config)?;
    let search = Search::new(sim, config)?;
    let start = search.now();
    let (ff_run, ff) = search.execute(&FaultSchedule::default())?;
    let executed = search.now();
    let ff_graph = search.graph(&ff_run, &ff)?;
    let built = search.now();
    let ff_goals: Vec<Row> = ff.post.iter().cloned().collect();
    let mut queue = Queue::new(&config.spec);
    let (first, mut incomplete, sat) = search.hypotheses(&ff_graph, &ff_goals, &[], &FaultSchedule::default())?;
    let suggested = first.len();
    queue.push(first);
    if let Some(o) = &config.observer {
        o.0.run_done(&RunProgress {
            runs: 1,
            queue: queue.order.len(),
            counterexamples: 0,
            faults: FaultSchedule::default(),
            good: true,
            goals: ff_graph.goal_count(),
            firings: ff_graph.firing_count(),
            suggested,
            execute_ns: executed.saturating_sub(start),
            lineage_ns: built.saturating_sub(executed),
            hypotheses_ns: search.now().saturating_sub(built),
            encode_ns: sat.encode_ns,
            enumerate_ns: sat.enumerate_ns,
            solves: sat.solves,
        });
    }
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
            let (stop, good, suggested, cost) = match processed {
                Processed::Bad(ce, cost) => {
                    counterexamples.push(*ce);
                    (!config.find_all, false, 0, cost)
                }
                Processed::Good(next, run_incomplete, cost) => {
                    if incomplete.is_none() {
                        incomplete = run_incomplete;
                    }
                    let suggested = next.len();
                    stats.suggested += suggested as u64;
                    queue.push(next);
                    stats.queue_peak = stats.queue_peak.max(queue.order.len());
                    (false, true, suggested, cost)
                }
            };
            if let Some(o) = &config.observer {
                o.0.run_done(&RunProgress {
                    runs: *runs,
                    queue: queue.order.len(),
                    counterexamples: counterexamples.len(),
                    faults: h.clone(),
                    good,
                    goals: cost.goals,
                    firings: cost.firings,
                    suggested,
                    execute_ns: cost.execute_ns,
                    lineage_ns: cost.lineage_ns,
                    hypotheses_ns: cost.hypotheses_ns,
                    encode_ns: cost.sat.encode_ns,
                    enumerate_ns: cost.sat.enumerate_ns,
                    solves: cost.sat.solves,
                });
            }
            Ok(stop)
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
                let spawned = std::thread::Builder::new()
                    .stack_size(blossom_ir::depth::EVAL_STACK_BYTES)
                    .spawn_scoped(scope, move || {
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
                if let Err(e) = spawned {
                    return Err(internal_error!("an LDFI worker could not start: {e}").into());
                }
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
    if counterexamples.is_empty()
        && let Some(why) = incomplete
    {
        return Err(LdfiError::Incomplete(why));
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
    asynchronous_lineage(sim, config)?;
    let search = Search::new(sim, config)?;
    let (ff_run, ff) = search.execute(&FaultSchedule::default())?;
    let ff_graph = search.graph(&ff_run, &ff)?;
    let mut runs: u64 = 1;
    let mut union: BTreeMap<BTreeSet<blossom_sim::Omission>, FaultSchedule> = BTreeMap::new();
    for goal in &ff.post {
        let target = std::slice::from_ref(goal);
        let mut found: Vec<FaultSchedule> = Vec::new();
        let mut queue = Queue::new(&config.spec);
        let (first, incomplete, _) = search.hypotheses(&ff_graph, target, &[], &FaultSchedule::default())?;
        if let Some(why) = incomplete {
            return Err(LdfiError::Incomplete(why));
        }
        queue.push(first);
        while let Some(h) = queue.pop() {
            if runs >= config.max_runs {
                return Err(LdfiError::RunBudget(config.max_runs));
            }
            runs += 1;
            // A run that ends in a program error falsifies every goal.
            let (run, outcome) = match search.try_execute(&h)? {
                Ran::Failed(_) => {
                    found.push(h);
                    continue;
                }
                Ran::Done(done) => *done,
            };
            if !outcome.post.contains(goal) {
                found.push(h);
                continue;
            }
            let graph = search.graph(&run, &outcome)?;
            let (next, incomplete, _) = search.hypotheses(&graph, target, &[], &h)?;
            if let Some(why) = incomplete {
                return Err(LdfiError::Incomplete(why));
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
