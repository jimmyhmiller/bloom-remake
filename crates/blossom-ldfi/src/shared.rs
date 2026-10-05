//! Hazards shared across the runs of one search (S12).
//!
//! A run's hazards are encoded from its lineage graph and its own faults (the seed), and most of that work repeats
//! from run to run: a hazard at tick `t` reads only the graph up to `t` (every tuple, firing and premise there) and,
//! of the seed, which nodes it crashes, the crashes up to `t`, and the omissions sent before `t`. A run's graph up to
//! `t` is fixed by those same faults (a crash at `c > t` and a message sent at `s >= t` change nothing before `t + 1`),
//! so two runs that agree on them agree on every hazard at `t`: [`Context`] is that part of the seed, and an entry
//! encoded under one context serves every later run with the same context at that tick.
//!
//! Only hazards whose value does not depend on how the encoder reached them are shared. The encoder memoizes an entry
//! only when it was computed as the root of its own search (no cycle reached a node above it), and such a value is
//! the fixpoint of the hazard equations for that entry, whatever the traversal: the least one for appearances (a
//! place met again on the path contributes false) and the greatest for goals (a goal met again counts as falsified).
//! A cycle through both kinds has no single fixpoint to agree on, so a run that meets one shares nothing. That keeps
//! every run's hypotheses independent of which runs came before it and of the worker count: the hypotheses are the
//! minimal models of the roots' functions, and the functions are the same however their circuits were built.
//!
//! The store holds at most a budget of entries; past it, the contexts least recently used are dropped whole.

use std::collections::BTreeSet;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use blossom_base::{DetMap, InternalError, internal_error};
use blossom_prov::GoalKey;
use blossom_sim::{FaultSchedule, Omission};
use blossom_value::time::{NodeId, Tick};

use crate::circuit::Hazard;
use crate::faults::FailureSpec;
use crate::hazard::PlaceKey;

/// The part of a run's faults that a hazard at a tick depends on: which nodes crash (a crash after the tick only as
/// "the node crashes later": under crash-restart a node crashes once, so a later crash rules out a new one; under
/// crash-stop the crash tick is kept), and the omissions sent before the tick.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Context {
    crashes: Vec<(NodeId, Option<Tick>)>,
    omissions: Vec<Omission>,
}

impl Context {
    /// The context of `seed` at `tick`.
    pub fn of(spec: &FailureSpec, seed: &FaultSchedule, tick: Tick) -> Context {
        Context {
            crashes: seed
                .crashes
                .iter()
                .map(|(n, c)| (*n, (spec.restart.is_none() || *c <= tick).then_some(*c)))
                .collect(),
            omissions: seed.omissions.iter().filter(|o| o.send < tick).copied().collect(),
        }
    }

    /// Whether the context names no fault tick: no omission, and every crash only as "the node crashes later". Only
    /// such contexts recur across runs (a run's faults are its own), so only they are shared.
    fn plain(&self) -> bool {
        self.omissions.is_empty() && self.crashes.iter().all(|(_, c)| c.is_none())
    }
}

/// A run's contexts, by tick: the context, whether earlier runs may have shared entries in it at that tick (look
/// them up), and whether this run should share its entries there (no run did yet).
#[derive(Default)]
pub struct RunContexts {
    pub ids: Vec<CtxId>,
    pub lookup: Vec<bool>,
    pub publish: Vec<bool>,
}

/// An interned [`Context`].
pub type CtxId = u32;

/// A shared entry other than a goal's or a place's: stream events and traffic at a node and tick; relation-level
/// negative support, the disjunction of a relation's goals at a tick, and at every tick up to one.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Other {
    Stream(NodeId, Tick),
    Traffic(NodeId, Tick),
    Neg(u32, Tick),
    AtTick(u32, Tick),
    Prefix(u32, Tick),
}

