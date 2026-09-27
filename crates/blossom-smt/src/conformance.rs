//! Solver conformance scripts shared by Z3 and cvc5.
use crate::{SmtAnswer, SmtError, SmtSolver, Sort, Term};
use std::time::Duration;
/// Exercise incremental scopes, models, assumptions and named unsat cores.
pub fn smt_suite(s: &mut dyn SmtSolver) -> Result<(), SmtError> {
    s.set_logic("QF_LIA")?;
    s.declare_fun("x", &[], &Sort::named("Int"))?;
    s.assert(&Term::app(">", [Term::symbol("x"), Term::int(0)]), Some("positive"))?;
    if s.check(&[], Duration::from_secs(5))? != SmtAnswer::Sat {
        return Err(SmtError::Process("positive assertion should be SAT".into()));
    }
    if !s.model()?.declarations.contains_key("x") {
        return Err(SmtError::Process("model lacks x".into()));
    }
    s.push()?;
    s.assert(&Term::app("<", [Term::symbol("x"), Term::int(0)]), Some("negative"))?;
    if s.check(&[], Duration::from_secs(5))? != SmtAnswer::Unsat {
        return Err(SmtError::Process("contradictory assertions should be UNSAT".into()));
    }
    let core = s.unsat_core()?;
    if !core.iter().any(|x| x.as_ref() == "positive") || !core.iter().any(|x| x.as_ref() == "negative") {
        return Err(SmtError::Process(format!("incomplete unsat core: {core:?}")));
    }
    s.pop(1)?;
    if s.check(&[], Duration::from_secs(5))? != SmtAnswer::Sat {
        return Err(SmtError::Process("pop failed to restore SAT".into()));
    }
    Ok(())
}
