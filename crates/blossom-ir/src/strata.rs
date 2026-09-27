//! strata data model.
use crate::ProgramDigest;
use blossom_base::{IndexVec, idx::*};
use serde::{Deserialize, Serialize};
// blossom-ir::strata
/// Stratification in the documented IR contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stratification {
    pub strata: IndexVec<StratumId, Stratum>, // topological order
    pub rel_stratum: IndexVec<RelId, StratumId>,
    pub temporal: Vec<RuleId>, // the final pseudo-stratum: inductive and async rules (SEM-022 step 5)
    pub program: ProgramDigest, // which program this stratification belongs to
}
/// Stratum in the documented IR contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stratum {
    pub rels: Vec<RelId>,
    pub rules: Vec<RuleId>,      // deductive rules whose head is in `rels`
    pub recursive: bool,         // non-trivial SCC or self-loop
    pub lattice_recursive: bool, // recursion through lattice ops (ENG-142 classification applies)
    pub z_stratum: bool,         // contains Weighted relations (ENG-062/070 boundary)
}
