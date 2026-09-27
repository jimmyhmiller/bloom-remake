//! Backend-independent deterministic SAT conformance suite.
use crate::backends::ExhaustiveSolver;
use crate::{SatError, SatOutcome, SatSolver, SolveLimits};
/// Compare small generated CNFs, assumptions, cores, incremental additions and complete
/// model enumeration with the independent exhaustive evaluator. No ambient randomness.
pub fn sat_suite(make: impl Fn() -> Box<dyn SatSolver>) -> Result<(), SatError> {
    let mut seed = 0x75431ae49u64;
    for _case in 0..128 {
        let mut actual = make();
        let mut oracle = ExhaustiveSolver::default();
        let vars = (0..5)
            .map(|_| {
                oracle.new_var();
                actual.new_var()
            })
            .collect::<Vec<_>>();
        let mut clauses = Vec::new();
        for _ in 0..12 {
            let mut clause = Vec::new();
            for _ in 0..3 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                let v = *vars
                    .get((seed >> 32) as usize % vars.len())
                    .ok_or(SatError::InvalidState("test variable"))?;
                clause.push(if seed & 1 == 0 { v.positive() } else { v.negative() });
            }
            actual.add_clause(&clause)?;
            oracle.add_clause(&clause)?;
            clauses.push(clause);
        }
        for assumptions in [
            vec![],
            vec![vars.first().ok_or(SatError::InvalidState("test variable"))?.positive()],
            vec![vars.first().ok_or(SatError::InvalidState("test variable"))?.negative()],
        ] {
            let expected = oracle.solve(&assumptions, &SolveLimits::default())?;
            let got = actual.solve(&assumptions, &SolveLimits::default())?;
            if got != expected {
                return Err(SatError::Backend("conformance verdict mismatch".into()));
            }
            if got == SatOutcome::Sat {
                for c in &clauses {
                    let mut valid = false;
                    for l in c {
                        valid |= actual.value(l.var())? != l.is_negative();
                    }
                    if !valid {
                        return Err(SatError::Backend("model violates a clause".into()));
                    }
                }
                for l in &assumptions {
                    if actual.value(l.var())? == l.is_negative() {
                        return Err(SatError::Backend("model violates assumption".into()));
                    }
                }
            } else if !assumptions.is_empty() {
                let mut core = Vec::new();
                for l in &assumptions {
                    if actual.failed_assumption(*l)? {
                        core.push(*l);
                    }
                }
                if oracle.solve(&core, &SolveLimits::default())? != SatOutcome::Unsat {
                    return Err(SatError::Backend("invalid assumption core".into()));
                }
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        loop {
            let expected = oracle.solve(&[], &SolveLimits::default())?;
            let got = actual.solve(&[], &SolveLimits::default())?;
            if got != expected {
                return Err(SatError::Backend("incremental verdict mismatch".into()));
            }
            if got == SatOutcome::Unsat {
                break;
            }
            let mut bits = 0u64;
            let mut block = Vec::new();
            for v in &vars {
                let value = actual.value(*v)?;
                if value {
                    bits |= 1 << v.0;
                }
                block.push(if value { v.negative() } else { v.positive() });
            }
            if !seen.insert(bits) {
                return Err(SatError::Backend("duplicate enumerated model".into()));
            }
            actual.add_clause(&block)?;
            oracle.add_clause(&block)?;
        }
    }
    Ok(())
}
