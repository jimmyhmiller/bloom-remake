//! Consecutive independent rules share an immutable snapshot while workers evaluate them.
//! Publication stays in the original stratum/rule order. Recursive or lattice strata and every
//! effectful rule are barriers; retained maps keep their coordinator-owned incremental cache.
use super::*;

#[derive(Clone)]
pub(super) enum Stage {
    Serial(Stratum),
    Independent(Vec<RuleId>),
}

struct Job<'a> {
    rule: &'a Rule,
    plan: Arc<Plan>,
    drivers: Vec<(Driver, usize)>,
    orders: Vec<(Option<usize>, Arc<rule::Order>)>,
}

impl Engine {
    pub(super) fn make_stages(&self) -> Vec<Stage> {
        let p = self.program.get();
        let mut stages = Vec::new();
        let mut ids = Vec::new();
        let mut reads = BTreeSet::new();
        let mut writes = BTreeSet::new();
        let temporal = Stratum {
            aggregates: Vec::new(),
            rules: self.inductive.iter().chain(self.asynchronous.iter()).copied().collect(),
            recursive: false,
        };
        for s in self.strata.iter().chain(std::iter::once(&temporal)) {
            // Settling a lattice early can expose a transient conflict. Preserve its entire stratum.
            let lattice = s.aggregates.iter().chain(&s.rules).any(|id| {
                self.plans
                    .get(id)
                    .and_then(|plan| self.stores.get(&plan.head))
                    .is_some_and(|s| s.cell.is_some())
            });
            if s.recursive || lattice {
                flush(&mut stages, &mut ids, &mut reads, &mut writes);
                stages.push(Stage::Serial(s.clone()));
                continue;
            }
            for id in s.aggregates.iter().chain(&s.rules) {
                let Some(plan) = self.plans.get(id) else {
                    continue;
                };
                let eligible = self.parallel_rules.contains(id)
                    && !self.map_rules.contains(id)
                    && plan.regime != Regime::Recompute
                    && plan.copy.is_none()
                    && plan.upsert_delete.is_none()
                    && plan.frame.is_none()
                    && p.rules
                        .get(*id)
                        .is_some_and(|r| !self.stores.is_alias(StoreKey::Main(r.head.rel)));
                if !eligible {
                    flush(&mut stages, &mut ids, &mut reads, &mut writes);
                    stages.push(Stage::Serial(Stratum {
                        aggregates: Vec::new(),
                        rules: vec![*id],
                        recursive: false,
                    }));
                    continue;
                }
                let head = self.stores.physical(plan.head);
                let deps: BTreeSet<_> = plan.dep_keys.iter().map(|k| self.stores.physical(*k)).collect();
                if ids.len() == 64 || writes.contains(&head) || reads.contains(&head) || !writes.is_disjoint(&deps) {
                    flush(&mut stages, &mut ids, &mut reads, &mut writes);
                }
                ids.push(*id);
                writes.insert(head);
                reads.extend(deps);
            }
        }
        flush(&mut stages, &mut ids, &mut reads, &mut writes);
        stages
    }

    pub(super) fn run_stages(&mut self, p: &Program, input: &StepInput<'_>) -> Result<(), EvalError> {
        let stages = self.stages.clone();
        for stage in stages.iter() {
            match stage {
                Stage::Serial(s) if s.recursive => self.recursive_stratum(p, input, s)?,
                Stage::Serial(s) => {
                    for id in &s.aggregates {
                        self.maintain(p, input, *id)?;
                    }
                    self.settle(input.tick)?;
                    for id in &s.rules {
                        self.maintain(p, input, *id)?;
                    }
                }
                Stage::Independent(ids) => self.maintain_independent(p, input, ids)?,
            }
            self.settle(input.tick)?;
        }
        Ok(())
    }

    fn maintain_independent(&mut self, p: &Program, input: &StepInput<'_>, ids: &[RuleId]) -> Result<(), EvalError> {
        let active = ids
            .iter()
            .copied()
            .map(|id| self.rule_active(p, input, id).map(|live| (id, live)))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter_map(|(id, live)| live.then_some(id))
            .collect::<Vec<_>>();
        if active.len() < 2 {
            for id in active {
                self.maintain(p, input, id)?;
            }
            return Ok(());
        }
        let mut jobs = Vec::with_capacity(active.len());
        let mut prepare_error = None;
        for id in active {
            let rule = p
                .rules
                .get(id)
                .ok_or_else(|| internal_error!("scheduled rule {id:?}"))?;
            let plan = self
                .plans
                .get(&id)
                .ok_or_else(|| internal_error!("scheduled plan {id:?}"))?
                .clone();
            let full = plan.regime == Regime::Scoped || (self.rebuilding && !self.view_rules.contains(&id));
            let drivers = match self.evaluation_drivers(p, input, rule, &plan, full) {
                Ok(drivers) => drivers,
                Err(error) => {
                    // A later failed probe must not mask an earlier rule's evaluation/apply error.
                    prepare_error = Some(error);
                    break;
                }
            };
            let orders = self.driver_orders(rule, &plan, &drivers);
            jobs.push(Job {
                rule,
                plan,
                drivers,
                orders,
            });
        }
        let work = jobs
            .iter()
            .map(|job| {
                if job.drivers.iter().any(|(d, _)| matches!(d, Driver::Full)) {
                    job.plan
                        .atoms
                        .iter()
                        .filter_map(|i| match job.rule.body.lits.get(*i) {
                            Some(Literal::Pos(a)) => self.stores.get(&rule::atom_store(a)).map(Store::present_len),
                            _ => None,
                        })
                        .max()
                        .unwrap_or(1)
                } else {
                    job.drivers.len()
                }
            })
            .sum::<usize>();
        let results = {
            let created = self.new_blobs.borrow();
            let read = ParallelRead {
                p,
                input,
                node: self.node,
                shared: &self.shared,
                stores: &self.stores,
                created: &created,
                profile: self.fn_work.is_some(),
            };
            let run = |job: &Job<'_>| read.run(job.rule, &job.plan, &job.drivers, &job.orders);
            if work >= PARALLEL_GRAIN * 2
                && let Some(pool) = &self.workers
            {
                use rayon::prelude::*;
                let terms = jobs.iter().map(|job| job.drivers.len() as u64).sum::<u64>();
                self.parallel_terms.set(self.parallel_terms.get() + terms);
                pool.install(|| jobs.par_iter().map(run).collect::<Vec<_>>())
            } else {
                jobs.iter().map(run).collect::<Vec<_>>()
            }
        };
        // An error from a later worker never masks an earlier rule's error or publishes its output.
        for (Job { rule, plan, .. }, result) in jobs.into_iter().zip(results) {
            let (terms, functions) = result?;
            if let Some(profile) = &self.fn_work {
                let mut profile = profile.borrow_mut();
                for (id, w) in functions {
                    let entry = profile.entry(id).or_default();
                    entry.calls += w.calls;
                    entry.steps += w.steps;
                    entry.self_steps += w.self_steps;
                }
            }
            self.count(rule.id, terms.examined, terms.steps);
            self.apply(p, input, rule, &plan, terms)?;
        }
        if let Some(error) = prepare_error {
            return Err(error);
        }
        Ok(())
    }
}

fn flush(
    stages: &mut Vec<Stage>,
    ids: &mut Vec<RuleId>,
    reads: &mut BTreeSet<StoreKey>,
    writes: &mut BTreeSet<StoreKey>,
) {
    if !ids.is_empty() {
        stages.push(Stage::Independent(std::mem::take(ids)));
    }
    reads.clear();
    writes.clear();
}