impl Other {
    fn tick(&self) -> Tick {
        match self {
            Other::Stream(_, t)
            | Other::Traffic(_, t)
            | Other::Neg(_, t)
            | Other::AtTick(_, t)
            | Other::Prefix(_, t) => *t,
        }
    }
}

/// Per key, its hazard under each context it was encoded in.
type Table<K> = DetMap<K, Vec<(CtxId, Hazard)>>;

/// The entries of one shard, by kind: goals, appearances and removals of places, the others.
#[derive(Default)]
struct Shard {
    goals: Table<GoalKey>,
    appears: Table<PlaceKey>,
    removes: Table<PlaceKey>,
    others: Table<Other>,
}

/// A run's entries to share, each under its context.
#[derive(Default)]
pub struct Entries {
    pub goals: Vec<(GoalKey, CtxId, Hazard)>,
    pub appears: Vec<(PlaceKey, CtxId, Hazard)>,
    pub removes: Vec<(PlaceKey, CtxId, Hazard)>,
    pub others: Vec<(Other, CtxId, Hazard)>,
}

fn find(ctx: CtxId, found: Option<&Vec<(CtxId, Hazard)>>) -> Option<Hazard> {
    found.and_then(|v| v.iter().find(|(c, _)| *c == ctx).map(|(_, h)| h.clone()))
}

/// Adds `entries` to `table`; returns how many were new, per context.
fn add<K: std::hash::Hash + Eq>(
    table: &mut Table<K>,
    entries: Vec<(K, CtxId, Hazard)>,
    added: &mut DetMap<CtxId, usize>,
) {
    for (key, ctx, h) in entries {
        let slot = table.entry(key).or_default();
        if !slot.iter().any(|(c, _)| *c == ctx) {
            slot.push((ctx, h));
            *added.entry(ctx).or_insert(0) += 1;
        }
    }
}

fn evict<K: std::hash::Hash + Eq>(table: &mut Table<K>, gone: &BTreeSet<CtxId>) {
    table.retain(|_, v| {
        v.retain(|(c, _)| !gone.contains(c));
        !v.is_empty()
    });
}

/// The interned contexts, and per context when a run last used it and how many entries it holds.
#[derive(Default)]
struct Contexts {
    ids: DetMap<Context, CtxId>,
    /// Per plain context, the last tick a run shared its entries up to.
    frontier: Vec<Option<u64>>,
    last_used: Vec<u64>,
    entries: Vec<usize>,
    total: usize,
}

/// Shards by tick: lookups at different ticks take different locks.
const SHARDS: usize = 16;

/// The hazards a search's runs share (see the module docs). Safe to share between worker threads.
pub struct SharedHazards {
    shards: Vec<RwLock<Shard>>,
    contexts: RwLock<Contexts>,
    budget: usize,
    /// Runs that interned their contexts, for recency.
    runs: AtomicU64,
}

impl SharedHazards {
    /// A store of at most `budget` entries.
    pub fn new(budget: usize) -> SharedHazards {
        SharedHazards {
            shards: (0..SHARDS).map(|_| RwLock::new(Shard::default())).collect(),
            contexts: RwLock::new(Contexts::default()),
            budget,
            runs: AtomicU64::new(0),
        }
    }

    fn shard(&self, tick: Tick) -> Result<&RwLock<Shard>, InternalError> {
        self.shards
            .get((tick.0 % SHARDS as u64) as usize)
            .ok_or_else(|| internal_error!("no hazard shard for {tick:?}"))
    }

