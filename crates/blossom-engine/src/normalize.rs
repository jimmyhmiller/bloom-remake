//! Expose independent, infallible conjuncts to the join scheduler. Their original order
//! remains intact; the scheduler can test a bound prefix before fetching later atoms.
//! Fallible guards remain whole: splitting one could change LANGUAGE §9.14's error order.
use blossom_base::internal_error;
use blossom_ir::{
    ValidatedProgram,
    core::{BinOp, Expr, Literal},
    tick::EvalError,
};

pub(crate) fn guards(program: &ValidatedProgram) -> Result<ValidatedProgram, EvalError> {
    fn split(e: Expr, out: &mut Vec<Literal>) {
        match e {
            Expr::Binary {
                op: BinOp::And,
                lhs,
                rhs,
            } => {
                split(*lhs, out);
                split(*rhs, out);
            }
            e => out.push(Literal::Guard(e)),
        }
    }
    let mut p = program.get().clone();
    let mut changed = false;
    for rule in p.rules.iter_mut() {
        let mut lits = Vec::new();
        for lit in std::mem::take(&mut rule.body.lits) {
            match lit {
                Literal::Guard(e @ Expr::Binary { op: BinOp::And, .. }) if e.cannot_fail() => {
                    changed = true;
                    split(e, &mut lits);
                }
                lit => lits.push(lit),
            }
        }
        rule.body.lits = lits;
    }
    if !changed {
        return Ok(program.clone());
    }
    ValidatedProgram::validate(p).map_err(|errors| internal_error!("invalid guard normalization: {errors:?}").into())
}
