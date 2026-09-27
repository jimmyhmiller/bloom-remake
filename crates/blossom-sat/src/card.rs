//! Balanced unary totalizers. Output index j-1 means sum >= j; at most k+1
//! outputs are built so an upper bound k can be asserted with !outputs[k].
//! Outputs encode equivalence, allowing either polarity in assumptions.
use crate::{Lit, SatError, SatSolver};

pub fn totalizer(s: &mut dyn SatSolver, lits: &[Lit], k: u32) -> Result<Vec<Lit>, SatError> {
    generalized_totalizer(
        s,
        &lits.iter().copied().map(|l| (l, 1)).collect::<Vec<_>>(),
        u64::from(k),
    )
}
pub fn generalized_totalizer(s: &mut dyn SatSolver, lits: &[(Lit, u64)], k: u64) -> Result<Vec<Lit>, SatError> {
    let total = lits.iter().try_fold(0u64, |sum, (_, w)| {
        sum.checked_add(*w).ok_or(SatError::EncodingTooLarge)
    })?;
    let cap = usize::try_from(total.min(k.saturating_add(1))).map_err(|_| SatError::EncodingTooLarge)?;
    build(s, lits, cap)
}
fn build(s: &mut dyn SatSolver, lits: &[(Lit, u64)], cap: usize) -> Result<Vec<Lit>, SatError> {
    match lits {
        [] => Ok(vec![]),
        [(l, w)] => {
            let len = usize::try_from((*w).min(cap as u64)).map_err(|_| SatError::EncodingTooLarge)?;
            let mut out = Vec::new();
            out.try_reserve_exact(len).map_err(|_| SatError::EncodingTooLarge)?;
            out.resize(len, *l);
            Ok(out)
        }
        _ => {
            let (left, right) = lits.split_at(lits.len() / 2);
            let a = build(s, left, cap)?;
            let b = build(s, right, cap)?;
            let len = a.len().checked_add(b.len()).ok_or(SatError::EncodingTooLarge)?.min(cap);
            let mut out = Vec::new();
            out.try_reserve_exact(len).map_err(|_| SatError::EncodingTooLarge)?;
            for _ in 0..len {
                out.push(s.new_var().positive());
            }
            for i in 0..=a.len() {
                for j in 0..=b.len() {
                    let sum = i + j;
                    if sum > 0 && sum <= len {
                        let mut clause = Vec::with_capacity(3);
                        if i > 0 {
                            clause.push(!*a.get(i - 1).ok_or(SatError::EncodingTooLarge)?);
                        }
                        if j > 0 {
                            clause.push(!*b.get(j - 1).ok_or(SatError::EncodingTooLarge)?);
                        }
                        clause.push(*out.get(sum - 1).ok_or(SatError::EncodingTooLarge)?);
                        s.add_clause(&clause)?;
                    }
                    if sum < len {
                        let mut clause = Vec::with_capacity(3);
                        if let Some(l) = a.get(i) {
                            clause.push(*l);
                        }
                        if let Some(l) = b.get(j) {
                            clause.push(*l);
                        }
                        clause.push(!*out.get(sum).ok_or(SatError::EncodingTooLarge)?);
                        s.add_clause(&clause)?;
                    }
                }
            }
            Ok(out)
        }
    }
}
