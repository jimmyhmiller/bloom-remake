//! Dedalus core program data, independent of execution and surface syntax.
use blossom_base::{IndexVec, QualName, RuleLabel, Span, Symbol, idx::*};
pub use blossom_value::class::*;
use blossom_value::{TypeTable, Value, time::Duration, types::FieldNo};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
/// Lattice data and shared operation classes.
pub mod lattice {
    pub use super::{LatOpDecl, LatticeCtor, LatticeDef};
    pub use blossom_value::class::*;
}
// blossom-ir::core::lattice (built on blossom-lattice's catalogue)
/// LatticeDef data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LatticeDef {
    pub id: LatticeTypeId,
    pub name: QualName,
    pub ctor: LatticeCtor,
    pub ops: Vec<LatOpDecl>,       // the operation catalogue for this type (R04 §2.4, normative)
    pub height: HeightClass,       // Acc | PStable | Unknown   (ENG-142)
    pub laws: LawStatus,           // Builtin | Proved | Tested | Refuted   (ODD-09 (c), TEST-087)
    pub distributive: bool,        // exact threshold supports allowed (TEST-140)
    pub dense_domain: Option<u16>, // element domain size ≤ 256 (enums, Node<R> of a static role): bitmask repr (§4.5)
}
/// LatticeCtor data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LatticeCtor {
    Bool,
    Max(TypeId),
    Min(TypeId), // adjoined ⊥ = ∓∞ (LANG-281)
    Set(TypeId),
    Map(TypeId, LatticeTypeId),
    Bag(TypeId),
    PSet(TypeId),
    Pair(LatticeTypeId, LatticeTypeId),
    Product(Vec<(Symbol, LatticeTypeId)>),
    Lex { chain: LatticeTypeId, inner: LatticeTypeId },
    WithBot(LatticeTypeId),
    WithTop(LatticeTypeId),
    Conflict(TypeId),
    Point(TypeId),
    Unit,
    VecUnion(LatticeTypeId),
    UnionFind(TypeId), // LANG-130 (VClock is an alias of Map(Node, Max(u64)))
    Dom { version: LatticeTypeId, value: TypeId }, // LDom / MV-register (LANG-132)
    Causal(DotStoreKind),
    Tombstone { base: LatticeTypeId, tomb: TombKind }, // LANG-133/134
    DomPairUnsafe(LatticeTypeId, TypeId),              // LANG-136: `unsafe` only
    Extern(ExternLatticeRef),                          // LANG-135 (3): a Rust type implementing Merge
}
/// LatOpDecl data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LatOpDecl {
    pub name: Symbol,
    pub params: Vec<(TypeId, MonoClass)>, // class per argument, relative to the natural order (LANG-125 amended)
    pub ret: TypeId,
    pub kind: LatOpKind, // Threshold | Morphism | Bimorphism | Monotone | Antitone | NonMonotone | Stable { after: Symbol }
    pub join_prime: bool, // thresholds only: t(a ⊔ b) ⇒ t(a) ∨ t(b); ANA-141 needs it (BENCH-302)
    pub derivative: Option<FnId>, // f′(x, dx) for semi-naive over Mon non-morphisms (ENG-141, P1)
    pub incompatible_thresholds: bool, // generic `threshold(t1..tn)` precondition (LANG-126)
}

/// FnDecl data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FnDecl {
    pub id: FnId,
    pub name: QualName,
    pub params: Vec<(Symbol, TypeId)>,
    pub ret: TypeId,
    /// An `Ir` body's variables: the parameters first (in order), then every `let` and closure binding. Empty for
    /// other bodies.
    pub vars: IndexVec<VarId, VarDecl>,
    pub body: FnBody,
    pub props: FnProps,
}
/// FnBody data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FnBody {
    Ir(Expr), // pure, total, non-recursive (LANGUAGE §16.1)
    Extern {
        path: Arc<str>,
        memo: bool,
    }, // `extern fn`: pure by declaration, memoized per input per tick (LANG-181);
    // purity checked by double evaluation under simulation (§11.7)
    TableFn {
        path: Arc<str>,
        outputs: Vec<(Symbol, TypeId)>,
    }, // `extern table fn` (LANG-183): may read the world;
    // its rows are recorded as trace inputs (§0.2, §6.4)
    Builtin(BuiltinFn), // Appendix B library
}
/// FnProps data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FnProps {
    pub classes: Vec<MonoClass>, // per parameter; NonMonotone ⇒ call needs a bang
    pub injective: Claim,
    pub commutative: Claim,
    pub associative: Claim,
    pub idempotent: Claim,
    pub stable_after: Option<Symbol>, // `stable fn … after t`
    /// Whether the function's evaluation counts against the step budget (LANGUAGE §16.1). A format's generated
    /// functions are bounded by their input (every loop by the bytes left), so they are not metered; a metered
    /// function they call starts a budget of its own.
    #[serde(default = "metered_default")]
    pub metered: bool,
}

fn metered_default() -> bool {
    true
}

/// UdaDecl data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UdaDecl {
    pub id: UdaId,
    pub state: TypeId,
    pub init: FnId,
    pub step: FnId,
    pub combine: Option<FnId>,
    pub finish: FnId,
    pub props: FnProps,
} // LANGUAGE §10.8
/// ServiceDecl data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceDecl {
    pub id: ServiceId,
    pub name: QualName,
    pub call: RelId, /* channel to $host */
    pub result: RelId,
}

