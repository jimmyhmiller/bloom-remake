//! plan data model.
use crate::{ProgramDigest, core::*};
use blossom_base::{FeatureId, idx::*};
use blossom_value::{Lane, ScalarKind};
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use std::sync::Arc;
/// PhysicalProgram in the documented IR contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalProgram {
    pub program: ProgramDigest,
    pub role: Option<RoleId>,
    pub planner_version: u32,
    pub abi: u32,                 // must equal blossom_engine::abi::VERSION (checked at load)
    pub profile: PlanProfile,     // Production | Literal (LDFI, §3.10) | Perturbed { seed }
    pub features: Vec<FeatureId>, // features the plan needs: checked against the executor (§3.10)
    pub rels: IndexVec<RelId, PhysRel>,
    pub strata: IndexVec<StratumId, PhysStratum>,
    pub temporal: TemporalPlan, // inductive + async rules (SEM-003)
    pub natives: IndexVec<NativeId, NativeOp>,
    pub agg_tables: IndexVec<AggTableId, AggTablePlan>,
    pub buffers: IndexVec<BufferId, BufferPlan>, // fused tick-local relations materialized without an index (§3.7)
    pub ingest: IngestPlan,                      // slots for deliveries, timers, host inputs, boot()/recovered(),
    // table-function rows; per-channel CALM branching bits (§6.2)
    pub prov: ProvPlan,           // tier, backward slice, annotation columns
    pub digests: DigestPlan,      // which incremental digests to maintain (§4.11)
    pub empty_tick_effects: bool, // an empty tick can change state or send (heartbeats, §6.2)
    pub limits: PlanLimits,       // iteration bound, elastic θ, batch size, fuel, max group size …
}
/// Only `ValidatedPlan::validate` constructs this; the engine and codegen accept nothing else.
/// A plan whose invariants have been checked. Validation is implemented in M3.2.
#[derive(Clone, Debug)]
pub struct ValidatedPlan(Arc<PhysicalProgram>);

/// PhysRel in the documented IR contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysRel {
    pub rel: RelId,
    pub shape: RowShapeSpec, // { words: u16, lane: Lane::U32 | Lane::U64 } (ENG-020, §4.1)
    pub cols: Vec<ColEnc>,
    pub hidden: HiddenCols, // sender?, principal?, weight?, (rule, height)? — only when read
    pub key: KeyLayout,     // word positions of key / payload / lattice columns
    pub repr: Repr,
    pub segments: SegmentPlan, // which of frame / standing / weighted / carried / transient exist
    pub persistence: PhysPersistence, // None | Frame { deletions } | Identity | Resolved | Soft | Sealed | Range
    pub support: Option<SupportPlan>, // §3.4.4: deductive support kept apart from the frame
    pub dedup: DedupMode,      // Primary | TickLocal | None (§3.7)
    pub update: UpdateMode,    // AppendOnly | InPlacePayload (§4.2)
    pub durable: Option<DurablePlan>, // WAL relation id + field layout (DIST-081)
    pub history: HistoryPolicy, // CurrentOnly | UntilFrontier | UntilEot (ENG-029, LDFI)
    pub indexes: Vec<IndexDef>,
    pub consumers: SmallVec<[StratumId; 4]>, // strata to mark dirty when this relation changes (ENG-061)
    pub capacity_hint: Option<u64>,          // deployment `[capacity]` table (§4.12)
}
/// ColEnc in the documented IR contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColEnc {
    Direct(ScalarKind),       // order-preserving word (§4.1)
    Interned(TypeId),         // hash-consed, reference-counted; equality is word equality
    Bulk(TypeId),             // payload-only: arena handle + cached fingerprint (ARCH-22)
    LatInline(LatticeTypeId), // LBool / LMax / LMin / LPoint over a Direct scalar; dense-domain sets
    LatObj(LatticeTypeId),    // handle into the lattice heap
}
/// Repr in the documented IR contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Repr {
    Rows,
    Nullary,
    Cells,
    Weighted,
    Range { col: ColIdx },
    EqRel, /* P1 */
    Brie,  /* P1 */
}
/// IndexDef in the documented IR contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexDef {
    pub id: IndexId,
    pub kind: IndexKind,
    pub cols: SmallVec<[u16; 4]>,
    pub scope: Scope,
    pub build: Build,
}
/// IndexKind in the documented IR contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum IndexKind {
    Primary,
    Hash,
    Sorted {
        covering: bool,
        order: SortOrder, /* Word | Canonical */
    },
}
/// Build in the documented IR contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Build {
    Eager,
    Lazy,
} // Lazy: COLT, built on first probe (ENG-026)