    /// The context of `seed` at every tick `0..=eot`, interned, marked as used by a new run; where to look entries up,
    /// and where to share them.
    pub fn contexts(&self, spec: &FailureSpec, seed: &FaultSchedule) -> Result<RunContexts, InternalError> {
        let run = self.runs.fetch_add(1, Ordering::Relaxed) + 1;
        let mut ctx = self
            .contexts
            .write()
            .map_err(|_| internal_error!("the shared hazards' context table is poisoned"))?;
        let mut out = RunContexts::default();
        let mut last: Option<(Context, CtxId, bool)> = None;
        for t in 0..=spec.eot.0 {
            let c = Context::of(spec, seed, Tick(t));
            let (id, plain) = match &last {
                Some((prev, id, plain)) if *prev == c => (*id, *plain),
                _ => {
                    let next = u32::try_from(ctx.ids.len()).map_err(|_| internal_error!("too many contexts"))?;
                    let id = *ctx.ids.entry(c.clone()).or_insert(next);
                    if id == next {
                        ctx.frontier.push(None);
                        ctx.last_used.push(0);
                        ctx.entries.push(0);
                    }
                    (id, c.plain())
                }
            };
            if let Some(slot) = ctx.last_used.get_mut(id as usize) {
                *slot = run;
            }
            let frontier = ctx.frontier.get(id as usize).copied().flatten();
            out.ids.push(id);
            out.lookup.push(plain && frontier.is_some_and(|f| t <= f));
            out.publish.push(plain && frontier.is_none_or(|f| t > f));
            last = Some((c, id, plain));
        }
        Ok(out)
    }

