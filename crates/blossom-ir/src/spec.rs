//! spec data model.
use crate::{ProgramDigest, core::*};
use blossom_base::{QualName, Symbol, idx::*};
use blossom_value::Value;
use serde::{Deserialize, Serialize};
/// SpecProgram in the documented IR contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecProgram {
    pub name: QualName,
    pub target: ProgramDigest,
    pub nodes: Vec<Symbol>,
    pub assign: Vec<(RoleId, Vec<Symbol>)>,
    pub faults: Option<FailureModel>, // eot, eff, crashes, model (sync|async), delay, round (ODD-16)
    pub facts: Vec<SpecFact>,         // static facts per node, and timestamped input events
    pub trace_rels: IndexVec<RelId, TraceRelDecl>, // interval relations r$hist(Node, x̄, From, To) per target relation;
    // sent$c(From, To, x̄, SendTick); crash(Node, Tick); hb (virtual)
    pub rules: IndexVec<RuleId, Rule>, // stratified Datalog over trace relations; cross-location allowed
    pub constructs: IndexVec<ConstructId, Construct>, // Quorum and SpecOracle constructs
    pub pre: Option<RelId>,
    pub post: Option<RelId>,
    pub invariants: Vec<SpecInvariant>,
    pub liveness: Vec<LivenessDecl>,
    pub proofs: Vec<ProveDecl>,    // `prove G by induction using L…`
    pub expects: Vec<Expectation>, // confluent(out), deterministic(out)
    pub checks: Vec<CheckDecl>,    // ldfi | bmc | smt | sim | asp, each with `expect holds|fails`
}
/// Network and process failures quantified by a spec.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureModel {
    pub eot: bool,
    pub eff: Option<u32>,
    pub crashes: Option<u32>,
    pub model: NetworkModel,
    pub delay: Option<u32>,
    pub round: Option<u32>,
}
/// Synchronous or asynchronous execution model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetworkModel {
    Sync,
    Async,
}
/// Static or timestamped fact on a named node.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecFact {
    pub node: Symbol,
    pub rel: RelId,
    pub row: Vec<Value>,
    pub tick: Option<u64>,
}
/// Half-open interval trace relation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceRelDecl {
    pub id: RelId,
    pub name: QualName,
    pub target: Option<RelId>,
    pub columns: Vec<TypeId>,
    pub kind: TraceRelKind,
}
/// Semantic source of a trace relation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TraceRelKind {
    History,
    Sent,
    Crash,
    HappensBefore,
    Input,
}
/// Safety condition checked on a complete trace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecInvariant {
    pub name: QualName,
    pub violation: RelId,
}
/// Temporal property checked by the spec engine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LivenessDecl {
    pub name: QualName,
    pub antecedent: RelId,
    pub consequent: RelId,
}
/// Inductive proof obligation over a spec.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProveDecl {
    pub goal: RelId,
    pub invariants: Vec<RelId>,
}
/// Observable property expected of a relation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Expectation {
    Confluent(RelId),
    Deterministic(RelId),
}
/// Verification backend and expected result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckDecl {
    pub backend: CheckBackend,
    pub expect: CheckResult,
}
/// Spec-checking backend.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckBackend {
    Ldfi,
    Bmc,
    Smt,
    Sim,
    Asp,
}
/// Expected check outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckResult {
    Holds,
    Fails,
}
