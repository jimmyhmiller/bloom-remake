//! Production CDCL backends, a bounded exhaustive oracle, and a DIMACS recorder.
use crate::{LimitHit, Lit, SatError, SatOutcome, SatSolver, SolveLimits, State, Var, budget::Budget};
use std::io::Write;

#[derive(Default)]
pub struct ExhaustiveSolver {
    state: State,
    clauses: Vec<Vec<Lit>>,
}
fn satisfied(l: Lit, bits: u64) -> bool {
    (bits >> l.var().0 & 1 != 0) != l.is_negative()
}
impl ExhaustiveSolver {
    fn exists_model(&self, assumptions: &[Lit], budget: &Budget, conflicts: &mut u64) -> Result<bool, LimitHit> {
        for bits in 0..(1u64 << self.state.vars) {
            if let Some(hit) = budget.hit(*conflicts) {
                return Err(hit);
            }
            if assumptions.iter().all(|l| satisfied(*l, bits))
                && self.clauses.iter().all(|c| c.iter().any(|l| satisfied(*l, bits)))
            {
                return Ok(true);
            }
            *conflicts += 1;
        }
        Ok(false)
    }
}
impl SatSolver for ExhaustiveSolver {
    fn backend(&self) -> &'static str {
        "exhaustive"
    }
    fn new_var(&mut self) -> Var {
        self.state.new_var()
    }
    fn add_clause(&mut self, lits: &[Lit]) -> Result<(), SatError> {
        self.state.check(lits)?;
        self.state.invalidate();
        self.clauses.push(lits.to_vec());
        Ok(())
    }
    fn solve(&mut self, assumptions: &[Lit], limits: &SolveLimits) -> Result<SatOutcome, SatError> {
        self.state.invalidate();
        self.state.check(assumptions)?;
        self.state.assumptions = assumptions.to_vec();
        let budget = Budget::new(limits);
        let mut conflicts = 0;
        let mut outcome = SatOutcome::Unsat;
        if self.state.vars > 24 {
            outcome = SatOutcome::Unknown(LimitHit::Variables);
        } else {
            for bits in 0..(1u64 << self.state.vars) {
                if let Some(hit) = budget.hit(conflicts) {
                    outcome = SatOutcome::Unknown(hit);
                    break;
                }
                if assumptions.iter().all(|l| satisfied(*l, bits))
                    && self.clauses.iter().all(|c| c.iter().any(|l| satisfied(*l, bits)))
                {
                    self.state.model = (0..self.state.vars).map(|v| bits >> v & 1 != 0).collect();
                    outcome = SatOutcome::Sat;
                    break;
                }
                conflicts += 1;
            }
        }
        if outcome == SatOutcome::Unsat {
            // Return an irreducible assumption core. An UNSAT formula with no
            // assumptions has an empty core; otherwise remove every assumption
            // whose absence still leaves the formula UNSAT.
            let mut core = assumptions.to_vec();
            let mut i = 0;
            while i < core.len() {
                let mut candidate = core.clone();
                candidate.remove(i);
                match self.exists_model(&candidate, &budget, &mut conflicts) {
                    Ok(true) => i += 1,
                    Ok(false) => core = candidate,
                    Err(hit) => {
                        outcome = SatOutcome::Unknown(hit);
                        break;
                    }
                }
            }
            if outcome == SatOutcome::Unsat {
                self.state.core = core;
            }
        }
        self.state.outcome = Some(outcome);
        Ok(outcome)
    }
    fn value(&self, v: Var) -> Result<bool, SatError> {
        self.state.value(v)
    }
    fn failed_assumption(&self, l: Lit) -> Result<bool, SatError> {
        self.state.failed(l)
    }
}
/// Records CNF without pretending to solve it. Solving returns an explicit backend error.
#[derive(Default)]
pub struct DimacsDump {
    vars: u32,
    clauses: Vec<Vec<Lit>>,
}
impl DimacsDump {
    pub fn write(&self, mut writer: impl Write) -> Result<(), SatError> {
        writeln!(writer, "p cnf {} {}", self.vars, self.clauses.len())?;
        for clause in &self.clauses {
            for l in clause {
                let n = i64::from(l.var().0) + 1;
                write!(writer, "{} ", if l.is_negative() { -n } else { n })?;
            }
            writeln!(writer, "0")?;
        }
        Ok(())
    }
}
impl SatSolver for DimacsDump {
    fn backend(&self) -> &'static str {
        "dimacs"
    }
    fn new_var(&mut self) -> Var {
        let v = Var(self.vars);
        self.vars += 1;
        v
    }
    fn add_clause(&mut self, lits: &[Lit]) -> Result<(), SatError> {
        for l in lits {
            if l.var().0 >= self.vars {
                return Err(SatError::InvalidVariable(l.var()));
            }
        }
        self.clauses.push(lits.to_vec());
        Ok(())
    }
    fn solve(&mut self, _: &[Lit], _: &SolveLimits) -> Result<SatOutcome, SatError> {
        Err(SatError::InvalidState("a solving backend; DIMACS only records clauses"))
    }
    fn value(&self, _: Var) -> Result<bool, SatError> {
        Err(SatError::InvalidState("a solving backend"))
    }
    fn failed_assumption(&self, _: Lit) -> Result<bool, SatError> {
        Err(SatError::InvalidState("a solving backend"))
    }
}