    fn read(&self, tick: Tick) -> Result<std::sync::RwLockReadGuard<'_, Shard>, InternalError> {
        self.shard(tick)?
            .read()
            .map_err(|_| internal_error!("a shared hazard shard is poisoned"))
    }

    /// The hazard of goal `key` under context `ctx`, if a run shared it.
    pub fn goal(&self, key: &GoalKey, ctx: CtxId) -> Result<Option<Hazard>, InternalError> {
        let shard = self.read(key.tick)?;
        Ok(find(ctx, shard.goals.get(key)))
    }

    /// Whether a tuple can appear at place `key`, under context `ctx`, if a run shared it.
    pub fn appear(&self, key: &PlaceKey, ctx: CtxId) -> Result<Option<Hazard>, InternalError> {
        let shard = self.read(key.3)?;
        Ok(find(ctx, shard.appears.get(key)))
    }

    /// Whether a tuple at place `key` can be lost, under context `ctx`, if a run shared it.
    pub fn remove(&self, key: &PlaceKey, ctx: CtxId) -> Result<Option<Hazard>, InternalError> {
        let shard = self.read(key.3)?;
        Ok(find(ctx, shard.removes.get(key)))
    }

    pub fn other(&self, key: Other, ctx: CtxId) -> Result<Option<Hazard>, InternalError> {
        let shard = self.read(key.tick())?;
        Ok(find(ctx, shard.others.get(&key)))
    }

    /// Shares a run's entries, and moves each context's frontier to the last tick `shared` says the run shared it at;
    /// then drops the least recently used contexts while the store is over its budget.
    pub fn publish(&self, entries: Entries, shared: &RunContexts) -> Result<(), InternalError> {
        let shard_of = |t: Tick| (t.0 % SHARDS as u64) as usize;
        let mut parts: Vec<Entries> = (0..SHARDS).map(|_| Entries::default()).collect();
        for e in entries.goals {
            if let Some(p) = parts.get_mut(shard_of(e.0.tick)) {
                p.goals.push(e);
            }
        }
        for e in entries.appears {
            if let Some(p) = parts.get_mut(shard_of(e.0.3)) {
                p.appears.push(e);
            }
        }
        for e in entries.removes {
            if let Some(p) = parts.get_mut(shard_of(e.0.3)) {
                p.removes.push(e);
            }
        }
        for e in entries.others {
            if let Some(p) = parts.get_mut(shard_of(e.0.tick())) {
                p.others.push(e);
            }
        }
        let mut added: DetMap<CtxId, usize> = DetMap::default();
        for (lock, part) in self.shards.iter().zip(parts) {
            let mut shard = lock
                .write()
                .map_err(|_| internal_error!("a shared hazard shard is poisoned"))?;
            add(&mut shard.goals, part.goals, &mut added);
            add(&mut shard.appears, part.appears, &mut added);
            add(&mut shard.removes, part.removes, &mut added);
            add(&mut shard.others, part.others, &mut added);
        }
        let gone: BTreeSet<CtxId> = {
            let mut ctx = self
                .contexts
                .write()
                .map_err(|_| internal_error!("the shared hazards' context table is poisoned"))?;
            for (t, (id, publish)) in shared.ids.iter().zip(&shared.publish).enumerate() {
                if *publish && let Some(f) = ctx.frontier.get_mut(*id as usize) {
                    *f = Some(f.map_or(t as u64, |f| f.max(t as u64)));
                }
            }
            for (id, n) in added {
                if let Some(e) = ctx.entries.get_mut(id as usize) {
                    *e += n;
                }
                ctx.total += n;
            }
            if ctx.total <= self.budget {
                return Ok(());
            }
            // Down to three quarters of the budget, least recently used first, so eviction stays rare.
            let target = self.budget / 4 * 3;
            let mut order: Vec<CtxId> = (0..ctx.entries.len())
                .filter_map(|i| u32::try_from(i).ok())
                .filter(|i| ctx.entries.get(*i as usize).is_some_and(|n| *n > 0))
                .collect();
            order.sort_by_key(|i| (ctx.last_used.get(*i as usize).copied().unwrap_or(0), *i));
            let mut gone = BTreeSet::new();
            for id in order {
                if ctx.total <= target {
                    break;
                }
                let n = ctx.entries.get(id as usize).copied().unwrap_or(0);
                ctx.total -= n;
                if let Some(e) = ctx.entries.get_mut(id as usize) {
                    *e = 0;
                }
                if let Some(f) = ctx.frontier.get_mut(id as usize) {
                    *f = None;
                }
                gone.insert(id);
            }
            gone
        };
        for lock in &self.shards {
            let mut shard = lock
                .write()
                .map_err(|_| internal_error!("a shared hazard shard is poisoned"))?;
            evict(&mut shard.goals, &gone);
            evict(&mut shard.appears, &gone);
            evict(&mut shard.removes, &gone);
            evict(&mut shard.others, &gone);
        }
        Ok(())
    }

    /// The entries the store holds.
    pub fn len(&self) -> usize {
        self.contexts.read().map_or(0, |c| c.total)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_looks_up_plain_contexts_earlier_runs_shared_and_shares_past_their_frontier() {
        let spec = FailureSpec::new(6, 4, 1, 2).unwrap().with_restart(1).unwrap();
        let store = SharedHazards::new(1000);
        let crash_at = |t: u64| {
            let mut f = FaultSchedule::default();
            f.crashes.insert(NodeId(0), Tick(t));
            spec.with_restarts(f)
        };
        // A crash at 3: ticks 0..=2 know only that node 0 crashes later (plain); from 3 on the crash is named.
        let first = store.contexts(&spec, &crash_at(3)).unwrap();
        assert_eq!(first.lookup, [false; 7]);
        assert_eq!(first.publish, [true, true, true, false, false, false, false]);
        assert_eq!(first.ids.first(), first.ids.get(2));
        assert_ne!(first.ids.get(2), first.ids.get(3));
        store.publish(Entries::default(), &first).unwrap();
        // A crash at 5 shares that context up to 4: it looks up 0..=2 and shares 3 and 4.
        let later = store.contexts(&spec, &crash_at(5)).unwrap();
        assert_eq!(later.ids.first(), first.ids.first());
        assert_eq!(later.lookup, [true, true, true, false, false, false, false]);
        assert_eq!(later.publish, [false, false, false, true, true, false, false]);
        // A lost message names its send tick from the next tick on.
        let mut lost = FaultSchedule::default();
        lost.omissions.insert(Omission {
            from: NodeId(0),
            to: NodeId(1),
            send: Tick(2),
        });
        let o = store.contexts(&spec, &lost).unwrap();
        assert_eq!(o.publish, [true, true, true, false, false, false, false]);
    }
}