/// A byte stream (FOREIGN-PROTOCOLS §1): a TCP endpoint the program owns at the byte level. The runtime feeds its
/// event relations and carries out the rows of its host relations after the tick's durable writes are synced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamDecl {
    pub name: QualName,
    pub kind: StreamKind,
    pub placement: Placement,
    /// `opened(c: Conn, peer: String, at: Instant)`; a connect stream's is `opened(c: Conn, req: u64, peer: String,
    /// at: Instant)`, naming the dial request it answers.
    pub opened: RelId,
    /// `data(c: Conn, seq: u64, bytes: Bytes)`: the next chunk read from `c`; `seq` counts from 0 per connection.
    pub data: RelId,
    /// `closed(c: Conn, reason: String)`.
    pub closed: RelId,
    /// A connect stream's `failed(req: u64, reason: String)`: a dial that did not connect.
    pub failed: Option<RelId>,
    /// `write(c: Conn, seq: u64, parts: Vec<Part>)`, to the host: written in `seq` order per connection.
    pub write: RelId,
    /// `close(c: Conn)`, to the host: close after the writes already sent.
    pub close: RelId,
    /// `pause(c: Conn)`, to the host: stop reading `c` (the chunks already read still arrive), so the peer's sends
    /// wait in its TCP window; `resume(c: Conn)` reads it again.
    pub pause: RelId,
    pub resume: RelId,
    /// A connect stream's `dial(req: u64, addr: String)`, to the host.
    pub dial: Option<RelId>,
}
/// StreamKind data in the Dedalus core IR.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamKind {
    /// Accepts connections on the address the deployment gives it.
    Listen,
    /// Opens connections on request (`dial`).
    Connect,
}
/// A stream event relation's role (FOREIGN-PROTOCOLS §1.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum StreamEvent {
    Opened,
    Data,
    Closed,
    Failed,
}
/// A request a program makes of the host through a stream (FOREIGN-PROTOCOLS §1.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum HostOp {
    Write,
    Close,
    Dial,
    Pause,
    Resume,
}

/// Program data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
// FEATURE: ENG-001
pub struct Program {
    pub meta: ProgramMeta,
    pub types: TypeTable,
    pub lattices: IndexVec<LatticeTypeId, LatticeDef>,
    pub groups: IndexVec<GroupTypeId, GroupDef>,
    pub consts: IndexVec<ConstId, Value>, // folded constants and literals (canonical values)
    pub params: IndexVec<ParamId, ParamDecl>, // deploy-time parameters, bound at deployment, traced
    pub fns: IndexVec<FnId, FnDecl>,
    pub udas: IndexVec<UdaId, UdaDecl>,
    pub services: IndexVec<ServiceId, ServiceDecl>,
    pub streams: Vec<StreamDecl>, // byte streams (FOREIGN-PROTOCOLS §1), in declaration order
    pub roles: IndexVec<RoleId, RoleDecl>,
    pub rels: IndexVec<RelId, RelDecl>,
    pub rules: IndexVec<RuleId, Rule>,
    pub facts: Vec<Fact>, // rows of `static` relations (CR-16)
    pub constructs: IndexVec<ConstructId, Construct>,
    pub sites: IndexVec<SiteId, Site>,
    pub invariants: IndexVec<InvariantId, InvariantDecl>, // LANG-200; lowered to `violation` rules, kept for reporting
    pub migrations: Vec<MigrationDecl>,                   // LANG-262: separate rule sets over `old.*`
    pub translations: Vec<TranslationDecl>,               // LANG-263: tuple-local codec-layer rules
}
/// ProgramMeta data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramMeta {
    pub name: Symbol,
    pub version: u32,
    pub edition: u16,
    pub compiler: Arc<str>, // compiler version string (trace header, TEST-010)
    pub prf_version: u16,
    pub encoding_version: u16,
    pub program_id: [u8; 16], // stable id of the program lineage (schema.lock)
}
/// RoleDecl data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleDecl {
    pub id: RoleId,
    pub name: QualName,
    pub kind: RoleKind,
}
/// RoleKind data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoleKind {
    Process,
    Cluster,
    External,
}

/// RelDecl data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelDecl {
    pub id: RelId,
    pub name: QualName,
    pub class: RelClass,
    pub schema: Schema,
    pub persistence: Persistence,
    pub durable: bool,                   // WAL-logged; committed before sends (SEM-072)
    pub interface: Option<InterfaceDir>, // Input | Output (LANG-003)
    pub placement: Placement,            // Shared | Role(RoleId)
    pub origin: Origin,                  // User(Span) | Generated { construct } — generated ⇒ provenance-transparent
    pub attrs: RelAttrs,
    pub span: Span,
}
/// RelClass data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelClass {
    /// Defined by rules: `table`, `scratch`, `view`, `cell`, an instance's interface, and every generated relation.
    Idb,
    /// Holds at every tick; rows come from `facts` and deployment config; no rule may write it.
    Static,
    /// Runtime-fed and tick-local.
    Event(EventSource),
    /// Asynchronous relation. Column 0 is the destination (CR-14). The receiving side is tick-local.
    Channel(ChannelDecl),
    /// Weighted collection (LANG-138): `zset table` (ℤ) or `bag table` (ℕ). Heads use `HeadMode::ZAdd`.
    Weighted(WeightKind),
    /// `#[readonly] table`: host-maintained and persistent; the host writes it between ticks; rules only read it (LANG-051).
    HostTable,
    /// Requests to the host (a stream's `write`, `close`, `dial`): written by async rules, never read; each tick's rows
    /// leave with the tick's output, released after its durable writes are synced (FOREIGN-PROTOCOLS §1.2).
    HostOut(HostOp),
}
/// EventSource data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventSource {
    Input,                      // a program root's `input`; an instance's input is Idb + interface
    InputSeal { input: RelId }, // host seal of an input key (LANGUAGE §14.4, input seals)
    Timer(TimerDecl),
    Boot,
    Recovered,
    Stdin,
    SessionOpen,
    SessionClosed,
    ServiceResult(ServiceId),
    ClusterVersion,      // LANG-264: LMax<u32>, sampled per tick, recorded
    Stream(StreamEvent), // FOREIGN-PROTOCOLS §1: a stream's `opened`, `data`, `closed`, `failed`
}
/// TimerDecl data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimerDecl {
    pub clock: TimerClock, /* Physical | Logical */
    pub every: Option<Duration>,
    pub ticks: Option<u64>,
    pub times: Option<u64>,
    pub once_after: Option<Duration>,
    pub once: bool,
    /// `while G`: the timer fires only while `G` held at the end of the node's latest tick (LANGUAGE §15.2).
    #[serde(default)]
    pub guard: Option<RelId>,
}
/// ChannelDecl data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelDecl {
    pub form: ChannelForm,   // Direction { src: RoleId, dst: RoleId } | Column | NodeToNode
    pub loopback: bool,      // LANG-046
    pub host_endpoint: bool, // `stdout` and service-call channels (`@$host`)
    pub fault: FaultModel,   // Lossy (default) | LossyDelayed | Reliable | ReliableOrdered (LANG-155)
    pub partition: Option<PartitionSpec>, // LANG-154: key expression + optional `over` relation
    pub sealed_by: Option<SealDecl>, // LANG-207: key columns + producer relation
    pub wrapper: Option<WrapperKind>, // LANG-158: Dots (W2) | Cumulative (W3) | Tree (W4, P2)
    pub acl: AclSpec,        // Inferred | Explicit(…) (LANG-242)
    pub egress_to_external: bool, // replies to Sessions only
    pub replicated: bool,    // Blazes `Rep` (ANA-041), `#[replicated]`
}