#[cfg(feature = "sat-cadical")]
mod cadical {
    use super::*;
    use rustsat::solvers::{ControlSignal, LimitConflicts, Solve, SolveIncremental, SolverResult, Terminate};
    use rustsat::types::{Lit as RLit, TernaryVal, Var as RVar};
    #[derive(Default)]
    pub struct CadicalSolver {
        inner: rustsat_cadical::CaDiCaL<'static, 'static>,
        state: State,
    }
    fn rl(l: Lit) -> RLit {
        RLit::new(l.var().0, l.is_negative())
    }
    fn error(e: impl std::fmt::Display) -> SatError {
        SatError::Backend(e.to_string())
    }
    impl SatSolver for CadicalSolver {
        fn backend(&self) -> &'static str {
            "cadical"
        }
        fn new_var(&mut self) -> Var {
            self.state.new_var()
        }
        fn add_clause(&mut self, lits: &[Lit]) -> Result<(), SatError> {
            self.state.check(lits)?;
            self.state.invalidate();
            self.inner
                .add_clause(lits.iter().copied().map(rl).collect())
                .map_err(error)
        }
        fn solve(&mut self, assumptions: &[Lit], limits: &SolveLimits) -> Result<SatOutcome, SatError> {
            self.state.invalidate();
            self.state.check(assumptions)?;
            self.state.assumptions = assumptions.to_vec();
            if self.state.vars > 0 {
                self.inner.reserve(RVar::new(self.state.vars - 1)).map_err(error)?;
            }
            let budget = std::sync::Arc::new(Budget::new(limits));
            if let Some(hit) = budget.hit(0) {
                let out = SatOutcome::Unknown(hit);
                self.state.outcome = Some(out);
                return Ok(out);
            }
            let conflict_limit = limits
                .conflicts
                .map(u32::try_from)
                .transpose()
                .map_err(|_| SatError::Backend("CaDiCaL conflict limit exceeds u32".into()))?;
            self.inner.limit_conflicts(conflict_limit).map_err(error)?;
            let callback_budget = budget.clone();
            self.inner.attach_terminator(move || {
                if callback_budget.hit(0) == Some(LimitHit::Time) {
                    ControlSignal::Terminate
                } else {
                    ControlSignal::Continue
                }
            });
            let result = self
                .inner
                .solve_assumps(&assumptions.iter().copied().map(rl).collect::<Vec<_>>());
            self.inner.detach_terminator();
            let outcome = match result.map_err(error)? {
                SolverResult::Sat => {
                    self.state.model = (0..self.state.vars)
                        .map(|v| {
                            self.inner
                                .var_val(RVar::new(v))
                                .map(|x| x == TernaryVal::True)
                                .map_err(error)
                        })
                        .collect::<Result<_, _>>()?;
                    SatOutcome::Sat
                }
                SolverResult::Unsat => {
                    self.state.core = self
                        .inner
                        .core()
                        .map_err(error)?
                        .into_iter()
                        .map(|l| Lit::from_rustsat(l).negate())
                        .collect();
                    SatOutcome::Unsat
                }
                SolverResult::Interrupted => {
                    SatOutcome::Unknown(budget.hit(0).unwrap_or(if limits.conflicts.is_some() {
                        LimitHit::Conflicts
                    } else {
                        LimitHit::Interrupted
                    }))
                }
            };
            self.state.outcome = Some(outcome);
            Ok(outcome)
        }
        fn value(&self, v: Var) -> Result<bool, SatError> {
            self.state.value(v)
        }
        fn failed_assumption(&self, l: Lit) -> Result<bool, SatError> {
            self.state.failed(l)
        }
    }
    impl Lit {
        fn from_rustsat(l: RLit) -> Self {
            Lit(l.vidx32() * 2 + u32::from(l.is_neg()))
        }
    }
}
#[cfg(feature = "sat-cadical")]
pub use cadical::CadicalSolver;