/// PhysStratum in the documented IR contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysStratum {
    pub id: StratumId,
    pub rules: Vec<RulePlan>,
    pub natives: Vec<NativeId>,
    pub recursive: bool,
    pub triggers: RelBitSet, // a change to any of these dirties the stratum (lookups included, §3.3)
    pub time_varying: bool,  // reads $now/$tick/$incarnation/rand/choose_rand (§3.4.2)
    pub pipeline: Option<PipelinePlan>, // acyclic tick-local region run as one in-out tree (ENG-006, §3.7)
    pub iteration_bound: u32, // Kleene rounds (ENG-047/140) → BLSR007 with a witness
    pub elastic: Option<ElasticPolicy>, // ENG-064 (P1): switch Counted → Recompute past θ
}
/// RulePlan in the documented IR contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulePlan {
    pub rule: RuleId,
    pub regime: Regime,              // §3.4
    pub regime_reason: RegimeReason, // printed by `blossom plan --dump regimes`
    pub versions: Vec<VersionPlan>,  // §3.3
    pub prov: ProvCapture,           // None | Firing { neg_reads: bool } | Annotate
}
/// Regime in the documented IR contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Regime {
    Standing,
    Transient,
    Counted,
    Recompute,
}
/// VersionPlan in the documented IR contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionPlan {
    pub delta: Option<OccId>, // the occurrence (atom or lookup) that reads Δ; None = base version
    pub alternatives: SmallVec<[OpTree; 1]>, // ENG-089: precompiled join orders (P1: >1)
    pub switch: Option<SwitchRule>, // observed |Δ|/|full| thresholds with hysteresis
    pub slots: u16,
}
/// OpTree in the documented IR contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpTree {
    pub ops: Vec<Op>,
    pub root: OpId,
}

