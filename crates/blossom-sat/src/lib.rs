//! Incremental SAT backends with explicit result states and assumption cores.
#![deny(unsafe_op_in_unsafe_fn)]
pub mod backends;
mod budget;
pub mod card;
pub mod conformance;

use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Var(pub u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Lit(pub u32);
impl Var {
    pub const fn positive(self) -> Lit {
        Lit(self.0 * 2)
    }
    pub const fn negative(self) -> Lit {
        Lit(self.0 * 2 + 1)
    }
}
impl Lit {
    pub const fn var(self) -> Var {
        Var(self.0 / 2)
    }
    pub const fn is_negative(self) -> bool {
        self.0 & 1 != 0
    }
    pub const fn negate(self) -> Self {
        Self(self.0 ^ 1)
    }
}
impl std::ops::Not for Lit {
    type Output = Self;
    fn not(self) -> Self {
        self.negate()
    }
}
#[derive(Clone, Debug, Default)]
pub struct SolveLimits {
    pub conflicts: Option<u64>,
    pub time: Option<Duration>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitHit {
    Conflicts,
    Time,
    Variables,
    Interrupted,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SatOutcome {
    Sat,
    Unsat,
    Unknown(LimitHit),
}
#[derive(Debug, thiserror::Error)]
pub enum SatError {
    #[error("backend {backend} is unavailable; compile feature {feature}")]
    BackendUnavailable { backend: String, feature: &'static str },
    #[error("unknown SAT backend {0}")]
    UnknownBackend(String),
    #[error("SAT variable {0:?} has not been allocated")]
    InvalidVariable(Var),
    #[error("SAT operation requires {0}")]
    InvalidState(&'static str),
    #[error("SAT backend: {0}")]
    Backend(String),
    #[error("cardinality size is not representable")]
    EncodingTooLarge,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Unimplemented(#[from] blossom_base::Unimplemented),
    #[error(transparent)]
    Internal(#[from] blossom_base::InternalError),
}
pub trait SatSolver: Send {
    fn backend(&self) -> &'static str;
    fn new_var(&mut self) -> Var;
    fn add_clause(&mut self, lits: &[Lit]) -> Result<(), SatError>;
    fn solve(&mut self, assumptions: &[Lit], limits: &SolveLimits) -> Result<SatOutcome, SatError>;
    fn value(&self, v: Var) -> Result<bool, SatError>;
    fn failed_assumption(&self, l: Lit) -> Result<bool, SatError>;
}

#[derive(Default)]
pub(crate) struct State {
    vars: u32,
    outcome: Option<SatOutcome>,
    model: Vec<bool>,
    assumptions: Vec<Lit>,
    core: Vec<Lit>,
}
impl State {
    fn new_var(&mut self) -> Var {
        // The trait's infallible allocation follows native solver allocation: exhaustion is a
        // programmer/resource failure, not a SAT outcome. Memory exhausts before this limit.
        let v = Var(self.vars);
        self.vars += 1;
        self.invalidate();
        v
    }
    fn invalidate(&mut self) {
        self.outcome = None;
        self.model.clear();
        self.core.clear();
        self.assumptions.clear();
    }
    fn check_var(&self, v: Var) -> Result<(), SatError> {
        if v.0 < self.vars {
            Ok(())
        } else {
            Err(SatError::InvalidVariable(v))
        }
    }
    fn check(&self, lits: &[Lit]) -> Result<(), SatError> {
        for l in lits {
            self.check_var(l.var())?;
        }
        Ok(())
    }
    fn value(&self, v: Var) -> Result<bool, SatError> {
        self.check_var(v)?;
        if self.outcome != Some(SatOutcome::Sat) {
            return Err(SatError::InvalidState("a SAT result"));
        }
        self.model
            .get(v.0 as usize)
            .copied()
            .ok_or(SatError::InvalidState("a complete model"))
    }
    fn failed(&self, l: Lit) -> Result<bool, SatError> {
        self.check_var(l.var())?;
        if self.outcome != Some(SatOutcome::Unsat) || self.assumptions.is_empty() {
            return Err(SatError::InvalidState("UNSAT under assumptions"));
        }
        if !self.assumptions.contains(&l) {
            return Err(SatError::InvalidState("a literal in the last assumptions"));
        }
        Ok(self.core.contains(&l))
    }
}
pub fn select_backend(name: &str) -> Result<Box<dyn SatSolver>, SatError> {
    match name {
        "exhaustive" => Ok(Box::new(backends::ExhaustiveSolver::default())),
        "dimacs" => Ok(Box::new(backends::DimacsDump::default())),
        "cadical" => {
            #[cfg(feature = "sat-cadical")]
            {
                Ok(Box::new(backends::CadicalSolver::default()))
            }
            #[cfg(not(feature = "sat-cadical"))]
            {
                Err(SatError::BackendUnavailable {
                    backend: name.into(),
                    feature: "sat-cadical",
                })
            }
        }
        "batsat" => {
            #[cfg(feature = "sat-batsat")]
            {
                Ok(Box::new(backends::BatSolver::default()))
            }
            #[cfg(not(feature = "sat-batsat"))]
            {
                Err(SatError::BackendUnavailable {
                    backend: name.into(),
                    feature: "sat-batsat",
                })
            }
        }
        _ => Err(SatError::UnknownBackend(name.into())),
    }
}
#[cfg(test)]
mod tests;