/// SEM-100: every relation has key columns and one value lattice (a product of its lattice columns).
/// A set relation is the 𝔹 case: every column is a key and there are no lattice columns.
/// Schema data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
// FEATURE: SEM-100
pub struct Schema {
    pub cols: Vec<Column>,    // positional; channels: col 0 is always the destination (CR-14)
    pub key: Vec<ColIdx>,     // default: every non-lattice column; empty = singleton
    pub payload: Vec<ColIdx>, // non-key, non-lattice: governed by the key FD (SEM-050, runtime check)
    pub lattice: Vec<(ColIdx, LatticeTypeId)>, // merged per key (CR-51)
}
/// Column data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    pub name: Symbol,
    pub ty: TypeId,
    pub field_no: Option<FieldNo>,
    pub default: Option<ConstId>,
    pub since: Option<u32>,
    pub deprecated: Option<u32>,
    pub hidden_dest: bool, /* direction-form col 0 */
}

/// Persistence data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Persistence {
    /// Tick-local: empty at the start of every tick unless re-derived (SEM-008).
    None,
    /// `table`: `r(x̄)@next :- r(x̄), notin r$del(x̄).` The rule is in `rules`, is tagged `ConstructKind::Persist`,
    /// and is what the oracle evaluates. `del == None`: deletions are rejected (sealed and range tables).
    /// `guard`: `table … while BODY` (LANGUAGE §7.2) adds `r$keep(x̄)` to the body, a relation of the same construct
    /// derived from `r(x̄), BODY`.
    Frame {
        rule: RuleId,
        del: Option<RelId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        guard: Option<RelId>,
    },
    /// Persistent lattice: the implicit identity rule `r(k̄; X)@next :- r(k̄; X).` (SEM-104).
    Identity { rule: RuleId },
    /// A relation-level `resolve` policy replaces the frame rule (LANGUAGE §10.7); the construct owns the rules.
    Resolved { construct: ConstructId },
    /// Soft tables persist through their generated `$s` relation (LANGUAGE §7.9); the construct owns the rules.
    Soft { construct: ConstructId },
}
/// RelAttrs data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelAttrs {
    pub nondet: Option<Arc<str>>,             // LANG-204 reason
    pub deterministic: bool,                  // `#[deterministic]` assertion (checked, BLS0603)
    pub monotone: bool,                       // `monotone view` assertion (checked, BLS0702; ANA-020)
    pub final_output: bool,                   // LANG-212
    pub atomic: bool,                         // LANG-206
    pub handler: Option<Arc<str>>,            // LANG-186 host handler path
    pub materialize: Option<MaterializeHint>, // LANG-053: hint only, never changes meaning
    pub finite: Option<FiniteSpec>,           // ANA-122 declared finite component
    pub range_col: Option<ColIdx>,            // LANG-050
    pub partition: Option<PartitionSpec>,     // table `partition by` (BLSR008 ownership)
    pub sealed_by: Option<SealDecl>,          // local seals on tables and inputs
}
/// Fact data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fact {
    pub rel: RelId,
    pub row: Vec<ConstId>,
    pub span: Span,
}

/// Rule data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub id: RuleId,
    pub label: RuleLabel,
    pub kind: RuleKind,
    pub head: Head,
    pub body: Body,
    pub role: Option<RoleId>,           // the `$role(R)` guard; None only in role-free programs
    pub construct: Option<ConstructId>, // the construct whose expansion this rule belongs to
    pub span: Span,
}
/// RuleKind data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleKind {
    Deductive, // same node, same tick; may recurse
    Inductive, // `@next`: t+1, evaluated once on the completed fixpoint
    Async,     // `@async`: delivered to args[0] at a later tick
}
/// Head data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Head {
    pub rel: RelId,
    pub args: Vec<HeadArg>, // exactly one per schema column; for a channel, args[0] is the destination
    pub mode: HeadMode,
}
/// HeadArg data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HeadArg {
    Term(Term),
    Agg(AggCall),
}
/// HeadMode data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HeadMode {
    Insert,                               // set insert; lattice columns merge (CR-51)
    ZAdd { weight: Term },                // Weighted relations: `r(x̄) += w`
    Violation { invariant: InvariantId }, // `violation(name, key)`: feeds nothing (LANGUAGE §17.1)
}
/// AggCall data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggCall {
    pub func: AggFunc,
    pub args: Vec<Term>,          // aggregated tuple; distinct valuations (set semantics, LANG-100)
    pub order: Option<OrderSpec>, // canonical-order tiebreak keys (LANG-118)
}
/// AggFunc data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AggFunc {
    Count,
    Sum,
    Min,
    Max,
    Avg,
    BoolAnd,
    BoolOr,
    CollectVec,
    CollectSet,
    CollectMap,                        // canonical order (LANG-118); duplicate map key = BLSR005
    Percentile { num: u32, den: u32 }, // nearest rank, canonical tiebreak
    OlaSum,
    OlaCount,
    OlaAvg,     // LANG-113; implicitly #[nondet("progressive")]
    Uda(UdaId), // LANG-105; evaluated as fold_ordered unless proved C+A
}
/// A rule body: an unordered conjunction. The planner orders literals.
/// Body data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Body {
    pub vars: IndexVec<VarId, VarDecl>,
    pub lits: Vec<Literal>,
}
/// VarDecl data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VarDecl {
    pub name: Symbol,
    pub ty: TypeId,
    pub non_bottom: bool, /* SEM-101 N4 refinement */
}
/// Literal data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Literal {
    Pos(Atom),                                         // generator; lattice columns range over non-⊥ cells only
    Neg(Atom),                                         // notin; range-restricted (ANA-001)
    Bind { pat: Pattern, expr: Expr },                 // `X := e`; a refutable pattern (`Some(X) := e`) filters
    Guard(Expr),                                       // boolean filter
    Lookup { var: VarId, rel: RelId, key: Vec<Term> }, // V = r[k̄]: the cell value, ⊥ if absent (LANG-280)
    Gen { pat: Pattern, src: GenSource },              // `p in e`, ranges, table functions (LANG-088, 092, 183)
}
impl Literal {
    /// Whether a check can never raise a runtime error: a negation or a lookup, or a guard or binding whose
    /// expression cannot fail (a refutable binding only filters). Positive atoms are not checks; generators count as
    /// fallible.
    pub fn cannot_fail(&self) -> bool {
        match self {
            Literal::Neg(_) | Literal::Lookup { .. } => true,
            Literal::Guard(e) | Literal::Bind { expr: e, .. } => e.cannot_fail(),
            Literal::Pos(_) | Literal::Gen { .. } => false,
        }
    }
}
/// Atom data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Atom {
    pub rel: RelId,
    pub args: Vec<Term>, // exactly one per schema column; a received channel atom's args[0] is $self
    pub sender: Option<Term>, // `from s`: channel and loopback atoms only
    pub principal: Option<Term>, // `principal p`: channel and loopback atoms only
    pub weight: Option<Term>, // Weighted relations only: binds the non-zero weight (LANGUAGE §11.10)
    pub spec: Option<SpecAt>, // spec programs only: location and time (LANG-070)
    pub span: Span,
}
/// SpecAt data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecAt {
    pub loc: Term,
    pub time: SpecTime,
}
/// SpecTime data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpecTime {
    Eval,
    At(Term),
    Ever,
    Sent,
} // evaluation point | `at tick k` | `ever` | `sent` (network relation)
/// Term data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Term {
    Var(VarId),
    Const(ConstId),
    Wild,
}
/// GenSource data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GenSource {
    Value(Expr),   // Vec/Set/Map value in canonical order; Map yields (k, v)
    Lattice(Expr), // set-like lattice: a morphism (LANG-123)
    TableFn {
        f: FnId,
        inputs: Vec<Term>,
    },
    Range {
        lo: Expr,
        hi: Expr,
        kind: RangeKind, /* HalfOpen | Closed | OpenOpen | OpenClosed */
        ring_bits: Option<u16>,
    },
}
/// Pattern data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pattern {
    Var(VarId),
    Wild,
    Const(ConstId),
    Tuple(Vec<Pattern>),
    Variant {
        ty: TypeId,
        number: u32,
        fields: Vec<Pattern>,
    },
    Struct {
        ty: TypeId,
        fields: Vec<(u32, Pattern)>,
    },
}

