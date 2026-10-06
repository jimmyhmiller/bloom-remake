//! The inspector (BROWSER.md "The inspector"): why an element is on the page.
//!
//! The engine runs the rounds and keeps no provenance; the oracle re-runs a round on demand with capture on, from the
//! state the round started from and its events (the host's [`History`]). An element's rows (`elem`, `attr`, `text`)
//! are explained by the firings that derived them, recursively through the round's views, down to its event and the
//! table rows it read; a table row leads back to the round that wrote it. Rows of generated relations (the expansion
//! of a statement) are provenance-transparent: their reasons stand in for them.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use blossom_base::{RelId, RuleId};
use blossom_ir::core::{Origin, Persistence, Program};
use blossom_ir::tick::{Changes, Instance, Row, TickInput, TickOutput};
use blossom_oracle::{Limits, Oracle};
use blossom_value::time::{Instant, NodeId, Tick};
use serde::Serialize;

use crate::HostError;

/// The rounds the inspector can explain: the latest [`History::KEEP`]. It keeps the state the oldest of them started
/// from and what each changed, so a round costs the host only its changes; the state a round started from is rebuilt
/// when the inspector asks ([`History::before`]).
pub struct History {
    /// The carried state the oldest kept round started from.
    base: Instance,
    rounds: VecDeque<Round>,
}

/// One round: its events, and how it changed the state the next round starts from.
pub struct Round {
    pub tick: u64,
    /// The clock the round ran at.
    pub now: Instant,
    pub events: Vec<(RelId, Row)>,
    pub changes: Changes,
    /// The rows it wrote (`changes`' insertions).
    pub inserted: BTreeSet<(RelId, Row)>,
}

impl History {
    /// How many rounds are kept.
    pub const KEEP: usize = 500;

    pub fn new() -> History {
        History {
            base: Instance::default(),
            rounds: VecDeque::new(),
        }
    }

    /// Records a round. The oldest kept one, past [`History::KEEP`], folds into the base state.
    pub fn push(&mut self, round: Round) {
        if self.rounds.len() == History::KEEP
            && let Some(oldest) = self.rounds.pop_front()
        {
            oldest.changes.apply(&mut self.base);
        }
        self.rounds.push_back(round);
    }

    /// Forgets every round: the next starts from `base`.
    pub fn reset(&mut self, base: Instance) {
        self.base = base;
        self.rounds.clear();
    }

    /// The carried state round `tick` started from, rebuilt from the base and the changes of the rounds before it.
    fn before(&self, tick: u64) -> Option<Instance> {
        let mut state = self.base.clone();
        for r in &self.rounds {
            if r.tick == tick {
                return Some(state);
            }
            r.changes.apply(&mut state);
        }
        None
    }

    fn get(&self, tick: u64) -> Option<&Round> {
        self.rounds.iter().find(|r| r.tick == tick)
    }

    fn last(&self) -> Option<&Round> {
        self.rounds.back()
    }
}

impl Default for History {
    fn default() -> History {
        History::new()
    }
}

/// Why a fact holds: what made it (a rule, an event, a write of an earlier round), and the facts that made that.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Why {
    /// The fact, as Blossom writes it: `todos(0, "Buy milk", false)`.
    pub fact: String,
    /// What made it: `rule add`, `the event of round 7`, `written in round 7 by rule add`, ….
    pub how: String,
    /// The round it held in.
    pub round: u64,
    pub because: Vec<Why>,
}

/// The deepest an explanation goes.
const DEPTH: usize = 24;

/// Explains facts of the rounds in a history, re-running each round it needs once.
pub struct Explainer<'a> {
    program: &'a Program,
    oracle: Oracle,
    history: &'a History,
    runs: RefCell<BTreeMap<u64, Arc<TickOutput>>>,
    /// The state each round started from, rebuilt once per round asked about.
    befores: RefCell<BTreeMap<u64, Arc<Instance>>>,
    /// The facts whose explanation was started (once: later ones refer to it), and those finished.
    started: RefCell<BTreeSet<(RelId, Row, u64)>>,
    done: RefCell<BTreeMap<(RelId, Row, u64), Found>>,
}

/// A fact's explanation, and the rules that made it (a generated fact's: the rules its reasons came from).
#[derive(Clone)]
struct Found {
    whys: Vec<Why>,
    rules: Vec<String>,
}

/// A firing: the rules it stands for, and its reasons.
struct Fired {
    rules: Vec<String>,
    because: Vec<Why>,
}

/// "rule `a`", or "rules `a`, `b`".
fn names(rules: &[String]) -> String {
    let quoted: Vec<String> = rules.iter().map(|r| format!("`{r}`")).collect();
    match quoted.len() {
        1 => format!("rule {}", quoted.join("")),
        _ => format!("rules {}", quoted.join(", ")),
    }
}

