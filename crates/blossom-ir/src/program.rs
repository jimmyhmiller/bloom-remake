//! Validated programs and canonical content identities.
use crate::{IrError, core::Program};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
/// BLAKE3-256 of a canonical, span-free program.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProgramDigest(pub [u8; 32]);
/// A structurally checked program. Raw programs cannot enter an evaluator.
#[derive(Clone)]
pub struct ValidatedProgram {
    program: Arc<Program>,
    digest: ProgramDigest,
}
impl ValidatedProgram {
    /// Checks all twelve IR invariants.
    pub fn validate(p: Program) -> Result<Self, Vec<IrError>> {
        let errors = crate::validate::validate(&p);
        if !errors.is_empty() {
            return Err(errors);
        }
        let digest = ProgramDigest(
            *blake3::hash(
                &postcard::to_allocvec(&crate::canonical::canonical(&p).map_err(|e| vec![e])?)
                    .map_err(|e| vec![IrError::builder(e.to_string())])?,
            )
            .as_bytes(),
        );
        Ok(Self {
            program: Arc::new(p),
            digest,
        })
    }
    /// Validated program data.
    pub fn get(&self) -> &Program {
        &self.program
    }
    /// The local runtime view for one declared role.
    pub fn project(&self, role: blossom_base::idx::RoleId) -> Result<Self, IrError> {
        crate::projection::project(self, role)
    }
    /// The least program computing the view `view` from the durable relations it reads, each an input of it
    /// (docs/design/DATABASE.md §5), and those relations' names. A view that reads anything else (but static
    /// relations) is refused.
    pub fn query(
        &self,
        view: blossom_base::idx::RelId,
        role: Option<blossom_base::idx::RoleId>,
    ) -> Result<(Self, Vec<String>), IrError> {
        crate::query::query(self, view, role)
    }
    /// Whether every read of `rel` by a rule is an atom (`r(…)` or `notin r(…)`): no lookup, aggregate weight or
    /// other expression reads it. A reader may then take only the rows its atoms can match.
    pub fn read_only_by_atoms(&self, rel: blossom_base::idx::RelId) -> bool {
        crate::query::read_only_by_atoms(&self.program, rel)
    }
    /// Cached identity.
    pub fn digest(&self) -> ProgramDigest {
        self.digest
    }
}