/// Expr data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Expr {
    Term(Term),
    Param(ParamId),
    Scalar(BuiltinScalar), // $now $tick $self $incarnation $host
    Unary {
        op: UnOp,
        arg: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    }, // arithmetic is checked (BLSR004); CanonLt/CanonLe
    Call {
        f: FnRef,
        args: Vec<Expr>,
    },
    Construct {
        ty: TypeId,
        variant: Option<u32>,
        fields: Vec<Expr>,
    }, // tuple / struct / enum / Some
    Field {
        base: Box<Expr>,
        index: u32,
    },
    If {
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
    },
    Match {
        scrut: Box<Expr>,
        arms: Vec<(Pattern, Option<Expr>, Expr)>,
    },
    Collection {
        kind: CollKind,
        elems: Vec<Expr>,
    }, // Vec, Set, Map literals
    Lattice {
        op: LatOpRef,
        args: Vec<Expr>,
    }, // join, lift, threshold, morphism, reveal (NM) …
    Let {
        pat: Pattern,
        value: Box<Expr>,
        body: Box<Expr>,
    }, // fn bodies only
    Closure {
        params: Vec<VarId>,
        body: Box<Expr>,
    }, // only as an argument to built-in combinators, fn bodies only
    /// `expr` at type `ty`: a literal whose value does not decide its type (an empty collection, `None`) carries the
    /// type its context gave it. Evaluates to `expr`.
    Typed {
        ty: TypeId,
        expr: Box<Expr>,
    },
}
/// FnRef data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FnRef {
    Fn(FnId),
    Builtin(BuiltinFn),
}
/// Built-ins used by lowerings. Each is a pure function of its arguments and the tick's recorded inputs.
/// BuiltinFn data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuiltinFn {
    Prio {
        site: SiteId,
    }, // $prio(site, X̄, Ȳ) = (PRF_σc(site, fp(X̄), fp(Ȳ)), Ȳ) (SEM-084/085)
    RandPrio {
        site: SiteId,
    }, // $rprio: PRF_σnode(site, incarnation, tick, fp(X̄), fp(Ȳ)) (choose_rand!)
    Rand,
    RandFloat,
    RandRange, // PRF_σnode("rand", incarnation, tick, fp(k̄)) (LANG-175)
    /// `error("message")` (LANGUAGE Appendix B): a located hard error (BLSR010). `ty` is the type the call stands
    /// in for; the call never returns.
    Error {
        ty: TypeId,
    },
    Route {
        role: RoleId,
    }, // rendezvous hashing over canonically ordered members (LANG-154)
    Majority {
        domain: MajorityDomain,
    }, // |s ∩ R| > |R| / 2 (LANGUAGE §11.6); FOL: quorum sort (VER-008)
    ClusterVersionAtLeast(u32), // threshold over the ClusterVersion event (SEM-092)
    ZWeight {
        rel: RelId,
    },
    ZDelta {
        rel: RelId,
    },
    Unwrap {
        rel: RelId,
    },
    Entries, // LANGUAGE §11.10 (ENG-070)
    PrincipalOf,
    RoleOf,
    Size {
        role: RoleId,
    },
    Len,
    IntCast(blossom_value::types::IntTy), // `x as T` from an integer or an `f64`: out of range is BLSR004 (LANGUAGE §5.1)
    Lib(LibFn),                           // the built-in library (LANGUAGE Appendix B); the receiver, if any, first
    Concat,                               // `a ++ b` on String, Bytes or Vec (LANGUAGE §9.12)
    Contains,
    Keys,
    Values,
    ToString,
    Hash64,
    Fingerprint,
    /// `x as f64` from an integer (the nearest double) or an `f64` (LANGUAGE §5.1).
    FloatCast,
}

/// The work one evaluation of a pure function may do (LANGUAGE §16.1, BLSR012): closure applications plus the
/// elements of every `range` built as a vector, counted from a call made outside any function to its return (a
/// `range` built outside a function counts alone). Functions have no recursion, but a fold over `range(0, u64::MAX)`
/// would still never end; past this budget the evaluation is a located hard error. Both evaluators count the same
/// steps, so they fail at the same valuation.
pub const FN_STEP_BUDGET: u64 = 10_000_000;