#[cfg(feature = "sat-batsat")]
mod bat {
    use super::*;
    use batsat::{Callbacks, SolverInterface, lbool};
    #[derive(Default)]
    struct Limits {
        budget: Option<Budget>,
        conflicts: u64,
    }
    impl Callbacks for Limits {
        fn on_new_clause(&mut self, _: &[batsat::Lit], kind: batsat::clause::Kind) {
            if kind == batsat::clause::Kind::Learnt {
                self.conflicts += 1;
            }
        }
        fn stop(&self) -> bool {
            self.budget.as_ref().is_some_and(|b| b.hit(self.conflicts).is_some())
        }
    }
    #[derive(Default)]
    pub struct BatSolver {
        inner: batsat::Solver<Limits>,
        state: State,
    }
    fn bl(l: Lit) -> batsat::Lit {
        use batsat::intmap::AsIndex;
        batsat::Lit::new(batsat::Var::from_index(l.var().0 as usize), !l.is_negative())
    }
    impl SatSolver for BatSolver {
        fn backend(&self) -> &'static str {
            "batsat"
        }
        fn new_var(&mut self) -> Var {
            self.inner.new_var_default();
            self.state.new_var()
        }
        fn add_clause(&mut self, lits: &[Lit]) -> Result<(), SatError> {
            self.state.check(lits)?;
            self.state.invalidate();
            self.inner.add_clause_reuse(&mut lits.iter().copied().map(bl).collect());
            Ok(())
        }
        fn solve(&mut self, assumptions: &[Lit], limits: &SolveLimits) -> Result<SatOutcome, SatError> {
            self.state.invalidate();
            self.state.check(assumptions)?;
            self.state.assumptions = assumptions.to_vec();
            self.inner.cb_mut().budget = Some(Budget::new(limits));
            self.inner.cb_mut().conflicts = 0;
            let hit = self.inner.cb().budget.as_ref().and_then(|b| b.hit(0));
            let result = if hit.is_some() {
                lbool::UNDEF
            } else {
                self.inner
                    .solve_limited(&assumptions.iter().copied().map(bl).collect::<Vec<_>>())
            };
            let out = if result == lbool::TRUE {
                self.state.model = (0..self.state.vars)
                    .map(|v| self.inner.value_lit(bl(Var(v).positive())) == lbool::TRUE)
                    .collect();
                SatOutcome::Sat
            } else if result == lbool::FALSE {
                self.state.core = assumptions
                    .iter()
                    .copied()
                    .filter(|l| self.inner.unsat_core().contains(&bl(l.negate())))
                    .collect();
                SatOutcome::Unsat
            } else {
                SatOutcome::Unknown(
                    self.inner
                        .cb()
                        .budget
                        .as_ref()
                        .and_then(|b| b.hit(self.inner.cb().conflicts))
                        .unwrap_or(LimitHit::Interrupted),
                )
            };
            self.inner.cb_mut().budget = None;
            self.state.outcome = Some(out);
            Ok(out)
        }
        fn value(&self, v: Var) -> Result<bool, SatError> {
            self.state.value(v)
        }
        fn failed_assumption(&self, l: Lit) -> Result<bool, SatError> {
            self.state.failed(l)
        }
    }
}
#[cfg(feature = "sat-batsat")]
pub use bat::BatSolver;
