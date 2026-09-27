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