/// A function or method of the built-in library (LANGUAGE Appendix B). The receiver, if any, is the first argument;
/// a combinator's closure is the last. Every one is total: a position past the end is `None`, never an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum LibFn {
    /// `range(lo, hi)`: the `u64`s from `lo` up to, not including, `hi` (empty when `hi <= lo`).
    Range,
    /// `v.get(i) -> Option<T>`.
    VecGet,
    /// `v.first()`, `v.last() -> Option<T>`.
    VecFirst,
    VecLast,
    /// `v.push(x)`: a copy with `x` appended.
    VecPush,
    /// `v.concat(w)`.
    VecConcat,
    /// `v.is_empty()`.
    VecIsEmpty,
    /// `v.reverse()`.
    VecReverse,
    /// `v.flatten()` on a `Vec<Vec<T>>`: the inner vectors' elements, in order.
    VecFlatten,
    /// `v.enumerate() -> Vec<(u64, T)>`.
    VecEnumerate,
    /// `v.map(|x| e)`, `v.filter(|x| b)`, `v.filter_map(|x| o)`, `v.all(|x| b)`, `v.any(|x| b)`.
    VecMap,
    VecFilter,
    VecFilterMap,
    VecAll,
    VecAny,
    /// `v.fold(init, |acc, x| e)`: left to right.
    VecFold,
    /// `v.scan(init, |acc, x| e) -> Vec<A>`: a fold's accumulator after each element, left to right.
    VecScan,
    /// `v.scan_while(init, |acc, x| e) -> Vec<A>`, `e: Option<A>`: as `scan`, stopping at the first element whose
    /// step is `None` (the elements after it are not visited; over a `range`, not walked).
    VecScanWhile,
    /// `v.to_set() -> Set<T>`: the elements, each once.
    VecToSet,
    /// `v.to_map() -> Map<K, V>` on a `Vec<(K, V)>`: each key with the value of its last pair.
    VecToMap,
    /// `m.get(k) -> Option<V>`.
    MapGet,
    /// `o.is_some()`, `o.is_none()`, `o.unwrap_or(d)`.
    OptIsSome,
    OptIsNone,
    OptUnwrapOr,
    /// `o.map(|x| e)`, `o.and_then(|x| o2)`.
    OptMap,
    OptAndThen,
    /// `b.slice(lo, hi) -> Option<Bytes>`: `None` unless `lo <= hi <= len`.
    BytesSlice,
    /// `b.concat(c)`.
    BytesConcat,
    /// `s.split_whitespace() -> Vec<String>`: the non-empty runs between Unicode whitespace.
    StrSplitWhitespace,
    /// `s.to_lowercase()`: Unicode lowercase mapping.
    StrToLowercase,
    /// `s.to_utf8() -> Bytes`.
    StrToUtf8,
    /// `s.trim() -> String`: without leading and trailing Unicode whitespace.
    StrTrim,
    /// `n.to_string() -> String` on any integer: its decimal digits, with a `-` when negative.
    IntToString,
    /// `s.parse_i64() -> Option<i64>`: the decimal integer `s` spells (an optional sign, then digits), `None` for
    /// anything else or a value outside `i64`.
    StrParseI64,
    /// `Duration::from_millis(n: i64) -> Duration` (BLSR004 when it does not fit).
    DurationFromMillis,
    /// `d.as_millis() -> i64`: whole milliseconds, truncated toward zero.
    DurationAsMillis,
    /// `t.as_millis() -> i64` on an `Instant`: whole milliseconds since the deployment epoch (the Unix epoch in a
    /// deployment), truncated toward zero.
    InstantAsMillis,
    /// `b.from_utf8() -> Option<String>`: `None` unless `b` is valid UTF-8.
    BytesFromUtf8,
    /// `b.u8_at(pos)`, `b.i8_at(pos)`, `b.u16_be_at(pos)`, … `b.i64_be_at(pos) -> Option<T>`: the big-endian integer
    /// of type `T` at `pos`, `None` past the end. `T` is one of the 8-, 16-, 32- and 64-bit integer types.
    BytesRead(blossom_value::types::IntTy),
    /// `b.put_u8(pos, x)`, … `b.put_i64_be(pos, x) -> Option<Bytes>`: a copy with the big-endian `x` written at
    /// `pos`, `None` unless it fits inside `b`.
    BytesPut(blossom_value::types::IntTy),
    /// `Bytes::from_u8(x)`, … `Bytes::from_i64_be(x)`: the big-endian bytes of `x`.
    BytesFrom(blossom_value::types::IntTy),
    /// `b.uvarint_at(pos) -> Option<(u64, u64)>`: an unsigned LEB128 varint and the position after it; `None` when it
    /// is truncated, longer than 10 bytes, or does not fit a `u64`.
    BytesUvarintAt,
    /// `b.varint_at(pos) -> Option<(i64, u64)>`: a zigzag varint (as `uvarint_at`, then zigzag-decoded).
    BytesVarintAt,
    /// `Bytes::uvarint(x: u64)`: the shortest unsigned LEB128 encoding.
    BytesUvarint,
    /// `Bytes::varint(x: i64)`: the shortest zigzag LEB128 encoding.
    BytesVarint,
    /// `Bytes::empty()`.
    BytesEmpty,
    /// `Bytes::join(v: Vec<Bytes>)`: the concatenation, in order.
    BytesJoin,
    /// `Blob::of(b: Bytes) -> Blob`: the handle of `b` (its BLAKE3 hash and length); the evaluator hands the bytes to
    /// the host with the tick's output (FOREIGN-PROTOCOLS §5).
    BlobOf,
    /// `blob.read(lo, hi) -> Option<Bytes>`: bytes `lo..hi` of the blob, `None` unless `lo <= hi <= len`.
    BlobRead,
    /// `abs(x)` on an integer (BLSR004 when `-x` does not fit) or an `f64`.
    Abs,
    /// `min(a, b)`, `max(a, b)` of two integers of one type or two `f64`s (the values' order).
    Min,
    Max,
    /// `clamp(x, lo, hi)`: `max(lo, min(x, hi))`; BLSR004 when `lo > hi`.
    Clamp,
    /// `x.sqrt()`, `x.floor()`, `x.ceil()`, `x.round()` (half away from zero), `x.trunc()` on an `f64` (LANGUAGE §5.1:
    /// correctly rounded, canonical).
    FloatSqrt,
    FloatFloor,
    FloatCeil,
    FloatRound,
    FloatTrunc,
    /// `x.to_string()` on an `f64`: the shortest decimal that reads back as `x`, without an exponent.
    FloatToString,
}