/// Op in the documented IR contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    Scan {
        rel: RelId,
        read: Read,
        bind: Binds,
        check: ColChecks,
        next: OpId,
    },
    Probe {
        rel: RelId,
        index: IndexId,
        key: Operands,
        read: Read,
        bind: Binds,
        check: ColChecks,
        next: OpId,
    },
    Range {
        rel: RelId,
        index: IndexId,
        prefix: Operands,
        lo: Bound,
        hi: Bound,
        read: Read,
        bind: Binds,
        next: OpId,
    },
    Changes {
        rel: RelId,
        read: Read,
        bind: Binds,
        next: OpId,
    }, // cells / payloads changed in the epoch range
    Intersect {
        var: Slot,
        leapers: SmallVec<[Leaper; 4]>,
        next: OpId,
    }, // leapfrog / treefrog (ENG-084, P1)
    Node {
        cover: OpId,
        probes: SmallVec<[ProbeSpec; 4]>,
        next: OpId,
    }, // vectorized Free Join node (ENG-080, P1)
    Exists {
        rel: RelId,
        index: IndexId,
        key: Operands,
        read: Read,
        negate: bool,
        next: OpId,
    }, // semi/anti-join
    Filter {
        cond: CExpr,
        next: OpId,
    },
    Let {
        pat: SlotPattern,
        expr: CExpr,
        next: OpId,
    }, // refutable patterns filter
    Unnest {
        pat: SlotPattern,
        src: CExpr,
        next: OpId,
    },
    TableFn {
        f: FnId,
        inputs: Operands,
        outputs: Binds,
        next: OpId,
    },
    Lookup {
        rel: RelId,
        key: Operands,
        out: Slot,
        next: OpId,
    }, // lattice cell value or ⊥
    Aggregate {
        table: AggTableId,
        group: Operands,
        args: Operands,
    }, // flushed after the pass
    EarlyExit {
        rel: RelId,
        next: OpId,
    }, // nullary head already true (ENG-048)
    Tee {
        outs: SmallVec<[OpId; 4]>,
    }, // push fan-out to several consumers (§3.7)
    Emit {
        sink: Sink,
        cols: Operands,
        weight: Option<Operand>,
        prov: bool,
    },
}
/// Read in the documented IR contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Read {
    pub segs: SegMask,
    pub src: Source,
} // which segments (§3.4, §4.2), which epoch range
/// Source in the documented IR contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    All,     // live rows visible to this read (epoch-bounded: excludes rows appended by the running iteration)
    Delta,   // rows appended and cells/payloads changed in the previous epoch of this fixpoint (Soufflé Δ)
    Old,     // All ∖ Delta
    TickNew, // everything born or changed since this tick began (standing continuation, §3.4)
    ZDelta,
    ZNew,
    ZOld,             // Counted regime (§3.4.5)
    Buffer(BufferId), // a fused tick-local buffer (§3.7)
}
/// Sink in the documented IR contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sink {
    Insert { rel: RelId, segment: Segment }, // set insert or in-place lattice merge; key FD checked (BLSR001)
    ZAdd { rel: RelId },                     // user Z-set contribution (§2.4)
    Derive { rel: RelId },                   // Counted output with derivation counts (§3.4.5)
    Buffer { id: BufferId },                 // append to a fused tick-local buffer (no index)
    StageNext { rel: RelId },
    StageDel { rel: RelId },
    StageUpsert { rel: RelId }, // t+1 staging
    Outbox { channel: RelId },  // async; destination is cols[0]; lattice columns merged at the sender (CR-52)
    Native { op: NativeId, port: u8 },
    Violation { invariant: InvariantId },
}
/// Planner mode, preserving literal expansions when requested.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanProfile {
    Production,
    Literal,
    Perturbed { seed: u64 },
}
/// A row's physical width and word lane.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowShapeSpec {
    pub words: u16,
    pub lane: Lane,
}
/// Optional wire-derived columns, allocated only if read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HiddenCols {
    pub sender: Option<u16>,
    pub principal: Option<u16>,
    pub weight: Option<u16>,
    pub rule: Option<u16>,
    pub height: Option<u16>,
}
/// Word positions for the three logical column classes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyLayout {
    pub key: Vec<u16>,
    pub payload: Vec<u16>,
    pub lattice: Vec<u16>,
}
/// Materialized temporal and tick-local row segments.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentPlan {
    pub frame: bool,
    pub standing: bool,
    pub weighted: bool,
    pub carried: bool,
    pub transient: bool,
}
/// Persisted representation of a relation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PhysPersistence {
    None,
    Frame { deletions: bool },
    Identity,
    Resolved,
    Soft,
    Sealed,
    Range,
}
/// Counted support attached to a frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportPlan {
    pub derivations: bool,
    pub negative: bool,
}
/// Duplicate-elimination placement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DedupMode {
    Primary,
    TickLocal,
    None,
}
/// Payload update strategy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdateMode {
    AppendOnly,
    InPlacePayload,
}
/// Durable field layout used by the WAL.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurablePlan {
    pub wal_rel: RelId,
    pub fields: Vec<u32>,
}
/// Retention of historical versions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HistoryPolicy {
    CurrentOnly,
    UntilFrontier,
    UntilEot,
}
/// Which partition of an index sees a read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scope {
    Tick,
    Frame,
    Standing,
    Carried,
    All,
}
/// Sorted-index comparison strategy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortOrder {
    Word,
    Canonical,
}
/// Compact relation-trigger set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelBitSet(pub Vec<RelId>);
/// Tick-local fused pipeline, with its source and sink relations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelinePlan {
    pub inputs: Vec<RelId>,
    pub outputs: Vec<RelId>,
    pub tree: OpTree,
}
/// Elastic maintenance switch threshold.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElasticPolicy {
    pub threshold_num: u32,
    pub threshold_den: u32,
    pub hysteresis_num: u32,
    pub hysteresis_den: u32,
}
/// Why the planner selected a maintenance regime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegimeReason {
    TransientInput,
    TimeVarying,
    Aggregate,
    Negation,
    Weighted,
    KeyedUpdate,
    Recursive,
    Monotone,
    Finite,
    Forced,
}
/// Provenance captured at a rule emission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProvCapture {
    None,
    Firing { neg_reads: bool },
    Annotate,
}
/// Runtime alternative threshold with hysteresis.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchRule {
    pub ratio_num: u32,
    pub ratio_den: u32,
    pub hysteresis_num: u32,
    pub hysteresis_den: u32,
}
/// Compiled value slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slot(pub u16);
/// Input to a physical operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Operand {
    Slot(Slot),
    Encoded { lane: Lane, word: u64 },
}
/// Ordered operation inputs.
pub type Operands = Vec<Operand>;
/// Binding of relation columns to slots.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binds(pub Vec<(u16, Slot)>);
/// Equality and constant checks on an indexed row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColChecks(pub Vec<(u16, Operand)>);
/// Inclusive or exclusive range endpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Bound {
    Unbounded,
    Included(Operand),
    Excluded(Operand),
}
/// Trie cursor for an intersection step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Leaper {
    pub rel: RelId,
    pub index: IndexId,
    pub prefix: Operands,
    pub col: u16,
}
/// A Free Join probe and bound columns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeSpec {
    pub rel: RelId,
    pub index: IndexId,
    pub key: Operands,
    pub binds: Binds,
    pub check: ColChecks,
}
/// Refutable binding over compiled slots.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SlotPattern {
    Slot(Slot),
    Wild,
    Const(Operand),
    Tuple(Vec<SlotPattern>),
    Variant { number: u32, fields: Vec<SlotPattern> },
}
/// Compiled scalar expression, independent of executor types.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CExpr {
    Operand(Operand),
    Scalar(BuiltinScalar),
    Unary {
        op: UnOp,
        arg: Box<CExpr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<CExpr>,
        rhs: Box<CExpr>,
    },
    Call {
        target: CallTarget,
        args: Vec<CExpr>,
    },
    Construct {
        ty: TypeId,
        variant: Option<u32>,
        fields: Vec<CExpr>,
    },
    Field {
        base: Box<CExpr>,
        index: u32,
    },
    If {
        cond: Box<CExpr>,
        then: Box<CExpr>,
        els: Box<CExpr>,
    },
    Collection {
        kind: CollKind,
        elems: Vec<CExpr>,
    },
    Lattice {
        op: LatOpRef,
        args: Vec<CExpr>,
    },
}
/// Resolved call target for the interpreter and codegen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CallTarget {
    Builtin(BuiltinFn),
    Function(FnId),
    Extern { slot: u32 },
}
/// Segment bitmask for a read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegMask(pub u8);
/// Relation segment written by a sink.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Segment {
    Frame,
    Standing,
    Weighted,
    Carried,
    Transient,
}
/// Final temporal rule plans.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemporalPlan {
    pub inductive: Vec<RulePlan>,
    pub asynchronous: Vec<RulePlan>,
}
/// Physical operator replacing one complete construct expansion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NativeOp {
    Persist { rel: RelId },
    Identity { rel: RelId },
    Choose(ChooseSpec),
    MultiChoose(MultiChooseSpec),
    Index(IndexSpec),
    Seq(SeqSpec),
    FoldOrdered(FoldSpec),
    ArgExt(ArgExtSpec),
    AggDefault(AggDefaultSpec),
    Upsert { rel: RelId, staging: RelId, del: RelId },
    Resolve(ResolveSpec),
    SoftTable(SoftSpec),
    Sealed { rel: RelId, sealed: RelId },
    Range { rel: RelId, col: ColIdx },
    Seal(SealSpec),
    Wrapped(WrapSpec),
    LogicalTimer { rel: RelId, every: u64 },
    LatticeFold { cell: RelId },
    Snapshot(SnapshotSpec),
    Finality(FinalitySpec),
}
/// Aggregation state maintained by a plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggTablePlan {
    pub group: Operands,
    pub function: AggFunc,
    pub input: RelId,
    pub output: RelId,
}
/// Materialized tick-local buffer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BufferPlan {
    pub id: BufferId,
    pub shape: RowShapeSpec,
    pub capacity_hint: Option<u64>,
}
/// External delivery slots and per-channel branching facts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestPlan {
    pub deliveries: Vec<RelId>,
    pub timers: Vec<RelId>,
    pub host_inputs: Vec<RelId>,
    pub boot: Option<RelId>,
    pub recovered: Option<RelId>,
    pub table_fns: Vec<FnId>,
    pub branching: Vec<(RelId, bool)>,
}
/// Provenance mode and selected backward slice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvPlan {
    pub tier: ProvTier,
    pub rules: Vec<RuleId>,
    pub annotation_cols: Vec<(RelId, u16)>,
}
/// Provenance capture level.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProvTier {
    Off,
    B,
    C,
}
/// Incremental state, outbox and choice digest selection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestPlan {
    pub state: bool,
    pub outbox: bool,
    pub choices: bool,
    pub changed: Vec<RelId>,
}
/// Finite operational bounds whose breaches are runtime errors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanLimits {
    pub iterations: u32,
    pub batch_size: u32,
    pub fuel: u64,
    pub max_group: u32,
    pub natives: bool,
    pub elastic_threshold: Option<u32>,
}
/// BLAKE3-256 of a canonical physical plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PlanDigest(pub [u8; 32]);

impl ValidatedPlan {
    /// The checked physical data. The validator is supplied by M3.2.
    pub fn get(&self) -> &PhysicalProgram {
        &self.0
    }
}

impl PhysicalProgram {
    /// The identity of this particular executable plan, including profile, ABI and operator order.
    pub fn digest(&self) -> Result<PlanDigest, crate::IrError> {
        let bytes = postcard::to_allocvec(self).map_err(|e| crate::IrError::builder(e.to_string()))?;
        Ok(PlanDigest(*blake3::hash(&bytes).as_bytes()))
    }
}
