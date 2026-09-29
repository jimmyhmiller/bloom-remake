//! obs data model.
use crate::core::ViolationAction;
use blossom_base::{RuleLabel, idx::*};
use blossom_value::{Digest128, Fingerprint};
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use std::sync::Arc;
/// ChoiceEntry in the documented IR contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChoiceEntry {
    pub site: SiteId,
    pub group_fp: Fingerprint,
    pub chosen_fp: Fingerprint,
    pub candidates: u32,
    pub reason: ChoiceReason, /* Priority | Cost | Sticky | Override */
}
/// ViolationRecord in the documented IR contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViolationRecord {
    pub invariant: InvariantId,
    pub key_fp: Fingerprint,
    pub action: ViolationAction,
}
/// TickDigests in the documented IR contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TickDigests {
    pub state: Digest128,
    pub outbox: Digest128,
    pub choices: Digest128,
    pub changed: SmallVec<[(RelId, Digest128); 8]>,
}
/// ProgramErrorRecord in the documented IR contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramErrorRecord {
    #[serde(deserialize_with = "deserialize_code")]
    pub code: &'static str,
    pub rule: Option<RuleLabel>,
    pub detail: Arc<str>,
} // §6.6
/// Why a candidate was selected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChoiceReason {
    Priority,
    Cost,
    Sticky,
    Override,
}

fn deserialize_code<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<&'static str, D::Error> {
    let code = blossom_base::codes::Code::deserialize(deserializer)?;
    Ok(code.as_str())
}

/// One distinct rule firing observed by an evaluator: the Tier C record of the literal plan profile that provenance
/// graphs and LDFI are built from (ARCHITECTURE §4.9, §8.2). A firing is identified by its rule and the tuples it
/// read, so an evaluator reports each firing once per tick however often its fixpoint rediscovers it.
// FEATURE: ENG-112
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FiringRecord {
    pub rule: RuleId,
    pub kind: FiringKind,
    /// The head tuple, one value per schema column (for an async rule column 0 is the destination).
    pub head: Arc<[blossom_value::Value]>,
    /// The positive body reads, in body order. An aggregate firing lists the reads of every contributing
    /// valuation of its group (Molly's conjunctive encoding, ENG-112).
    pub reads: Vec<PosRead>,
    /// The negated body reads (ENG-113), in body order.
    pub negations: Vec<NegRead>,
}

/// Whether a firing derived one head tuple from one valuation, or an aggregate row from all of its contributors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum FiringKind {
    Rule,
    // FEATURE: TEST-024
    Aggregate,
}

/// A positive body read: the tuple a body atom matched.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PosRead {
    pub rel: RelId,
    pub row: Arc<[blossom_value::Value]>,
}

/// A negated body read: the relation and the pattern that had no match (`None` for a wildcard column).
// FEATURE: ENG-113
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NegRead {
    pub rel: RelId,
    pub pattern: Vec<Option<blossom_value::Value>>,
}