/// Construct data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Construct {
    pub id: ConstructId,
    pub kind: ConstructKind,
    pub rules: Vec<RuleId>,  // the normative expansion
    pub rels: Vec<RelId>,    // generated relations (all Origin::Generated, provenance-transparent)
    pub surface: SurfaceRef, // module, label, statement text, span: what reports show
}
/// SurfaceRef data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceRef {
    pub module: QualName,
    pub label: Option<Symbol>,
    pub stmt: Option<Arc<str>>,
    pub span: Span,
}

/// ConstructKind data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConstructKind {
    // Provenance-only groupings: never replaced; the planner may fuse them (§3.7).
    HandlerHeader {
        when: RelId,
    },
    Block {
        rel: RelId,
    },
    ViewAlternatives {
        view: RelId,
    },
    Projection {
        rel: RelId,
        proj: RelId,
    }, // `r$p1` for existential wildcards (§9.3)
    NotExists {
        helper: RelId,
    },
    Outer,
    Any,
    Forall {
        fa: RelId,
        miss: RelId,
        /// The domain is closed (a static relation or a role's members): the quantifier is then monotone in its body
        /// (LANGUAGE §9.8), although its expansion negates.
        closed: bool,
    },
    DeltaRead {
        rel: RelId,
        prev: RelId,
    },
    Interpose,
    Localize,
    /// A Molly `.ded` relation's Dedalus meaning (LANGUAGE §21.1): the generated channel that carries its `@async`
    /// derivations or the generated input that carries its `@k` facts, with the rule that feeds it into `rel`.
    DedRelation {
        rel: RelId,
    },
    /// A role's member relation `R$members(N)` (LANGUAGE §6.10), whose rows come from the deployment.
    Members {
        role: RoleId,
    },
    Invariant {
        id: InvariantId,
    },
    SpecOracle,
    Service {
        id: ServiceId,
    },
    // Constructs with a native implementation (ARCH-02). Each spec carries exactly what the native operator
    // needs; the expansion carries the meaning.
    Persist {
        rel: RelId,
        del: Option<RelId>,
    }, // ENG-003
    Identity {
        rel: RelId,
    }, // persistent lattice (SEM-104)
    Upsert {
        rel: RelId,
        staging: RelId,
        del: RelId,
    }, // SEM-051 keyed staging (LANGUAGE §8.2)
    Resolve(ResolveSpec),         // LANG-117: relation or statement level
    Choose(ChooseSpec),           // LANG-108/114/115, ENG-068
    MultiChoose(MultiChooseSpec), // LANG-116, ENG-075
    Index(IndexSpec),             // LANG-097, ENG-072; also top!/limit!/percentile
    Seq(SeqSpec),                 // LANG-098
    FoldOrdered(FoldSpec),        // LANG-110, ENG-073; reduce!, non-C/A UDAs
    ArgExt(ArgExtSpec),
    AggDefault(AggDefaultSpec), // LANG-103/106
    SoftTable(SoftSpec),        // LANG-048, CR-17
    Sealed {
        rel: RelId,
        sealed: RelId,
    },
    Range {
        rel: RelId,
        col: ColIdx,
    }, // LANG-049/050
    LogicalTimer {
        rel: RelId,
        every: u64,
    }, // LANGUAGE §15.2
    Seal(SealSpec),         // LANGUAGE §14.4: producer log, votes, digests
    Snapshot(SnapshotSpec), // LANG-139
    Wrapped(WrapSpec),      // LANG-158, DIST-015/016
    LatticeFold {
        cell: RelId,
    }, // `lset{…}`, `lmax{…}` in expressions
    Finality(FinalitySpec), // ANA-121: M⁻/M⁺ bounds programs (P1)
    Quorum(QuorumSpec),     // spec `quorum v in R { … }` (VER-008)
}
/// ChooseSpec data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChooseSpec {
    pub site: SiteId,
    pub candidates: RelId,    // X̄ ∪ Ȳ (∪ cost) candidate relation (ENG-068 input)
    pub group: Vec<ColIdx>,   // X̄
    pub choice: Vec<ColIdx>,  // Ȳ
    pub policy: ChoosePolicy, // Priority | Least { cost } | Most { cost } | Rand
    // Rand: PRF key = (σ_node, incarnation, tick, site, fp(X̄), fp(Ȳ)) (SEM-085)
    pub sticky: Option<StickySpec>, // `sticky`: the `held` relation carried with @next; `durable` makes it durable
    pub overrides: Option<RelId>,   // `__choice` override input (TEST-012), simulation and replay only
    pub output: RelId,              // `chosen`
}
/// FinalitySpec data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinalitySpec {
    pub output: RelId,
    pub lower: Vec<RuleId>, // M⁻: positive atoms over L(R); `notin A` only if A ∉ M⁺ (ANA-121)
    pub upper: Vec<RuleId>, // M⁺: positive atoms over Up(R), demand-driven; `notin A` if A ∉ M⁻
    pub status: RelId,      // (tuple, status) with status ∈ {provisional, final_present, final_absent}
}
/// Site data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Site {
    pub id: SiteId,
    pub stable: Arc<str>, // "M::N::op#k" or "M::rel::resolve" (LANGUAGE §4.3)
    pub key: u64,         // SipHash-1-3(stable): the PRF domain separator
    pub kind: SiteKind,   // Choose | ChooseLeast | ChooseMost | ChooseRand | Sticky | Seq | Resolve | Route
    pub construct: ConstructId,
}