impl<'a> Explainer<'a> {
    pub fn new(
        program: &'a Program,
        oracle: blossom_ir::ValidatedProgram,
        roles: Vec<Option<blossom_base::RoleId>>,
        seed: blossom_value::Seed,
        history: &'a History,
    ) -> Result<Explainer<'a>, HostError> {
        let fail = |e: blossom_oracle::OracleError| HostError::Round {
            tick: 0,
            error: e.to_string(),
        };
        let oracle = Oracle::with_externs(
            oracle,
            Limits::default(),
            Arc::new(blossom_value::ExternRegistry::default()),
        )
        .map_err(fail)?
        .with_roles(roles)
        .with_seed(seed)
        .map_err(fail)?
        .with_node_names(vec![Arc::from("app")])
        .map_err(fail)?;
        Ok(Explainer {
            program,
            oracle,
            history,
            runs: RefCell::new(BTreeMap::new()),
            befores: RefCell::new(BTreeMap::new()),
            started: RefCell::new(BTreeSet::new()),
            done: RefCell::new(BTreeMap::new()),
        })
    }

    /// Round `tick`, re-run with its firings.
    fn run(&self, tick: u64) -> Result<Option<Arc<TickOutput>>, HostError> {
        if let Some(out) = self.runs.borrow().get(&tick) {
            return Ok(Some(Arc::clone(out)));
        }
        let (Some(round), Some(before)) = (self.history.get(tick), self.before(tick)) else {
            return Ok(None);
        };
        let out = self
            .oracle
            .tick(&TickInput {
                node: NodeId(0),
                incarnation: 1,
                tick: Tick(tick),
                now: round.now,
                carried: &before,
                events: &round.events,
                delivered: &[],
                ingress: &[],
                capture: true,
                blobs: &blossom_value::NoBlobs,
            })
            .map_err(|e| HostError::Round {
                tick,
                error: e.to_string(),
            })?;
        let out = Arc::new(out);
        self.runs.borrow_mut().insert(tick, Arc::clone(&out));
        Ok(Some(out))
    }

    /// The carried state round `tick` started from.
    fn before(&self, tick: u64) -> Option<Arc<Instance>> {
        if let Some(b) = self.befores.borrow().get(&tick) {
            return Some(Arc::clone(b));
        }
        let b = Arc::new(self.history.before(tick)?);
        self.befores.borrow_mut().insert(tick, Arc::clone(&b));
        Some(b)
    }

    /// `rel(row)`, as Blossom writes it.
    fn fact(&self, rel: RelId, row: &Row) -> String {
        let Some(decl) = self.program.rels.get(rel) else {
            return format!("{rel:?}{row:?}");
        };
        let values: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let ty = decl.schema.cols.get(i).map(|c| c.ty);
                blossom_ir::printer::value_text(Some(self.program), v, ty, &|n| format!("node {}", n.0))
            })
            .collect();
        format!("{}({})", decl.name, values.join(", "))
    }

    /// A rule's name, as the program writes it: its statement's label (a generated rule's, without its expansion's
    /// suffix).
    fn rule(&self, rule: RuleId) -> String {
        let Some(r) = self.program.rules.get(rule) else {
            return format!("{rule:?}");
        };
        if let Some(label) = r
            .construct
            .and_then(|c| self.program.constructs.get(c))
            .and_then(|c| c.surface.label)
        {
            return label.as_str().to_owned();
        }
        let text = r.label.text.as_ref();
        text.split(['$', '/', '#']).next().unwrap_or(text).to_owned()
    }

    /// Whether a rule is part of a construct's expansion with no statement of its own (an `upsert`'s write): the rules
    /// that derived the generated rows it read are what made its head.
    fn transparent(&self, rule: RuleId) -> bool {
        self.program
            .rules
            .get(rule)
            .and_then(|r| r.construct)
            .and_then(|c| self.program.constructs.get(c))
            .is_some_and(|c| c.surface.label.is_none())
    }

    fn generated(&self, rel: RelId) -> bool {
        self.program
            .rels
            .get(rel)
            .is_some_and(|r| matches!(r.origin, Origin::Generated { .. }))
    }

    /// Whether `rule` is a table's frame rule (a row kept from one round to the next).
    fn frame(&self, rule: RuleId) -> bool {
        self.program
            .rules
            .get(rule)
            .and_then(|r| self.program.rels.get(r.head.rel))
            .is_some_and(|rel| match rel.persistence {
                Persistence::Frame { rule: f, .. } | Persistence::Identity { rule: f } => f == rule,
                _ => false,
            })
    }

    /// Whether a rule's heads hold in the round it fires in (else in the next: an inductive rule's write).
    fn same_round(&self, rule: RuleId) -> bool {
        self.program
            .rules
            .get(rule)
            .is_some_and(|r| r.kind != blossom_ir::core::RuleKind::Inductive)
    }

    /// Why `rel(row)` holds in round `tick`.
    pub fn why(&self, rel: RelId, row: &Row, tick: u64, depth: usize) -> Result<Vec<Why>, HostError> {
        Ok(self.explain(rel, row, tick, depth)?.whys)
    }

    fn explain(&self, rel: RelId, row: &Row, tick: u64, depth: usize) -> Result<Found, HostError> {
        let fact = self.fact(rel, row);
        let leaf = |how: &str| Found {
            whys: vec![Why {
                fact: fact.clone(),
                how: how.to_owned(),
                round: tick,
                because: Vec::new(),
            }],
            rules: Vec::new(),
        };
        if depth >= DEPTH {
            return Ok(leaf("(deeper reasons not shown)"));
        }
        let key = (rel, Arc::clone(row), tick);
        if self.started.borrow().contains(&key) {
            // Explained above: a generated fact stands for its reasons, so those are what is referred to.
            return Ok(match self.done.borrow().get(&key) {
                Some(found) if self.generated(rel) => Found {
                    whys: found
                        .whys
                        .iter()
                        .map(|w| Why {
                            fact: w.fact.clone(),
                            how: "(explained above)".to_owned(),
                            round: w.round,
                            because: Vec::new(),
                        })
                        .collect(),
                    rules: found.rules.clone(),
                },
                Some(found) => Found {
                    rules: found.rules.clone(),
                    ..leaf("(explained above)")
                },
                None => leaf("(explained above)"),
            });
        }
        self.started.borrow_mut().insert(key.clone());
        let found = self.derive(rel, row, tick, depth, &leaf)?;
        self.done.borrow_mut().insert(key, found.clone());
        Ok(found)
    }

    fn derive(
        &self,
        rel: RelId,
        row: &Row,
        tick: u64,
        depth: usize,
        leaf: &dyn Fn(&str) -> Found,
    ) -> Result<Found, HostError> {
        let Some(round) = self.history.get(tick) else {
            return Ok(leaf("(before the rounds the inspector keeps)"));
        };
        if round.events.iter().any(|(r, x)| *r == rel && x == row) {
            return Ok(leaf(&format!("the event of round {tick}")));
        }
        let Some(out) = self.run(tick)? else {
            return Ok(leaf("(before the rounds the inspector keeps)"));
        };
        // Derived this round (not a table's frame: a kept row is explained by the round that wrote it).
        let mut fired = Vec::new();
        for f in out.firings.iter() {
            let Some(r) = self.program.rules.get(f.rule) else {
                continue;
            };
            if r.head.rel == rel && f.head == *row && !self.frame(f.rule) && self.same_round(f.rule) {
                fired.push(self.firing(f, tick, depth)?);
            }
        }
        if !fired.is_empty() {
            return Ok(self.splice(rel, row, tick, fired));
        }
        // Kept: written by an earlier round.
        if self.before(tick).is_some_and(|b| b.rows(rel).any(|x| x == row)) {
            return self.written(rel, row, tick, depth);
        }
        Ok(leaf("(held: a static fact, or a fact of the program)"))
    }

    /// A firing: the rule that made its head, and why each row it read held.
    fn firing(&self, f: &blossom_ir::obs::FiringRecord, tick: u64, depth: usize) -> Result<Fired, HostError> {
        let mut because = Vec::new();
        let mut makers = Vec::new();
        for read in &f.reads {
            let found = self.explain(read.rel, &read.row, tick, depth + 1)?;
            because.extend(found.whys);
            if self.generated(read.rel) {
                makers.extend(found.rules);
            }
        }
        for neg in &f.negations {
            let pattern: Vec<String> = neg
                .pattern
                .iter()
                .map(|p| match p {
                    Some(v) => {
                        blossom_ir::printer::value_text(Some(self.program), v, None, &|n| format!("node {}", n.0))
                    }
                    None => "_".to_owned(),
                })
                .collect();
            let name = self
                .program
                .rels
                .get(neg.rel)
                .map(|r| r.name.to_string())
                .unwrap_or_default();
            // A generated relation is the expansion's; say whose.
            let how = match self.program.rels.get(neg.rel).map(|r| &r.origin) {
                Some(Origin::Generated { construct }) => {
                    match self.program.constructs.get(*construct).and_then(|c| c.surface.label) {
                        Some(label) => format!("nothing matched (a relation of `{label}`'s expansion)"),
                        None => "nothing matched (a relation of a statement's expansion)".to_owned(),
                    }
                }
                _ => "nothing matched".to_owned(),
            };
            because.push(Why {
                fact: format!("not {name}({})", pattern.join(", ")),
                how,
                round: tick,
                because: Vec::new(),
            });
        }
        let rules = if self.transparent(f.rule) && !makers.is_empty() {
            makers.dedup();
            makers
        } else {
            vec![self.rule(f.rule)]
        };
        Ok(Fired { rules, because })
    }

    /// The explanation of `rel(row)` from its firings in round `tick`: the fact once per firing, with the rule's
    /// reasons; a generated relation's fact is replaced by its reasons.
    fn splice(&self, rel: RelId, row: &Row, tick: u64, fired: Vec<Fired>) -> Found {
        let rules: Vec<String> = fired.iter().flat_map(|f| f.rules.iter().cloned()).collect();
        if self.generated(rel) {
            return Found {
                whys: fired.into_iter().flat_map(|f| f.because).collect(),
                rules,
            };
        }
        Found {
            whys: fired
                .into_iter()
                .map(|f| Why {
                    fact: self.fact(rel, row),
                    how: format!("{} in round {tick}", names(&f.rules)),
                    round: tick,
                    because: f.because,
                })
                .collect(),
            rules,
        }
    }

    /// Why a table row held in round `tick`: the round that wrote it (the latest one before, in the history), and its
    /// write.
    fn written(&self, rel: RelId, row: &Row, tick: u64, depth: usize) -> Result<Found, HostError> {
        let fact = self.fact(rel, row);
        let writer = self
            .history
            .rounds
            .iter()
            .rev()
            .filter(|r| r.tick < tick)
            .find(|r| r.inserted.contains(&(rel, Arc::clone(row))));
        let Some(writer) = writer else {
            return Ok(Found {
                whys: vec![Why {
                    fact,
                    how: format!(
                        "kept in round {tick}: restored from storage, or written before the rounds the inspector keeps"
                    ),
                    round: tick,
                    because: Vec::new(),
                }],
                rules: Vec::new(),
            });
        };
        let Some(out) = self.run(writer.tick)? else {
            return Err(HostError::Round {
                tick: writer.tick,
                error: "the round is in the history but could not be re-run".to_owned(),
            });
        };
        let mut fired = Vec::new();
        for f in out.firings.iter() {
            let Some(r) = self.program.rules.get(f.rule) else {
                continue;
            };
            if r.head.rel == rel && f.head == *row && !self.frame(f.rule) && !self.same_round(f.rule) {
                fired.push(self.firing(f, writer.tick, depth)?);
            }
        }
        let rules = fired.iter().flat_map(|f| f.rules.iter().cloned()).collect();
        Ok(Found {
            whys: fired
                .into_iter()
                .map(|f| Why {
                    fact: fact.clone(),
                    how: format!(
                        "kept in round {tick}, written in round {} by {}",
                        writer.tick,
                        names(&f.rules)
                    ),
                    round: tick,
                    because: f.because,
                })
                .collect(),
            rules,
        })
    }

    /// The rows of `rel` at the end of round `tick`.
    pub fn rows(&self, rel: RelId, tick: u64) -> Result<Vec<Row>, HostError> {
        Ok(self
            .run(tick)?
            .map(|out| out.instance.rows(rel).cloned().collect())
            .unwrap_or_default())
    }

    /// The last round, the one whose page is shown.
    pub fn last(&self) -> Option<u64> {
        self.history.last().map(|r| r.tick)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blossom_value::value::IntValue;

    fn row(n: u64) -> Row {
        Arc::from(vec![blossom_value::Value::Int(IntValue::U64(n))])
    }

    #[test]
    fn the_state_a_round_started_from_is_rebuilt_from_the_base_and_the_changes() {
        let rel = RelId::from_raw(0);
        // Round t inserts t and deletes t - 3: the state before round t is {t-3 .. t-1} (from 0).
        let start: Instance = {
            let mut i = Instance::default();
            i.insert(rel, row(1_000_000));
            i
        };
        let mut history = History::new();
        history.reset(start.clone());
        let mut states = vec![start.clone()];
        let mut state = start;
        let rounds = History::KEEP as u64 + 40;
        for t in 0..rounds {
            let mut changes = Changes::default();
            changes.inserted.insert(rel, vec![row(t)]);
            if t >= 3 {
                changes.deleted.insert(rel, vec![row(t - 3)]);
            }
            history.push(Round {
                tick: t,
                now: Instant(0),
                events: Vec::new(),
                changes: changes.clone(),
                inserted: BTreeSet::new(),
            });
            changes.apply(&mut state);
            states.push(state.clone());
        }
        // The oldest rounds have folded into the base; every kept round's state is the one it started from.
        assert!(history.before(0).is_none());
        let first = rounds - History::KEEP as u64;
        for t in first..rounds {
            assert_eq!(history.before(t).as_ref(), states.get(t as usize), "round {t}");
        }
    }
}
