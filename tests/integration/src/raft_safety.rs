//! Raft's safety properties (Fig. 3) per group, checked by a simulator observer on the brokers' state of a program
//! built on `examples/kafka/raft.bls` (its `rlog`, `commit`, `won`, `rterm` and `match_idx` relations): election
//! safety, log matching through each entry's recorded `prev`, state machine safety over committed entries, leader
//! completeness, and that a leader's record of a follower is true while the follower is in the leader's term.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use blossom_artifact::bls::BlsArtifact;
use blossom_base::RelId;
use blossom_ir::tick::{Instance, Row};
use blossom_sim::cluster::Observer;
use blossom_value::Value;
use blossom_value::value::IntValue;

/// A row's group (its first column).
fn group(row: &Row) -> Result<Value, String> {
    row.first().cloned().ok_or_else(|| format!("{row:?} has no group"))
}

fn u64_at(row: &Row, col: usize) -> Result<u64, String> {
    match row.get(col) {
        Some(Value::Int(IntValue::U64(x))) => Ok(*x),
        other => Err(format!("column {col} of {row:?} is {other:?}, not a u64")),
    }
}

/// The checks are at most this far apart (virtual nanoseconds): every check reads every log. A safety violation
/// stays in the state (a log, a commit index, a term won), so a later check finds it.
const EVERY: i64 = 5_000_000;

/// The observer's state: what it has seen across checks.
pub struct GroupSafety {
    rlog: RelId,
    commit: RelId,
    won: RelId,
    rterm: RelId,
    match_idx: RelId,
    /// The snapshot points (entries up to one are compacted away: committed, and not looked for).
    rsnap: Option<RelId>,
    /// Per group: the broker that won each term.
    winners: BTreeMap<(Value, u64), usize>,
    /// Per group and committed index: the entry, and the term of the broker that was first seen to commit it.
    committed: BTreeMap<(Value, u64), (Row, u64)>,
    /// The highest commit index seen per group (for a test's progress check).
    pub progress: BTreeMap<Value, u64>,
    /// When the state was last checked.
    last: i64,
    /// Per broker and group: the highest index already checked against the committed entries.
    checked: BTreeMap<(usize, Value), u64>,
}

impl GroupSafety {
    /// The observer for `a`'s Raft relations; `None` if the program lacks one.
    pub fn of(a: &BlsArtifact) -> Option<GroupSafety> {
        Some(GroupSafety {
            rlog: a.rel_named("rlog")?,
            commit: a.rel_named("commit")?,
            won: a.rel_named("won")?,
            rterm: a.rel_named("rterm")?,
            match_idx: a.rel_named("match_idx")?,
            rsnap: a.rel_named("rsnap"),
            winners: BTreeMap::new(),
            committed: BTreeMap::new(),
            progress: BTreeMap::new(),
            last: i64::MIN,
            checked: BTreeMap::new(),
        })
    }

    /// Wraps the observer so a test can read it after the run.
    pub fn shared(self) -> (Rc<RefCell<GroupSafety>>, Box<dyn Observer>) {
        let rc = Rc::new(RefCell::new(self));
        (rc.clone(), Box::new(SharedSafety(rc)))
    }