/// DotStoreKind data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DotStoreKind {
    Set(TypeId),
    Map(TypeId, LatticeTypeId),
}
/// TombKind data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TombKind {
    RemoveWins,
    AddWins,
}
/// ExternLatticeRef data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternLatticeRef {
    pub path: Arc<str>,
    pub codec: Arc<str>,
}
/// GroupDef data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupDef {
    pub id: GroupTypeId,
    pub ctor: GroupCtor,
    pub ring: bool,
}
/// GroupCtor data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GroupCtor {
    Z,
    Zn(u64),
    ZSet(TypeId),
    Tuple(Vec<GroupTypeId>),
    Map(TypeId, GroupTypeId),
    User {
        name: QualName,
        zero: FnId,
        add: FnId,
        neg: FnId,
        mul: Option<FnId>,
    },
}
/// ParamDecl data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParamDecl {
    pub id: ParamId,
    pub name: QualName,
    pub ty: TypeId,
    pub default: Option<ConstId>,
    pub span: Span,
}
/// InvariantDecl data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvariantDecl {
    pub id: InvariantId,
    pub name: QualName,
    pub action: ViolationAction,
    pub span: Span,
}
/// ViolationAction data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViolationAction {
    Record,
    Warn,
    Abort,
}
/// MigrationDecl data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationDecl {
    pub from: u32,
    pub rules: Vec<Rule>,
    pub span: Span,
}
/// TranslationDecl data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslationDecl {
    pub channel: RelId,
    pub version: u32,
    pub direction: TranslationDirection,
    pub rules: Vec<Rule>,
    pub span: Span,
}
/// TranslationDirection data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TranslationDirection {
    To,
    From,
}
/// InterfaceDir data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InterfaceDir {
    Input,
    Output,
}
/// Placement data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Placement {
    Shared,
    Role(RoleId),
}
/// Origin data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Origin {
    User(Span),
    Generated { construct: ConstructId },
}
/// WeightKind data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WeightKind {
    ZSet,
    Bag,
}
/// TimerClock data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimerClock {
    Physical,
    Logical,
}
/// ChannelForm data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChannelForm {
    Direction { src: RoleId, dst: RoleId },
    Column,
    NodeToNode,
}
/// FaultModel data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FaultModel {
    Lossy,
    LossyDelayed,
    Reliable,
    ReliableOrdered,
}
/// PartitionSpec data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionSpec {
    pub key: Expr,
    pub over: Option<RelId>,
}
/// SealDecl data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealDecl {
    pub key: Vec<ColIdx>,
    pub producers: RelId,
}
/// WrapperKind data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WrapperKind {
    Dots,
    Cumulative,
    Tree,
}
/// AclSpec data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AclSpec {
    Inferred,
    /// `#[accept(…)]` (LANGUAGE §18.3): narrows the inferred ACL to these sources.
    Explicit(AclExplicit),
}
/// The sources an explicit ACL admits: the listed roles' nodes, client sessions when `external`, and when
/// `principal_in` is set only senders whose principal is in that unary relation of the receiving node.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AclExplicit {
    pub roles: Vec<RoleId>,
    pub external: bool,
    pub principal_in: Option<RelId>,
}
/// MaterializeHint data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MaterializeHint {
    Always,
    Never,
    Auto,
}
/// FiniteSpec data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FiniteSpec {
    pub cols: Vec<ColIdx>,
    pub bound: u64,
}
/// OrderSpec data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderSpec {
    pub keys: Vec<OrderKey>,
}
/// OrderKey data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderKey {
    pub term: Term,
    pub descending: bool,
}
/// RangeKind data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RangeKind {
    HalfOpen,
    Closed,
    OpenOpen,
    OpenClosed,
}
/// BuiltinScalar data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuiltinScalar {
    Now,
    Tick,
    SelfNode,
    Incarnation,
    Host,
}
/// UnOp data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnOp {
    Not,
    Neg,
    BitNot,
}
/// BinOp data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    CanonLt,
    CanonLe,
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}
/// CollKind data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CollKind {
    Vec,
    Set,
    Map,
}
/// LatOpRef data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LatOpRef {
    pub lattice: LatticeTypeId,
    pub op: Symbol,
}
/// MajorityDomain data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MajorityDomain {
    Role(RoleId),
    Relation(RelId),
}
/// ChoosePolicy data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChoosePolicy {
    Priority,
    Least { cost: ColIdx },
    Most { cost: ColIdx },
    Rand,
}
/// StickySpec data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StickySpec {
    pub held: RelId,
    pub release: Option<RelId>,
    pub durable: bool,
}
/// SiteKind data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SiteKind {
    Choose,
    ChooseLeast,
    ChooseMost,
    ChooseRand,
    Sticky,
    Seq,
    Resolve,
    Route,
}
/// ResolveSpec data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolveSpec {
    pub rel: RelId,
    pub candidates: RelId,
    pub output: RelId,
    pub group: Vec<ColIdx>,
    pub policy: ResolvePolicy,
    pub site: Option<SiteId>,
}
/// ResolvePolicy data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolvePolicy {
    Least(ColIdx),
    Most(ColIdx),
    Choose,
    Merge(FnId),
    Reject,
    /// `resolve prefer(…)` (LANGUAGE §10.7): the candidates are one tick's ranked writes; those of the least rank
    /// (the candidates' column `rank`) per group survive.
    Prefer {
        rank: ColIdx,
    },
}
/// MultiChooseSpec data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiChooseSpec {
    pub site: SiteId,
    pub candidates: RelId,
    pub dependencies: Vec<FunctionalDependency>,
    pub output: RelId,
}
/// FunctionalDependency data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionalDependency {
    pub key: Vec<ColIdx>,
    pub value: Vec<ColIdx>,
}
/// IndexSpec data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexSpec {
    pub input: RelId,
    pub output: RelId,
    pub group: Vec<ColIdx>,
    pub order: OrderSpec,
    pub mode: IndexMode,
}
/// IndexMode data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum IndexMode {
    Index,
    Top(u64),
    Limit(u64),
    Percentile { num: u32, den: u32 },
}
/// SeqSpec data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeqSpec {
    pub site: SiteId,
    pub input: RelId,
    pub output: RelId,
    pub assigned: RelId,
    pub counter: RelId,
    pub durable: bool,
}
/// FoldSpec data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoldSpec {
    pub input: RelId,
    pub output: RelId,
    pub group: Vec<ColIdx>,
    pub order: OrderSpec,
    pub init: ConstId,
    pub step: FnId,
    pub finish: Option<FnId>,
}
/// ArgExtSpec data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArgExtSpec {
    pub input: RelId,
    pub output: RelId,
    pub group: Vec<ColIdx>,
    pub cost: ColIdx,
    pub maximum: bool,
}
/// AggDefaultSpec data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggDefaultSpec {
    pub input: RelId,
    pub drivers: RelId,
    pub output: RelId,
    pub group: Vec<ColIdx>,
    pub default: ConstId,
}
/// SoftSpec data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SoftSpec {
    pub rel: RelId,
    pub storage: RelId,
    pub ttl: Option<Duration>,
    pub max: Option<u64>,
}
/// SealSpec data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealSpec {
    pub rel: RelId,
    pub log: RelId,
    pub votes: RelId,
    pub sealed: RelId,
    pub key: Vec<ColIdx>,
    pub producers: RelId,
}
/// SnapshotSpec data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotSpec {
    pub input: RelId,
    pub output: RelId,
    pub progress: RelId,
    pub threshold: ConstId,
}
/// WrapSpec data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrapSpec {
    pub channel: RelId,
    pub output: RelId,
    pub context: RelId,
    pub outbuf: RelId,
    pub kind: WrapperKind,
}
/// QuorumSpec data in the Dedalus core IR.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuorumSpec {
    pub domain: RelId,
    pub votes: RelId,
    pub output: RelId,
    pub group: Vec<ColIdx>,
}

impl Expr {
    /// Whether evaluating this expression can never raise a runtime error, for any values of its variables:
    /// variables, constants, scalars, comparisons, boolean and bitwise connectives, construction, field access and
    /// `if` over such expressions. Arithmetic (it is checked, BLSR004), shifts, casts, calls, matches, collection
    /// literals and lattice operations can fail. This decides the order of a rule body's checks (LANGUAGE §9.14),
    /// which both evaluators follow.
    pub fn cannot_fail(&self) -> bool {
        match self {
            Expr::Term(_) | Expr::Param(_) | Expr::Scalar(_) => true,
            Expr::Unary { op, arg } => matches!(op, UnOp::Not | UnOp::BitNot) && arg.cannot_fail(),
            Expr::Binary { op, lhs, rhs } => {
                matches!(
                    op,
                    BinOp::Eq
                        | BinOp::Ne
                        | BinOp::Lt
                        | BinOp::Le
                        | BinOp::Gt
                        | BinOp::Ge
                        | BinOp::CanonLt
                        | BinOp::CanonLe
                        | BinOp::And
                        | BinOp::Or
                        | BinOp::BitAnd
                        | BinOp::BitOr
                        | BinOp::BitXor
                ) && lhs.cannot_fail()
                    && rhs.cannot_fail()
            }
            Expr::Construct { fields, .. } => fields.iter().all(Expr::cannot_fail),
            Expr::Field { base, .. } => base.cannot_fail(),
            Expr::Typed { expr, .. } => expr.cannot_fail(),
            Expr::If { cond, then, els } => cond.cannot_fail() && then.cannot_fail() && els.cannot_fail(),
            Expr::Call { .. }
            | Expr::Match { .. }
            | Expr::Collection { .. }
            | Expr::Lattice { .. }
            | Expr::Let { .. }
            | Expr::Closure { .. } => false,
        }
    }

    /// Whether evaluating this expression can depend on a tick's recorded time, incarnation or PRF stream.
    pub fn time_varying(&self) -> bool {
        match self {
            Self::Scalar(BuiltinScalar::Now | BuiltinScalar::Tick | BuiltinScalar::Incarnation) => true,
            Self::Call {
                f:
                    FnRef::Builtin(
                        BuiltinFn::Rand | BuiltinFn::RandFloat | BuiltinFn::RandRange | BuiltinFn::RandPrio { .. },
                    ),
                ..
            } => true,
            Self::Unary { arg, .. } | Self::Field { base: arg, .. } => arg.time_varying(),
            Self::Binary { lhs, rhs, .. } => lhs.time_varying() || rhs.time_varying(),
            Self::Call { args, .. }
            | Self::Construct { fields: args, .. }
            | Self::Collection { elems: args, .. }
            | Self::Lattice { args, .. } => args.iter().any(Self::time_varying),
            Self::If { cond, then, els } => cond.time_varying() || then.time_varying() || els.time_varying(),
            Self::Match { scrut, arms } => {
                scrut.time_varying()
                    || arms
                        .iter()
                        .any(|(_, guard, body)| guard.as_ref().is_some_and(Self::time_varying) || body.time_varying())
            }
            Self::Let { value, body, .. } => value.time_varying() || body.time_varying(),
            Self::Closure { body, .. } => body.time_varying(),
            Self::Typed { expr, .. } => expr.time_varying(),
            Self::Term(_) | Self::Param(_) | Self::Scalar(_) => false,
        }
    }
}

impl ConstructKind {
    /// Stable constructor name for IR displays and canonical ordering.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::HandlerHeader { .. } => "HandlerHeader",
            Self::Block { .. } => "Block",
            Self::ViewAlternatives { .. } => "ViewAlternatives",
            Self::Projection { .. } => "Projection",
            Self::NotExists { .. } => "NotExists",
            Self::Outer => "Outer",
            Self::Any => "Any",
            Self::Forall { .. } => "Forall",
            Self::DeltaRead { .. } => "DeltaRead",
            Self::Interpose => "Interpose",
            Self::Localize => "Localize",
            Self::DedRelation { .. } => "DedRelation",
            Self::Members { .. } => "Members",
            Self::Invariant { .. } => "Invariant",
            Self::SpecOracle => "SpecOracle",
            Self::Service { .. } => "Service",
            Self::Persist { .. } => "Persist",
            Self::Identity { .. } => "Identity",
            Self::Upsert { .. } => "Upsert",
            Self::Resolve(..) => "Resolve",
            Self::Choose(..) => "Choose",
            Self::MultiChoose(..) => "MultiChoose",
            Self::Index(..) => "Index",
            Self::Seq(..) => "Seq",
            Self::FoldOrdered(..) => "FoldOrdered",
            Self::ArgExt(..) => "ArgExt",
            Self::AggDefault(..) => "AggDefault",
            Self::SoftTable(..) => "SoftTable",
            Self::Sealed { .. } => "Sealed",
            Self::Range { .. } => "Range",
            Self::LogicalTimer { .. } => "LogicalTimer",
            Self::Seal(..) => "Seal",
            Self::Snapshot(..) => "Snapshot",
            Self::Wrapped(..) => "Wrapped",
            Self::LatticeFold { .. } => "LatticeFold",
            Self::Finality(..) => "Finality",
            Self::Quorum(..) => "Quorum",
        }
    }
}