    /// A leader's record of a follower is true: while the follower is in the leader's term, its log agrees with the
    /// leader's at the index the leader recorded (an acknowledgement is sent only after the entries are durable, and
    /// in one term only that term's leader changes the follower's log). A record kept from an earlier leadership
    /// breaks this (the S4 review's bug).
    fn followers_are_as_recorded(&self, nodes: &[Option<&Instance>]) -> Result<(), String> {
        // Per broker: each group's term, each (group, index)'s entry term, and each group's snapshot point.
        type Terms = (BTreeMap<Value, u64>, BTreeMap<(Value, u64), u64>, BTreeMap<Value, u64>);
        let mut views: Vec<Option<Terms>> = Vec::new();
        for s in nodes {
            views.push(match s {
                None => None,
                Some(state) => {
                    let mut terms = BTreeMap::new();
                    for r in state.rows(self.rterm) {
                        terms.insert(group(r)?, u64_at(r, 1)?);
                    }
                    let mut entries = BTreeMap::new();
                    for r in state.rows(self.rlog) {
                        entries.insert((group(r)?, u64_at(r, 1)?), u64_at(r, 2)?);
                    }
                    let mut snaps = BTreeMap::new();
                    if let Some(rs) = self.rsnap {
                        for r in state.rows(rs) {
                            snaps.insert(group(r)?, u64_at(r, 1)?);
                        }
                    }
                    Some((terms, entries, snaps))
                }
            });
        }
        for (n, state) in nodes.iter().enumerate() {
            let (Some(state), Some(Some((terms, entries, snaps)))) = (state, views.get(n)) else {
                continue;
            };
            for w in state.rows(self.won) {
                let (g, t) = (group(w)?, u64_at(w, 1)?);
                if terms.get(&g).copied() != Some(t) {
                    continue;
                }
                for m in state.rows(self.match_idx).filter(|r| r.first() == Some(&g)) {
                    let Some(Value::Node(f)) = m.get(1) else { continue };
                    let i = u64_at(m, 2)?;
                    let Some(Some((fterms, fentries, fsnaps))) = views.get(f.0 as usize) else {
                        continue;
                    };
                    if i == 0 || fterms.get(&g).copied() != Some(t) {
                        continue;
                    }
                    // An entry compacted on either side is committed: nothing to compare.
                    if snaps.get(&g).is_some_and(|s| i <= *s) || fsnaps.get(&g).is_some_and(|s| i <= *s) {
                        continue;
                    }
                    let mine = entries.get(&(g.clone(), i));
                    let theirs = fentries.get(&(g.clone(), i));
                    if mine.is_none() || mine != theirs {
                        return Err(format!(
                            "broker {n} leads {g:?} in term {t} and records that broker {} holds its entry {i} (term \
                             {mine:?}), which holds {theirs:?}",
                            f.0
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

impl Observer for GroupSafety {
    fn due(&self, now: i64) -> bool {
        now >= self.last.saturating_add(EVERY)
    }

    fn observe(&mut self, now: i64, nodes: &[Option<&Instance>]) -> Result<(), String> {
        self.last = now;
        self.followers_are_as_recorded(nodes)?;
        // The groups some live broker holds entries of. A group with committed entries that none holds was deleted
        // (a deleted Kafka topic's partitions go from every replica), or every broker holding it is down: a broker
        // with no entries of it is not asked to have them then.
        let mut present: BTreeSet<Value> = BTreeSet::new();
        for state in nodes.iter().flatten() {
            present.extend(state.rows(self.rlog).filter_map(|r| r.first().cloned()));
        }
        let gone: BTreeSet<Value> = self
            .committed
            .keys()
            .map(|k| k.0.clone())
            .filter(|g| !present.contains(g))
            .collect();
        for (n, state) in nodes.iter().enumerate() {
            let Some(state) = state else { continue };
            // Per group: its term here, its log (index → row), its commit index.
            let mut terms: BTreeMap<Value, u64> = BTreeMap::new();
            for r in state.rows(self.rterm) {
                terms.insert(group(r)?, u64_at(r, 1)?);
            }
            let mut logs: BTreeMap<Value, BTreeMap<u64, Row>> = BTreeMap::new();
            for r in state.rows(self.rlog) {
                logs.entry(group(r)?).or_default().insert(u64_at(r, 1)?, r.clone());
            }
            // Per group: its snapshot point (index, term).
            let mut snaps: BTreeMap<Value, (u64, u64)> = BTreeMap::new();
            if let Some(rs) = self.rsnap {
                for r in state.rows(rs) {
                    snaps.insert(group(r)?, (u64_at(r, 1)?, u64_at(r, 2)?));
                }
            }
            for (g, log) in &logs {
                let snap = snaps.get(g).copied();
                for (i, entry) in log {
                    let before = if *i == 1 {
                        Some(0)
                    } else if snap.is_some_and(|s| s.0 + 1 == *i) {
                        snap.map(|s| s.1)
                    } else {
                        log.get(&(i - 1)).map(|r| u64_at(r, 2)).transpose()?
                    };
                    // A hole above the snapshot point: entries are stored after their predecessor, and only
                    // truncated from an index to the end.
                    if *i > 1 && before.is_none() && snap.is_none_or(|s| s.0 + 1 < *i) {
                        return Err(format!("broker {n} group {g:?}: entry {i} without entry {}", i - 1));
                    }
                    let prev = u64_at(entry, 3)?;
                    if let Some(pt) = before
                        && prev != pt
                    {
                        return Err(format!(
                            "broker {n} group {g:?}: entry {i} records prev {prev}, but the entry before has term {pt}"
                        ));
                    }
                }
            }
            for r in state.rows(self.won) {
                let key = (group(r)?, u64_at(r, 1)?);
                if let Some(other) = self.winners.insert(key.clone(), n)
                    && other != n
                {
                    return Err(format!("election safety: brokers {other} and {n} both won {key:?}"));
                }
            }
            let empty = BTreeMap::new();
            for r in state.rows(self.commit) {
                let g = group(r)?;
                let commit = u64_at(r, 1)?;
                let log = logs.get(&g).unwrap_or(&empty);
                if log.is_empty() && gone.contains(&g) {
                    continue;
                }
                let term = terms.get(&g).copied().unwrap_or(0);
                let p = self.progress.entry(g.clone()).or_insert(0);
                *p = (*p).max(commit);
                // Entries this broker committed earlier were checked then; a committed entry that changed later
                // would also break log matching or leader completeness, which are checked in full.
                // A restarted broker relearns its commit index: what it commits again is checked again.
                let from = self.checked.get(&(n, g.clone())).copied().unwrap_or(0).min(commit);
                self.checked.insert((n, g.clone()), commit);
                let snap = snaps.get(&g).map_or(0, |s| s.0);
                for i in from.min(commit) + 1..=commit {
                    if i <= snap {
                        continue;
                    }
                    let Some(entry) = log.get(&i) else {
                        return Err(format!(
                            "broker {n} committed {g:?} through {commit} but holds no entry {i}"
                        ));
                    };
                    match self.committed.get(&(g.clone(), i)) {
                        None => {
                            self.committed.insert((g.clone(), i), (entry.clone(), term));
                        }
                        Some((seen, _)) if seen != entry => {
                            return Err(format!(
                                "state machine safety: broker {n} committed {entry:?} at {i}, where {seen:?} was"
                            ));
                        }
                        Some(_) => {}
                    }
                }
            }
            // Leader completeness: a leader of term T holds every entry committed in an earlier term.
            for r in state.rows(self.won) {
                let g = group(r)?;
                let t = u64_at(r, 1)?;
                let log = logs.get(&g).unwrap_or(&empty);
                if terms.get(&g).copied() != Some(t) || (log.is_empty() && gone.contains(&g)) {
                    continue;
                }
                let snap = snaps.get(&g).map_or(0, |s| s.0);
                for ((cg, i), (entry, at)) in &self.committed {
                    if *cg == g && *at < t && *i > snap && log.get(i) != Some(entry) {
                        return Err(format!(
                            "leader completeness: broker {n} leads {g:?} in term {t} without {entry:?} at {i}"
                        ));
                    }
                }
            }
        }
        // A snapshot point is a committed entry: no broker compacts (or installs) past what was committed, and the
        // point's term is that entry's.
        if let Some(rs) = self.rsnap {
            for (n, state) in nodes.iter().enumerate() {
                let Some(state) = state else { continue };
                for r in state.rows(rs) {
                    let (g, i, t) = (group(r)?, u64_at(r, 1)?, u64_at(r, 2)?);
                    if self.progress.get(&g).is_none_or(|c| i > *c) {
                        return Err(format!(
                            "broker {n}'s snapshot point {i} of {g:?} is past every commit seen"
                        ));
                    }
                    if let Some((entry, _)) = self.committed.get(&(g.clone(), i))
                        && u64_at(entry, 2)? != t
                    {
                        return Err(format!(
                            "broker {n}'s snapshot point {i} of {g:?} has term {t}, committed {entry:?}"
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

/// The observer, shared so the test reads its progress afterwards.
struct SharedSafety(Rc<RefCell<GroupSafety>>);

impl Observer for SharedSafety {
    fn due(&self, now: i64) -> bool {
        self.0.borrow().due(now)
    }

    fn observe(&mut self, now: i64, nodes: &[Option<&Instance>]) -> Result<(), String> {
        self.0.borrow_mut().observe(now, nodes)
    }
}
